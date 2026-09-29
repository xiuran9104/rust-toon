#!/usr/bin/env bash
set -euo pipefail

# One-time local migration: copy every object from the legacy MinIO volume
# (service "minio", volume "rust-toon-minio") into the RustFS service that
# replaced it. The legacy volume is only ever read and is never removed; the
# temporary source container is disposable.
#
# Required: docker. Optional: SOURCE_VOLUME, COMPOSE_FILE, S3_ACCESS_KEY,
# S3_SECRET_KEY, S3_BUCKET, MINIO_SOURCE_IMAGE.
#
# For a host running the distributed compose stack instead of the local one,
# point both overrides at that project, e.g.:
#   COMPOSE_FILE=script/docker/docker-compose.distributed.yml \
#   SOURCE_VOLUME=rust-toon-distributed_minio-data bash script/migrate-minio-to-rustfs.sh

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose_file="${COMPOSE_FILE:-$script_dir/docker/docker-compose.yml}"
bucket="${S3_BUCKET:-rust-toon}"
access_key="${S3_ACCESS_KEY:-rust_toon}"
secret_key="${S3_SECRET_KEY:-rust_toon_password}"
source_image="${MINIO_SOURCE_IMAGE:-minio/minio:RELEASE.2025-04-22T22-12-26Z}"
mc_image="${S3_MC_IMAGE:-minio/mc:RELEASE.2025-04-16T18-13-26Z}"
source_container="rust-toon-minio-migration-source"
network="rust-toon-minio-migration"

cleanup() {
  docker rm -f "$source_container" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT

command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
[[ -f "$compose_file" ]] || { echo "compose file not found: $compose_file" >&2; exit 1; }

# Locate the legacy MinIO volume created by the previous compose project
# without guessing its project prefix.
source_volume="${SOURCE_VOLUME:-$(docker volume ls --format '{{.Name}}' | grep -E '(^|[_-])rust-toon-minio$' | head -n 1 || true)}"
if [[ -z "$source_volume" ]]; then
  echo "Legacy MinIO volume not found (looked for *rust-toon-minio)." >&2
  echo "Pass SOURCE_VOLUME=<name> explicitly or list candidates with: docker volume ls" >&2
  exit 1
fi
echo "Source volume: $source_volume"

# Start the RustFS target from the updated compose project and wait for it.
docker compose -f "$compose_file" up -d rustfs >/dev/null
target_container="$(docker compose -f "$compose_file" ps -q rustfs)"
[[ -n "$target_container" ]] || { echo "RustFS container did not start" >&2; exit 1; }
for _ in $(seq 1 45); do
  docker exec "$target_container" curl -fsS http://127.0.0.1:9000/health >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$target_container" curl -fsS http://127.0.0.1:9000/health >/dev/null

# Expose the legacy volume through a disposable MinIO container. It joins the
# migration network only; no host port is published. The mount must stay
# writable because MinIO maintains on-disk metadata, but this engine only ever
# read these objects before and leaves them in place.
docker rm -f "$source_container" >/dev/null 2>&1 || true
docker run -d --name "$source_container" \
  -v "$source_volume:/data" \
  -e MINIO_ROOT_USER="$access_key" \
  -e MINIO_ROOT_PASSWORD="$secret_key" \
  "$source_image" server /data >/dev/null
for _ in $(seq 1 45); do
  docker exec "$source_container" curl -fsS http://127.0.0.1:9000/minio/health/ready >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$source_container" curl -fsS http://127.0.0.1:9000/minio/health/ready >/dev/null

docker network create "$network" >/dev/null
docker network connect --alias minio-source "$network" "$source_container" >/dev/null
docker network connect --alias rustfs-target "$network" "$target_container" >/dev/null

mc_run() {
  docker run --rm --network "$network" \
    -e MC_HOST_source="http://${access_key}:${secret_key}@minio-source:9000" \
    -e MC_HOST_target="http://${access_key}:${secret_key}@rustfs-target:9000" \
    "$mc_image" "$@"
}

source_count="$(mc_run ls --recursive "source/$bucket" | wc -l)"
echo "Source objects: $source_count"
mc_run mb --ignore-existing "target/$bucket" >/dev/null
mc_run mirror --preserve "source/$bucket" "target/$bucket"

target_count="$(mc_run ls --recursive "target/$bucket" | wc -l)"
echo "Target objects: $target_count"
if [[ "$source_count" != "$target_count" ]]; then
  echo "Object counts differ after migration ($source_count -> $target_count)." >&2
  echo "Keep the legacy volume and inspect with: mc diff" >&2
  exit 1
fi

echo "Migration complete. The legacy volume '$source_volume' was kept untouched."
echo "After verifying the application, remove it manually with: docker volume rm $source_volume"
