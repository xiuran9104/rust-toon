#!/usr/bin/env bash
set -euo pipefail

postgres_container="rust-toon-production-e2e-postgres"
nats_container="rust-toon-production-e2e-nats"
s3_container="rust-toon-production-e2e-s3"
postgres_port="${TEST_PRODUCTION_POSTGRES_PORT:-55435}"
nats_port="${TEST_PRODUCTION_NATS_PORT:-54224}"
nats_monitor_port="${TEST_PRODUCTION_NATS_MONITOR_PORT:-58224}"
s3_port="${TEST_PRODUCTION_S3_PORT:-59002}"
worker_port="${TEST_PRODUCTION_WORKER_PORT:-58103}"
s3_image="${S3_IMAGE:-rustfs/rustfs:1.0.0}"
upload_dir="$(mktemp -d)"
worker_pid=""
cleanup() {
  if [[ -n "$worker_pid" ]]; then
    kill "$worker_pid" >/dev/null 2>&1 || true
    wait "$worker_pid" >/dev/null 2>&1 || true
  fi
  docker rm -f "$postgres_container" "$nats_container" "$s3_container" >/dev/null 2>&1 || true
  rm -rf -- "$upload_dir"
}
trap cleanup EXIT
docker rm -f "$postgres_container" "$nats_container" "$s3_container" >/dev/null 2>&1 || true

for command_name in cargo curl docker ffmpeg; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "$command_name is required for the production export E2E test" >&2
    exit 1
  }
done

docker run -d --name "$postgres_container" \
  -e POSTGRES_USER=rust_toon \
  -e POSTGRES_PASSWORD=rust_toon \
  -e POSTGRES_DB=rust_toon_test \
  -p "$postgres_port:5432" postgres:18 >/dev/null
docker run -d --name "$nats_container" \
  -p "$nats_port:4222" \
  -p "$nats_monitor_port:8222" \
  nats:2 --jetstream --store_dir=/data --http_port=8222 >/dev/null
docker run -d --name "$s3_container" \
  -e RUSTFS_ACCESS_KEY=rust_toon \
  -e RUSTFS_SECRET_KEY=rust_toon_password \
  -p "$s3_port:9000" "$s3_image" >/dev/null

for _ in $(seq 1 30); do
  docker exec "$postgres_container" pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$postgres_container" pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test >/dev/null

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${nats_monitor_port}/healthz?js-enabled-only=true" \
    >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${nats_monitor_port}/healthz?js-enabled-only=true" >/dev/null

export TEST_DATABASE_URL="postgres://rust_toon:rust_toon@127.0.0.1:${postgres_port}/rust_toon_test"
export DATABASE_URL="$TEST_DATABASE_URL"
export TEST_UPLOAD_DIR="$upload_dir"
export NATS_URL="nats://127.0.0.1:${nats_port}"
export S3_ENDPOINT="http://127.0.0.1:${s3_port}"
export S3_ACCESS_KEY="rust_toon"
export S3_SECRET_KEY="rust_toon_password"
export S3_BUCKET="rust-toon"

# The gateway owns schema initialization in production. Run the same SQLx
# migration chain before starting the independent worker, then exercise the
# actual JetStream/worker path from the Toon handler test below.
cargo test -p rust-toon-framework-database --test migrations \
  applies_all_migrations_to_empty_postgres -- --ignored --nocapture
cargo build -p rust-toon-worker
TOON_WORKER_INSTANCE_ID="production-export-e2e" \
TOON_WORKER_HOST="127.0.0.1" \
TOON_WORKER_PORT="$worker_port" \
TOON_WORKER_CONCURRENCY="1" \
./target/debug/rust-toon-worker >"$upload_dir/worker.log" 2>&1 &
worker_pid="$!"
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:${worker_port}/readyz" >/dev/null 2>&1 && break
  if ! kill -0 "$worker_pid" >/dev/null 2>&1; then
    echo "Toon worker exited before becoming ready" >&2
    tail -n 200 "$upload_dir/worker.log" >&2 || true
    exit 1
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:${worker_port}/readyz" >/dev/null
cargo test -p rust-toon-toon-server production_e2e_tests -- --ignored --nocapture
