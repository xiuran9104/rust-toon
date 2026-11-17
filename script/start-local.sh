#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-backend}"

case "$mode" in
  infra|gateway|worker|backend|all) ;;
  *)
    echo "Usage: $0 [infra|gateway|worker|backend|all]" >&2
    exit 2
    ;;
esac

docker compose -f "$repository_root/script/docker/docker-compose.yml" \
  up -d --wait --wait-timeout 90
if [[ "$mode" == infra ]]; then
  docker compose -f "$repository_root/script/docker/docker-compose.yml" ps
  exit 0
fi

export DATABASE_URL="${DATABASE_URL:-postgres://rust_toon:rust_toon@127.0.0.1:5432/rust_toon}"
export REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}"
export NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
export JWT_SECRET="${JWT_SECRET:-local-development-jwt-secret-change-me-32bytes}"
export NACOS_ENABLED="${NACOS_ENABLED:-true}"
export NACOS_REQUIRED="${NACOS_REQUIRED:-true}"
export NACOS_SERVER_ADDR="${NACOS_SERVER_ADDR:-127.0.0.1:8848}"
export NACOS_USERNAME="${NACOS_USERNAME:-rust_toon}"
export NACOS_PASSWORD="${NACOS_PASSWORD:-rust_toon_nacos_password}"
export RUST_LOG="${RUST_LOG:-info}"
# 本地视频服务（如自建 OpenAI 兼容网关）常同步等待数分钟出片，默认 120 秒
# 超时会打断生成，因此本地默认放宽到上限 900 秒。
export AI_REQUEST_TIMEOUT_SECONDS="${AI_REQUEST_TIMEOUT_SECONDS:-900}"

cd "$repository_root"
gateway_ready_url="${LOCAL_GATEWAY_READY_URL:-http://127.0.0.1:${GATEWAY_PORT:-8080}/readyz}"
worker_ready_url="${LOCAL_WORKER_READY_URL:-http://127.0.0.1:${WORKER_PORT:-8081}/readyz}"
frontend_url="${LOCAL_FRONTEND_URL:-http://127.0.0.1:5666}"
gateway_port="${GATEWAY_PORT:-8080}"
worker_port="${WORKER_PORT:-8081}"
frontend_port="${LOCAL_FRONTEND_PORT:-5666}"
is_ready() { curl -fsS --max-time 2 "$1" >/dev/null 2>&1; }
port_is_listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_for_existing() {
  local name="$1" url="$2"
  echo "$name port is already in use; waiting for its readiness endpoint: $url"
  for _ in $(seq 1 90); do
    is_ready "$url" && return 0
    sleep 1
  done
  echo "$name owns its port but did not become ready: $url" >&2
  return 1
}

if [[ "$mode" == gateway ]]; then
  if is_ready "$gateway_ready_url"; then
    echo "Gateway is already ready: $gateway_ready_url"
    exit 0
  fi
  if port_is_listening "$gateway_port"; then
    wait_for_existing "Gateway" "$gateway_ready_url"
    exit $?
  fi
  exec cargo run -p rust-toon-gateway
fi
if [[ "$mode" == worker ]]; then
  if is_ready "$worker_ready_url"; then
    echo "Worker is already ready: $worker_ready_url"
    exit 0
  fi
  if port_is_listening "$worker_port"; then
    wait_for_existing "Worker" "$worker_ready_url"
    exit $?
  fi
  exec cargo run -p rust-toon-worker
fi

gateway_pid=""
worker_pid=""
frontend_pid=""
cleanup() {
  [[ -n "$gateway_pid" ]] && kill "$gateway_pid" 2>/dev/null || true
  [[ -n "$worker_pid" ]] && kill "$worker_pid" 2>/dev/null || true
  [[ -n "$frontend_pid" ]] && kill "$frontend_pid" 2>/dev/null || true
  [[ -n "$gateway_pid" ]] && wait "$gateway_pid" 2>/dev/null || true
  [[ -n "$worker_pid" ]] && wait "$worker_pid" 2>/dev/null || true
  [[ -n "$frontend_pid" ]] && wait "$frontend_pid" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

if is_ready "$gateway_ready_url"; then
  echo "Reusing ready Gateway: $gateway_ready_url"
elif port_is_listening "$gateway_port"; then
  wait_for_existing "Gateway" "$gateway_ready_url"
else
  cargo run -p rust-toon-gateway &
  gateway_pid=$!
fi

# The gateway owns migrations. On a clean database the worker must not start
# until /readyz confirms that the full migration chain has committed.
for _ in $(seq 1 180); do
  if curl -fsS "$gateway_ready_url" >/dev/null 2>&1; then
    break
  fi
  if [[ -n "$gateway_pid" ]] && ! kill -0 "$gateway_pid" >/dev/null 2>&1; then
    echo "Gateway exited before completing database migrations" >&2
    exit 1
  fi
  sleep 1
done
if ! curl -fsS "$gateway_ready_url" >/dev/null 2>&1; then
  echo "Gateway did not become ready: $gateway_ready_url" >&2
  exit 1
fi

if is_ready "$worker_ready_url"; then
  echo "Reusing ready Worker: $worker_ready_url"
elif port_is_listening "$worker_port"; then
  wait_for_existing "Worker" "$worker_ready_url"
else
  cargo run -p rust-toon-worker &
  worker_pid=$!
fi
if [[ "$mode" == backend ]]; then
  started_pids=()
  [[ -n "$gateway_pid" ]] && started_pids+=("$gateway_pid")
  [[ -n "$worker_pid" ]] && started_pids+=("$worker_pid")
  if [[ ${#started_pids[@]} -eq 0 ]]; then
    echo "Gateway and Worker are already running"
    exit 0
  fi
  wait -n "${started_pids[@]}"
  exit $?
fi
if is_ready "$frontend_url"; then
  echo "Reusing ready frontend: $frontend_url"
elif port_is_listening "$frontend_port"; then
  wait_for_existing "Frontend" "$frontend_url"
else
  pnpm --dir apps/web --filter @vben/web-antd run dev &
  frontend_pid=$!
fi
started_pids=()
[[ -n "$gateway_pid" ]] && started_pids+=("$gateway_pid")
[[ -n "$worker_pid" ]] && started_pids+=("$worker_pid")
[[ -n "$frontend_pid" ]] && started_pids+=("$frontend_pid")
if [[ ${#started_pids[@]} -eq 0 ]]; then
  echo "Gateway, Worker, and frontend are already running"
  exit 0
fi
wait -n "${started_pids[@]}"
