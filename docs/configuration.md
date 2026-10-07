# 配置说明

本文档列出 rust-toon 的全部配置项，每项均已对照代码核实（出处见"来源"列）。架构背景见 [technical-solution.md](technical-solution.md)，部署步骤见 [deployment.md](deployment.md)。

## 1. 后端环境变量

后端的启动参数通过环境变量配置；少量非敏感运行参数可由 r-nacos 在进程运行期间热更新。生产环境建议将网关配置写入 `/etc/rust-toon/gateway.env`，worker 配置写入 `/etc/rust-toon/toon-worker.env`，并由 systemd `EnvironmentFile` 加载（样例见 `deploy/env/`）。

### 1.1 服务监听（`crates/framework/common/src/config.rs`）

`ServiceConfig::from_env("gateway", 8080)` 按服务名大写加前缀读取：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `GATEWAY_HOST` | `0.0.0.0` | 监听地址 |
| `GATEWAY_PORT` | `8080` | 监听端口（非法值回退默认） |
| `GATEWAY_DRAIN_DELAY_SECONDS` | production 为 `5`，其他环境为 `0` | SIGTERM 后先让 `/readyz` 失败并等待负载均衡摘流；范围 0～300 秒 |

### 1.2 数据库（`crates/framework/database/src/config.rs`）

| 变量 | 默认值 | 校验规则 |
| --- | --- | --- |
| `DATABASE_URL` | 无（**必填**，缺失报错 `DATABASE_URL is required`） | 必须以 `postgres://` 或 `postgresql://` 开头 |
| `DATABASE_MIN_CONNECTIONS` | `1` | 正整数，且 ≤ 最大连接数 |
| `DATABASE_MAX_CONNECTIONS` | `20` | 正整数，不能为 0 |
| `DATABASE_ACQUIRE_TIMEOUT_SECONDS` | `5` | 正整数，不能为 0 |

本地开发：`postgres://rust_toon:rust_toon@127.0.0.1:5432/rust_toon`。

### 1.3 安全 / JWT（`crates/framework/security/src/config.rs`）

| 变量 | 默认值 | 校验规则 |
| --- | --- | --- |
| `JWT_SECRET` | 无（**必填**） | **至少 32 字节**，否则启动报错 `JWT_SECRET must contain at least 32 bytes` |
| `JWT_ISSUER` | `rust-toon` | 签发与校验 JWT `iss` |
| `JWT_AUDIENCE` | `rust-toon-api` | 签发与校验 JWT `aud` |
| `JWT_ACCESS_TOKEN_TTL_SECONDS` | `900` | 正整数，访问令牌有效期（秒） |

### 1.4 Redis 与限流（`crates/framework/redis/src/lib.rs`、`rate_limit.rs`）

Redis 为**可选**：`REDIS_URL` 未设置或连接失败时，缓存与限流自动关闭，服务照常启动（日志告警）。

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `REDIS_URL` | 无（不设置则禁用 Redis） | 如 `redis://127.0.0.1:6379` |
| `REDIS_KEY_PREFIX` | `rust-toon` | 缓存键前缀 |
| `REDIS_CONNECT_TIMEOUT_SECONDS` | `3` | 连接超时 |
| `RATE_LIMIT_NAMESPACE` | `rate-limit` | 限流键命名空间 |
| `RATE_LIMIT_MAX_REQUESTS` | `300` | 窗口内最大请求数（按 IP+方法+路径） |
| `RATE_LIMIT_WINDOW_SECONDS` | `60` | 限流窗口（秒） |
| `RATE_LIMIT_TRUST_PROXY_HEADERS` | `false` | 是否信任入口代理重写后的 `X-Forwarded-For`；仅在 Gateway 只能由受控代理访问时启用 |

### 1.5 r-nacos 动态配置（`crates/framework/dynamic-config`）

Gateway 和 Worker 使用 Rust `nacos-sdk` 订阅各自的 JSON 文档。初次读取和每次推送都会先反序列化并检查 schema、未知字段与数值边界，通过后才用 `tokio::watch` 原子替换当前快照；校验失败不会污染运行值，而是继续使用 last-known-good。SDK 开启本地缓存并在运行期断线后持续重连。r-nacos 只承载非敏感可调参数，数据库 URL、JWT、API key、对象存储/NATS 凭据和 r-nacos 自身账号必须继续放在环境变量或 Secret 中。

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `NACOS_ENABLED` | 设置了 `NACOS_SERVER_ADDR` 时为 `true` | 是否启用动态配置；仅接受 `true/false/1/0` |
| `NACOS_REQUIRED` | `false` | 为 `true` 时，SDK 无法建立连接、初始文档无效或无法注册 listener 会令服务启动失败；文档尚未创建不算失败，服务用环境变量默认值启动并等待首次推送 |
| `NACOS_SERVER_ADDR` | 无 | SDK HTTP 地址，不带协议，例如 `127.0.0.1:8848` 或 `rust-toon-rnacos:8848`；SDK 同时连接对应 gRPC 端口 9848 |
| `NACOS_NAMESPACE` | 空（public） | r-nacos namespace ID；基础清单使用 public，若改为自定义 namespace，必须先在控制台创建 |
| `NACOS_GROUP` | `RUST_TOON` | 配置分组 |
| `NACOS_DATA_ID` | `rust-toon-{service}.json` | Gateway 默认为 `rust-toon-gateway.json`，Worker 默认为 `rust-toon-toon-worker.json` |
| `NACOS_USERNAME` / `NACOS_PASSWORD` | 无 | OpenAPI 客户端账号，必须成对设置；不得写入 ConfigMap |
| `NACOS_CACHE_DIR` | `/tmp/rust-toon-nacos-cache` | SDK last-known-good 磁盘缓存根目录；只读根文件系统需挂载可写 `emptyDir` 或持久目录 |
| `NACOS_CONNECT_TIMEOUT_SECONDS` | `15` | 启动建连、初次读取和注册 listener 超时，范围 1～120 秒 |

Gateway 文档只允许动态调整限流额度和窗口：

```json
{
  "schemaVersion": 1,
  "rateLimit": {
    "maxRequests": 300,
    "windowSeconds": 60
  }
}
```

Worker 文档只允许调整四个后台扫描周期，不会动态改变任务并发、lease、heartbeat 或 drain deadline：

```json
{
  "schemaVersion": 1,
  "dispatcherIntervalMillis": 500,
  "schedulerIntervalMillis": 1000,
  "reaperIntervalSeconds": 5,
  "cleanupIntervalSeconds": 5
}
```

可直接从 `deploy/rnacos/rust-toon-gateway.json` 和 `deploy/rnacos/rust-toon-toon-worker.json` 复制初始模板。未发布文档时，字段值来自已有 `RATE_LIMIT_*` / `TOON_WORKER_*` 环境变量。通过 r-nacos 控制台发布后，无需重启 Gateway/Worker；控制台历史版本执行回滚会产生一次普通推送，同样经过应用校验后生效。不要删除 `schemaVersion`，也不要把完整环境文件或 Secret 复制到动态文档。

### 1.6 Web / CORS（`crates/framework/web/src/middleware.rs`）

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `WEB_PERMISSIVE_CORS`（别名 `CORS_PERMISSIVE`） | `false` | 为 `1`/`true`/`yes` 时放行任意来源/方法/头；生产应在反代收敛跨域而非开启此项 |

无论是否开启，网关都会写入/透传 `x-request-id` 并输出访问日志（tower-http Trace）。

### 1.7 文件存储

| 变量 | 默认值 | 说明 | 来源 |
| --- | --- | --- | --- |
| `INFRA_UPLOAD_MAX_BYTES` | `20971520`（20 MiB） | `/infra/file/upload` 的文件体积上限；最大可配置为 100 MiB | `infra-server/src/lib.rs` |

当前应用运行时文件统一存入 S3 兼容对象存储；`INFRA_UPLOAD_DIR` 只由旧本地文件迁移脚本读取，不再是 Gateway 运行参数。FFmpeg/供应商下载的中间文件使用系统临时目录并在完成或恢复时清理。

### 1.8 对象存储（RustFS，S3 兼容）（`crates/modules/toon-server/src/toonflow_storage.rs`）

网关以自实现的 AWS SigV4 签名直连 S3 兼容对象存储（本地为 RustFS）：：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `S3_ENDPOINT` | `http://127.0.0.1:9000` | S3 endpoint（末尾 `/` 会被裁剪） |
| `S3_ACCESS_KEY` | `rust_toon` | 与 compose 的 `RUSTFS_ACCESS_KEY` 对应 |
| `S3_SECRET_KEY` | `rust_toon_password` | 与 compose 的 `RUSTFS_SECRET_KEY` 对应 |
| `S3_BUCKET` | `rust-toon` | 桶名 |
| `S3_REGION` | `us-east-1` | 签名区域 |

### 1.9 可观测性（`crates/framework/telemetry`）

Gateway 与 Toon Worker 默认在各自监听端口暴露 `GET /metrics`，使用 OpenMetrics 文本格式，包含按路由模板聚合的 HTTP 请求量、耗时、在途请求，以及 Worker 的任务结果、执行耗时、在途任务、outbox 投递、租约回收和对象清理指标。指标标签不会写入原始 URL、查询串、项目 ID 或视频 ID，避免泄露令牌和产生无界基数。

`/metrics` 不经过业务认证、审计和用户限流，必须仅允许 Prometheus 从内网抓取；面向公网的 Nginx/Ingress 应显式拒绝该路径。生产日志建议设为 JSON，开发环境可继续使用默认文本格式。

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `RUST_LOG` | `info` | `tracing` 的 EnvFilter；例如 `info,rust_toon_toon_server=debug` |
| `TELEMETRY_LOG_FORMAT` | `text` | `text` 保持现有日志输出兼容；生产建议使用 `json` 供 Loki/ELK 采集 |
| `TELEMETRY_METRICS_ENABLED` | `true` | 设为 `false` 时不挂载 `/metrics`，指标记录也变为无操作 |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | 无 | 可选 OTLP gRPC trace endpoint；优先级高于通用 endpoint |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | 无 | 可选通用 OTLP gRPC endpoint，例如 `http://otel-collector:4317`；两个 endpoint 均未配置时不会创建 exporter 或发起外部连接 |
| `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT` | 无 | trace 专用导出超时（毫秒），优先级高于通用 timeout；范围 100～300000 |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `10000` | 通用单批导出超时（毫秒），范围 100～300000 |
| `OTEL_SDK_DISABLED` | `false` | 设为 `true` 时即使配置了 endpoint 也禁用 OTLP trace 导出 |

启用 OTLP 后，HTTP `traceparent` / `tracestate` 会被提取并设置为 `http_request` server span 的父上下文，可发送到 OpenTelemetry Collector、开启 OTLP receiver 的 SkyWalking 或 Tempo。W3C carrier 会随 PostgreSQL outbox 和 NATS job envelope 持久化，Worker 消费时恢复父上下文，因此 Gateway → outbox → JetStream → Worker 保持同一条分布式 trace；业务 `trace_id` 仍作为便于检索的字段保留。

### 1.10 其他

| 变量 | 默认值 | 说明 | 来源 |
| --- | --- | --- | --- |
| `RUST_ENV` | `development` | 运行环境标识，展示在 infra 监控的服务器信息中 | `infra-server/src/monitor.rs` |
| `READINESS_REQUIRE_REDIS` | 设置了 `REDIS_URL` 时为 `true` | Redis 不可用时让 `/readyz` 返回 503 | `gateway/src/readiness.rs` |
| `READINESS_REQUIRE_OBJECT_STORAGE` | 生产环境或设置了 `S3_ENDPOINT` 时为 `true` | 使用生产链路同款签名 S3 请求验证凭据和 bucket；不可访问时让 `/readyz` 返回 503 | `gateway/src/readiness.rs` |
| `READINESS_REQUIRE_FFMPEG` | `false` | 仅在承担成片导出的节点上启用；缺少或无法执行 FFmpeg/FFprobe 时让 `/readyz` 返回 503 | `gateway/src/readiness.rs` |
| `SECRET_ENCRYPTION_KEY` | 无默认值，至少 32 字节 | 邮件、短信、OAuth 与数据源等敏感字段的独立 AES-256-GCM 密钥（新写入为 `enc:v2:`；旧 `enc:v1:` 数据保持兼容） | `framework/security/src/secret.rs` |
| `TEST_DATABASE_URL` | 无 | 仅测试使用：迁移测试与分镜数据库集成测试 | `toon-server/src/lib.rs` 测试、`script/test-database-migrations.sh` |
| `TEST_POSTGRES_PORT` | `55432` | 迁移测试脚本起临时 PostgreSQL 容器所用端口 | `script/test-database-migrations.sh` |
| `AI_REQUEST_TIMEOUT_SECONDS` | `120` | AI Provider 单次 HTTP 请求总超时，实际限制在 5～900 秒；连接超时固定为 15 秒 | `ai-server/src/provider.rs` |
| `AI_REQUEST_RETRIES` | `2` | AI Provider 失败重试次数，实际最多 5 次；连接失败、超时、408/409/425/429 和 5xx 会指数退避重试，支持上游 `Retry-After` | `ai-server/src/provider.rs` |
| `PIREN_SIDECAR_URL` | `http://127.0.0.1:7750` | AgentEngine 平台的 Agent 引擎 sidecar 地址（`AgentEngine` 模型配置的平台字符串为 `AgentEngine`，亦接受 `agent-engine`）；请求路径固定为 `/sidecar/v1/turn`，超时与重试复用 `AI_REQUEST_TIMEOUT_SECONDS` / `AI_REQUEST_RETRIES` | `ai-server/src/provider/agent_engine.rs` |
| `PIREN_SIDECAR_SECRET` | 无 | AgentEngine 代理 JWT 的 HS256 签名密钥，**至少 32 字节**；未设置或过短时 AgentEngine 调用在首次使用时失败。仅用于 sidecar 代理令牌（`iss=rust-toon`、`aud=piren-sidecar`、有效期 300 秒），与用户登录 `JWT_SECRET` 相互独立 | `ai-server/src/provider/agent_engine.rs` |
| `AI_VIDEO_POLL_INTERVAL_SECONDS` | `5` | 异步视频任务轮询间隔 | `toon-server/src/ai_client.rs` |
| `AI_VIDEO_POLL_TIMEOUT_SECONDS` | `600` | 异步视频任务最长等待时间 | `toon-server/src/ai_client.rs` |

### 1.11 分布式任务与 Toon Worker

最终成片等长任务不在 HTTP 网关进程中执行。网关在同一个 PostgreSQL 事务中写入业务任务和 `toonflow.distributed_jobs`，worker 的 dispatcher 再把任务引用投递到 NATS JetStream。PostgreSQL 是任务真相源，JetStream 使用显式 ACK 和至少一次投递；worker 通过数据库租约、心跳和 fencing token 保证多个实例竞争时只有租约持有者能够提交结果。

视频 Provider 返回成功后，Gateway 会先以流式方式把源视频归档到项目对象存储，再把视频记录标记为“生成成功”；成片任务只接受这些项目内不可变对象路径，避免排队或重试期间上游签名 URL 过期、换内容。对象清理在 DELETE 前会再次校验 cleanup lease，并检查成片、视频、图片、分镜与连续帧引用；仍被引用的对象只关闭清理任务，不执行删除。

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `NATS_URL` | `nats://127.0.0.1:4222` | Worker 使用的 NATS JetStream 地址；Gateway 只事务性写 PostgreSQL outbox |
| `NATS_CREDENTIALS_FILE` | 无 | 可选 NATS `.creds` 文件路径；密钥只从文件加载，不写入日志 |
| `NATS_TLS_REQUIRED` | `false` | 是否强制 NATS TLS；配置 CA 或客户端证书时必须为 `true` |
| `NATS_TLS_CA_FILE` | 无 | 可选自定义 CA PEM 文件路径 |
| `NATS_TLS_CLIENT_CERT_FILE` / `NATS_TLS_CLIENT_KEY_FILE` | 无 | 可选 mTLS 客户端证书与私钥路径，必须成对配置 |
| `TOON_WORKER_HOST` | `0.0.0.0` | Worker 健康检查服务监听地址 |
| `TOON_WORKER_PORT` | `8081` | Worker 健康检查服务监听端口 |
| `TOON_WORKER_CONCURRENCY` | `4` | 单个 worker 实例并发执行上限；横向副本数另由 systemd/Compose/Kubernetes 控制 |
| `TOON_WORKER_LEASE_SECONDS` | `300` | 数据库任务租约时长 |
| `TOON_WORKER_HEARTBEAT_SECONDS` | `30` | 执行中任务续租与 JetStream progress ACK 周期，必须小于租约时长 |
| `TOON_WORKER_DISPATCH_INTERVAL_MS` | `500` | PostgreSQL outbox 扫描和 JetStream 发布周期 |
| `TOON_WORKER_SCHEDULER_INTERVAL_MS` | `1000` | `infra_job` 到期扫描周期；多 Worker 通过数据库行锁协调 |
| `TOON_WORKER_PUBLISH_CLAIM_SECONDS` | `15` | dispatcher 发布前持有的短 PostgreSQL claim；实例崩溃后由其他 dispatcher 接管 |
| `TOON_WORKER_REPUBLISH_AFTER_SECONDS` | `300` | 数据库仍为非终态但 JetStream 消息丢失或超过 MaxDeliver 时，轮换 `message_id` 并重新发布的等待时间 |
| `TOON_WORKER_REAPER_INTERVAL_SECONDS` | `15` | 过期租约扫描与重试调度周期 |
| `TOON_WORKER_CLEANUP_INTERVAL_SECONDS` | `5` | 独立对象清理 subsystem 的扫描周期；不会阻塞 worker 注册心跳与任务 reaper |
| `TOON_WORKER_CLEANUP_TIMEOUT_SECONDS` | `30` | 单个对象存储删除请求超时，必须短于任务租约；失败按 PostgreSQL 时间指数退避，最多 20 次 |
| `TOON_WORKER_STAGING_CLEANUP_DELAY_SECONDS` | `1800` | 失败 attempt 的未引用上传至少延迟多久再删除；实际不会短于对象存储流超时，避免 DELETE/PUT 竞态 |
| `TOON_WORKER_DRAIN_TIMEOUT_SECONDS` | `45` | Worker 停机时取消在途执行、按当前 fencing token 无损重新排队且不消耗业务重试次数的最长等待时间 |
| `TOON_WORKER_MAX_SOURCE_BYTES` | `2147483648` | 单个成片源视频的最大字节数 |
| `TOON_WORKER_MAX_JOB_SOURCE_BYTES` | `10737418240` | 单次成片任务全部源视频的累计最大字节数 |
| `TOON_WORKER_SOURCE_TIMEOUT_SECONDS` | `1800` | 下载外部源视频的总超时 |
| `TOON_PROVIDER_VIDEO_CONCURRENCY` | `2` | Gateway 同时轮询、下载并归档 Provider 视频的全局并发上限；达到上限时新请求返回 429 |
| `TOON_PROVIDER_VIDEO_MAX_BYTES` | `2147483648` | 单个 Provider 视频归档时允许占用的最大临时文件字节数 |
| `TOON_PROVIDER_VIDEO_TEMP_QUOTA_BYTES` | `4294967296` | Gateway Provider 视频临时文件的进程级总预留配额；启动时会清理上次异常退出遗留的 `.download` 文件 |
| `TOON_WORKFLOW_STALE_SECONDS` | `7200` | 运行期 panic 兜底扫描判定工作流节点失联前的最小年龄；正常重启由启动修复立即处理 |
| `TOON_WORKER_FFMPEG_TIMEOUT_SECONDS` | `7200` | 单个 FFmpeg 标准化或合并进程的总超时 |
| `TOON_WORKER_STALE_WORKDIR_SECONDS` | `604800` | Worker 启动时删除的遗留 attempt 临时目录最小年龄；应大于 FFmpeg 超时 |
| `S3_CONNECT_TIMEOUT_SECONDS` | `10` | Gateway/Worker 连接对象存储的超时 |
| `S3_REQUEST_TIMEOUT_SECONDS` | `30` | 对象存储 HEAD、DELETE 和小对象请求总超时 |
| `S3_STREAM_TIMEOUT_SECONDS` | `1800` | 对象存储视频流式 GET/PUT 总超时，防止连接永久占用 Worker |
| `NATS_JOB_REPLICAS` | `1` | JetStream stream 副本数；三节点生产集群设为 `3` |
| `NATS_JOB_MAX_BYTES` | `10737418240` | stream 最大磁盘字节数，达到上限时拒绝新消息而不是删除未 ACK 的旧任务 |
| `NATS_JOB_ACK_WAIT_SECONDS` | `120` | 未收到 ACK/progress ACK 后的重投等待时间 |
| `NATS_JOB_MAX_DELIVER` | `20` | 单条消息最大 JetStream 投递次数；数据库 `max_attempts` 仍是业务重试上限 |
| `NATS_JOB_MAX_ACK_PENDING` | `32` | durable consumer 允许的最大未 ACK 消息数 |

Worker 提供 `/livez` 和 `/readyz`。负载均衡器或编排器应使用 `/readyz`，它必须在 PostgreSQL、JetStream、对象存储和 FFmpeg 链路可用后才返回成功。网关与 worker 必须共享 PostgreSQL 和对象存储，所有 worker 必须共享 JetStream；视频吞吐量由 worker 副本数及 `TOON_WORKER_CONCURRENCY` 决定。当前 Agent/Workflow 仍有进程内运行协调，Gateway 暂时必须保持单副本，不能把 HTTP 无状态路由误当成整个 Gateway 已可横向扩容。

网关提供三个探针：`/health` 保留旧版固定 200 及 `checked_at` 响应字段，`/livez` 只确认进程存活，`/readyz` 检查 PostgreSQL 以及按上述开关要求的 Redis、对象存储、FFmpeg 和 FFprobe。依赖探针均有超时，生产负载均衡应使用 `/readyz`，旧监控或进程管理器可继续使用 `/health`，新部署建议使用 `/livez`。

模型能力矩阵写入 `ai.model_configs.config.capabilities`，支持 `videoModes`、`durationResolutionMap`、`thinkLevels` 和 `multiReference`。视频调用会在请求上游前校验模式以及时长/分辨率组合；未配置能力矩阵的旧模型保持兼容。

### 1.12 启动账号说明

`sql/postgresql/0001_initial.sql` 的基线数据创建启用的 `super_admin` 用户 `admin`，本地初始密码为 `admin123`。`system-server/src/bootstrap.rs` 在启动时只校验至少存在一个启用的超级管理员；它不会创建用户，也不会重置现有密码。

当前 Gateway **不读取** `BOOTSTRAP_ADMIN_USERNAME` 或 `BOOTSTRAP_ADMIN_PASSWORD`。这些历史样例变量已经从启动脚本、环境样例和 Kubernetes 清单中移除，不应继续配置。已有数据库始终保留数据库中当前的密码；环境变量不会覆盖它。

前端开发环境的 `VITE_APP_DEFAULT_USERNAME` / `VITE_APP_DEFAULT_PASSWORD` 只是登录表单预填值，不是凭据来源。连续五次密码错误会触发数据库持久化的临时锁定。生产首次迁移完成后，应通过受控入口立即修改基线密码，并清理浏览器中遗留的本地令牌；不要把本地默认凭据写入生产 Secret。

### 1.13 视频基础质检

视频基础质检 `toon.video_quality` 复用 Worker 的租约、心跳与任务投递配置，不需要新增环境变量。当前固定限制：单片源文件最大 512 MiB，FFprobe 检查超时 30 秒，完整解码检查超时 300 秒；质检执行最多尝试 3 次，不触发付费视频重生成。时长误差容限取请求时长的 15% 与 0.75 秒中的较大值，显示比例相对误差容限为 5%。黑场（至少 0.5 秒）和冻结（至少 2 秒）仅记录提醒。视觉内容、口型和审美需要人工审片，基础检查通过不代表这些维度已验收。

## 2. 前端环境变量（`apps/web/apps/web-antd/`）

### 2.1 开发（`.env.development`）

| 变量 | 值 | 说明 |
| --- | --- | --- |
| `VITE_PORT` | `5666` | dev server 端口 |
| `VITE_BASE` | `/` | 部署基础路径 |
| `VITE_BASE_URL` | `http://127.0.0.1:8080` | 后端绝对地址，也是 Vite 开发环境的 `/api` 代理目标 |
| `VITE_GLOB_API_URL` | `/api` | REST 与 Agent WebSocket 的浏览器侧 API 基础地址 |
| `VITE_UPLOAD_TYPE` | `server` | 上传方式（server=经后端） |
| `VITE_DEVTOOLS` / `VITE_INJECT_APP_LOADING` | `false` / `true` | 开发工具 / 全局 loading |
| `VITE_APP_DEFAULT_USERNAME` / `VITE_APP_DEFAULT_PASSWORD` | `admin` / `admin123` | 仅登录页默认填充；不创建账号或修改数据库密码 |

`VITE_GLOB_API_URL=/api` 表示浏览器请求路径；开发环境中的 HTTP 与 WebSocket 请求由 Vite 代理到 `VITE_BASE_URL`。Gateway 改用其他端口时，只需同步修改 `VITE_BASE_URL`。也可以把 `VITE_GLOB_API_URL` 设置为完整后端地址直接连接，此时本地开发需要启用 `WEB_PERMISSIVE_CORS=true`。生产环境应使用同源反向代理，不要启用宽松 CORS。

### 前后端路径约定

Vite 和生产 Nginx 都去掉浏览器路径最前面的一个 `/api`；业务 API 文件写 Gateway 实际接收的路径，不自行拼接 `/api`。

| 层级 | 任务分页示例 |
| --- | --- |
| 前端 `requestClient.post` | `/task/getTaskApi` |
| 浏览器请求 | `POST /api/task/getTaskApi` |
| Vite / Nginx 转发给 Gateway | `POST /task/getTaskApi` |
| Rust 注册路由 | `/task/getTaskApi`，绑定 `post(query_tasks)` |

旧 Toonflow 客户端仍可直连 `/api/task/getTaskApi`。兼容路由与去掉前缀的路由必须使用同一处理函数和鉴权，不能只注册旧路径后假定代理会保留前缀。新 Toonflow 接口优先使用 `/toonflow/...` 命名空间；前端声明和 Rust 路由在同一次变更中提交。

任务列表及详情的 `id`、`retryOfId` 使用十进制字符串传输，避免 Snowflake ID 超过 JavaScript 安全整数范围后丢失精度。详情请求的 `taskId` 应原样传回，不使用 `Number()` 转换；后端同时接受旧客户端的整数输入。新增大整数标识字段也应采用字符串契约。

`apps/web/apps/web-antd/src/api/toonflow/routes.test.ts` 自动比对前端所有请求的 HTTP 方法和路径与 Rust 注册表，并展开条件路径、动态 ID 和下载方法。该测试随前端 unit 检查进入 CI。Rust `proxy_route_tests` 验证兼容路径实际匹配到鉴权；Gateway E2E 同时验证原始路径和代理去掉前缀后的任务接口。代理部署后仍需通过浏览器域名检查接口，不能只测试 Gateway 端口。

### 2.2 生产（`.env.production`）

`VITE_BASE=/`、`VITE_BASE_URL=http://127.0.0.1:8080`、`VITE_GLOB_API_URL=/api`、`VITE_UPLOAD_TYPE=server`、`VITE_COMPRESS=none`、`VITE_PWA=false`、`VITE_ROUTER_HISTORY=hash`、`VITE_INJECT_APP_LOADING=true`、`VITE_ARCHIVER=true`（构建后额外产出 `dist.zip`）、`VITE_APP_CAPTCHA_ENABLE=false`。

### 2.3 公共（`.env`）

应用标题 `VITE_APP_TITLE=Rust Toon 管理平台`、命名空间 `VITE_APP_NAMESPACE=rust-toon-vben-antd`、store 加密密钥 `VITE_APP_STORE_SECURE_KEY`（**生产必须替换**）、租户开关 `VITE_APP_TENANT_ENABLE=true`、验证码开关 `VITE_APP_CAPTCHA_ENABLE=false` 等。

## 3. 本地基础设施（`script/docker/docker-compose.yml`）

| 服务 | 镜像 | 端口 | 关键配置 |
| --- | --- | --- | --- |
| postgres | `postgres:18` | `5432` | 用户/密码/库均为 `rust_toon`，数据卷 `rust-toon-postgres` |
| redis | `redis:8` | `6379` | 无认证 |
| nats | `nats:2` | `4222`、`8222` | 已启用 JetStream，文件存储卷 `rust-toon-nats`；8222 为监控端口 |
| rustfs | `rustfs/rustfs:1.0.0` | `9000`（S3）、`9001`（控制台） | 账号 `rust_toon` / `rust_toon_password`，数据卷 `rust-toon-rustfs` |
| rnacos | `qingpan/rnacos:v0.8.6` | `8848`（SDK）、`9848`（gRPC）、`10848`（控制台） | 本地账号 `rust_toon` / `rust_toon_nacos_password`，数据卷 `rust-toon-rnacos` |

启动：`docker compose -f script/docker/docker-compose.yml up -d`。注意不要把 `sql/postgresql` 挂载进 PostgreSQL 初始化目录——数据库初始化由网关的 SQLx 迁移负责。
