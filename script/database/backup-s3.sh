#!/usr/bin/env bash
set -euo pipefail
umask 077

s3_endpoint="${S3_ENDPOINT:-http://127.0.0.1:9000}"
s3_access_key="${S3_ACCESS_KEY:-}"
s3_secret_key="${S3_SECRET_KEY:-}"
s3_bucket="${S3_BUCKET:-rust-toon}"
backup_dir="${S3_BACKUP_DIR:-/var/backups/rust-toon/s3}"
retention_days="${BACKUP_RETENTION_DAYS:-14}"
backup_set_id="${BACKUP_SET_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"

usage() {
  echo "Usage: S3_ACCESS_KEY=... S3_SECRET_KEY=... $0 [--output-dir DIR] [--retention-days DAYS] [--bucket NAME]"
}

while (($#)); do
  case "$1" in
    --output-dir)
      backup_dir="${2:?--output-dir requires a directory}"
      shift 2
      ;;
    --retention-days)
      retention_days="${2:?--retention-days requires a number}"
      shift 2
      ;;
    --bucket)
      s3_bucket="${2:?--bucket requires a name}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$s3_access_key" || -z "$s3_secret_key" ]]; then
  echo "S3_ACCESS_KEY and S3_SECRET_KEY are required" >&2
  exit 1
fi
if [[ ! "$retention_days" =~ ^[0-9]+$ ]] || ((retention_days < 1)); then
  echo "BACKUP_RETENTION_DAYS must be a positive integer" >&2
  exit 1
fi
if [[ ! "$backup_set_id" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "BACKUP_SET_ID contains unsupported characters" >&2
  exit 1
fi
case "$backup_dir" in
  ""|/|/var|/var/backups)
    echo "Refusing unsafe backup directory: $backup_dir" >&2
    exit 1
    ;;
esac

backup_dir="$(realpath -m -- "$backup_dir")"
case "$backup_dir" in
  /|/var|/var/backups)
    echo "Refusing unsafe backup directory: $backup_dir" >&2
    exit 1
    ;;
esac

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
s3ctl_bin="${S3CTL_BIN:-$repo_root/target/release/s3ctl}"
if [[ ! -x "$s3ctl_bin" ]]; then
  s3ctl_bin="$repo_root/target/debug/s3ctl"
fi
[[ -x "$s3ctl_bin" ]] || {
  echo "s3ctl is required; build it with: cargo build --release -p rust-toon-s3ctl" >&2
  exit 1
}
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "sha256sum is required" >&2; exit 1; }

mkdir -p "$backup_dir"
chmod 700 "$backup_dir"

final_path="$backup_dir/rust-toon-s3-$backup_set_id"
publish_lock="$backup_dir/.rust-toon-s3-$backup_set_id.lock"
created_at="$(date -u +%Y%m%dT%H%M%SZ)"
temporary_path=""
owns_publish_lock=false

cleanup() {
  if [[ -n "$temporary_path" ]]; then
    rm -rf -- "$temporary_path"
  fi
  if [[ "$owns_publish_lock" == "true" ]]; then
    rmdir -- "$publish_lock" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if ! mkdir -- "$publish_lock" 2>/dev/null; then
  echo "A object storage backup is already being published for backup set $backup_set_id" >&2
  exit 1
fi
owns_publish_lock=true
if [[ -e "$final_path" || -L "$final_path" ]]; then
  echo "Refusing to overwrite existing object storage backup: $final_path" >&2
  exit 1
fi

temporary_path="$(mktemp -d "$backup_dir/.rust-toon-s3-$backup_set_id.XXXXXX")"
s3ctl() {
  S3_ENDPOINT="$s3_endpoint" \
  S3_ACCESS_KEY="$s3_access_key" \
  S3_SECRET_KEY="$s3_secret_key" \
  S3_BUCKET="$s3_bucket" \
    "$s3ctl_bin" "$@"
}
s3ctl stat-bucket >/dev/null
mkdir -p "$temporary_path/objects"
s3ctl mirror-to "$temporary_path/objects"

jq -cn --arg bucket "$s3_bucket" --arg created_at "$created_at" --arg set_id "$backup_set_id" \
  '{formatVersion: 1, bucket: $bucket, createdAt: $created_at, setId: $set_id}' \
  > "$temporary_path/manifest.json"
(
  cd "$temporary_path"
  find objects manifest.json -type f -print0 \
    | LC_ALL=C sort -z \
    | xargs -0 sha256sum -- > SHA256SUMS
)

if ! mv -T -- "$temporary_path" "$final_path"; then
  echo "Failed to publish object storage backup without replacing an existing destination: $final_path" >&2
  exit 1
fi
temporary_path=""
chmod -R go-rwx "$final_path"

find "$backup_dir" -mindepth 1 -maxdepth 1 -type d \
  -name 'rust-toon-s3-*' -mtime "+$retention_days" -exec rm -rf -- {} +

echo "$final_path"
