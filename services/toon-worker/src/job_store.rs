use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_toon_framework_jobs::{INFRA_SCHEDULED_JOB_KIND, ScheduledJobPayload, next_occurrence};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct JobStore {
    pool: PgPool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DispatchJob {
    pub id: i64,
    pub message_id: Uuid,
    pub task_id: i64,
    pub kind: String,
    pub trace_id: String,
    pub trace_context: Value,
    pub payload: Value,
    pub attempt: i32,
    pub publish_token: Uuid,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedJob {
    pub id: i64,
    pub task_id: i64,
    pub trace_id: String,
    pub payload: Value,
    pub attempt: i32,
    pub max_attempts: i32,
    pub lease_token: Uuid,
    pub lease_until: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct DueInfraJob {
    id: i64,
    handler_name: String,
    handler_param: Option<String>,
    cron_expression: String,
    retry_count: i32,
    retry_interval: i32,
    monitor_timeout: i32,
    next_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct ReapedJob {
    id: i64,
    task_id: i64,
    state: String,
    last_error: Option<String>,
    previous_result: Option<Value>,
}

#[derive(Debug)]
pub enum ClaimResult {
    Claimed(ClaimedJob),
    Busy,
    Terminal,
    Missing,
    EnvelopeMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    Retry,
    Terminal,
    LeaseLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionDisposition {
    Completed,
    AlreadyCompleted,
    LeaseLost,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReapResult {
    pub retried: u64,
    pub failed: u64,
}

impl JobStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Claims due scheduler rows under `SKIP LOCKED`, writes their task and
    /// outbox records atomically, then advances the schedule. Multiple workers
    /// can run this loop without producing duplicate occurrences.
    pub async fn schedule_due_infra_jobs(&self, limit: i64) -> anyhow::Result<u64> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let jobs: Vec<DueInfraJob> = sqlx::query_as(
            "SELECT id,handler_name,handler_param,cron_expression,retry_count,
                    retry_interval,monitor_timeout,next_run_at
             FROM infra_job
             WHERE deleted=0 AND status=1
               AND (next_run_at IS NULL OR next_run_at <= now())
             ORDER BY next_run_at NULLS FIRST,id
             FOR UPDATE SKIP LOCKED
             LIMIT $1",
        )
        .bind(limit.clamp(1, 1_000))
        .fetch_all(&mut *tx)
        .await?;
        let mut scheduled = 0_u64;
        for job in jobs {
            let scheduled_at = job.next_run_at;
            let after = scheduled_at.map_or(now, |value| value.max(now));
            let next_run_at =
                next_occurrence(&job.cron_expression, after).map_err(anyhow::Error::msg)?;

            // A NULL schedule means this row predates the scheduler migration
            // or was explicitly resynchronised. Initialize it without firing a
            // surprise catch-up execution.
            if let Some(scheduled_at) = scheduled_at {
                let payload = ScheduledJobPayload {
                    infra_job_id: job.id,
                    handler_name: job.handler_name.clone(),
                    handler_param: job.handler_param.clone(),
                    scheduled_at,
                    triggered_by: "cron".to_string(),
                    retry_interval_millis: u64::try_from(job.retry_interval).unwrap_or_default(),
                    monitor_timeout_millis: u64::try_from(job.monitor_timeout).unwrap_or_default(),
                };
                let payload = serde_json::to_value(payload)?;
                let task_id: i64 = sqlx::query_scalar(
                    "INSERT INTO toonflow.tasks
                     (project_id,task_class,related_objects,model,description,state,start_time)
                     VALUES(NULL,'infraJob',$1,'rust-worker',$2,'running',
                            (extract(epoch from clock_timestamp())*1000)::bigint)
                     RETURNING id",
                )
                .bind(serde_json::json!({"infraJobId": job.id}).to_string())
                .bind(format!("执行定时任务 {}", job.handler_name))
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO toonflow.distributed_jobs
                     (message_id,task_id,kind,trace_id,trace_context,payload,max_attempts)
                     VALUES($1,$2,$3,$4,$5,$6,$7)",
                )
                .bind(Uuid::new_v4())
                .bind(task_id)
                .bind(INFRA_SCHEDULED_JOB_KIND)
                .bind(
                    rust_toon_framework_telemetry::current_trace_id()
                        .unwrap_or_else(|| format!("infra-job-{task_id}")),
                )
                .bind(serde_json::json!(
                    rust_toon_framework_telemetry::current_trace_context()
                ))
                .bind(payload)
                .bind(job.retry_count.clamp(0, 99) + 1)
                .execute(&mut *tx)
                .await?;
                scheduled += 1;
            }
            sqlx::query(
                "UPDATE infra_job
                 SET last_scheduled_at=$2,next_run_at=$3,update_time=now()
                 WHERE id=$1",
            )
            .bind(job.id)
            .bind(scheduled_at)
            .bind(next_run_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(scheduled)
    }

    pub async fn prepare_dispatch(
        &self,
        worker_id: &str,
        limit: i64,
        publish_claim: Duration,
        republish_after: Duration,
    ) -> Result<Vec<DispatchJob>, sqlx::Error> {
        sqlx::query_as(
            "WITH candidates AS (
               SELECT id
               FROM toonflow.distributed_jobs
               WHERE state IN ('queued','retry')
                 AND available_at <= now()
                 AND (publish_until IS NULL OR publish_until <= now())
                 AND (
                   published_at IS NULL
                   OR published_at <= now() - make_interval(secs => $4::double precision)
                 )
               ORDER BY priority DESC,available_at,id
               FOR UPDATE SKIP LOCKED
               LIMIT $2
             )
             UPDATE toonflow.distributed_jobs jobs
             SET message_id=CASE
                   WHEN jobs.published_at IS NOT NULL THEN gen_random_uuid()
                   ELSE jobs.message_id
                 END,
                 published_at=NULL,
                 publish_owner=$1,
                 publish_token=gen_random_uuid(),
                 publish_until=now() + make_interval(secs => $3::double precision),
                 updated_at=now()
             FROM candidates
             WHERE jobs.id=candidates.id
             RETURNING jobs.id,jobs.message_id,jobs.task_id,jobs.kind,jobs.trace_id,jobs.trace_context,
                       jobs.payload,jobs.attempt,jobs.publish_token",
        )
        .bind(worker_id)
        .bind(limit.clamp(1, 1_000))
        .bind(duration_seconds(publish_claim, 15))
        .bind(duration_seconds(republish_after, 300))
        .fetch_all(&self.pool)
        .await
    }

    pub async fn mark_published(
        &self,
        id: i64,
        message_id: Uuid,
        publish_token: Uuid,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query(
            "UPDATE toonflow.distributed_jobs
             SET published_at=now(),publish_owner=NULL,publish_token=NULL,
                 publish_until=NULL,updated_at=now()
             WHERE id=$1 AND message_id=$2 AND published_at IS NULL
               AND (
                 (publish_token=$3 AND state IN ('queued','retry'))
                 OR
                 (publish_token IS NULL
                  AND state IN ('running','succeeded','failed','canceled'))
               )",
        )
        .bind(id)
        .bind(message_id)
        .bind(publish_token)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
    }

    pub async fn reject_dispatch(
        &self,
        id: i64,
        message_id: Uuid,
        publish_token: Uuid,
        reason: &str,
    ) -> Result<bool, sqlx::Error> {
        let reason = truncate_error(reason);
        let mut tx = self.pool.begin().await?;
        let task_id: Option<i64> = sqlx::query_scalar(
            "UPDATE toonflow.distributed_jobs
             SET state='failed',last_error=$4,completed_at=now(),updated_at=now(),
                 publish_owner=NULL,publish_token=NULL,publish_until=NULL
             WHERE id=$1 AND message_id=$2 AND publish_token=$3
               AND state IN ('queued','retry')
             RETURNING task_id",
        )
        .bind(id)
        .bind(message_id)
        .bind(publish_token)
        .bind(&reason)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(task_id) = task_id else {
            tx.rollback().await?;
            return Ok(false);
        };
        mark_task_failed(&mut tx, task_id, &reason).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn claim(
        &self,
        job_id: i64,
        message_id: Uuid,
        task_id: i64,
        kind: &str,
        worker_id: &str,
        lease: Duration,
    ) -> Result<ClaimResult, sqlx::Error> {
        let lease_token = Uuid::new_v4();
        let claimed = sqlx::query_as::<_, ClaimedJob>(
            "UPDATE toonflow.distributed_jobs
             SET state='running',attempt=attempt+1,lease_owner=$5,lease_token=$6,
                 lease_until=now() + make_interval(secs => $7::double precision),
                 heartbeat_at=now(),last_error=NULL,
                 publish_owner=NULL,publish_token=NULL,publish_until=NULL,
                 updated_at=now()
             WHERE id=$1
               AND message_id=$2 AND task_id=$3 AND kind=$4
               AND attempt < max_attempts
               AND state IN ('queued','retry') AND available_at <= now()
             RETURNING id,task_id,trace_id,payload,attempt,
                       max_attempts,lease_token,lease_until",
        )
        .bind(job_id)
        .bind(message_id)
        .bind(task_id)
        .bind(kind)
        .bind(worker_id)
        .bind(lease_token)
        .bind(duration_seconds(lease, 300))
        .fetch_optional(&self.pool)
        .await?;
        if let Some(claimed) = claimed {
            return Ok(ClaimResult::Claimed(claimed));
        }

        let row: Option<(String, Uuid, i64, String)> = sqlx::query_as(
            "SELECT state,message_id,task_id,kind
             FROM toonflow.distributed_jobs WHERE id=$1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            None => ClaimResult::Missing,
            Some((_, stored_message_id, stored_task_id, stored_kind))
                if stored_message_id != message_id
                    || stored_task_id != task_id
                    || stored_kind != kind =>
            {
                ClaimResult::EnvelopeMismatch
            }
            Some((state, _, _, _))
                if matches!(state.as_str(), "succeeded" | "failed" | "canceled") =>
            {
                ClaimResult::Terminal
            }
            Some(_) => ClaimResult::Busy,
        })
    }

    pub async fn heartbeat(
        &self,
        job_id: i64,
        lease_token: Uuid,
        lease: Duration,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query(
            "UPDATE toonflow.distributed_jobs
             SET heartbeat_at=now(),
                 lease_until=now() + make_interval(secs => $3::double precision),
                 updated_at=now()
             WHERE id=$1 AND state='running' AND lease_token=$2 AND lease_until > now()",
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(duration_seconds(lease, 300))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
    }

    pub async fn complete(
        &self,
        job_id: i64,
        lease_token: Uuid,
        result: &Value,
    ) -> Result<CompletionDisposition, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let task_id: Option<i64> = sqlx::query_scalar(
            "UPDATE toonflow.distributed_jobs
             SET state='succeeded',result=$3,completed_at=now(),updated_at=now(),
                 lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL
             WHERE id=$1 AND state='running' AND lease_token=$2 AND lease_until > now()
             RETURNING task_id",
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(result)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(task_id) = task_id else {
            let state: Option<String> =
                sqlx::query_scalar("SELECT state FROM toonflow.distributed_jobs WHERE id=$1")
                    .bind(job_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            tx.rollback().await?;
            return Ok(if state.as_deref() == Some("succeeded") {
                CompletionDisposition::AlreadyCompleted
            } else {
                CompletionDisposition::LeaseLost
            });
        };
        sqlx::query(
            "UPDATE toonflow.tasks
             SET state='success',related_objects=$2,reason=NULL
             WHERE id=$1 AND state='running'",
        )
        .bind(task_id)
        .bind(result.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CompletionDisposition::Completed)
    }

    pub async fn fail(
        &self,
        job_id: i64,
        lease_token: Uuid,
        reason: &str,
        retry_after: Duration,
    ) -> Result<FailureDisposition, sqlx::Error> {
        let reason = truncate_error(reason);
        let mut tx = self.pool.begin().await?;
        let updated: Option<(i64, String, Option<Value>)> = sqlx::query_as(
            "WITH current AS (
               SELECT id,result
               FROM toonflow.distributed_jobs
               WHERE id=$1 AND state='running' AND lease_token=$2 AND lease_until > now()
               FOR UPDATE
             )
             UPDATE toonflow.distributed_jobs jobs
             SET state=CASE WHEN attempt >= max_attempts THEN 'failed' ELSE 'retry' END,
                 available_at=CASE
                   WHEN attempt >= max_attempts THEN available_at
                   ELSE now() + make_interval(secs => $4::double precision)
                 END,
                 message_id=CASE
                   WHEN attempt >= max_attempts THEN message_id ELSE gen_random_uuid()
                 END,
                 published_at=CASE
                   WHEN attempt >= max_attempts THEN published_at ELSE NULL
                 END,
                 last_error=$3,
                 completed_at=CASE WHEN attempt >= max_attempts THEN now() ELSE NULL END,
                 lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL,
                 publish_owner=NULL,publish_token=NULL,publish_until=NULL,result=NULL,
                 updated_at=now()
             FROM current
             WHERE jobs.id=current.id
             RETURNING jobs.task_id,jobs.state,current.result",
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(&reason)
        .bind(duration_seconds(retry_after, 30))
        .fetch_optional(&mut *tx)
        .await?;
        let Some((task_id, state, previous_result)) = updated else {
            tx.rollback().await?;
            return Ok(FailureDisposition::LeaseLost);
        };
        enqueue_staging_cleanup(&mut tx, job_id, previous_result.as_ref(), &reason).await?;
        if state == "failed" {
            mark_task_failed(&mut tx, task_id, &reason).await?;
        } else {
            sqlx::query("UPDATE toonflow.tasks SET reason=$2 WHERE id=$1 AND state='running'")
                .bind(task_id)
                .bind(format!("任务将自动重试：{reason}"))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(if state == "failed" {
            FailureDisposition::Terminal
        } else {
            FailureDisposition::Retry
        })
    }

    pub async fn requeue_for_shutdown(
        &self,
        job_id: i64,
        lease_token: Uuid,
        reason: &str,
    ) -> Result<FailureDisposition, sqlx::Error> {
        let reason = truncate_error(reason);
        let mut tx = self.pool.begin().await?;
        let updated: Option<(i64, Option<Value>)> = sqlx::query_as(
            "WITH current AS (
               SELECT id,result
               FROM toonflow.distributed_jobs
               WHERE id=$1 AND state='running' AND lease_token=$2
               FOR UPDATE
             )
             UPDATE toonflow.distributed_jobs jobs
             SET state='retry',attempt=GREATEST(attempt-1,0),available_at=now(),
                 message_id=gen_random_uuid(),published_at=NULL,last_error=$3,
                 completed_at=NULL,
                 lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL,
                 publish_owner=NULL,publish_token=NULL,publish_until=NULL,result=NULL,
                 updated_at=now()
             FROM current
             WHERE jobs.id=current.id
             RETURNING jobs.task_id,current.result",
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(&reason)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((task_id, previous_result)) = updated else {
            tx.rollback().await?;
            return Ok(FailureDisposition::LeaseLost);
        };
        enqueue_staging_cleanup(&mut tx, job_id, previous_result.as_ref(), &reason).await?;
        sqlx::query("UPDATE toonflow.tasks SET reason=$2 WHERE id=$1 AND state='running'")
            .bind(task_id)
            .bind("Worker 正在维护，任务已无损转交其他实例")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(FailureDisposition::Retry)
    }

    pub async fn reap_expired(&self, limit: i64) -> Result<ReapResult, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let updated: Vec<ReapedJob> = sqlx::query_as(
            "WITH expired AS (
               SELECT id,result FROM toonflow.distributed_jobs
               WHERE state='running' AND lease_until <= now()
               ORDER BY lease_until,id
               FOR UPDATE SKIP LOCKED
               LIMIT $1
             )
             UPDATE toonflow.distributed_jobs jobs
             SET state=CASE WHEN jobs.attempt >= jobs.max_attempts THEN 'failed' ELSE 'retry' END,
                 available_at=now(),
                 message_id=CASE
                   WHEN jobs.attempt >= jobs.max_attempts THEN jobs.message_id
                   ELSE gen_random_uuid()
                 END,
                 published_at=CASE
                   WHEN jobs.attempt >= jobs.max_attempts THEN jobs.published_at ELSE NULL
                 END,
                 last_error=coalesce(jobs.last_error,'Worker 租约过期，任务将由其他实例接管'),
                 completed_at=CASE WHEN jobs.attempt >= jobs.max_attempts THEN now() ELSE NULL END,
                 lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL,
                 publish_owner=NULL,publish_token=NULL,publish_until=NULL,result=NULL,
                 updated_at=now()
             FROM expired
             WHERE jobs.id=expired.id
             RETURNING jobs.id,jobs.task_id,jobs.state,jobs.last_error,
                       expired.result AS previous_result",
        )
        .bind(limit.clamp(1, 1_000))
        .fetch_all(&mut *tx)
        .await?;
        let mut result = ReapResult::default();
        for reaped in updated {
            enqueue_staging_cleanup(
                &mut tx,
                reaped.id,
                reaped.previous_result.as_ref(),
                reaped.last_error.as_deref().unwrap_or("Worker 租约过期"),
            )
            .await?;
            if reaped.state == "failed" {
                result.failed += 1;
                mark_task_failed(
                    &mut tx,
                    reaped.task_id,
                    reaped.last_error.as_deref().unwrap_or("Worker 租约过期"),
                )
                .await?;
            } else {
                result.retried += 1;
            }
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn register_worker(
        &self,
        instance_id: &str,
        concurrency: i32,
        metadata: &Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO toonflow.worker_instances
             (instance_id,service,status,concurrency,metadata)
             VALUES($1,'toon-worker','ready',$2,$3)
             ON CONFLICT(instance_id) DO UPDATE SET
               status='ready',concurrency=excluded.concurrency,
               started_at=now(),heartbeat_at=now(),metadata=excluded.metadata",
        )
        .bind(instance_id)
        .bind(concurrency)
        .bind(metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn heartbeat_worker(&self, instance_id: &str) -> Result<bool, sqlx::Error> {
        sqlx::query(
            "UPDATE toonflow.worker_instances SET heartbeat_at=now()
             WHERE instance_id=$1 AND status='ready'",
        )
        .bind(instance_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
    }

    pub async fn set_worker_status(
        &self,
        instance_id: &str,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE toonflow.worker_instances
             SET status=$2,heartbeat_at=now() WHERE instance_id=$1",
        )
        .bind(instance_id)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn mark_task_failed(
    tx: &mut Transaction<'_, Postgres>,
    task_id: i64,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE toonflow.videos SET state='生成失败',error_reason=$2,
         generation_context=jsonb_set(generation_context,'{quality,state}','\"error\"'::jsonb)
         WHERE state='生成中' AND generation_context->'quality'->>'taskId'=$1::text",
    )
    .bind(task_id.to_string())
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE toonflow.tasks SET state='failed',reason=$2 WHERE id=$1 AND state='running'",
    )
    .bind(task_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn enqueue_staging_cleanup(
    tx: &mut Transaction<'_, Postgres>,
    job_id: i64,
    result: Option<&Value>,
    reason: &str,
) -> Result<(), sqlx::Error> {
    let Some(object_path) = result
        .and_then(|value| value.get("stagingObjectPath"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO toonflow.storage_cleanup_tasks
         (object_path,resource_type,resource_id,error_reason,attempts,state,
          create_time,update_time,next_attempt_at)
         VALUES(
           $1,'distributed_job_staging',$2,$3,1,'pending',
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
           now() + make_interval(secs => $4::double precision)
         )",
    )
    .bind(object_path)
    .bind(job_id)
    .bind(truncate_error(reason))
    .bind(staging_cleanup_delay_seconds())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn staging_cleanup_delay_seconds() -> i64 {
    let configured = std::env::var("TOON_WORKER_STAGING_CLEANUP_DELAY_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (60..=86_400).contains(value))
        .unwrap_or(1_800);
    let stream_timeout = std::env::var("S3_STREAM_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (30..=86_400).contains(value))
        .unwrap_or(1_800);
    i64::try_from(configured.max(stream_timeout)).unwrap_or(86_400)
}

fn duration_seconds(duration: Duration, fallback: u64) -> i64 {
    let seconds = if duration.is_zero() {
        fallback
    } else {
        duration.as_secs().max(1)
    };
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn truncate_error(reason: &str) -> String {
    reason.chars().take(4_000).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{duration_seconds, truncate_error};

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL"]
    async fn terminal_quality_failure_finalizes_video_without_reviving_cancellation() {
        use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
        let pool = connect(
            &DatabaseConfig::new(
                std::env::var("TEST_DATABASE_URL").unwrap(),
                1,
                3,
                Duration::from_secs(10),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        migrate(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let project_id = chrono::Utc::now().timestamp_micros();
        sqlx::query("INSERT INTO toonflow.projects(id,name,create_time,update_time) VALUES($1,'quality failure test',0,0)")
            .bind(project_id).execute(&mut *tx).await.unwrap();
        let task_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.tasks(project_id,task_class,state) VALUES($1,'videoQuality','running') RETURNING id")
            .bind(project_id).fetch_one(&mut *tx).await.unwrap();
        let video_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.videos(project_id,state,generation_context) VALUES($1,'生成中',$2) RETURNING id")
            .bind(project_id).bind(serde_json::json!({"quality":{"state":"pending","taskId":task_id}})).fetch_one(&mut *tx).await.unwrap();
        super::mark_task_failed(&mut tx, task_id, "检查执行失败")
            .await
            .unwrap();
        let result: (String, String) = sqlx::query_as(
            "SELECT state,generation_context->'quality'->>'state' FROM toonflow.videos WHERE id=$1",
        )
        .bind(video_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(result, ("生成失败".into(), "error".into()));
        sqlx::query("UPDATE toonflow.videos SET state='已取消' WHERE id=$1")
            .bind(video_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        super::mark_task_failed(&mut tx, task_id, "late worker error")
            .await
            .unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(state, "已取消");
        tx.rollback().await.unwrap();
    }

    #[test]
    fn errors_are_bounded_on_character_boundaries() {
        let reason = "故".repeat(5_000);
        let truncated = truncate_error(&reason);
        assert_eq!(truncated.chars().count(), 4_000);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn sql_interval_seconds_are_positive_and_bounded() {
        assert_eq!(duration_seconds(Duration::ZERO, 30), 30);
        assert_eq!(duration_seconds(Duration::from_millis(1), 30), 1);
        assert_eq!(duration_seconds(Duration::from_secs(42), 30), 42);
    }
}
