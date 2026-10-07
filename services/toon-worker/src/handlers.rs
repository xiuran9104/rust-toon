use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use rust_toon_framework_jobs::{INFRA_SCHEDULED_JOB_KIND, ScheduledJobPayload};
use serde_json::{Value, json};

use crate::job_store::{ClaimedJob, JobStore};

#[async_trait]
pub trait JobHandler: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn execute(&self, store: &JobStore, job: &ClaimedJob) -> Result<Value, String>;
}

#[derive(Clone, Default)]
pub struct HandlerRegistry {
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
}

impl HandlerRegistry {
    pub fn register<H: JobHandler + 'static>(&mut self, handler: H) -> Result<(), String> {
        let kind = handler.kind();
        if self.handlers.insert(kind, Arc::new(handler)).is_some() {
            return Err(format!("duplicate distributed job handler: {kind}"));
        }
        Ok(())
    }

    pub fn handlers(&self) -> impl Iterator<Item = Arc<dyn JobHandler>> + '_ {
        self.handlers.values().cloned()
    }

    #[cfg(test)]
    pub fn contains(&self, kind: &str) -> bool {
        self.handlers.contains_key(kind)
    }
}

pub struct VideoExportHandler;

pub struct VideoQualityHandler;

#[async_trait]
impl JobHandler for VideoQualityHandler {
    fn kind(&self) -> &'static str {
        rust_toon_toon_server::VIDEO_QUALITY_JOB_KIND
    }

    async fn execute(&self, store: &JobStore, job: &ClaimedJob) -> Result<Value, String> {
        rust_toon_toon_server::execute_distributed_quality(
            store.pool(),
            job.id,
            job.task_id,
            job.lease_token,
            job.payload.clone(),
        )
        .await
    }
}

#[async_trait]
impl JobHandler for VideoExportHandler {
    fn kind(&self) -> &'static str {
        rust_toon_toon_server::VIDEO_EXPORT_JOB_KIND
    }

    async fn execute(&self, store: &JobStore, job: &ClaimedJob) -> Result<Value, String> {
        rust_toon_toon_server::execute_distributed_export(
            store.pool(),
            job.id,
            job.task_id,
            job.lease_token,
            job.payload.clone(),
        )
        .await
    }
}

pub struct ScheduledInfraHandler;

#[async_trait]
impl JobHandler for ScheduledInfraHandler {
    fn kind(&self) -> &'static str {
        INFRA_SCHEDULED_JOB_KIND
    }

    async fn execute(&self, store: &JobStore, job: &ClaimedJob) -> Result<Value, String> {
        let payload: ScheduledJobPayload = serde_json::from_value(job.payload.clone())
            .map_err(|error| format!("定时任务参数无效：{error}"))?;
        let started_at = Utc::now();
        let log_id = start_job_log(store, job, &payload, started_at).await?;
        let execution = execute_named_handler(store, &payload);
        let result = if payload.monitor_timeout_millis == 0 {
            execution.await
        } else {
            let timeout =
                Duration::from_millis(payload.monitor_timeout_millis.clamp(1, 86_400_000));
            match tokio::time::timeout(timeout, execution).await {
                Ok(result) => result,
                Err(_) => Err(format!("定时任务执行超时（{} 毫秒）", timeout.as_millis())),
            }
        };
        finish_job_log(store, log_id, started_at, &result).await?;
        result
    }
}

async fn start_job_log(
    store: &JobStore,
    job: &ClaimedJob,
    payload: &ScheduledJobPayload,
    started_at: chrono::DateTime<Utc>,
) -> Result<i64, String> {
    if let Some(log_id) = sqlx::query_scalar::<_, i64>(
        "UPDATE infra_job_log
         SET execute_index=$2,begin_time=$3,end_time=NULL,duration=0,status=0,result=NULL,
             update_time=now()
         WHERE distributed_job_id=$1
         RETURNING id",
    )
    .bind(job.id)
    .bind(job.attempt)
    .bind(started_at.naive_utc())
    .fetch_optional(store.pool())
    .await
    .map_err(|error| format!("无法更新定时任务日志：{error}"))?
    {
        return Ok(log_id);
    }

    sqlx::query_scalar(
        "INSERT INTO infra_job_log
         (id,job_id,handler_name,handler_param,execute_index,begin_time,status,distributed_job_id)
         VALUES(nextval('infra_job_log_seq'),$1,$2,$3,$4,$5,0,$6)
         RETURNING id",
    )
    .bind(payload.infra_job_id)
    .bind(&payload.handler_name)
    .bind(&payload.handler_param)
    .bind(job.attempt)
    .bind(started_at.naive_utc())
    .bind(job.id)
    .fetch_one(store.pool())
    .await
    .map_err(|error| format!("无法创建定时任务日志：{error}"))
}

async fn finish_job_log(
    store: &JobStore,
    log_id: i64,
    started_at: chrono::DateTime<Utc>,
    result: &Result<Value, String>,
) -> Result<(), String> {
    let ended_at = Utc::now();
    let duration_ms = (ended_at - started_at)
        .num_milliseconds()
        .clamp(0, i64::from(i32::MAX));
    let (status, text) = match result {
        Ok(value) => (1_i16, value.to_string()),
        Err(reason) => (2_i16, reason.clone()),
    };
    sqlx::query(
        "UPDATE infra_job_log
         SET end_time=$2,duration=$3,status=$4,result=$5,update_time=now()
         WHERE id=$1",
    )
    .bind(log_id)
    .bind(ended_at.naive_utc())
    .bind(i32::try_from(duration_ms).unwrap_or(i32::MAX))
    .bind(status)
    .bind(text)
    .execute(store.pool())
    .await
    .map_err(|error| format!("无法完成定时任务日志：{error}"))?;
    Ok(())
}

async fn execute_named_handler(
    store: &JobStore,
    payload: &ScheduledJobPayload,
) -> Result<Value, String> {
    match payload.handler_name.as_str() {
        "infra.noop" => Ok(payload
            .handler_param
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| json!({"ok": true}))),
        "infra.database.ping" => {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(store.pool())
                .await
                .map_err(|error| format!("数据库健康检查失败：{error}"))?;
            Ok(json!({"ok": true, "database": "postgresql"}))
        }
        name => Err(format!(
            "未注册定时任务处理器：{name}；可用处理器：infra.noop, infra.database.ping"
        )),
    }
}

pub struct TestNoopHandler;

#[async_trait]
impl JobHandler for TestNoopHandler {
    fn kind(&self) -> &'static str {
        crate::TEST_JOB_KIND
    }

    async fn execute(&self, _store: &JobStore, job: &ClaimedJob) -> Result<Value, String> {
        let sleep_ms = job
            .payload
            .get("sleepMs")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .min(300_000);
        if sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
        Ok(job
            .payload
            .get("result")
            .cloned()
            .unwrap_or_else(|| json!({"ok": true})))
    }
}

#[cfg(test)]
mod tests {
    use super::{HandlerRegistry, ScheduledInfraHandler, VideoExportHandler, VideoQualityHandler};
    use rust_toon_framework_jobs::INFRA_SCHEDULED_JOB_KIND;

    #[test]
    fn registry_exposes_each_job_kind_once() {
        let mut registry = HandlerRegistry::default();
        registry
            .register(VideoQualityHandler)
            .expect("register video quality");
        assert!(registry.contains(rust_toon_toon_server::VIDEO_QUALITY_JOB_KIND));
        registry
            .register(VideoExportHandler)
            .expect("register video");
        registry
            .register(ScheduledInfraHandler)
            .expect("register scheduler");
        assert!(registry.contains(rust_toon_toon_server::VIDEO_EXPORT_JOB_KIND));
        assert!(registry.contains(INFRA_SCHEDULED_JOB_KIND));
        assert!(registry.register(ScheduledInfraHandler).is_err());
    }
}
