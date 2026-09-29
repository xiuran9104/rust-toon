#!/usr/bin/env bash
set -euo pipefail

backend_container="rust-toon-distributed-edge-e2e-backend"
edge_container="rust-toon-distributed-edge-e2e-nginx"
test_network="rust-toon-distributed-edge-e2e"
edge_port="${TEST_DISTRIBUTED_EDGE_PORT:-58083}"
work_dir="$(mktemp -d)"

cleanup() {
  docker rm -f "$edge_container" "$backend_container" >/dev/null 2>&1 || true
  docker network rm "$test_network" >/dev/null 2>&1 || true
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

for command_name in curl docker; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "$command_name is required for the distributed deployment test" >&2
    exit 1
  }
done

if docker compose version >/dev/null 2>&1; then
  compose=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
  compose=(docker-compose)
else
  echo "Docker Compose v2 is required" >&2
  exit 1
fi

export POSTGRES_PASSWORD="p@ss:word"
export DATABASE_URL="postgres://rust_toon:p%40ss%3Aword@postgres:5432/rust_toon"
export JWT_SECRET="distributed-compose-test-secret-at-least-32-bytes"
export S3_SECRET_KEY="distributed-compose-object-secret"
export NACOS_USERNAME="distributed-compose-config-admin"
export NACOS_PASSWORD="distributed-compose-config-secret"
export RNACOS_CLUSTER_TOKEN="distributed-compose-cluster-token"
export RNACOS_BACKUP_TOKEN="distributed-compose-backup-token-at-least-32-bytes"
"${compose[@]}" -f script/docker/docker-compose.distributed.yml config \
  >"$work_dir/compose.yml"

if grep -q 'container_name:' "$work_dir/compose.yml"; then
  echo "Distributed compose must not pin container_name when workers scale" >&2
  exit 1
fi
if ! grep -q 'DATABASE_URL: postgres://rust_toon:p%40ss%3Aword@postgres:5432/rust_toon' \
  "$work_dir/compose.yml"; then
  echo "URL-encoded DATABASE_URL was not preserved" >&2
  exit 1
fi

docker rm -f "$edge_container" "$backend_container" >/dev/null 2>&1 || true
docker network rm "$test_network" >/dev/null 2>&1 || true
docker network create "$test_network" >/dev/null

docker run -d --name "$backend_container" \
  --network "$test_network" --network-alias gateway \
  python:3.13-slim-trixie \
  python -u -c 'from http.server import BaseHTTPRequestHandler, HTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        print(self.path, flush=True)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(self.path.encode())
    def log_message(self, *_):
        pass
HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()' >/dev/null

docker run -d --name "$edge_container" \
  --network "$test_network" \
  -p "$edge_port:8080" \
  -v "$PWD/deploy/nginx/distributed.conf:/etc/nginx/conf.d/default.conf:ro" \
  nginx:1.28-alpine >/dev/null

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${edge_port}/edge-health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${edge_port}/edge-health" >/dev/null

api_body="$(
  curl -fsS \
    "http://127.0.0.1:${edge_port}/api/system/auth/login?token=must-not-appear"
)"
direct_body="$(
  curl -fsS "http://127.0.0.1:${edge_port}/readyz?token=must-not-appear"
)"
if [[ "$api_body" != '/system/auth/login?token=must-not-appear' ]]; then
  echo "Edge did not strip exactly one /api prefix: $api_body" >&2
  exit 1
fi
if [[ "$direct_body" != '/readyz?token=must-not-appear' ]]; then
  echo "Edge changed a direct gateway route: $direct_body" >&2
  exit 1
fi
for metrics_path in /metrics /api/metrics; do
  metrics_status="$(
    curl -sS -o /dev/null -w '%{http_code}' \
      "http://127.0.0.1:${edge_port}${metrics_path}"
  )"
  if [[ "$metrics_status" != '404' ]]; then
    echo "Edge exposed private metrics path ${metrics_path}: HTTP ${metrics_status}" >&2
    exit 1
  fi
done

edge_logs="$(docker logs "$edge_container" 2>&1)"
if [[ "$edge_logs" == *must-not-appear* ]]; then
  echo "Edge access log leaked query credentials" >&2
  echo "$edge_logs" >&2
  exit 1
fi

echo "distributed compose and edge routing checks passed"
