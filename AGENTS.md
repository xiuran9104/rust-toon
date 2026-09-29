# AI Startup Guide

This file contains the repository invariants and safety rules for AI coding agents. Read it before starting services or changing deployment artifacts. Do not duplicate the full operations manual here: use `docs/deployment.md` for commands, `docs/configuration.md` for configuration semantics, and `docs/README.md` for the documentation map.

## Project Shape

- Backend: Rust workspace, HTTP gateway entrypoint at `services/gateway` and durable media worker at `services/toon-worker`.
- Frontend: Vben Admin app at `apps/web`, main app package `@vben/web-antd`.
- Database migrations: `sql/postgresql`, executed automatically by the Rust gateway on startup. `0001_initial.sql` is the consolidated schema and baseline data.
- Bootstrap reference: `sql/bootstrap/current.sql` is a reference-only `pg_dump` snapshot and is never loaded by the application. The migration chain is sufficient to initialize a new server without `current.sql`.
- Local infrastructure: PostgreSQL, Redis, NATS, RustFS, and r-nacos via `script/docker/docker-compose.yml`.

The durable worker is horizontally scalable. Keep the current Gateway at one production replica: Toon Agent/Workflow live-run registries are still process-local even though video export and cleanup jobs are distributed. Do not advertise or configure Gateway horizontal scaling until those realtime runtimes are migrated to durable workers.

Do not mount `sql/postgresql` into PostgreSQL init scripts. The gateway owns database initialization through SQLx, and PostgreSQL should start as an empty database.

## Database Change Workflow

Whenever a database schema or baseline-data change is made:

1. Add a new numbered migration under `sql/postgresql`; never edit a migration that has already been released or applied.
2. Make the migration idempotent so both upgraded databases and empty-database bootstrap are supported.
3. Run `bash script/test-database-migrations.sh` to prove an empty PostgreSQL instance reaches the latest schema and baseline data without importing `current.sql`.
4. After applying all migrations to a clean reference database, export a fresh `sql/bootstrap/current.sql` with `pg_dump` for review and comparison. The gateway must remain fully functional when this snapshot is absent.
5. Update the expected migration count and relevant baseline assertions in `crates/framework/database/tests/migrations.rs`.

The migration history was intentionally reset to the consolidated `0001_initial.sql`. A database from the older, pre-consolidation lineage needs an explicit backup-and-migration decision; never infer that a current database should be deleted. Databases already carrying the current `_sqlx_migrations` history must be upgraded in place. Do not rewrite `0001` or any subsequently applied migration.

## Local Development Startup

The canonical local procedure is `docs/deployment.md#2-本地开发`. For the default ports, run from the repository root:

```bash
bash script/start-local.sh all
```

Available modes are `infra`, `gateway`, `worker`, `backend`, and `all`. The `backend` and `all` modes wait for Gateway readiness before starting the Worker; standalone `worker` mode assumes a ready Gateway has already completed migrations. When ports or existing local data may conflict, inspect containers, named volumes, and listeners before starting. Never run `docker compose down -v`, remove a named volume, or recreate PostgreSQL merely to resolve a port conflict.

For manual debugging, start services in this order:

1. PostgreSQL, Redis, NATS with JetStream, RustFS, and r-nacos.
2. Gateway; wait for `/readyz` so migrations have committed.
3. Toon Worker.
4. Vben frontend.

Open:

- Frontend: `http://127.0.0.1:5666`
- Backend health: `http://127.0.0.1:8080/health`
- Backend liveness/readiness: `http://127.0.0.1:8080/livez`, `http://127.0.0.1:8080/readyz`
- Worker liveness/readiness: `http://127.0.0.1:8081/livez`, `http://127.0.0.1:8081/readyz`
- OpenAPI: `http://127.0.0.1:8080/openapi.json`
- RustFS console: `http://127.0.0.1:9001`
- r-nacos console: `http://127.0.0.1:10848`

Default local application account:

- Username: `admin`
- Password: `admin123`

Default local r-nacos account: `rust_toon` / `rust_toon_nacos_password`.

The application account is baseline data from `sql/postgresql/0001_initial.sql`. Gateway startup only verifies that an enabled `super_admin` exists; it does not create users and does not read `BOOTSTRAP_ADMIN_USERNAME` or `BOOTSTRAP_ADMIN_PASSWORD`. Existing passwords are never reset at startup. Change the development password after first login and never expose the baseline credential in production.

## Local Verification

Use these checks after startup:

```bash
curl -fsS http://127.0.0.1:8080/health
cargo test --workspace
bash script/test-database-migrations.sh
bash script/test-gateway-e2e.sh
bash script/test-production-e2e.sh
bash script/test-distributed-jobs-e2e.sh
bash script/test-rnacos-dynamic-config.sh
bash script/test-s3-backup.sh
pnpm --dir apps/web run test:unit
pnpm --dir apps/web --filter @vben/web-antd run typecheck
```

Frontend production build check:

```bash
pnpm --dir apps/web --filter @vben/web-antd run build
```

## Production Deployment

Use `docs/deployment.md#4-新服务器生产部署` as the canonical production runbook and `docs/configuration.md` as the configuration reference. The local `script/docker/docker-compose.yml` contains development credentials and exposed host ports; it is not a production manifest. Use the hardened distributed Compose skeleton, systemd examples, Kubernetes base, or managed dependencies described in the deployment guide.

Production invariants:

- Run exactly one Gateway replica until Agent/Workflow live-run coordination is durable.
- Scale media capacity through Toon Worker replicas.
- Start Workers only after Gateway readiness confirms migrations have completed.
- Keep PostgreSQL, Redis, NATS, RustFS, and r-nacos credentials outside Git.
- Worker nodes need FFmpeg and FFprobe; Gateway nodes do not execute video merges.
- Serve frontend static output through a reverse proxy/CDN and keep `/metrics` off public routes.

## Backups

Back up PostgreSQL and object storage as one recovery set. The production entrypoint is:

```bash
sudo bash script/database/backup-consistent-set.sh
```

The coordinator gracefully stops the configured Gateway/Worker systemd units, runs both component backups with the same `BACKUP_SET_ID`, publishes a set manifest, and restarts only units that were active. The matching restore scripts require an explicit `--confirm`. See `docs/deployment.md#47-备份与恢复`; do not reconstruct a recovery set by pairing unrelated PostgreSQL and object storage backups.

## Common Problems

- `DATABASE_URL is required`: export it or add it to the systemd environment file.
- JWT startup error: `JWT_SECRET` must be at least 32 bytes.
- `no enabled super administrator`: verify that the complete migration chain and baseline data were applied. Startup does not create an administrator.
- Repeated local login failures: the baseline account is `admin` / `admin123`; five failures trigger a persistent temporary lockout.
- Frontend API 404: check `VITE_BASE_URL`, `VITE_GLOB_API_URL`, and Nginx `/api/` proxy prefix handling.
- SSE responses arrive all at once: disable proxy buffering and increase read timeout.
- `/readyz` returns 503: inspect the per-dependency checks and verify PostgreSQL plus required Redis/object-storage/FFmpeg endpoints.
- Queued video exports never start: verify at least one worker is ready and NATS has JetStream enabled; check `toonflow.distributed_jobs` for `last_error` and lease state.
- Port already in use: check `ss -ltnp | rg ':(8080|8081|5666|4222|5432|6379)'`.
