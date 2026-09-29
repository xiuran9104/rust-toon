#!/usr/bin/env bash
set -euo pipefail

# One-time migration for legacy /upload/* paths.
# Required: DATABASE_URL and AWS CLI. Optional: S3_ENDPOINT, S3_ACCESS_KEY,
# S3_SECRET_KEY, S3_BUCKET, S3_ALIAS, INFRA_UPLOAD_DIR.

: "${DATABASE_URL:?DATABASE_URL is required}"
command -v aws >/dev/null || { echo "需要安装 aws CLI" >&2; exit 1; }
S3_ENDPOINT="${S3_ENDPOINT:-http://127.0.0.1:9000}"
S3_ACCESS_KEY="${S3_ACCESS_KEY:-rust_toon}"
S3_SECRET_KEY="${S3_SECRET_KEY:-rust_toon_password}"
S3_BUCKET="${S3_BUCKET:-rust-toon}"
UPLOAD_DIR="${INFRA_UPLOAD_DIR:-storage/uploads}"

if [[ ! -d "$UPLOAD_DIR" ]]; then
  echo "没有找到旧上传目录：$UPLOAD_DIR"
  exit 0
fi

export AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY"
export AWS_SECRET_ACCESS_KEY="$S3_SECRET_KEY"
export AWS_DEFAULT_REGION="${S3_REGION:-us-east-1}"
AWS=(aws --endpoint-url "$S3_ENDPOINT")
"${AWS[@]}" s3api head-bucket --bucket "$S3_BUCKET" >/dev/null 2>&1 || \
  "${AWS[@]}" s3 mb "s3://$S3_BUCKET" >/dev/null

psql "$DATABASE_URL" -At -F $'\t' -c \
  "SELECT DISTINCT file_path FROM toonflow.images WHERE file_path LIKE '/upload/%' AND file_path IS NOT NULL" |
while IFS=$'\t' read -r file_path; do
  [[ -z "$file_path" ]] && continue
  relative="${file_path#/upload/}"
  source="$UPLOAD_DIR/$relative"
  if [[ ! -f "$source" ]]; then
    echo "跳过不存在文件：$source" >&2
    continue
  fi
  key="toonflow/migrated/$relative"
  echo "迁移：$source -> $key"
  "${AWS[@]}" s3 cp "$source" "s3://$S3_BUCKET/$key" >/dev/null
  new_path="/toonflow/assets/files/$key"
  psql "$DATABASE_URL" -v old_path="$file_path" -v new_path="$new_path" -q -c \
    "UPDATE toonflow.images SET file_path :'new_path' WHERE file_path = :'old_path';" >/dev/null
  rm -f -- "$source"
done

# Migrate legacy Infra file records as well. New uploads keep the same `/upload/*`
# URL for compatibility, but the object is stored under object storage `infra/`.
psql "$DATABASE_URL" -At -F $'\t' -c \
  "SELECT id, path FROM infra_file WHERE path LIKE '/upload/%' OR url LIKE '/upload/%'" |
while IFS=$'\t' read -r file_id file_path; do
  [[ -z "$file_id" || -z "$file_path" ]] && continue
  relative="${file_path#/upload/}"
  source="$UPLOAD_DIR/$relative"
  if [[ ! -f "$source" ]]; then
    echo "跳过不存在的 Infra 文件：$source" >&2
    continue
  fi
  key="infra/$relative"
  echo "迁移 Infra：$source -> $key"
  "${AWS[@]}" s3 cp "$source" "s3://$S3_BUCKET/$key" >/dev/null
  rm -f -- "$source"
done

# Remove empty legacy directories, leaving the root directory intact.
find "$UPLOAD_DIR" -mindepth 1 -type d -empty -delete
echo "旧上传文件迁移完成。"
