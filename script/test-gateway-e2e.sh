#!/usr/bin/env bash
set -euo pipefail

postgres_container="rust-toon-gateway-e2e-postgres"
redis_container="rust-toon-gateway-e2e-redis"
s3_container="rust-toon-gateway-e2e-s3"
postgres_port="${TEST_GATEWAY_POSTGRES_PORT:-55436}"
redis_port="${TEST_GATEWAY_REDIS_PORT:-56380}"
s3_port="${TEST_GATEWAY_S3_PORT:-59000}"
gateway_port="${TEST_GATEWAY_PORT:-58081}"
s3_image="${S3_IMAGE:-rustfs/rustfs:1.0.0}"
work_dir="$(mktemp -d)"
gateway_pid=""
completed=false

cleanup() {
  if [[ -n "$gateway_pid" ]]; then
    kill "$gateway_pid" >/dev/null 2>&1 || true
    wait "$gateway_pid" >/dev/null 2>&1 || true
  fi
  docker rm -f "$postgres_container" "$redis_container" "$s3_container" >/dev/null 2>&1 || true
  if [[ "$completed" != true && -f "$work_dir/gateway.log" ]]; then
    echo "Gateway log:" >&2
    tail -n 200 "$work_dir/gateway.log" >&2 || true
  fi
  rm -rf -- "$work_dir"
}
trap cleanup EXIT
docker rm -f "$postgres_container" "$redis_container" "$s3_container" >/dev/null 2>&1 || true

for command_name in cargo curl docker ffmpeg ffprobe node; do
  command -v "$command_name" >/dev/null || {
    echo "$command_name is required for the gateway E2E test" >&2
    exit 1
  }
done

docker run -d --name "$postgres_container" \
  -e POSTGRES_USER=rust_toon \
  -e POSTGRES_PASSWORD=rust_toon \
  -e POSTGRES_DB=rust_toon_test \
  -p "$postgres_port:5432" postgres:18 >/dev/null
docker run -d --name "$redis_container" \
  -p "$redis_port:6379" redis:8 >/dev/null
docker run -d --name "$s3_container" \
  -e RUSTFS_ACCESS_KEY=rust_toon \
  -e RUSTFS_SECRET_KEY=rust_toon_password \
  -p "$s3_port:9000" "$s3_image" >/dev/null

for _ in $(seq 1 45); do
  docker exec "$postgres_container" pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$postgres_container" pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test >/dev/null

for _ in $(seq 1 45); do
  docker exec "$redis_container" redis-cli ping 2>/dev/null | grep -q '^PONG$' && break
  sleep 1
done
docker exec "$redis_container" redis-cli ping | grep -q '^PONG$'

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null

cargo build -p rust-toon-gateway

export DATABASE_URL="postgres://rust_toon:rust_toon@127.0.0.1:${postgres_port}/rust_toon_test"
export REDIS_URL="redis://127.0.0.1:${redis_port}"
export JWT_SECRET="gateway-e2e-secret-with-at-least-32-bytes"
export GATEWAY_HOST="127.0.0.1"
export GATEWAY_PORT="$gateway_port"
export S3_ENDPOINT="http://127.0.0.1:${s3_port}"
export S3_ACCESS_KEY="rust_toon"
export S3_SECRET_KEY="rust_toon_password"
export S3_BUCKET="rust-toon"
export READINESS_REQUIRE_REDIS="true"
export READINESS_REQUIRE_OBJECT_STORAGE="true"
export READINESS_REQUIRE_FFMPEG="true"
export RATE_LIMIT_MAX_REQUESTS="2"
export RUST_LOG="warn"

./target/debug/rust-toon-gateway >"$work_dir/gateway.log" 2>&1 &
gateway_pid="$!"

for _ in $(seq 1 90); do
  curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null 2>&1 && break
  if ! kill -0 "$gateway_pid" >/dev/null 2>&1; then
    echo "Gateway exited before becoming ready" >&2
    exit 1
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null

health_status="$(
  curl -sS \
    -o "$work_dir/health.json" \
    -w '%{http_code}' \
    "http://127.0.0.1:${gateway_port}/health"
)"
if [[ "$health_status" != "200" ]]; then
  echo "Expected legacy /health to return 200; got $health_status" >&2
  exit 1
fi
node -e '
  const fs = require("node:fs");
  const health = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (health.service !== "gateway" || health.status !== "ok" || !health.checked_at) {
    throw new Error(`unexpected legacy health body: ${JSON.stringify(health)}`);
  }
  if ("checkedAt" in health || "checks" in health) {
    throw new Error(`legacy health contract changed: ${JSON.stringify(health)}`);
  }
' "$work_dir/health.json"

# Probe endpoints bypass the deliberately tiny global rate limit.
for _ in $(seq 1 4); do
  curl -fsS "http://127.0.0.1:${gateway_port}/health" >/dev/null
  curl -fsS "http://127.0.0.1:${gateway_port}/livez" >/dev/null
  curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null
done

# Metrics are scrape-only operations: they bypass auth/audit/user rate limits,
# use OpenMetrics, and expose bounded route templates rather than query data.
for _ in $(seq 1 4); do
  curl -fsS \
    -D "$work_dir/metrics.headers" \
    "http://127.0.0.1:${gateway_port}/metrics?token=must-not-appear" \
    >"$work_dir/metrics.txt"
done
if ! grep -qi '^content-type: application/openmetrics-text' "$work_dir/metrics.headers"; then
  echo "Gateway /metrics did not return OpenMetrics content type" >&2
  exit 1
fi
if ! grep -q '^rust_toon_http_requests_total' "$work_dir/metrics.txt"; then
  echo "Gateway /metrics did not expose HTTP request counters" >&2
  exit 1
fi
if grep -q 'must-not-appear' "$work_dir/metrics.txt"; then
  echo "Gateway metrics leaked query credentials" >&2
  exit 1
fi

docker exec "$postgres_container" psql -U rust_toon -d rust_toon_test -v ON_ERROR_STOP=1 -c \
  "INSERT INTO toonflow.tasks (id,task_class,state) VALUES (1789003357277804801,'api-contract','completed')" >/dev/null

# The smoke test runs in two passes. The first leaves its project behind and
# reports its id. A run is then inserted as if another session had started it
# and disconnected while it was still executing; the second pass must be told
# about that run and must be able to stop it. Its lease reaches into the
# future so the periodic stale-run sweep leaves it alone.
E2E_TASK_ID="1789003357277804801" E2E_BASE_URL="http://127.0.0.1:${gateway_port}" \
  E2E_KEEP_PROJECT=1 \
  node script/e2e/gateway-smoke.mjs | tee "$work_dir/smoke.log"
smoke_project_id="$(
  tail -n 1 "$work_dir/smoke.log" | node -e '
    let text = "";
    process.stdin
      .on("data", (chunk) => { text += chunk; })
      .on("end", () => {
        const { projectId } = JSON.parse(text);
        if (!Number.isSafeInteger(projectId)) process.exit(1);
        process.stdout.write(String(projectId));
      });
  '
)"
reattach_run_id="$(
  # -q suppresses the "INSERT 0 1" command tag; -At leaves only the returned id.
  docker exec "$postgres_container" psql -U rust_toon -d rust_toon_test -Atqc \
    "INSERT INTO toonflow.agent_runs(agent_type,isolation_key,project_id,input,state,think,think_level,start_time,heartbeat_at,lease_until)
     VALUES('scriptAgent','scriptAgent:${smoke_project_id}:project',${smoke_project_id},'started by a session that disconnected','running',false,0,0,0,now()+interval '10 minutes')
     RETURNING id"
)"
# Inject a running workflow run + node as if an orchestrator on another
# session owned them; the workflow pass must see it running and stop it.
workflow_run_id="$(
  docker exec "$postgres_container" psql -U rust_toon -d rust_toon_test -Atqc \
    "WITH s AS (
       INSERT INTO toonflow.scripts(id,name,content,project_id,create_time)
       VALUES(97000000000001,'gateway-e2e wf script','',${smoke_project_id},0) RETURNING id
     ), d AS (
       INSERT INTO toonflow.workflow_definitions(id,project_id,script_id,version,create_time,update_time)
       VALUES(97000000000002,${smoke_project_id},(SELECT id FROM s),1,0,0) RETURNING id
     ), r AS (
       INSERT INTO toonflow.workflow_runs(id,workflow_definition_id,project_id,script_id,definition_version,state,create_time,lease_until,heartbeat_at)
       VALUES(97000000000003,(SELECT id FROM d),${smoke_project_id},(SELECT id FROM s),1,'running',0,now()+interval '30 minutes',0) RETURNING id
     ), n AS (
       INSERT INTO toonflow.workflow_node_runs(id,workflow_run_id,node_id,node_type,state,input,create_time,lease_until,heartbeat_at)
       VALUES(97000000000004,(SELECT id FROM r),'director','director.plan','running','{}'::jsonb,0,now()+interval '30 minutes',0) RETURNING id
     )
     SELECT r.id FROM r"
)"
E2E_TASK_ID="1789003357277804801" E2E_BASE_URL="http://127.0.0.1:${gateway_port}" \
  E2E_REATTACH_PROJECT_ID="$smoke_project_id" E2E_REATTACH_RUN_ID="$reattach_run_id" \
  E2E_WORKFLOW_PROJECT_ID="$smoke_project_id" E2E_WORKFLOW_RUN_ID="$workflow_run_id" \
  node script/e2e/gateway-smoke.mjs

sleep 1
probe_audit_count="$(
  docker exec "$postgres_container" psql -U rust_toon -d rust_toon_test -Atc \
    "SELECT count(*) FROM public.infra_api_access_log WHERE request_url IN ('/health','/livez','/readyz','/metrics')"
)"
if [[ "$probe_audit_count" != "0" ]]; then
  echo "Operations probes polluted the API audit log: $probe_audit_count rows" >&2
  exit 1
fi

# The object-store probe uses the configured credentials and bucket rather
# than the storage engine's anonymous process-health endpoint.
docker stop "$s3_container" >/dev/null
curl -fsS "http://127.0.0.1:${gateway_port}/livez" >/dev/null
curl -fsS "http://127.0.0.1:${gateway_port}/health" >/dev/null
object_storage_readiness_status="$(
  curl -sS \
    -o "$work_dir/object-storage-not-ready.json" \
    -w '%{http_code}' \
    "http://127.0.0.1:${gateway_port}/readyz"
)"
if [[ "$object_storage_readiness_status" != "503" ]]; then
  echo "Expected /readyz to return 503 after object storage stopped; got $object_storage_readiness_status" >&2
  exit 1
fi
node -e '
  const fs = require("node:fs");
  const health = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (health.status !== "unavailable" || health.checks?.objectStorage?.status !== "failed") {
    throw new Error(`unexpected object-storage readiness body: ${JSON.stringify(health)}`);
  }
' "$work_dir/object-storage-not-ready.json"
docker start "$s3_container" >/dev/null
for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null

# Liveness must remain green while readiness turns red when a required
# dependency disappears. This catches probes that only return a static 200.
docker stop "$redis_container" >/dev/null
curl -fsS "http://127.0.0.1:${gateway_port}/livez" >/dev/null
curl -fsS "http://127.0.0.1:${gateway_port}/health" >/dev/null
readiness_status="$(
  curl -sS \
    -o "$work_dir/not-ready.json" \
    -w '%{http_code}' \
    "http://127.0.0.1:${gateway_port}/readyz"
)"
if [[ "$readiness_status" != "503" ]]; then
  echo "Expected /readyz to return 503 after Redis stopped; got $readiness_status" >&2
  exit 1
fi
node -e '
  const fs = require("node:fs");
  const health = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  if (health.status !== "unavailable" || health.checks?.redis?.status !== "failed") {
    throw new Error(`unexpected readiness body: ${JSON.stringify(health)}`);
  }
' "$work_dir/not-ready.json"

echo "gateway readiness failure transition passed"
completed=true
