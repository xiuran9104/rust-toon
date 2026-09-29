#!/usr/bin/env bash
set -euo pipefail

s3_endpoint="${S3_ENDPOINT:-http://127.0.0.1:9000}"
s3_access_key="${S3_ACCESS_KEY:-}"
s3_secret_key="${S3_SECRET_KEY:-}"
s3_bucket="${S3_BUCKET:-rust-toon}"
backup_path=""
confirmed=false
delete_extra=false
allow_bucket_mismatch=false

usage() {
  echo "Usage: $0 --backup DIR --confirm [--bucket NAME] [--delete-extra] [--allow-bucket-mismatch]"
  echo "S3_ACCESS_KEY and S3_SECRET_KEY must target the restore destination."
}

while (($#)); do
  case "$1" in
    --backup)
      backup_path="${2:?--backup requires a directory}"
      shift 2
      ;;
    --bucket)
      s3_bucket="${2:?--bucket requires a name}"
      shift 2
      ;;
    --delete-extra)
      delete_extra=true
      shift
      ;;
    --allow-bucket-mismatch)
      allow_bucket_mismatch=true
      shift
      ;;
    --confirm)
      confirmed=true
      shift
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

if [[ -z "$backup_path" || "$confirmed" != true ]]; then
  usage >&2
  exit 1
fi
if [[ -z "$s3_access_key" || -z "$s3_secret_key" ]]; then
  echo "S3_ACCESS_KEY and S3_SECRET_KEY are required" >&2
  exit 1
fi
backup_path="$(realpath -e -- "$backup_path")"
if [[ ! -d "$backup_path/objects" || -L "$backup_path/objects" \
  || ! -f "$backup_path/manifest.json" || -L "$backup_path/manifest.json" \
  || ! -f "$backup_path/SHA256SUMS" || -L "$backup_path/SHA256SUMS" ]]; then
  echo "Invalid object storage backup directory: $backup_path" >&2
  exit 1
fi

command -v mc >/dev/null || { echo "S3 client (mc) is required" >&2; exit 1; }
command -v cmp >/dev/null || { echo "cmp is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "sha256sum is required" >&2; exit 1; }

is_valid_bucket_name() {
  local bucket="$1"
  ((${#bucket} >= 3 && ${#bucket} <= 63)) \
    && [[ "$bucket" =~ ^[a-z0-9][a-z0-9.-]*[a-z0-9]$ ]] \
    && [[ "$bucket" != *..* ]] \
    && [[ ! "$bucket" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]
}

if ! is_valid_bucket_name "$s3_bucket"; then
  echo "Invalid target bucket name: $s3_bucket" >&2
  exit 1
fi

manifest_bucket_json=""
if ! manifest_bucket_json="$(
  jq -e -s '
    if length != 1 then
      error("manifest must contain exactly one JSON value")
    elif (.[0] | type) != "object" then
      error("manifest must be a JSON object")
    elif (.[0].formatVersion | type) != "number" or .[0].formatVersion != 1 then
      error("unsupported formatVersion")
    elif (.[0].bucket | type) != "string" then
      error("bucket must be a string")
    elif (.[0].bucket | length) < 3 or (.[0].bucket | length) > 63 then
      error("invalid bucket length")
    elif (.[0].bucket | test("^[a-z0-9][a-z0-9.-]*[a-z0-9]$") | not) then
      error("invalid bucket characters")
    elif (.[0].bucket | contains("..")) then
      error("invalid adjacent dots in bucket")
    elif (.[0].bucket | test("^([0-9]{1,3}\\.){3}[0-9]{1,3}$")) then
      error("bucket must not be an IP address")
    else
      .[0].bucket
    end
  ' "$backup_path/manifest.json" 2>/dev/null
)"; then
  echo "Invalid object storage backup manifest: expected formatVersion 1 and a bucket string" >&2
  exit 1
fi
target_bucket_json="$(jq -cn --arg bucket "$s3_bucket" '$bucket')"
if [[ "$manifest_bucket_json" != "$target_bucket_json" && "$allow_bucket_mismatch" != true ]]; then
  echo "Backup bucket $manifest_bucket_json does not match target bucket $target_bucket_json; use --allow-bucket-mismatch for an intentional cross-bucket restore" >&2
  exit 1
fi

validation_dir="$(mktemp -d)"
mc_config_dir=""
cleanup() {
  rm -rf -- "$validation_dir"
  if [[ -n "$mc_config_dir" ]]; then
    rm -rf -- "$mc_config_dir"
  fi
}
trap cleanup EXIT

if [[ -n "$(find "$backup_path" -mindepth 1 ! -type d ! -type f -print -quit)" ]]; then
  echo "Invalid object storage backup: symlinks and special files are not allowed" >&2
  exit 1
fi

# Do not pass paths from the untrusted checksum file to sha256sum --check.
# Instead, hash every actual regular file below the resolved backup root, then
# compare the two checksum record multisets. This simultaneously rejects path
# traversal, absolute paths, duplicates, missing/extra files, and changed data.
(
  cd "$backup_path"
  find . -type f ! -path './SHA256SUMS' -printf '%P\0' \
    | LC_ALL=C sort -z \
    | xargs -0 -r sha256sum -- \
    > "$validation_dir/actual-SHA256SUMS"
)
LC_ALL=C sort -- "$backup_path/SHA256SUMS" > "$validation_dir/declared.sorted"
LC_ALL=C sort -- "$validation_dir/actual-SHA256SUMS" > "$validation_dir/actual.sorted"
if ! cmp -s -- "$validation_dir/declared.sorted" "$validation_dir/actual.sorted"; then
  echo "Invalid SHA256SUMS: unsafe, duplicate, missing, extra, or modified file entry" >&2
  exit 1
fi

mc_config_dir="$(mktemp -d)"

MC_CONFIG_DIR="$mc_config_dir" mc alias set rust-toon-target \
  "$s3_endpoint" "$s3_access_key" "$s3_secret_key" >/dev/null
MC_CONFIG_DIR="$mc_config_dir" mc mb --ignore-existing "rust-toon-target/$s3_bucket" >/dev/null

mirror_args=(--quiet --overwrite --preserve)
if [[ "$delete_extra" == true ]]; then
  mirror_args+=(--remove)
fi
MC_CONFIG_DIR="$mc_config_dir" mc mirror "${mirror_args[@]}" \
  "$backup_path/objects" "rust-toon-target/$s3_bucket" >/dev/null

echo "Object storage restore completed. Run /readyz and an object-reference audit before enabling traffic."
