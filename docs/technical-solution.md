# 技术方案

本文档描述 rust-toon 的整体架构与关键技术设计，内容以当前代码为准。配置项见 [configuration.md](configuration.md)，部署操作见 [deployment.md](deployment.md)。

## 1. 项目概览

rust-toon 是一个动漫（短剧）制作管理平台，由以下部分组成：

- **后端**：Rust workspace，HTTP/WS 网关 `rust-toon-gateway`（`services/gateway`）与可横向扩容的持久媒体 worker（`services/toon-worker`），基于 axum 0.8 / tokio / SQLx（PostgreSQL）。
- **前端**：Vben Admin（5.7.0）monorepo，位于 `apps/web`，主应用包 `@vben/web-antd`（Vue + Ant Design Vue）。
- **数据与基础设施**：PostgreSQL（主存储、任务 outbox 与 SQLx 迁移）、Redis（缓存与限流，可选）、NATS JetStream（持久任务投递）、RustFS（S3 兼容对象存储）。

## 2. 仓库结构

```
├── Cargo.toml                 # Rust workspace（resolver = "3"，edition 2024）
├── services/gateway/          # 网关入口：路由组装、中间件、启动流程
├── services/toon-worker/      # 分布式媒体任务、租约接管、对象清理与探针
├── crates/
│   ├── framework/             # 框架层（与业务无关）
│   │   ├── common/            # 配置、健康检查、统一响应、日志、HTTP 服务启动
│   │   ├── database/          # PostgreSQL 连接池与 SQLx 迁移
│   │   ├── redis/             # Redis 客户端、JSON 缓存、限流中间件
│   │   ├── security/          # JWT、密码散列、认证/鉴权、权限模型
│   │   ├── web/               # AppError、CORS/RequestId/Trace 等 Web 层
│   │   ├── mq/                # NATS JetStream 连接、消息信封与显式 ACK
│   │   ├── tenant/            # 租户上下文扩展点（预留）
│   │   └── telemetry/         # JSON/text 日志、Prometheus 指标与可选 OTLP trace
│   └── modules/               # 业务模块，每模块一对 crate：
│       ├── system-api/server  #   账号、角色、菜单、字典、部门、租户、日志等管理域
│       ├── infra-api/server   #   参数配置、文件、定时任务、代码生成、监控等基础设施域
│       ├── ai-api/server      #   AI 模型配置、对话、绘画、音乐、知识库等
│       ├── media-api/server   #   媒体资产 CRUD
│       └── toon-api/server    #   toonflow 动漫制作业务（项目/剧本/分镜/视频/Agent）
├── apps/web/                  # Vben Admin 前端 monorepo
├── sql/
│   ├── postgresql/            # SQLx 迁移链（0001 起，唯一事实来源）
│   └── bootstrap/current.sql  # 参考用 pg_dump 快照，应用从不加载
├── script/                    # docker-compose、迁移测试、备份/恢复等脚本
├── deploy/                    # 环境变量、systemd、容器与 Kubernetes 部署样例
└── storage/uploads/           # 旧版本本地文件迁移来源（运行时统一使用对象存储）
```

`*-api` crate 放共享类型（能力描述、请求/响应模型、平台枚举），`*-server` crate 放路由与业务实现。`*-server` 不单独起进程，全部由 gateway 以库形式链接、合并路由。

## 3. 整体架构

### 3.1 模块化网关与持久媒体 Worker

所有业务模块编译进同一个网关二进制（`cargo run -p rust-toon-gateway`）。gateway 在 `services/gateway/src/main.rs` 中：

1. `init_telemetry("gateway")` 初始化 text/JSON 日志、进程内 Prometheus 指标与可选 OTLP trace；未配置 OTLP endpoint 时不会连接外部 collector。
2. `DatabaseConfig::from_env()` + `connect()` 建立 PostgreSQL 连接池，随后 `migrate()` 执行 SQLx 迁移。
3. `RedisConfig::from_env()` 可选连接 Redis；连接失败仅告警并降级（缓存与限流关闭），不阻断启动。
4. `SecurityConfig::from_env()` 构造 `TokenService`（JWT HS256）。
5. 依次构造 `SystemState`（带 Redis 缓存）、`InfraState`、`AiState`、`ToonState`、`MediaState`；`InfraState` 同样持有 `TokenService`，管理路由在后端强制认证和 RBAC。
6. `system_state.bootstrap().await?` 启动校验：确认数据库中存在启用的 `super_admin` 角色用户，否则拒绝启动（`crates/modules/system-server/src/bootstrap.rs`）。
7. 合并五个模块的路由，追加 `/`、`/openapi.json`、兼容存活接口 `/health`、新探针 `/livez`、`/readyz`、Prometheus `/metrics`，以及 404 fallback。
8. 全局中间件（自内向外生效）：数据库认证中间件 `authenticate_from_database` → 审计中间件 `audit::record`；若 Redis 可用，再加全局限流中间件 `rate_limit`。
9. `apply_web_layers` 追加 RequestId（`x-request-id`）、Trace、以及可选的宽松 CORS。
10. `serve()` 绑定 `GATEWAY_HOST:GATEWAY_PORT`（默认 `0.0.0.0:8080`），支持 Ctrl+C / SIGTERM 优雅停机。

最终成片合并与对象清理由 `rust-toon-worker` 执行。Gateway 在同一 PostgreSQL 事务中写入用户任务和 `distributed_jobs` outbox；Worker 将待投递行发布到 JetStream，并以数据库 lease、heartbeat 与 fencing token 保证多副本竞争、进程崩溃接管和过期结果拒绝。PostgreSQL 是任务真相源，JetStream 丢失消息后可由 outbox 重建。当前媒体 Worker 可运行 N 个副本；Agent/Workflow 的实时运行表仍含进程内协调，因此 Gateway 暂保持 1 个副本。

### 3.2 统一响应与错误

- 成功响应统一为 `ApiResponse { code, data, message }`（`framework/common/src/response.rs`）。
- 错误统一为 `AppError`（`framework/web/src/error.rs`），按类型映射 HTTP 状态码。

### 3.3 认证与鉴权

`framework/security`：

- **密码**：支持 bcrypt（兼容芋道/Yudao 历史数据）与 argon2（`password.rs`）。
- **访问令牌**：JWT，HS256，含 `iss`/`aud` 校验，默认 TTL 900 秒（`token.rs`）。
- **令牌落库**：登录时向 `system_oauth2_access_token` / `system_oauth2_refresh_token` 写入记录（`system-server/src/oauth2_token.rs`），支持 refresh-token 轮换与 logout 吊销。

gateway 全局挂 `authenticate_from_database`（`system-server/src/database_auth.rs`）：带 `Bearer` 的请求会校验 JWT 并核对 `system_oauth2_access_token` 中令牌仍然有效，然后用数据库中最新的角色/权限替换 JWT 内嵌的授权信息（权限实时生效）；不带令牌的请求直接放行，由各路由自己的 `authenticate` 中间件决定是否拒绝。各模块的受保护路由通过 `route_layer(from_fn_with_state(tokens, authenticate))` 要求登录，再用 `require(user, "xxx:yyy:zzz")` 做权限码校验（`super_admin` 角色直接放行）。

### 3.4 审计

- `services/gateway/src/audit.rs`：全局中间件，将业务 HTTP 请求写入 `infra_api_access_log`（方法、路径、UA、耗时等）；高频 `/health`、`/livez`、`/readyz` 探针不写审计表。
- `system-server/src/audit.rs`：业务操作日志 `system_operate_log` 与登录日志 `system_login_log`。

### 3.5 限流与缓存

- 限流：`framework/redis/src/rate_limit.rs`，按 `IP + 请求方法 + 路径` 维度在 Redis 计数（窗口默认 60 秒、上限默认 300 次），`/health`、`/livez`、`/readyz` 豁免；Redis 故障时放行并告警。客户端 IP 优先取 `x-forwarded-for`。
- 缓存：`RedisClient` 提供 `get_json` / `set_json` / `delete_by_pattern` 等，键带前缀（默认 `rust-toon`）。

### 3.6 分布式治理与可观测边界

当前部署单元是“模块化 Gateway + 持久媒体 Worker”，而不是把每个 CRUD 模块拆成独立进程。认证、文件、字典和配置作为 workspace 内的公共模块复用；只有需要独立扩缩和故障隔离的长耗时媒体任务进入 Worker。这样保持 Toonflow 的 API 与操作方式不变，也避免在没有独立数据所有权前形成共享数据库的伪微服务。

- **发现与配置**：Kubernetes 服务发现继续使用 Service/DNS；静态参数和密钥继续由 typed env + ConfigMap/Secret 管理。运行时非敏感参数由三节点 r-nacos Raft 集群保存并通过官方 Rust `nacos-sdk` 长连接推送，SDK 启动时加载磁盘缓存、断线后持续重连；应用对 JSON schema 和数值边界校验后才原子替换当前快照，非法版本保留 last-known-good。Gateway 限流额度/窗口以及 Worker dispatcher、scheduler、reaper、cleanup 周期已接入热更新，数据库地址、JWT、密钥、端口和并发/租约等结构性参数仍要求重启。
- **容错**：AI、对象存储、媒体下载和 NATS 发布统一经过 Tower bulkhead/circuit breaker/timeout；只有 GET/PUT/DELETE 等幂等 HTTP 方法允许退避重试，POST/PATCH 不会被隐式重放。Worker 另有 lease、heartbeat、fencing、任务级重试、接管与优雅排空。
- **事务一致性**：视频任务采用本地 PostgreSQL 事务 + outbox、JetStream 至少一次投递、幂等/围栏提交和对象清理补偿；不使用 XA/2PC。PostgreSQL 始终是任务真相源。
- **观测**：Gateway 与 Worker 支持 JSON stdout、OpenMetrics `/metrics` 和 OTLP gRPC trace。HTTP 入站提取、HTTP 出站注入 W3C `traceparent`/`tracestate`；上下文同时持久化到 PostgreSQL Outbox、NATS header/信封并由 Worker 恢复父 span。`deploy/k8s` 内置 Prometheus/Alertmanager/Grafana/Loki/Tempo/Collector，`deploy/logging-agent` 用 Vector 逐节点采集日志；已有托管平台时可只替换 exporter/datasource。
- **副本边界**：媒体 Worker 可以水平扩容；Agent/Workflow 活跃运行注册表仍含进程内状态，因此 Gateway 固定单副本。`deploy/k8s` 的 HPA 只作用于 Worker，不能把当前清单描述为 Gateway 高可用。

## 4. 数据库与迁移

- 迁移目录 `sql/postgresql/` 在编译期由 `sqlx::migrate!("../../../sql/postgresql")` 嵌入 `framework-database`（`database/src/postgres.rs`）。`0001_initial.sql` 是完整基线，后续变更以只增不改的编号迁移追加，新数据库由 Gateway 自动执行完整迁移链；当前迁移清单以目录内容和迁移测试为准，不在架构文档中重复维护版本上限。
- `migrate()` 启动时自动执行；执行前有保护：若数据库里已有业务表但没有 `_sqlx_migrations` 历史表，则拒绝运行，避免覆盖未知数据库。
- `sql/bootstrap/current.sql` 仅是参考快照，应用从不加载。
- 迁移变更流程（新增编号迁移、保持幂等、跑 `script/test-database-migrations.sh`、更新 `crates/framework/database/tests/migrations.rs` 断言）见根 `AGENTS.md` 与 [deployment.md](deployment.md)。

## 5. 业务模块

### 5.1 system（`crates/modules/system-server`）

管理域：认证（login / refresh-token / logout / me / get-permission-info）、用户、角色、菜单、部门、岗位、字典、通知公告、租户、地区（内置 `area.csv`）、社交用户、登录/操作日志等，路由前缀 `/system/*`。公开路由仅有 `/system/capabilities`、`/system/auth/login`、租户简单列表等少数几个（`transport.rs`）。

### 5.2 infra（`crates/modules/infra-server`）

基础设施域：参数配置、数据源配置、文件与文件配置、分布式定时任务（job / job-log）、代码生成（codegen）、API 访问日志、监控（`monitor.rs`，上报 `RUST_ENV`）等，路由前缀 `/infra/*`。除能力探针和文件读取外，管理路由全部要求登录，并按现有 `infra:*` 权限码在后端授权。上传有服务端体积上限并使用不可猜对象名；公开 `GET /upload/{*path}` 只允许栅格图片 inline，其余类型强制 attachment，同时返回 CSP 与 `nosniff`。`infra_job` 由所有 Worker 通过 PostgreSQL 行锁协调到期触发，再复用 outbox/JetStream/lease/fencing 执行；仓库内置安全诊断 Handler，新增业务 job kind 必须显式注册 Rust Handler。

### 5.3 ai（`crates/modules/ai-server`）

AI 能力域：

- **模型配置**存于 `ai.model_configs` 表（platform / type / model / api_key / url / config），CRUD 路由 `/ai/model/*`。模型类型：chat、image、speech、video、embedding、rerank、transcription、music。
- **Provider 体系**：`AiModelFactory`（`factory.rs`）按平台分发到 `provider/` 下的实现——OpenAI 兼容协议 Provider（覆盖 OpenAI、通义、星火、DeepSeek、火山引擎、混元、硅基流动、MiniMax、Moonshot、百川、阶跃、文心、智谱、Grok、Ollama、OpenAICompatible），以及 Anthropic、Gemini、AzureOpenAI、火山引擎媒体生成等专用 Provider；未实现的平台显式报错。`AiPlatform` 枚举见 `crates/modules/ai-api`。
- **子模块**：`chat`（对话/会话）、`chat_role`、`media`（图片/音乐生成，含 10 秒间隔的音乐任务轮询后台任务）、`midjourney`（含后台同步任务）、`knowledge`（知识库）、`tools`、`write`、`vector`。

### 5.4 media（`crates/modules/media-server`）

媒体资产 CRUD：`/media/assets` 及 `/media/capabilities`。

### 5.5 toon（`crates/modules/toon-server`）

视频生成的基础验收由 `toon.video_quality` 持久任务承担：Gateway 归档视频并提交 outbox，Worker 执行媒体检查，只有检查通过才将新视频标记为生成成功。检查报告和最终生成请求分别保存在 `toonflow.videos.generation_context` 的 `quality`、`request` 字段。失败报告保留视频供审片，黑场/冻结提醒不自动判定创作失败。旧视频可手动发起检查。

严格尾帧衔接只接受通过基础检查的前片，本次批量生成绑定本次前片版本，前片失败会停止依赖镜头。基础验收不覆盖人物身份、动作、口型与叙事质量；完整工作流仍依赖单 Gateway 进程。阶段范围见[媒体生成稳定性计划](media-generation-stability-plan.md)。

动漫制作业务核心，分为两组路由：

- REST 资源：`/toon/projects`、`/toon/episodes`、`/toon/scenes` 等。
- toonflow 兼容层：大量与 Toonflow 前端协议兼容的路由（`/toonflow/*`、`/api/*` 及不带前缀的别名），覆盖项目、小说、剧本、资产（素材库/AI 生图/提示词润色）、分镜（storyboard）、图片工作流、视频工作台（轨道、视频生成、导出）、配音、手册、任务、设置、技能管理等。

**最终成片归档**（`toonflow_video_export.rs` / `toonflow_episode_renders.rs`）：

- 视频工作台选择生成片段并提交 FFmpeg 合并；只有最终 MP4 上传成功且归档事务提交成功，任务才标记为 `success`。每次成功导出按剧集生成 V1、V2…，新版本默认成为 current，历史版本不会被覆盖。
- 项目成果接口为 `GET /toonflow/projects/{project_id}/video-archive`；单集版本接口为 `GET /toonflow/projects/{project_id}/episodes/{script_id}/renders`；`PATCH /toonflow/episode-renders/{render_id}/current` 可恢复历史版本。接口同时检查权限、项目所有权及剧本归属。
- 项目详情第五阶段“剧集成果”消费上述接口，展示的是合并后的最终成片而非单个生成片段，支持播放、下载、版本切换和回到对应剧集继续制作。

**Agent 运行时**（`toonflow_agents.rs` / `toonflow_agent_runtime.rs` / `toonflow_agent_tools.rs`）：

- HTTP 接口 `/api/agents/*`（chat / start / runState / stop / events / retry / memories / runs / clearMemory / tools/execute）与剧本计划接口 `/api/scriptAgent/*`。
- WebSocket：`GET /api/socket/{agent}`（别名 `/socket/{agent}`，`toonflow_ws.rs`），query 参数 `token`、`isolationKey`、`projectId`、`scriptId` 鉴权与隔离；客户端消息类型 `chat` / `stop` / `updateThinkConfig` / `updateContext`，协议对齐 Toonflow-app 的 socket 协议。
- 记忆向量化：取 `ai.model_configs` 中启用的 embedding 模型，经 `AiModelFactory` 生成向量写入 `toonflow.agent_memories`；写入长期记忆前会剥离协议 XML 标签。

**文件与对象存储**：

- 素材、生成片段、通用上传和最终成片统一进入对象存储；旧版本 `storage/uploads` 仅作为一次性迁移来源，中间处理文件使用受配额约束的系统临时目录。
- 对象存储交互在 `toonflow_storage.rs` 与 infra 的对象存储适配器中：以 AWS SigV4（HMAC-SHA256）签名 S3 请求。endpoint/密钥/桶/区域由 `S3_*` 环境变量配置。

**图片画布与结果校验**：

- 资产生成固定布局与比例：普通角色为 3:2 全身三视图，有单侧特征的角色为 2:1 全身四视图，独立服装与道具为 1:1 四宫格，无人场景为 16:9 单画面。角色编辑沿用资产规则，分镜编辑保留单画面与项目比例。
- `toonflow_image_contract.rs` 把 1K/2K/4K 与比例编译为明确像素尺寸，发送到供应商的 `size`，不再只发送让模型自行选择比例的清晰度档位。供应商可按其像素下限等比放大。无效或超出 1:3–3:1 的比例直接报错。
- 提示词润色只输出主体外观；生成时兼容清理历史 Markdown 手册中的特写、半身及布局条款，最终资产构图规则优先于描述与参考图。参考图中的裁切不应被继承。
- 角色生图与角色编辑追加一张不含五官的全身轮廓构图参考，固定三/四个人物占位、完整腿脚及上下留白；此图只控制布局，面貌、服装和画风仍由角色设定及人物参考图决定。图片模型须支持参考图生成。
- 基础角色首次按新链路生图时，使用项目的 `universalAi` 对话模型规划七项面部设定（骨骼脸型、眉、眼、鼻、嘴与下巴、肤质、辨识点），参考同项目其他基础角色，要求在至少三项稳定特征上区分，保留明确的双胞胎或相同面貌设定。面部设定保存到资产提示词的 `【角色面部身份 v1】` 段落，重试复用，不随机换脸；这一规划步骤会增加一次文本模型调用。规划失败会终止图片任务。衍生角色由后端强制读取本角色父资产的图片作为第一张参考图，同时继承面部设定；基础图缺失时拒绝无参考生成。
- 图片任务记录实际发送的提示词、像素尺寸及参考图数量（不保存参考图 Base64）。HTTP 与 Base64 结果均须通过文件解码、宽高比和最低分辨率检查，并成功保存至对象存储，才能标记任务成功。允许 3% 的比例对齐误差和 5% 的尺寸误差；超宽、低清或损坏结果报失败，不自动替换已有资产图片，也不自动付费重绘。
- 图片生成记录与编辑记录统一使用微秒精度 ID，避免轮询按 ID 取最新图片时，被旧的微秒编辑记录遮蔽新的毫秒生成记录。
- 角色设定图另外检查中性背景下画面上下边缘的连续主体像素，拦截“比例正确但人物腿部延伸到画外”的疑似裁切结果。检查限制：该启发式不是人体或人脸识别，复杂背景会跳过边缘判定，不能保证留白内的半身图、视图数量或身份相似度都能被识别。目前没有自动视觉语义验收；生成后仍需预览确认，不合格时重新生成。
- 回归脚本为 `script/test-image-contract.sh`，使用独立临时 PostgreSQL 和模拟图片供应商，不调用付费模型、不访问业务数据库。

## 6. 前端

`apps/web` 为 pnpm + turbo monorepo（Vben Admin 5.7.0，仓库不锁定 pnpm 版本），主应用 `apps/web/apps/web-antd`：

- 开发：`pnpm dev:antd`，端口 `5666`（`.env.development` 的 `VITE_PORT`），API 前缀 `/api`，指向 `http://127.0.0.1:8080`。
- 构建产物：`apps/web/apps/web-antd/dist`（`VITE_ARCHIVER=true` 时额外生成 `dist.zip`）。
- 项目详情五个阶段均为异步组件；FormCreate/Designer 仅在 `/infra/build` 安装，TinyMCE 在表单实际使用时加载，避免这些重资源进入业务首页首屏。
- 前端环境变量（`VITE_*`）详见 [configuration.md](configuration.md)。

## 7. 关键外部依赖端口

| 组件 | 端口 | 说明 |
| --- | --- | --- |
| gateway | 8080 | HTTP API、探针、内网 `/metrics` |
| toon-worker | 8081 | 仅运维用 `/livez`、`/readyz`、`/metrics` |
| 前端 dev server | 5666 | `VITE_PORT` |
| PostgreSQL | 5432 | 主数据库 |
| Redis | 6379 | 缓存/限流（可选） |
| NATS | 4222 / 8222 | JetStream 客户端 / 监控端口 |
| RustFS | 9000 / 9001 | S3 API / 控制台 |
| Prometheus / Alertmanager | 9090 / 9093 | 指标查询 / 告警聚合（仅集群内） |
| Grafana | 3000 | 指标、日志与 Trace 查询界面（默认仅集群内） |
| Loki / Tempo | 3100 / 3200 | 日志查询 / Trace 查询（仅集群内） |
| OTLP | 4317 / 4318 | Collector 与 Tempo 的 gRPC / HTTP 接收端口 |

compose 定义见 `script/docker/docker-compose.yml` 与 [deployment.md](deployment.md)。
