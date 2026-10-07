# 部署文档

本文档是本地开发与生产运维步骤的权威来源。配置项的含义与默认值见 [configuration.md](configuration.md)，架构说明见 [technical-solution.md](technical-solution.md)，完整文档分工见[文档导航](README.md)。根目录 `AGENTS.md` 只保留 AI 编码代理必须遵守的安全约束，不再复制完整部署流程。

## 1. 前置要求

- Rust stable（支持 Rust 2024 edition）。
- Docker 与 Docker Compose（本地基础设施）。
- Node.js `22.18+` 与 pnpm；仓库不锁定 pnpm 版本。Node.js 25 起不再内置 Corepack，如系统没有 `corepack` 命令，需要先单独安装 Corepack 或 pnpm。
- 生产环境另需 Nginx 或其他反向代理。

## 2. 本地开发

### 2.1 启动基础设施

```bash
docker compose -f script/docker/docker-compose.yml up -d
```

包含 PostgreSQL（5432）、Redis（6379）、启用持久化 JetStream 的 NATS（4222/8222）、RustFS（9000/9001）和 r-nacos（8848/9848/10848）。也可以使用便捷脚本 `script/start-local.sh [infra|gateway|worker|backend|all]`：它会先起 compose，再按模式启动服务（自动导出本地默认环境变量）。`backend` 同时启动网关与 worker，`all` 再加上前端；这两个模式会等待 Gateway 就绪后再启动 Worker。单独调试可用 `gateway` 或 `worker`，但 `worker` 模式要求已有就绪的 Gateway 完成迁移。

### 2.2 启动后端网关

```bash
export DATABASE_URL='postgres://rust_toon:rust_toon@127.0.0.1:5432/rust_toon'
export REDIS_URL='redis://127.0.0.1:6379'
export JWT_SECRET='local-development-jwt-secret-change-me-32bytes'
export S3_ENDPOINT='http://127.0.0.1:9000'
export S3_ACCESS_KEY='rust_toon'
export S3_SECRET_KEY='rust_toon_password'
export NACOS_ENABLED='true'
export NACOS_REQUIRED='true'
export NACOS_SERVER_ADDR='127.0.0.1:8848'
export NACOS_USERNAME='rust_toon'
export NACOS_PASSWORD='rust_toon_nacos_password'
export RUST_LOG='info'
cargo run -p rust-toon-gateway
```

网关启动时自动执行 `sql/postgresql` 下的全部迁移（空库从零建表并写入基线数据），并校验存在启用的超级管理员。

另开一个终端启动持久任务 worker；视频基础质检和最终成片合并由 worker 执行。新视频归档后需要 Worker 完成检查才能选用；未运行新版 Worker 时，视频会保持“等待基础质检”。现有尾帧提取仍可能在 Gateway 执行，使用跨镜头尾帧衔接的 Gateway 也需要 FFmpeg：

```bash
export DATABASE_URL='postgres://rust_toon:rust_toon@127.0.0.1:5432/rust_toon'
export NATS_URL='nats://127.0.0.1:4222'
export S3_ENDPOINT='http://127.0.0.1:9000'
export S3_ACCESS_KEY='rust_toon'
export S3_SECRET_KEY='rust_toon_password'
export NACOS_ENABLED='true'
export NACOS_REQUIRED='true'
export NACOS_SERVER_ADDR='127.0.0.1:8848'
export NACOS_USERNAME='rust_toon'
export NACOS_PASSWORD='rust_toon_nacos_password'
cargo run -p rust-toon-worker
```

### 2.3 启动前端

```bash
cd apps/web
corepack enable
pnpm install
pnpm dev:antd
```

### 2.4 访问入口

- 前端：`http://127.0.0.1:5666`
- 后端兼容存活检查：`http://127.0.0.1:8080/health`（固定 200，保留旧响应字段）
- 存活/就绪探针：`http://127.0.0.1:8080/livez`、`http://127.0.0.1:8080/readyz`
- Worker 存活/就绪探针：`http://127.0.0.1:8081/livez`、`http://127.0.0.1:8081/readyz`
- OpenAPI 文档：`http://127.0.0.1:8080/openapi.json`
- RustFS 控制台：`http://127.0.0.1:9001`（`rust_toon` / `rust_toon_password`）
- r-nacos 控制台：`http://127.0.0.1:10848`（`rust_toon` / `rust_toon_nacos_password`）

默认本地应用账号：`admin` / `admin123`。账号由基线迁移创建，Gateway 启动不会创建账号或重置密码；完整行为见[启动账号说明](configuration.md#112-启动账号说明)。首次登录后请立即修改密码。

### 2.5 本地验证

```bash
curl -fsS http://127.0.0.1:8080/health
cargo test --workspace
bash script/test-database-migrations.sh
bash script/test-ai-e2e.sh
bash script/test-image-contract.sh
bash script/test-gateway-e2e.sh
bash script/test-production-e2e.sh
bash script/test-distributed-deployment.sh
bash script/test-k8s-deployment.sh
bash script/test-distributed-jobs-e2e.sh
bash script/test-s3-backup.sh
pnpm --dir apps/web run test:unit
pnpm --dir apps/web --filter @vben/web-antd run typecheck
```

前端生产构建检查：`pnpm --dir apps/web --filter @vben/web-antd run build`。

## 3. 数据库迁移管理

- 迁移由网关启动时自动执行，`sql/postgresql` 下的全部编号迁移在编译期嵌入二进制；**不要**把该目录挂载到 PostgreSQL 的 initdb 目录。
- 变更流程（与根 `AGENTS.md` 一致）：
  1. 新增编号迁移文件，已发布/已应用的迁移不得修改。
  2. 迁移必须幂等，同时支持空库初始化与已有库升级。
  3. 运行 `bash script/test-database-migrations.sh` 验证空库可到达最新结构（脚本用 Docker 起临时 `postgres:18`，端口 `TEST_POSTGRES_PORT`，默认 55432）。
  4. 对应用全部迁移的干净参考库重新导出 `sql/bootstrap/current.sql`（`pg_dump` 快照，仅供查阅，应用从不加载）。
  5. 更新 `crates/framework/database/tests/migrations.rs` 中的迁移数量与基线断言。
- 保护机制：若数据库已有业务表但无 `_sqlx_migrations` 历史，启动迁移会拒绝执行，防止误覆盖。

## 4. 新服务器生产部署

### 4.1 克隆与基础设施

```bash
sudo install -d -o "$(id -un)" -g "$(id -gn)" -m 0755 /opt/rust-toon
git clone <repo-url> /opt/rust-toon
cd /opt/rust-toon
```

`script/docker/docker-compose.yml` 使用开发口令并把全部基础设施端口发布到宿主机，**不得直接用于生产**。单服务器生产使用 4.5 节的分布式 compose；多服务器部署使用托管 PostgreSQL/Redis/S3 与三节点 JetStream，或自行提供等价的加固集群。

### 4.2 网关环境文件

在 git 之外创建 `/etc/rust-toon/gateway.env`（样例见 `deploy/env/gateway.env.example`）：

```bash
DATABASE_URL=postgres://rust_toon:rust_toon@127.0.0.1:5432/rust_toon
DATABASE_MIN_CONNECTIONS=1
DATABASE_MAX_CONNECTIONS=12
REDIS_URL=redis://127.0.0.1:6379
JWT_SECRET=replace-with-a-strong-random-secret-at-least-32-bytes
S3_ENDPOINT=http://127.0.0.1:9000
S3_ACCESS_KEY=rust_toon
S3_SECRET_KEY=replace-with-a-strong-object-storage-secret
S3_BUCKET=rust-toon
GATEWAY_HOST=0.0.0.0
GATEWAY_PORT=8080
GATEWAY_DRAIN_DELAY_SECONDS=10
RUST_LOG=info
RUST_ENV=production
READINESS_REQUIRE_REDIS=true
READINESS_REQUIRE_OBJECT_STORAGE=true
```

`JWT_SECRET` 必须 ≥ 32 字节，否则启动失败。当前版本不支持 `BOOTSTRAP_ADMIN_USERNAME` / `BOOTSTRAP_ADMIN_PASSWORD`；不要把它们写入环境文件。全新数据库会由基线迁移创建 `admin`，应在首次受控登录后立即修改其密码。当前上传、生成片段和最终成片统一进入对象存储；旧版本遗留的 `storage/uploads` 应先用 `script/migrate-local-uploads-to-s3.sh` 迁移。AI 密钥落库加密可通过 `SECRET_ENCRYPTION_KEY` 独立指定（缺省回退 `JWT_SECRET`）。

对象存储后端已从 MinIO 换成 RustFS（MinIO 社区版已停止维护），应用侧只依赖标准 S3 数据面接口，仓库内也不再依赖任何 MinIO 组件——备份与测试使用自带的 `s3ctl` 客户端。升级注意：环境变量已从 `MINIO_*` 整体更名为 `S3_*`（`READINESS_REQUIRE_MINIO` → `READINESS_REQUIRE_OBJECT_STORAGE`），现有部署的环境文件需同步重命名。历史 MinIO 卷迁移脚本已随 MinIO 依赖一并移除；如仍有旧卷数据需要搬迁，请自行用任一 S3 客户端按对象复制。

### 4.3 构建与试运行

```bash
cargo build --release -p rust-toon-gateway -p rust-toon-worker
set -a; . /etc/rust-toon/gateway.env; set +a
./target/release/rust-toon-gateway
```

确认迁移与管理员就绪后 Ctrl+C 停止，交给 systemd 管理。

### 4.4 systemd

仓库提供样例 `deploy/systemd/rust-toon-gateway.service`（`User=rust-toon`、`EnvironmentFile=/etc/rust-toon/gateway.env`、开启 `ProtectSystem=strict` 等加固项；运行时对象进入对象存储，不开放仓库目录写权限）：

```bash
sudo useradd --system --home /opt/rust-toon --shell /usr/sbin/nologin rust-toon
sudo install -d -o root -g rust-toon -m 0750 /etc/rust-toon
```

将 release 二进制/仓库安装到 unit 约定的 `/opt/rust-toon`。Worker 节点还必须安装 `ffmpeg` 和 `ffprobe`（Debian/Ubuntu 可执行 `sudo apt-get install -y ffmpeg`）。

```bash
sudo install -o root -g root -m 0644 \
  deploy/systemd/rust-toon-gateway.service /etc/systemd/system/
sudo install -o root -g rust-toon -m 0640 \
  deploy/env/gateway.env.example /etc/rust-toon/gateway.env
sudoedit /etc/rust-toon/gateway.env
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-toon-gateway
sudo systemctl status rust-toon-gateway
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8080/readyz
```

在视频执行节点安装 `deploy/systemd/rust-toon-worker.service`，将 `deploy/env/toon-worker.env.example` 复制到 `/etc/rust-toon/toon-worker.env` 后替换凭据：

```bash
sudo install -o root -g root -m 0644 \
  deploy/systemd/rust-toon-worker.service /etc/systemd/system/
sudo install -o root -g rust-toon -m 0640 \
  deploy/env/toon-worker.env.example /etc/rust-toon/toon-worker.env
sudoedit /etc/rust-toon/toon-worker.env
sudo systemctl daemon-reload
sudo systemctl enable --now rust-toon-worker
curl -fsS http://127.0.0.1:8081/readyz
```

网关与 worker 可以部署到不同服务器。应用集群必须共享 PostgreSQL 和对象存储，所有 worker 还必须连接同一 JetStream；每个 worker 都使用独立实例 ID 和数据库租约，无需指定静态分片。当前 Agent/Workflow 运行协调仍含进程内状态，因此生产集群暂时只运行 **1 个 Gateway**；最终成片 worker 可以运行任意多个副本。待 Agent/Workflow 也迁到持久任务协议后，才能解除 Gateway 单副本限制。

单机 systemd 部署可用统一探针脚本检查网关、worker 和 JetStream；端点不在本机时通过同名环境变量覆盖：

```bash
bash script/check-distributed-health.sh
GATEWAY_READY_URL=https://api.example.com/readyz bash script/check-distributed-health.sh
```

Worker 默认只在 `127.0.0.1:8081` 暴露管理探针；远程节点通过主机监控 agent 或 `ssh worker-host curl -fsS http://127.0.0.1:8081/readyz` 检查，不要把管理端口直接暴露到公网。

### 4.5 Docker Compose 分布式部署

`script/docker/docker-compose.distributed.yml` 是独立的单机生产骨架，不继承本地 compose 的固定 `container_name`。业务流量只经 edge 进入；PostgreSQL、RustFS、r-nacos 控制台和 NATS 监控端口仅绑定 loopback，供本机维护使用。先用 `0600` 权限安装密钥文件，再启动 1 个 Gateway 和多个 worker：

```bash
sudo install -d -o root -g root -m 0700 /etc/rust-toon
sudo install -o root -g root -m 0600 \
  deploy/env/distributed.env.example /etc/rust-toon/distributed.env
sudoedit /etc/rust-toon/distributed.env

docker compose \
  --env-file /etc/rust-toon/distributed.env \
  -f script/docker/docker-compose.distributed.yml \
  config >/dev/null

docker compose \
  --env-file /etc/rust-toon/distributed.env \
  -f script/docker/docker-compose.distributed.yml \
  up -d --build --scale gateway=1 --scale toon-worker=4
```

如果使用 CI 已推送的镜像，则设置 `RUST_TOON_BACKEND_IMAGE`，显式拉取并禁止本地构建：

```bash
docker compose \
  --env-file /etc/rust-toon/distributed.env \
  -f script/docker/docker-compose.distributed.yml pull gateway toon-worker
docker compose \
  --env-file /etc/rust-toon/distributed.env \
  -f script/docker/docker-compose.distributed.yml \
  up -d --no-build --scale gateway=1 --scale toon-worker=4
```

`DATABASE_URL` 是完整 URI；若数据库密码含 `@`、`/`、`:`、`#` 或 `%`，必须在 URI 中 percent-encode，并保证解码后的密码与 `POSTGRES_PASSWORD` 相同。示例连接池预算为 1×12（Gateway）+ 4×8（worker）= 44，扩容 worker 前需按数据库 `max_connections` 重新核算并留出管理连接。

Nginx edge 会重新解析 Docker DNS，并在连接错误/502/503/504 时被动重试其他地址；它不主动消费容器 `/readyz`，因此这不是多 Gateway 的安全门禁。当前必须保持 Gateway=1。SSE 和 WebSocket 在连接建立后固定到该实例；edge access log 只记录 path，不记录可能包含临时凭据的 query，素材上传关闭请求缓冲并允许最大 2 GiB。检查探针和副本：

```bash
curl -fsS http://127.0.0.1:8080/readyz
docker compose \
  --env-file /etc/rust-toon/distributed.env \
  -f script/docker/docker-compose.distributed.yml ps
```

扩缩 worker 时重复 `up -d --scale gateway=1 --scale toon-worker=N`。worker 收到 SIGTERM 后立即停止领新任务，取消在途执行并用当前 fencing token 把尝试重新排队；整个过程受 `TOON_WORKER_DRAIN_TIMEOUT_SECONDS` 限制，数据库或网络故障时仍可在租约过期后由其他实例接管。`stop_grace_period` 必须大于 drain deadline，生产若提高它需同步提高容器停止宽限。Gateway 收到 SIGTERM 会先把 `/readyz` 置为不可用并等待 `GATEWAY_DRAIN_DELAY_SECONDS`，但单机 Compose 的 edge 只有被动故障重试；需要无损滚动 Gateway 时应由真正消费 readiness 的编排器/LB 摘流。

该 compose 不包含前端静态站；仍需按第 5 节使用 CDN/独立 Nginx 托管 `dist`，并把 `/api/` 指向 edge。Compose 中的 PostgreSQL、Redis、NATS、RustFS 和 r-nacos 都是单机持久化数据面；r-nacos 控制台默认只绑定 `127.0.0.1:10848`。高可用生产应替换为托管 PostgreSQL/Redis/对象存储、三节点 JetStream，并使用下一节的三节点 r-nacos 清单。JetStream 卷用于降低恢复延迟，但业务任务真相源是 PostgreSQL outbox，因此不能用 NATS 消息替代数据库备份。

r-nacos 新数据卷只会创建 `NACOS_USERNAME` / `NACOS_PASSWORD` 指定的初始化管理员。首次登录 `http://127.0.0.1:10848` 后发布 `RUST_TOON` group 下的 `rust-toon-gateway.json` 与 `rust-toon-toon-worker.json`；合法 JSON 见[配置文档](configuration.md#15-r-nacos-动态配置cratesframeworkdynamic-config)。文档不存在时应用使用环境变量默认值并保持订阅，因此可先启动再发布。生产完成引导后应创建单独的应用账号、轮换 Gateway/Worker 的 r-nacos 凭据，并把管理员只留给运维入口。

### 4.6 Kubernetes：1 Gateway + N Worker

`deploy/k8s` 提供生产基础清单，使用 Kustomize 管理以下资源：

- 固定单副本且使用 `Recreate` 更新策略的 Gateway Deployment/Service。Agent/Workflow 实时运行表仍有进程内状态，因此不能为 Gateway 配置 HPA，也不能把副本数改为 2；`Recreate` 会带来短暂升级窗口，并避免正常 Deployment 更新期间两个 revision 重叠。它不是分布式 leader lease，节点网络分区等极端场景仍需运维隔离故障节点。Gateway PDB 以 `minAvailable: 1` 阻止未协调的自愿驱逐，但不能消除节点故障或版本升级的单副本停机窗口；执行 node drain 前必须先安排维护窗口并临时调整/移除 PDB。
- 默认 2 副本的 Worker Deployment/Service、CPU HPA（2–8 副本）与 PDB。Worker 通过 PostgreSQL lease/fencing 和共享 JetStream durable consumer 横向扩容。
- 固定版本 `v0.8.6` 的三节点 r-nacos StatefulSet、每节点独立 10 GiB PVC、headless Raft 发现 Service、客户端/控制台 Service 与 `minAvailable: 2` PDB。OpenAPI 鉴权默认开启，控制台不对业务入口开放。
- `/livez`、`/readyz` 与启动探针、SIGTERM 宽限、non-root、只读根文件系统、默认 seccomp、移除 Linux capabilities、资源 request/limit 和临时盘上限。
- 默认拒绝入站/出站的 NetworkPolicy，以及 DNS、Gateway 入口、监控与外部依赖所需的最小端口规则。
- Gateway/Worker 临时目录使用有 `sizeLimit` 的 `emptyDir`；上传、生成片段和最终成片都写到写入共享对象存储，因此基础清单不创建无消费者的本地上传 PVC。
- Prometheus、Alertmanager、Grafana、Loki、Tempo 与 OpenTelemetry Collector。Prometheus/Loki/Tempo/Alertmanager 使用 PVC 保存运行数据，Grafana Dashboard 和数据源由 ConfigMap 声明式装载。

这些清单会部署 r-nacos，但**不部署 PostgreSQL、Redis、NATS 或对象存储**。部署前准备外部服务、默认 StorageClass、支持 NetworkPolicy 的 CNI，以及供 HPA 使用的 Metrics Server。基础 HPA 最大 8 个 Worker；按示例连接池计算为 Gateway 12 + Worker 8×8 = 76 个数据库连接，修改上限或副本数时必须重新核算 PostgreSQL 连接预算。生产 JetStream 建议三副本；若外部集群的 replication factor 不同，应同步修改 `NATS_JOB_REPLICAS`。

先构建并推送 `deploy/docker/Dockerfile.backend`，使用不可变 tag 或 digest，然后修改 `deploy/k8s/kustomization.yaml` 的 `images` 条目。不要部署示例中的 `.invalid` 镜像/endpoint。修改两个 ConfigMap 中的 S3 endpoint、桶和容量参数；敏感连接信息不要写入 ConfigMap 或 Git。

`deploy/k8s/secret.example.yaml` 仅列出 Secret key，故意不在 Kustomize resources 中，所有值都是不可用的 `REPLACE_ME`。它把 Gateway、Worker、r-nacos 和 Grafana 管理员凭据拆成四个 scoped Secret，只有 PostgreSQL/对象存储连接值需要分别写入前两份。首次安装时 Gateway/Worker 的 `NACOS_USERNAME` / `NACOS_PASSWORD` 必须与 r-nacos 初始化管理员匹配；集群建立后再创建应用账号并轮换。建议从权限为 `0600`、位于仓库外的文件或 External Secrets/Sealed Secrets 创建四份 Secret。以下是文件方式的安装顺序：

```bash
kubectl apply -f deploy/k8s/namespace.yaml

sudo install -o "$(id -un)" -g "$(id -gn)" -m 0600 \
  deploy/k8s/secret.example.yaml /secure/path/rust-toon-secret.yaml
${EDITOR:-vi} /secure/path/rust-toon-secret.yaml
kubectl apply -f /secure/path/rust-toon-secret.yaml

# 确认已修改 image、S3_ENDPOINT 和 NATS_JOB_REPLICAS 后再安装。
kubectl apply -k deploy/k8s
# Vector 需要读取节点上的容器日志，因此独立部署在 baseline
# Pod Security namespace，而不是 restricted 的应用 namespace。
kubectl apply -k deploy/logging-agent
```

`DATABASE_URL`、`REDIS_URL`、`NATS_URL` 与 r-nacos 凭据均放在 Secret 中；URI 密码的保留字符必须 percent-encode，生产 Redis/NATS 应使用 TLS。应用管理员不是由 Secret 引导创建：全新数据库迁移后，应通过仅限运维访问的入口使用基线账号登录并立即修改密码。r-nacos JSON 中列出的六类运行参数通过长连接热推送，不需要 rollout；其他 ConfigMap/Secret、连接凭据、端口、并发和 lease 仍是启动参数，修改后必须显式重启对应 Deployment。Gateway 使用 Recreate，重启期间会短暂不可用，需要在维护窗口执行。环境 overlay 也可以改用带内容哈希的 generator 或受控 reloader。更严格的生产集群应通过外部 Secret 控制器注入密钥，并对 Secret 启用静态加密与最小 RBAC。

全新数据库也可以一次性应用全部资源：Gateway 启动时先执行 SQLx 迁移；每个 Worker 的受限 init container 会持续访问 `rust-toon-gateway:8080/readyz`，只有迁移、管理员校验及 Gateway 必需依赖全部就绪后才启动 Worker。不要删除这个等待条件，也不要让 Worker 自行执行迁移。

基础 NetworkPolicy 只能按常用端口放行任意外部目的地，因为标准 Kubernetes NetworkPolicy 不支持 FQDN。请在环境 overlay 中把 PostgreSQL、Redis、NATS、对象存储和 HTTPS provider egress 收窄为实际 CIDR，或使用 CNI 的 FQDN policy；若托管服务使用非默认端口也要同步调整。把 Ingress Controller 和监控组件所在 namespace 显式打标后才允许访问：

```bash
kubectl label namespace ingress-nginx rust-toon.io/gateway-access=true
kubectl label namespace monitoring rust-toon.io/monitoring-access=true
kubectl label namespace config-admin rust-toon.io/config-admin-access=true
```

r-nacos 控制台 Service 的 10848 端口只接受带 `rust-toon.io/config-admin-access=true` 标签的 namespace。临时维护也可通过受控的 `kubectl -n rust-toon port-forward service/rust-toon-rnacos 10848:10848` 访问。每次发布前保留 JSON schemaVersion 并由第二人复核；发布后在应用日志中确认 `dynamic configuration update applied`。若新值异常，直接在 r-nacos 的配置历史中恢复上一版本；恢复会被长连接当作新 revision 推送，应用校验通过后立即生效。

集群入口、TLS 证书和前端静态站依赖各环境的 Ingress/Gateway API 与证书控制器，因此基础清单不内置。将业务 `/api/` 流量转发到 `Service/rust-toon-gateway:8080`，保持 SSE/WebSocket 超时及关闭代理缓冲等要求。若集群 DNS Pod 不使用 `k8s-app=kube-dns` 标签，应在 overlay 中调整 DNS egress selector。

Gateway 与 Worker 默认输出 JSON 结构化日志并在各自管理 HTTP 端口开放 `/metrics`。Gateway 的 `/metrics` 与业务 API 同在 8080：标准 NetworkPolicy 只能按 IP/端口过滤，**不能按 URL path 阻断**，因此面向公网的 Ingress/Nginx 必须显式拒绝精确路径 `/metrics`（例如 Nginx `location = /metrics { return 404; }`），不得把它随 `/api/` 或 `/` 暴露。内置 Prometheus 通过 `rust-toon-worker-metrics` headless Service 的 DNS A 记录逐 Pod 抓取所有 Worker，不会把多副本指标随机采样成一个进程。`rust-toon-prometheus-rules` 提供 Gateway/Worker 可用性、5xx、任务失败、租约恢复与对象清理告警；Alertmanager 的默认 receiver 只保留告警，不向外发送，生产必须在 Secret-backed overlay 中接入企业 webhook、邮件或 PagerDuty。

基础清单同时部署 `rust-toon-otel-collector` 和 Tempo，Gateway/Worker 通过 OTLP gRPC 4317 上报，Collector 批处理后写入 Tempo；Grafana 已预置 Prometheus、Loki、Tempo 数据源和 Rust Toon 总览面板。Loki 默认保留七天日志，Tempo 默认保留七天 trace，Prometheus 默认保留十五天或最多 18GB；按生产容量调整 PVC 与保留参数。若已有 SkyWalking、Jaeger、Grafana Cloud 或托管平台，可在 overlay 中替换 Collector exporter 和 Grafana datasource，不需要修改 Rust 应用。

`deploy/logging-agent` 使用 Rust 编写的 Vector DaemonSet 读取节点 `/var/log/pods`，只保留 `rust-toon` namespace 的容器日志并写入 Loki。其 ClusterRole 只有 `get/list/watch`，日志目录只读；由于 Kubernetes 日志采集必须使用 hostPath，它独立位于 `rust-toon-logging` baseline namespace。托管集群已有 Fluent Bit、Vector 或 OTel 日志 Agent 时不要重复安装该 DaemonSet，只需把现有采集器的 Rust Toon 日志输出到内置或托管 Loki。

部署后检查：

```bash
kubectl -n rust-toon rollout status deployment/rust-toon-gateway --timeout=10m
kubectl -n rust-toon rollout status deployment/rust-toon-worker --timeout=15m
kubectl -n rust-toon rollout status statefulset/rust-toon-rnacos --timeout=10m
kubectl -n rust-toon rollout status deployment/rust-toon-otel-collector --timeout=5m
kubectl -n rust-toon rollout status statefulset/rust-toon-prometheus --timeout=10m
kubectl -n rust-toon rollout status statefulset/rust-toon-loki --timeout=10m
kubectl -n rust-toon rollout status statefulset/rust-toon-tempo --timeout=10m
kubectl -n rust-toon-logging rollout status daemonset/rust-toon-vector --timeout=10m
kubectl -n rust-toon get pods,service,hpa,pdb,networkpolicy
kubectl -n rust-toon port-forward service/rust-toon-gateway 8080:8080
kubectl -n rust-toon port-forward service/rust-toon-grafana 3000:3000
curl -fsS http://127.0.0.1:8080/readyz
```

Gateway 在收到 SIGTERM 后先关闭 readiness 并等待 10 秒摘流，Pod 提供 45 秒终止宽限；Worker 停止领取任务并在 45 秒内把在途尝试安全重新排队，Pod 提供 75 秒宽限。不要把 Kubernetes 宽限缩短到应用 drain deadline 以下。基础清单把单 Pod 导出并发设为 1、Worker ephemeral-storage limit 设为 48 GiB、`/tmp` 的 `emptyDir` 设为 44 GiB，以容纳默认最多 10 GiB 源文件、规范化副本、最终输出和 FFmpeg 余量，并为容器日志/可写层保留 4 GiB；优先通过 Worker 副本扩容。Gateway 同样在 6 GiB limit 内只给 `/tmp` 分配 5 GiB。提高并发、分辨率或单任务上限前，必须用实际码率测算峰值并同步扩大 ephemeral-storage request/limit、`emptyDir.sizeLimit` 与节点磁盘预算。

无需集群即可验证 Kustomize 引用、YAML、单 Gateway 限制、探针、安全上下文、HPA/PDB 与 NetworkPolicy：

```bash
bash script/test-k8s-deployment.sh
bash script/test-observability-deployment.sh
bash script/test-rnacos-dynamic-config.sh
```

脚本优先使用本机 `kubectl kustomize` 或 `kustomize build`；没有这两个工具时使用 Ruby YAML/语义检查，最后还有 POSIX 工具的最小 fallback。CI 会执行同一检查。

Kubernetes 环境的 PostgreSQL/对象备份优先使用托管服务 PITR、CSI VolumeSnapshot/S3 versioning 或同一恢复点能力；下面的 systemd 停写协调器用于仓库提供的单机/systemd 拓扑，不能直接当作 Kubernetes 多节点备份方案。

### 4.7 备份与恢复

仓库提供脚本与定时器样例：

分布式 compose 把 PostgreSQL/RustFS 维护端口绑定到 `MAINTENANCE_BIND_ADDRESS`（默认 `127.0.0.1`），因此宿主机上的现有备份脚本可直接使用 `postgres://...@127.0.0.1:${POSTGRES_MAINTENANCE_PORT}/rust_toon` 和 `http://127.0.0.1:${RUSTFS_MAINTENANCE_PORT}`；不要把维护地址改成公网网卡。

- `script/database/backup-consistent-set.sh`：生产定时备份入口。它按反向依赖顺序优雅停止 `BACKUP_SYSTEMD_UNITS` 中当时正在运行的 Worker/Gateway，等待写入完全静止，以同一个 `BACKUP_SET_ID` 依次执行 PostgreSQL 与对象存储备份并原子发布 set manifest，最后只恢复原先运行的服务。任一组件失败也会执行恢复服务的 trap，且不会发布完整 set manifest。
- `script/database/backup-postgres.sh`：一致性协调器使用的 `pg_dump` 组件，也可在已人工停写时单独执行；要求 `DATABASE_URL`。`BACKUP_DIR`（默认 `/var/backups/rust-toon/postgresql`）、`BACKUP_RETENTION_DAYS`（默认 14）控制目录与保留天数。
- `script/database/restore-postgres.sh`：恢复，`--backup FILE --database-url URL --confirm`。
- `script/database/backup-s3.sh`：一致性协调器使用的对象组件，使用仓库自带 `s3ctl` 客户端镜像对象桶并生成逐对象 SHA-256 清单；配置 `S3_ENDPOINT`、`S3_ACCESS_KEY`、`S3_SECRET_KEY`、`S3_BUCKET` 和 `S3_BACKUP_DIR`，宿主机还需提供 `jq`。
- `script/database/restore-s3.sh`：严格校验 manifest 版本、bucket、普通文件全集和 SHA-256 清单后恢复对象；跨 bucket 恢复必须额外传入 `--allow-bucket-mismatch`。默认保留目标端额外对象，只有显式传入 `--delete-extra --confirm` 才执行镜像删除。
- `deploy/systemd/rust-toon-consistent-backup.service` + `.timer`：每日 03:15 触发唯一的一致性恢复集；旧的两个错峰 timer 已移除。单组件 service 仅供已人工停写后的诊断/补备份使用，不能把不同时间的组件产物拼成生产恢复集。

r-nacos 的 Raft 数据不属于 PostgreSQL + 对象存储业务一致性恢复集。Compose 部署应通过带 `RNACOS_BACKUP_TOKEN` 的 r-nacos 备份接口另存配置中心备份；Kubernetes 优先对三份 PVC 做协调快照或使用 r-nacos 备份接口，并定期演练配置历史恢复。即使配置中心备份暂时不可用，Gateway/Worker 仍保留 SDK 磁盘缓存和环境变量默认值，但这不能替代配置历史备份。

systemd 样例以 `rust-toon` 用户运行，启用前需构建仓库自带的 `s3ctl`（对象存储客户端，随发布二进制一起构建）、安装 `jq`、创建可写目录并保护包含凭据的环境文件：

```bash
sudo apt-get update && sudo apt-get install -y jq
cargo build --release -p rust-toon-s3ctl
sudo install -d -o root -g root -m 0700 \
  /var/backups/rust-toon/postgresql /var/backups/rust-toon/s3 /var/backups/rust-toon/sets
sudo install -d -o root -g rust-toon -m 0750 /etc/rust-toon
sudo install -o root -g root -m 0600 \
  deploy/env/backup.env.example /etc/rust-toon/backup.env
sudoedit /etc/rust-toon/backup.env
sudo install -o root -g root -m 0644 deploy/systemd/rust-toon-*-backup.service \
  deploy/systemd/rust-toon-*-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rust-toon-consistent-backup.timer
sudo systemctl start rust-toon-consistent-backup.service
```

生产恢复必须按 `sets/rust-toon-<set-id>.json` 选择同一个 set-id 指向的 PostgreSQL 与对象存储产物，不能再按“时间相近”自行配对。恢复期间保持 Gateway/Worker 停止，依次恢复两者，最后通过 `/readyz`、成果视频抽查和对象引用审计后恢复流量。维护窗口会短暂停止生成与 API 服务；若业务不能接受停写，应改用支持同一 as-of 版本的对象存储 versioning/快照方案。建议同时开启异地复制或 object lock；文件级镜像不能替代这些能力。

## 5. 前端生产部署

```bash
corepack enable
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web --filter @vben/web-antd run build
```

产物目录：`apps/web/apps/web-antd/dist`（另产出 `dist.zip`）。交给 Nginx/CDN 托管，SPA 路由回退 `index.html`，`/api/` 反代到网关：

```nginx
location / {
    try_files $uri $uri/ /index.html;
}

location = /api/metrics {
    return 404;
}

location /api/ {
    proxy_pass http://127.0.0.1:8080/;
    proxy_http_version 1.1;
    proxy_buffering off;
    proxy_read_timeout 600s;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

要点：`X-Forwarded-For` 影响后端限流的客户端识别；SSE/流式响应需要关闭代理缓冲并加大读超时；WebSocket 端点（`/api/socket/{agent}`）依赖 Upgrade 头。生产跨域应在反代层控制，不要开启 `WEB_PERMISSIVE_CORS`。`/api/metrics` 必须在通用 `/api/` 代理规则之前精确拒绝；Prometheus 从内网直接抓 Gateway `/metrics`。

## 6. 持续集成

根目录 `.github/workflows/ci.yml`（GitHub）和 `.gitcode/workflows/ci.yml`（GitCode/AtomGit）是仓库门禁，覆盖 Rust fmt/Clippy/workspace tests、空库迁移、本地 mock AI Provider E2E、前端 typecheck/unit/build、真实依赖黑盒 E2E、分布式故障恢复、备份恢复，以及应用/可观测 Kustomize 的严格 schema 和安全策略验证。仅供应商付费生成测试保持显式运行，不进入默认 CI。

`.github/workflows/delivery.yml` 在 main/master 的 CI 成功后构建并推送后端镜像到 GHCR，生成 SBOM/provenance、用 Sigstore keyless 签名镜像 digest，再把这个不可变 digest 自动部署到 `staging` GitHub Environment。手动运行 workflow 可选择 `production`；应为 production Environment 配置 required reviewers，形成受控审批。每个环境都必须设置 base64 编码的 `KUBECONFIG_B64` Secret，并提前创建三个业务 Secret。发布失败会对 Gateway/Worker 请求 `rollout undo`，且流水线逐一等待应用、监控和 Vector rollout 成功后才结束。

基础 Kustomize 固定使用 `rust-toon` / `rust-toon-logging` namespace；staging 与 production 的 `KUBECONFIG_B64` 应指向相互隔离的集群。若必须共用集群，应先增加带 `namePrefix`、独立 namespace 和独立 ClusterRole 名称的环境 overlay，不能让两个 Environment 互相覆盖。

Gateway 仍是单副本 `Recreate`，所以自动发布包含一个明确的短暂停机窗口；流水线不会擅自把它扩成双副本。Worker、日志 Agent 与数据面任务已支持多副本/故障接管。要实现 Gateway 无停机高可用，必须先把 Agent/Workflow 活跃运行和取消控制迁到 durable Worker/共享协调层，再改滚动策略与副本数。

## 7. 常见问题

- `DATABASE_URL is required`：未导出或未写入环境文件。
- JWT 启动报错：`JWT_SECRET` 必须至少 32 字节。
- 启动报 "no enabled super administrator"：数据库缺少基线超级管理员，检查迁移是否完整执行。
- 本地 `admin` 无法登录：确认使用数据库当前密码；空库基线密码为 `admin123`，环境变量不会重置它。连续五次失败会触发持久化临时锁定。
- 前端 API 404：检查 `VITE_BASE_URL`、`VITE_GLOB_API_URL` 与 Nginx `/api/` 前缀处理。
- SSE 一次性返回：反代未关缓冲或读超时太短。
- 端口占用：`ss -ltnp | rg ':(8080|5666|5432|6379)'`。
