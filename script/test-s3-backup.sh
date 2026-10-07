#!/usr/bin/env bash
set -euo pipefail

s3_container="rust-toon-s3-backup-test"
s3_port="${TEST_S3_BACKUP_PORT:-59010}"
s3_image="${S3_IMAGE:-rustfs/rustfs:1.0.0}"
test_dir="$(mktemp -d)"

cleanup() {
  docker rm -f "$s3_container" >/dev/null 2>&1 || true
  rm -rf -- "$test_dir"
}
trap cleanup EXIT
docker rm -f "$s3_container" >/dev/null 2>&1 || true

for command_name in cmp cp curl docker jq sha256sum; do
  command -v "$command_name" >/dev/null || {
    echo "$command_name is required for the object storage backup test" >&2
    exit 1
  }
done

# The in-repo s3ctl client keeps this test free of any external S3 CLI.
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q -p rust-toon-s3ctl
s3ctl="$repo_root/target/debug/s3ctl"

docker run -d --name "$s3_container" \
  -e RUSTFS_ACCESS_KEY=rust_toon \
  -e RUSTFS_SECRET_KEY=rust_toon_password \
  -p "$s3_port:9000" "$s3_image" >/dev/null

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null

export S3_ENDPOINT="http://127.0.0.1:${s3_port}"
export S3_ACCESS_KEY=rust_toon
export S3_SECRET_KEY=rust_toon_password
# RustFS can answer /health a moment before the S3 layer accepts writes.
for attempt in $(seq 1 10); do
  if S3_BUCKET=rust-toon "$s3ctl" make-bucket >/dev/null 2>&1; then
    break
  fi
  [[ "$attempt" == 10 ]] && { S3_BUCKET=rust-toon "$s3ctl" make-bucket; }
  sleep 1
done
printf '%s' 'merged-episode-render' | \
  S3_BUCKET=rust-toon "$s3ctl" put episodes/episode-1-v1.mp4

run_restore() {
  S3_ENDPOINT="http://127.0.0.1:${s3_port}" \
  S3_ACCESS_KEY=rust_toon \
  S3_SECRET_KEY=rust_toon_password \
  S3_BUCKET=rust-toon \
    bash script/database/restore-s3.sh "$@"
}

expect_restore_failure() {
  local case_name="$1"
  shift
  if run_restore "$@" >"$test_dir/${case_name}.log" 2>&1; then
    echo "restore unexpectedly accepted invalid backup case: $case_name" >&2
    exit 1
  fi
}

refresh_checksums() {
  local backup_dir="$1"
  (
    cd "$backup_dir"
    find objects manifest.json -type f -print0 \
      | LC_ALL=C sort -z \
      | xargs -0 sha256sum -- > SHA256SUMS
  )
}

backup_path="$(
  S3_ENDPOINT="http://127.0.0.1:${s3_port}" \
  S3_ACCESS_KEY=rust_toon \
  S3_SECRET_KEY=rust_toon_password \
  S3_BUCKET=rust-toon \
  S3_BACKUP_DIR="$test_dir/backups" \
    bash script/database/backup-s3.sh
)"

# A backup set ID is the public backup identifier. A second publication using
# the same identifier must fail instead of nesting into or replacing the first.
backup_set_id="${backup_path##*/rust-toon-s3-}"
fixed_date_bin="$test_dir/fixed-date-bin"
mkdir -p "$fixed_date_bin"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "%s"\n' "$backup_set_id" \
  > "$fixed_date_bin/date"
chmod 700 "$fixed_date_bin/date"
if collision_output="$(
  PATH="$fixed_date_bin:$PATH" \
  S3_ENDPOINT="http://127.0.0.1:${s3_port}" \
  S3_ACCESS_KEY=rust_toon \
  S3_SECRET_KEY=rust_toon_password \
  S3_BUCKET=rust-toon \
  S3_BACKUP_DIR="$test_dir/backups" \
    bash script/database/backup-s3.sh 2>&1
)"; then
  echo "a same-timestamp object storage backup unexpectedly overwrote its destination" >&2
  exit 1
fi
if [[ "$collision_output" != *"Refusing to overwrite existing object storage backup"* ]]; then
  echo "same-timestamp backup failed for an unexpected reason: $collision_output" >&2
  exit 1
fi

# A competing publisher must report the set ID without an unset-variable error,
# and must never remove a lock it does not own.
lock_set_id="concurrent-set-test"
competing_lock="$test_dir/backups/.rust-toon-s3-$lock_set_id.lock"
mkdir "$competing_lock"
if lock_output="$(
  BACKUP_SET_ID="$lock_set_id" \
  S3_ENDPOINT="http://127.0.0.1:${s3_port}" \
  S3_ACCESS_KEY=rust_toon \
  S3_SECRET_KEY=rust_toon_password \
  S3_BUCKET=rust-toon \
  S3_BACKUP_DIR="$test_dir/backups" \
    bash script/database/backup-s3.sh 2>&1
)"; then
  echo "a concurrent object storage backup unexpectedly acquired an existing lock" >&2
  exit 1
fi
if [[ "$lock_output" != *"backup set $lock_set_id"* ]]; then
  echo "lock collision did not report its backup set: $lock_output" >&2
  exit 1
fi
if [[ ! -d "$competing_lock" ]]; then
  echo "a failed object storage backup removed a publish lock it did not own" >&2
  exit 1
fi
rmdir -- "$competing_lock"

# Manifest parsing is strict, and a cross-bucket restore requires a separate,
# explicit opt-in in addition to --confirm.
invalid_version_type_backup="$test_dir/invalid-version-type-backup"
cp -a -- "$backup_path" "$invalid_version_type_backup"
printf '%s\n' '{"formatVersion":"1","bucket":"rust-toon"}' \
  > "$invalid_version_type_backup/manifest.json"
refresh_checksums "$invalid_version_type_backup"
expect_restore_failure invalid-version-type \
  --backup "$invalid_version_type_backup" --confirm

unsupported_version_backup="$test_dir/unsupported-version-backup"
cp -a -- "$backup_path" "$unsupported_version_backup"
printf '%s\n' '{"formatVersion":2,"bucket":"rust-toon"}' \
  > "$unsupported_version_backup/manifest.json"
refresh_checksums "$unsupported_version_backup"
expect_restore_failure unsupported-version \
  --backup "$unsupported_version_backup" --confirm

multiple_json_backup="$test_dir/multiple-json-backup"
cp -a -- "$backup_path" "$multiple_json_backup"
printf '%s\n%s\n' \
  '{"formatVersion":1,"bucket":"rust-toon"}' \
  '{"formatVersion":1,"bucket":"rust-toon"}' \
  > "$multiple_json_backup/manifest.json"
refresh_checksums "$multiple_json_backup"
expect_restore_failure multiple-json \
  --backup "$multiple_json_backup" --confirm

invalid_bucket_backup="$test_dir/invalid-bucket-backup"
cp -a -- "$backup_path" "$invalid_bucket_backup"
printf '%s\n' '{"formatVersion":1,"bucket":"../escape"}' \
  > "$invalid_bucket_backup/manifest.json"
refresh_checksums "$invalid_bucket_backup"
expect_restore_failure invalid-manifest-bucket \
  --backup "$invalid_bucket_backup" --bucket restored-copy \
  --allow-bucket-mismatch --confirm

newline_bucket_backup="$test_dir/newline-bucket-backup"
cp -a -- "$backup_path" "$newline_bucket_backup"
printf '%s\n' '{"formatVersion":1,"bucket":"rust-toon\n"}' \
  > "$newline_bucket_backup/manifest.json"
refresh_checksums "$newline_bucket_backup"
expect_restore_failure newline-manifest-bucket \
  --backup "$newline_bucket_backup" --confirm

expect_restore_failure bucket-mismatch \
  --backup "$backup_path" --bucket restored-copy --confirm
run_restore --backup "$backup_path" --bucket restored-copy \
  --allow-bucket-mismatch --confirm >/dev/null
copied_restore="$(S3_BUCKET=restored-copy "$s3ctl" get episodes/episode-1-v1.mp4)"
if [[ "$copied_restore" != "merged-episode-render" ]]; then
  echo "explicit cross-bucket restore did not match its source" >&2
  exit 1
fi

# Never dereference paths supplied by SHA256SUMS. A valid checksum for an
# absolute file outside the backup must still be rejected as an extra entry.
outside_file="$test_dir/outside-backup.txt"
printf '%s' 'must-not-be-part-of-the-backup' > "$outside_file"
read -r outside_hash _ < <(sha256sum -- "$outside_file")
unsafe_path_backup="$test_dir/unsafe-path-backup"
cp -a -- "$backup_path" "$unsafe_path_backup"
printf '%s  %s\n' "$outside_hash" "$outside_file" \
  >> "$unsafe_path_backup/SHA256SUMS"
expect_restore_failure unsafe-checksum-path \
  --backup "$unsafe_path_backup" --confirm

# The checksum file and the complete set of actual regular files must match in
# both directions: no omitted entries, undeclared files, or absent files.
missing_checksum_backup="$test_dir/missing-checksum-backup"
cp -a -- "$backup_path" "$missing_checksum_backup"
sed -i '\|  objects/episodes/episode-1-v1.mp4$|d' \
  "$missing_checksum_backup/SHA256SUMS"
expect_restore_failure missing-checksum-entry \
  --backup "$missing_checksum_backup" --confirm

undeclared_file_backup="$test_dir/undeclared-file-backup"
cp -a -- "$backup_path" "$undeclared_file_backup"
printf '%s' 'undeclared' > "$undeclared_file_backup/extra.txt"
expect_restore_failure undeclared-file \
  --backup "$undeclared_file_backup" --confirm

absent_file_backup="$test_dir/absent-file-backup"
cp -a -- "$backup_path" "$absent_file_backup"
rm -f -- "$absent_file_backup/objects/episodes/episode-1-v1.mp4"
expect_restore_failure absent-file \
  --backup "$absent_file_backup" --confirm

S3_BUCKET=rust-toon "$s3ctl" delete episodes/episode-1-v1.mp4 >/dev/null
run_restore --backup "$backup_path" --confirm >/dev/null

restored="$(S3_BUCKET=rust-toon "$s3ctl" get episodes/episode-1-v1.mp4)"
if [[ "$restored" != "merged-episode-render" ]]; then
  echo "restored object did not match its source" >&2
  exit 1
fi

# An empty object bucket still has a checksummed manifest and must remain restorable.
S3_BUCKET=rust-toon "$s3ctl" delete episodes/episode-1-v1.mp4 >/dev/null
empty_backup_path="$(
  S3_ENDPOINT="http://127.0.0.1:${s3_port}" \
  S3_ACCESS_KEY=rust_toon \
  S3_SECRET_KEY=rust_toon_password \
  S3_BUCKET=rust-toon \
  S3_BACKUP_DIR="$test_dir/empty-backups" \
    bash script/database/backup-s3.sh
)"
run_restore --backup "$empty_backup_path" --confirm >/dev/null

echo "object storage backup/restore integrity, collision, bucket, and empty-bucket checks passed"
