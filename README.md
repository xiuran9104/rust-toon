# Rust Toon

Rust Toon 是面向动漫/短剧生产的 AI 工作台。后端使用 Rust，前端使用 Vue 3 + Ant Design Vue；Toonflow 页面提供从项目、剧本、资产、分镜到音视频生成与 Agent 协作的一体化流程。

## 功能组成

- System：认证、用户、角色、权限、菜单、租户及后台管理能力。
- Infra：配置、文件、任务、日志、数据源等基础设施能力。
- AI：统一模型管理、聊天/SSE、工具调用、知识库、图片、Midjourney、音乐、语音、Embedding 和写作。
- Toonflow：项目、小说、剧本、事件、资产、分镜、音频、视频、Agent、提示词、Skill 和任务中心。
- Media：素材上传及媒体基础能力。

## 环境要求

- Rust stable（项目使用 Rust 2024 edition）
- PostgreSQL 18
- Node.js `22.18+`（Node 25+ 需单独安装 Corepack/pnpm）
- pnpm `11+`
- Docker 及 Docker Compose（推荐用于本地基础设施）

## 五分钟本地启动

从仓库根目录直接执行：

```bash
bash script/start-local.sh all
```

完整栈模式会依次启动 PostgreSQL、Redis、JetStream、RustFS、r-nacos、Gateway、Toon Worker 和前端，并等待网关迁移完成后才启动 Worker。其他模式：

- `infra`：只启动基础设施。
- `gateway` / `worker`：启动基础设施后，在前台运行指定服务；单独使用 `worker` 前必须已有就绪的 Gateway 完成迁移。
- `backend`：启动 Gateway 和 Worker。
- `all`：启动完整本地开发栈。

访问入口：

- 前端：`http://127.0.0.1:5666`
- Gateway 就绪：`http://127.0.0.1:8080/readyz`
- Worker 就绪：`http://127.0.0.1:8081/readyz`
- RustFS 控制台：`http://127.0.0.1:9001`
- r-nacos：`http://127.0.0.1:10848`

本地应用账号为 `admin` / `admin123`。该账号由数据库基线迁移创建，启动环境变量不会创建或重置管理员；首次登录后请立即修改密码。端口冲突、保留已有数据或手动分终端启动时，按[部署文档的本地开发章节](docs/deployment.md#2-本地开发)操作。

## 验证

```bash
cargo test --workspace
bash script/test-database-migrations.sh
bash script/test-gateway-e2e.sh
bash script/test-ai-e2e.sh
bash script/test-production-e2e.sh
bash script/test-rnacos-dynamic-config.sh
bash script/test-s3-backup.sh
pnpm --dir apps/web run test:unit
pnpm --dir apps/web --filter @vben/web-antd run typecheck
pnpm --dir apps/web --filter @vben/web-antd run build
```

真实图片与视频供应商测试会产生费用，因此不会进入默认测试。确认测试数据库已有可用模型配置后显式运行：

```bash
RUN_PAID_AI_E2E=1 \
DATABASE_URL='postgres://...' \
REAL_IMAGE_MODEL_ID='1' \
REAL_VIDEO_MODEL_ID='2' \
REAL_VIDEO_PAYLOAD_JSON='{"prompt":"A red paper boat slowly moving on calm water"}' \
bash script/test-real-ai-providers.sh
```

## 文档

- [文档导航与维护规则](docs/README.md)
- [AI 启动交接指南](AGENTS.md)
- [技术架构](docs/technical-solution.md)
- [配置与模型接入](docs/configuration.md)
- [启动、部署与运维](docs/deployment.md)
- [功能范围与验收口径](docs/parity-roadmap.md)

## 数据库备份

PostgreSQL 与对象存储必须作为同一个恢复集备份。生产入口是 `script/database/backup-consistent-set.sh`；恢复命令、保留策略和 systemd timer 见[部署文档](docs/deployment.md#47-备份与恢复)。
