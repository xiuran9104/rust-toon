#!/usr/bin/env bash
set -euo pipefail

postgres_container="rust-toon-distributed-e2e-postgres"
nats_container="rust-toon-distributed-e2e-nats"
s3_container="rust-toon-distributed-e2e-s3"
nats_volume="rust-toon-distributed-e2e-nats-data"
postgres_port="${TEST_DISTRIBUTED_POSTGRES_PORT:-55437}"
nats_port="${TEST_DISTRIBUTED_NATS_PORT:-54222}"
nats_monitor_port="${TEST_DISTRIBUTED_NATS_MONITOR_PORT:-58222}"
s3_port="${TEST_DISTRIBUTED_S3_PORT:-59003}"
gateway_port="${TEST_DISTRIBUTED_GATEWAY_PORT:-58082}"
worker_a_port="${TEST_DISTRIBUTED_WORKER_A_PORT:-58101}"
worker_b_port="${TEST_DISTRIBUTED_WORKER_B_PORT:-58102}"
work_dir="$(mktemp -d)"
gateway_pid=""
worker_a_pid=""
worker_b_pid=""
completed=false

cleanup() {
  for process_id in "$gateway_pid" "$worker_a_pid" "$worker_b_pid"; do
    if [[ -n "$process_id" ]]; then
      kill "$process_id" >/dev/null 2>&1 || true
      wait "$process_id" >/dev/null 2>&1 || true
    fi
  done
  docker rm -f "$postgres_container" "$nats_container" "$s3_container" \
    >/dev/null 2>&1 || true
  docker volume rm "$nats_volume" >/dev/null 2>&1 || true
  if [[ "$completed" != true ]]; then
    for log_file in gateway worker-a worker-b; do
      if [[ -f "$work_dir/${log_file}.log" ]]; then
        echo "${log_file} log:" >&2
        tail -n 200 "$work_dir/${log_file}.log" >&2 || true
      fi
    done
  fi
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

for command_name in cargo curl docker node; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "$command_name is required for the distributed jobs E2E test" >&2
    exit 1
  }
done

docker rm -f "$postgres_container" "$nats_container" "$s3_container" \
  >/dev/null 2>&1 || true
docker volume rm "$nats_volume" >/dev/null 2>&1 || true
docker volume create "$nats_volume" >/dev/null

docker run -d --name "$postgres_container" \
  -e POSTGRES_USER=rust_toon \
  -e POSTGRES_PASSWORD=rust_toon \
  -e POSTGRES_DB=rust_toon_test \
  -p "$postgres_port:5432" postgres:18 >/dev/null
docker run -d --name "$nats_container" \
  -v "$nats_volume:/data" \
  -p "$nats_port:4222" \
  -p "$nats_monitor_port:8222" \
  nats:2 --jetstream --store_dir=/data --http_port=8222 >/dev/null
docker run -d --name "$s3_container" \
  -e RUSTFS_ACCESS_KEY=rust_toon \
  -e RUSTFS_SECRET_KEY=rust_toon_password \
  -p "$s3_port:9000" \
  rustfs/rustfs:1.0.0 >/dev/null

for _ in $(seq 1 45); do
  docker exec "$postgres_container" \
    pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test \
    >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$postgres_container" \
  pg_isready -h 127.0.0.1 -p 5432 -U rust_toon -d rust_toon_test >/dev/null

for _ in $(seq 1 45); do
  curl -fsS \
    "http://127.0.0.1:${nats_monitor_port}/healthz?js-enabled-only=true" \
    >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS \
  "http://127.0.0.1:${nats_monitor_port}/healthz?js-enabled-only=true" \
  >/dev/null

for _ in $(seq 1 45); do
  curl -fsS "http://127.0.0.1:${s3_port}/health" \
    >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${s3_port}/health" >/dev/null
# RustFS does not bundle an S3 client; run mc in a sidecar sharing the storage
# container's network namespace so 127.0.0.1:9000 reaches the S3 API.
mc_image="${S3_MC_IMAGE:-minio/mc:RELEASE.2025-04-16T18-13-26Z}"
docker pull "$mc_image" >/dev/null
MC_HOST_local="http://rust_toon:rust_toon_password@127.0.0.1:9000" \
  docker run --rm --network "container:$s3_container" "$mc_image" \
  mb --ignore-existing local/rust-toon >/dev/null

cargo build -p rust-toon-gateway -p rust-toon-worker

export DATABASE_URL="postgres://rust_toon:rust_toon@127.0.0.1:${postgres_port}/rust_toon_test"
export JWT_SECRET="distributed-e2e-secret-with-at-least-32-bytes"
export RUST_ENV="development"
export RUST_LOG="warn"
export S3_ENDPOINT="http://127.0.0.1:${s3_port}"
export S3_ACCESS_KEY="rust_toon"
export S3_SECRET_KEY="rust_toon_password"
export S3_BUCKET="rust-toon"
export GATEWAY_HOST="127.0.0.1"
export GATEWAY_PORT="$gateway_port"
export READINESS_REQUIRE_REDIS="false"
export READINESS_REQUIRE_OBJECT_STORAGE="false"
export READINESS_REQUIRE_FFMPEG="false"

./target/debug/rust-toon-gateway >"$work_dir/gateway.log" 2>&1 &
gateway_pid="$!"
for _ in $(seq 1 90); do
  curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null 2>&1 && break
  if ! kill -0 "$gateway_pid" >/dev/null 2>&1; then
    echo "Gateway exited before completing database migrations" >&2
    exit 1
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:${gateway_port}/readyz" >/dev/null

# Schema ownership belongs to the gateway. Stop it here to prove that durable
# execution and recovery do not depend on an HTTP process remaining alive.
kill "$gateway_pid"
wait "$gateway_pid"
gateway_pid=""

db_scalar() {
  docker exec "$postgres_container" \
    psql -X -v ON_ERROR_STOP=1 -U rust_toon -d rust_toon_test -Atq -c "$1"
}

start_worker() {
  local instance_id="$1"
  local port="$2"
  local log_file="$3"
  TOON_WORKER_INSTANCE_ID="$instance_id" \
  TOON_WORKER_HOST="127.0.0.1" \
  TOON_WORKER_PORT="$port" \
  TOON_WORKER_CONCURRENCY="1" \
  TOON_WORKER_LEASE_SECONDS="3" \
  TOON_WORKER_HEARTBEAT_SECONDS="1" \
  TOON_WORKER_DISPATCH_INTERVAL_MS="50" \
  TOON_WORKER_PUBLISH_CLAIM_SECONDS="1" \
  TOON_WORKER_REPUBLISH_AFTER_SECONDS="1" \
  TOON_WORKER_REAPER_INTERVAL_SECONDS="1" \
  TOON_WORKER_CLEANUP_INTERVAL_SECONDS="1" \
  TOON_WORKER_CLEANUP_TIMEOUT_SECONDS="1" \
  TOON_WORKER_DRAIN_TIMEOUT_SECONDS="5" \
  TOON_WORKER_ENABLE_TEST_JOBS="true" \
  NATS_URL="nats://127.0.0.1:${nats_port}" \
  NATS_JOB_ACK_WAIT_SECONDS="2" \
  NATS_JOB_MAX_DELIVER="10" \
  ./target/debug/rust-toon-worker >"$work_dir/${log_file}.log" 2>&1 &
  printf '%s' "$!"
}

wait_for_worker() {
  local process_id="$1"
  local port="$2"
  local label="$3"
  for _ in $(seq 1 60); do
    curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1 && return 0
    if ! kill -0 "$process_id" >/dev/null 2>&1; then
      echo "$label exited before becoming ready" >&2
      return 1
    fi
    sleep 1
  done
  echo "$label did not become ready" >&2
  return 1
}

worker_a_pid="$(start_worker worker-a "$worker_a_port" worker-a)"
wait_for_worker "$worker_a_pid" "$worker_a_port" "worker-a"

curl -fsS \
  "http://127.0.0.1:${nats_monitor_port}/jsz?accounts=true&streams=true&config=true" \
  >"$work_dir/jsz.json"
node -e '
  const fs = require("node:fs");
  const jsz = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const details = [
    ...(Array.isArray(jsz.streams) ? jsz.streams : []),
    ...(jsz.account_details ?? []).flatMap((account) => account.stream_detail ?? []),
  ];
  const stream = details.find((item) => item.name === "RUST_TOON_JOBS");
  if (!stream) throw new Error(`missing durable stream: ${JSON.stringify(jsz)}`);
  if (stream.config?.storage !== "file" || stream.config?.retention !== "workqueue") {
    throw new Error(`stream is not a file-backed work queue: ${JSON.stringify(stream.config)}`);
  }
' "$work_dir/jsz.json"

task_id="$(db_scalar "
  INSERT INTO toonflow.tasks(
    task_class,related_objects,model,description,state,start_time,input,
    progress_current,progress_total
  ) VALUES(
    'distributedE2E','{}','test.noop','分布式租约接管 E2E','running',
    (extract(epoch FROM clock_timestamp()) * 1000)::bigint,'{}'::jsonb,0,1
  ) RETURNING id
")"
job_id="$(db_scalar "
  INSERT INTO toonflow.distributed_jobs(task_id,kind,payload,max_attempts)
  VALUES(
    ${task_id},'test.noop',
    '{\"sleepMs\":8000,\"result\":{\"takeover\":true}}'::jsonb,3
  ) RETURNING id
")"

first_snapshot=""
for _ in $(seq 1 100); do
  first_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(lease_owner,''),coalesce(lease_token::text,''))
    FROM toonflow.distributed_jobs WHERE id=${job_id}
  ")"
  [[ "$first_snapshot" == running\|1\|worker-a\|* ]] && break
  sleep 0.1
done
if [[ "$first_snapshot" != running\|1\|worker-a\|* ]]; then
  echo "worker-a did not acquire the first lease: $first_snapshot" >&2
  exit 1
fi
first_lease_token="${first_snapshot##*|}"

worker_b_pid="$(start_worker worker-b "$worker_b_port" worker-b)"
wait_for_worker "$worker_b_pid" "$worker_b_port" "worker-b"

# A live heartbeat is a hard fence: another replica must not increment the
# attempt or replace the lease owner while worker-a is healthy.
sleep 2
live_snapshot="$(db_scalar "
  SELECT concat_ws('|',state,attempt,coalesce(lease_owner,''),coalesce(lease_token::text,''))
  FROM toonflow.distributed_jobs WHERE id=${job_id}
")"
if [[ "$live_snapshot" != "running|1|worker-a|${first_lease_token}" ]]; then
  echo "worker-b stole a live lease: $live_snapshot" >&2
  exit 1
fi

# SIGKILL deliberately skips draining and ACK. JetStream must redeliver the
# unacknowledged message, while the DB reaper releases the expired lease.
kill -9 "$worker_a_pid"
wait "$worker_a_pid" >/dev/null 2>&1 || true
worker_a_pid=""

second_snapshot=""
for _ in $(seq 1 160); do
  second_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(lease_owner,''),coalesce(lease_token::text,''))
    FROM toonflow.distributed_jobs WHERE id=${job_id}
  ")"
  [[ "$second_snapshot" == running\|2\|worker-b\|* ]] && break
  sleep 0.1
done
if [[ "$second_snapshot" != running\|2\|worker-b\|* ]]; then
  echo "worker-b did not take over the expired lease: $second_snapshot" >&2
  exit 1
fi
second_lease_token="${second_snapshot##*|}"
if [[ -z "$second_lease_token" || "$second_lease_token" == "$first_lease_token" ]]; then
  echo "takeover did not issue a new fencing token" >&2
  exit 1
fi

terminal_snapshot=""
for _ in $(seq 1 160); do
  terminal_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(result->>'takeover',''))
    FROM toonflow.distributed_jobs WHERE id=${job_id}
  ")"
  [[ "$terminal_snapshot" == "succeeded|2|true" ]] && break
  sleep 0.1
done
if [[ "$terminal_snapshot" != "succeeded|2|true" ]]; then
  echo "taken-over job did not complete exactly once: $terminal_snapshot" >&2
  exit 1
fi

task_state="$(db_scalar "SELECT state FROM toonflow.tasks WHERE id=${task_id}")"
if [[ "$task_state" != "success" ]]; then
  echo "user-facing task was not finalized: $task_state" >&2
  exit 1
fi
published="$(db_scalar "
  SELECT (published_at IS NOT NULL)::text
  FROM toonflow.distributed_jobs WHERE id=${job_id}
")"
if [[ "$published" != "true" ]]; then
  echo "outbox row was never acknowledged as published" >&2
  exit 1
fi

# Graceful drain must cancel an in-flight executor, rotate its generation with
# the current fence, and exit before the configured deadline. A ready peer then
# completes the replacement delivery without waiting for lease expiry.
drain_task_id="$(db_scalar "
  INSERT INTO toonflow.tasks(
    task_class,related_objects,model,description,state,start_time,input,
    progress_current,progress_total
  ) VALUES(
    'distributedE2E','{}','test.noop','Worker drain E2E','running',
    (extract(epoch FROM clock_timestamp()) * 1000)::bigint,'{}'::jsonb,0,1
  ) RETURNING id
")"
drain_job_id="$(db_scalar "
  INSERT INTO toonflow.distributed_jobs(task_id,kind,payload,max_attempts)
  VALUES(
    ${drain_task_id},'test.noop',
    '{\"sleepMs\":8000,\"result\":{\"drained\":true}}'::jsonb,3
  ) RETURNING id
")"
drain_first_snapshot=""
for _ in $(seq 1 100); do
  drain_first_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(lease_owner,''),message_id::text)
    FROM toonflow.distributed_jobs WHERE id=${drain_job_id}
  ")"
  [[ "$drain_first_snapshot" == running\|1\|worker-b\|* ]] && break
  sleep 0.1
done
if [[ "$drain_first_snapshot" != running\|1\|worker-b\|* ]]; then
  echo "worker-b did not acquire the drain test job: $drain_first_snapshot" >&2
  exit 1
fi
drain_first_message_id="${drain_first_snapshot##*|}"

worker_a_pid="$(start_worker worker-c "$worker_a_port" worker-a)"
wait_for_worker "$worker_a_pid" "$worker_a_port" "worker-c"
kill "$worker_b_pid"
for _ in $(seq 1 80); do
  ! kill -0 "$worker_b_pid" >/dev/null 2>&1 && break
  sleep 0.1
done
if kill -0 "$worker_b_pid" >/dev/null 2>&1; then
  echo "worker-b exceeded its graceful drain deadline" >&2
  exit 1
fi
worker_b_pid=""

drain_terminal_snapshot=""
for _ in $(seq 1 160); do
  drain_terminal_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(result->>'drained',''),message_id::text)
    FROM toonflow.distributed_jobs WHERE id=${drain_job_id}
  ")"
  [[ "$drain_terminal_snapshot" == succeeded\|1\|true\|* ]] && break
  sleep 0.1
done
if [[ "$drain_terminal_snapshot" != succeeded\|1\|true\|* ]]; then
  echo "gracefully drained job was not taken over: $drain_terminal_snapshot" >&2
  exit 1
fi
drain_replacement_message_id="${drain_terminal_snapshot##*|}"
if [[ "$drain_replacement_message_id" == "$drain_first_message_id" ]]; then
  echo "graceful drain did not rotate the dispatch generation" >&2
  exit 1
fi

# Simulate a JetStream stream loss/MaxDeliver terminal condition: PostgreSQL
# says the non-terminal row was published, but no message exists in NATS. The
# stale-publication scanner must rotate the de-duplication ID before rebuilding
# the message from the durable outbox.
recovery_task_id="$(db_scalar "
  INSERT INTO toonflow.tasks(
    task_class,related_objects,model,description,state,start_time,input,
    progress_current,progress_total
  ) VALUES(
    'distributedE2E','{}','test.noop','丢失消息重建 E2E','running',
    (extract(epoch FROM clock_timestamp()) * 1000)::bigint,'{}'::jsonb,0,1
  ) RETURNING id
")"
recovery_job_id="$(db_scalar "
  INSERT INTO toonflow.distributed_jobs(
    task_id,kind,payload,max_attempts,published_at
  ) VALUES(
    ${recovery_task_id},'test.noop',
    '{\"result\":{\"recovered\":true}}'::jsonb,3,now()
  ) RETURNING id
")"
original_message_id="$(db_scalar "
  SELECT message_id::text FROM toonflow.distributed_jobs WHERE id=${recovery_job_id}
")"

recovery_snapshot=""
for _ in $(seq 1 160); do
  recovery_snapshot="$(db_scalar "
    SELECT concat_ws('|',state,attempt,coalesce(result->>'recovered',''),message_id::text)
    FROM toonflow.distributed_jobs WHERE id=${recovery_job_id}
  ")"
  [[ "$recovery_snapshot" == succeeded\|1\|true\|* ]] && break
  sleep 0.1
done
if [[ "$recovery_snapshot" != succeeded\|1\|true\|* ]]; then
  echo "stale PostgreSQL publication was not rebuilt: $recovery_snapshot" >&2
  exit 1
fi
recovered_message_id="${recovery_snapshot##*|}"
if [[ -z "$recovered_message_id" || "$recovered_message_id" == "$original_message_id" ]]; then
  echo "stale publication did not rotate message_id" >&2
  exit 1
fi
recovery_task_state="$(db_scalar "
  SELECT state FROM toonflow.tasks WHERE id=${recovery_task_id}
")"
if [[ "$recovery_task_state" != "success" ]]; then
  echo "reconstructed message did not finalize the user-facing task" >&2
  exit 1
fi

echo "JetStream takeover, bounded drain, and lost-message rebuild passed"
completed=true
