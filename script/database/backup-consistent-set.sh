#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
backup_root="${CONSISTENT_BACKUP_DIR:-/var/backups/rust-toon/sets}"
systemctl_bin="${SYSTEMCTL_BIN:-systemctl}"
postgres_backup_script="${POSTGRES_BACKUP_SCRIPT:-$script_dir/backup-postgres.sh}"
object_backup_script="${S3_BACKUP_SCRIPT:-$script_dir/backup-s3.sh}"
units_value="${BACKUP_SYSTEMD_UNITS:-rust-toon-gateway.service rust-toon-worker.service}"
set_id="${BACKUP_SET_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
temporary_manifest=""

if [[ ! "$set_id" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "BACKUP_SET_ID contains unsupported characters" >&2
  exit 1
fi
case "$backup_root" in
  ""|/|/var|/var/backups)
    echo "Refusing unsafe consistent-backup directory: $backup_root" >&2
    exit 1
    ;;
esac

command -v "$systemctl_bin" >/dev/null || { echo "systemctl is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
[[ -x "$postgres_backup_script" ]] || {
  echo "PostgreSQL backup script is not executable: $postgres_backup_script" >&2
  exit 1
}
[[ -x "$object_backup_script" ]] || {
  echo "Object storage backup script is not executable: $object_backup_script" >&2
  exit 1
}

read -r -a configured_units <<< "$units_value"
if ((${#configured_units[@]} == 0)); then
  echo "BACKUP_SYSTEMD_UNITS must name the Gateway and every Worker unit" >&2
  exit 1
fi

active_units=()
for unit in "${configured_units[@]}"; do
  if "$systemctl_bin" is-active --quiet "$unit"; then
    active_units+=("$unit")
  fi
done
if ((${#active_units[@]} == 0)) && [[ "${BACKUP_ALLOW_ALREADY_QUIESCED:-false}" != "true" ]]; then
  echo "None of BACKUP_SYSTEMD_UNITS are active; refusing an unverifiable online backup" >&2
  exit 1
fi

restart_units() {
  local original_status=$?
  local restart_status=0
  trap - EXIT
  if [[ -n "$temporary_manifest" ]]; then
    rm -f -- "$temporary_manifest"
  fi
  for unit in "${active_units[@]}"; do
    if ! "$systemctl_bin" start "$unit"; then
      echo "Failed to restart $unit after backup" >&2
      restart_status=1
    fi
  done
  if ((original_status != 0)); then
    exit "$original_status"
  fi
  exit "$restart_status"
}
trap restart_units EXIT

# Stop in reverse dependency order (workers before the Gateway), allowing the
# worker's SIGTERM drain/fencing protocol to finish before the data snapshot.
for ((index=${#active_units[@]} - 1; index >= 0; index--)); do
  "$systemctl_bin" stop "${active_units[$index]}"
done
for unit in "${active_units[@]}"; do
  if "$systemctl_bin" is-active --quiet "$unit"; then
    echo "Failed to quiesce $unit" >&2
    exit 1
  fi
done

mkdir -p "$backup_root"
chmod 700 "$backup_root"
manifest="$backup_root/rust-toon-$set_id.json"
temporary_manifest="$manifest.partial"
if [[ -e "$manifest" || -L "$manifest" ]]; then
  echo "Refusing to overwrite existing backup-set manifest: $manifest" >&2
  exit 1
fi

postgres_path="$(BACKUP_SET_ID="$set_id" "$postgres_backup_script")"
object_storage_path="$(BACKUP_SET_ID="$set_id" "$object_backup_script")"

jq -cn \
  --arg set_id "$set_id" \
  --arg created_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg postgres "$postgres_path" \
  --arg objectStorage "$object_storage_path" \
  --argjson quiesced_units "$(printf '%s\n' "${active_units[@]}" | jq -R . | jq -s .)" \
  '{formatVersion: 1, setId: $set_id, createdAt: $created_at,
    consistency: "services-quiesced", quiescedUnits: $quiesced_units,
    postgresql: $postgres, objectStorage: $objectStorage}' > "$temporary_manifest"
mv -- "$temporary_manifest" "$manifest"
temporary_manifest=""
chmod 600 "$manifest"

echo "$manifest"
