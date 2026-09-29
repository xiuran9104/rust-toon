#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_dir="$(mktemp -d)"

cleanup() {
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

for command_name in jq; do
  command -v "$command_name" >/dev/null || {
    echo "$command_name is required for the consistent backup test" >&2
    exit 1
  }
done

state_dir="$test_dir/state"
mkdir -p "$state_dir"
touch "$state_dir/active-rust-toon-gateway.service"
touch "$state_dir/active-rust-toon-worker.service"

fake_systemctl="$test_dir/systemctl"
cat > "$fake_systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${FAKE_SYSTEMCTL_STATE_DIR:?}"
action="${1:?}"
shift
case "$action" in
  is-active)
    [[ "${1:-}" == "--quiet" ]] && shift
    unit="${1:?}"
    [[ -f "$state_dir/active-$unit" ]]
    ;;
  stop)
    unit="${1:?}"
    printf 'stop %s\n' "$unit" >> "$state_dir/actions.log"
    rm -f -- "$state_dir/active-$unit"
    ;;
  start)
    unit="${1:?}"
    printf 'start %s\n' "$unit" >> "$state_dir/actions.log"
    touch "$state_dir/active-$unit"
    ;;
  *)
    echo "unexpected fake systemctl action: $action" >&2
    exit 2
    ;;
esac
EOF

fake_postgres_backup="$test_dir/backup-postgres"
cat > "$fake_postgres_backup" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${FAKE_SYSTEMCTL_STATE_DIR:?}"
set_id="${BACKUP_SET_ID:?}"
[[ ! -e "$state_dir/active-rust-toon-gateway.service" ]]
[[ ! -e "$state_dir/active-rust-toon-worker.service" ]]
printf '%s\n' "$set_id" > "$state_dir/postgres-set-id"
backup_path="$state_dir/rust-toon-$set_id.dump"
touch "$backup_path"
printf '%s\n' "$backup_path"
EOF

fake_object_backup="$test_dir/backup-object-storage"
cat > "$fake_object_backup" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

state_dir="${FAKE_SYSTEMCTL_STATE_DIR:?}"
set_id="${BACKUP_SET_ID:?}"
[[ ! -e "$state_dir/active-rust-toon-gateway.service" ]]
[[ ! -e "$state_dir/active-rust-toon-worker.service" ]]
printf '%s\n' "$set_id" > "$state_dir/object-set-id"
if [[ "${FAKE_OBJECT_BACKUP_FAIL:-false}" == "true" ]]; then
  exit 42
fi
backup_path="$state_dir/rust-toon-s3-$set_id"
mkdir "$backup_path"
printf '%s\n' "$backup_path"
EOF
chmod 700 "$fake_systemctl" "$fake_postgres_backup" "$fake_object_backup"

run_coordinator() {
  FAKE_SYSTEMCTL_STATE_DIR="$state_dir" \
  SYSTEMCTL_BIN="$fake_systemctl" \
  POSTGRES_BACKUP_SCRIPT="$fake_postgres_backup" \
  S3_BACKUP_SCRIPT="$fake_object_backup" \
  CONSISTENT_BACKUP_DIR="$test_dir/sets" \
  BACKUP_SYSTEMD_UNITS="rust-toon-gateway.service rust-toon-worker.service inactive.service" \
  BACKUP_SET_ID="$1" \
    bash "$repo_root/script/database/backup-consistent-set.sh"
}

manifest="$(run_coordinator test-set-001)"
[[ -f "$manifest" ]]
[[ "$(cat "$state_dir/postgres-set-id")" == "test-set-001" ]]
[[ "$(cat "$state_dir/object-set-id")" == "test-set-001" ]]
[[ -f "$state_dir/active-rust-toon-gateway.service" ]]
[[ -f "$state_dir/active-rust-toon-worker.service" ]]
[[ ! -f "$state_dir/active-inactive.service" ]]

expected_actions=$'stop rust-toon-worker.service\nstop rust-toon-gateway.service\nstart rust-toon-gateway.service\nstart rust-toon-worker.service'
[[ "$(cat "$state_dir/actions.log")" == "$expected_actions" ]]

jq -e \
  --arg postgres "$state_dir/rust-toon-test-set-001.dump" \
  --arg objectStorage "$state_dir/rust-toon-s3-test-set-001" \
  '.formatVersion == 1 and .setId == "test-set-001" and
   .consistency == "services-quiesced" and
   .quiescedUnits == ["rust-toon-gateway.service", "rust-toon-worker.service"] and
   .postgresql == $postgres and .objectStorage == $objectStorage' \
  "$manifest" >/dev/null

: > "$state_dir/actions.log"
if FAKE_OBJECT_BACKUP_FAIL=true run_coordinator test-set-failure \
  > "$test_dir/failure.out" 2> "$test_dir/failure.err"; then
  echo "consistent backup unexpectedly succeeded after a component failure" >&2
  exit 1
fi
[[ -f "$state_dir/active-rust-toon-gateway.service" ]]
[[ -f "$state_dir/active-rust-toon-worker.service" ]]
[[ ! -e "$test_dir/sets/rust-toon-test-set-failure.json" ]]
[[ ! -e "$test_dir/sets/rust-toon-test-set-failure.json.partial" ]]
[[ "$(cat "$state_dir/actions.log")" == "$expected_actions" ]]

echo "Consistent backup set ID, quiesce ordering, manifest, and failure recovery checks passed"
