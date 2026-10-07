mod handlers;
mod job_store;

use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use async_nats::jetstream::{AckKind, Message};
use axum::{
    Json, Router, extract::State, http::StatusCode, middleware::from_fn_with_state,
    response::IntoResponse, routing::get,
};
use futures_util::StreamExt;
use handlers::{
    HandlerRegistry, JobHandler, ScheduledInfraHandler, TestNoopHandler, VideoExportHandler,
    VideoQualityHandler,
};
use job_store::{ClaimResult, CompletionDisposition, FailureDisposition, JobStore};
use rust_toon_framework_common::ServiceConfig;
use rust_toon_framework_database::{DatabaseConfig, connect, ping};
use rust_toon_framework_dynamic_config::{NacosConfig, WorkerRuntimeConfig, subscribe_json};
use rust_toon_framework_mq::{Broker, JobEnvelope, NatsConfig};
use rust_toon_framework_telemetry::{
    Metrics, TraceContext, init_telemetry, record_http_metrics, set_parent_from_trace_context,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::watch, task::JoinSet};
use tracing::{Instrument, error, info, info_span, warn};
use uuid::Uuid;

const SERVICE_NAME: &str = "toon-worker";
const TEST_JOB_KIND: &str = "test.noop";

#[derive(Debug, Clone)]
struct WorkerSettings {
    instance_id: String,
    concurrency: usize,
    lease: Duration,
    heartbeat: Duration,
    dispatch_interval: Duration,
    publish_claim: Duration,
    republish_after: Duration,
    reaper_interval: Duration,
    cleanup_interval: Duration,
    cleanup_timeout: Duration,
    scheduler_interval: Duration,
    drain_timeout: Duration,
    test_jobs: bool,
}

impl WorkerSettings {
    fn from_env() -> anyhow::Result<Self> {
        let concurrency = env_number("TOON_WORKER_CONCURRENCY", 4, 1, 256)? as usize;
        let lease = Duration::from_secs(env_number("TOON_WORKER_LEASE_SECONDS", 300, 2, 86_400)?);
        let heartbeat =
            Duration::from_secs(env_number("TOON_WORKER_HEARTBEAT_SECONDS", 30, 1, 3_600)?);
        anyhow::ensure!(
            heartbeat < lease,
            "TOON_WORKER_HEARTBEAT_SECONDS must be shorter than TOON_WORKER_LEASE_SECONDS"
        );
        let dispatch_interval = Duration::from_millis(env_number(
            "TOON_WORKER_DISPATCH_INTERVAL_MS",
            500,
            10,
            60_000,
        )?);
        let publish_claim = Duration::from_secs(env_number(
            "TOON_WORKER_PUBLISH_CLAIM_SECONDS",
            15,
            1,
            3_600,
        )?);
        let republish_after = Duration::from_secs(env_number(
            "TOON_WORKER_REPUBLISH_AFTER_SECONDS",
            300,
            1,
            86_400,
        )?);
        let reaper_interval = Duration::from_secs(env_number(
            "TOON_WORKER_REAPER_INTERVAL_SECONDS",
            15,
            1,
            3_600,
        )?);
        let cleanup_interval = Duration::from_secs(env_number(
            "TOON_WORKER_CLEANUP_INTERVAL_SECONDS",
            5,
            1,
            3_600,
        )?);
        let cleanup_timeout = Duration::from_secs(env_number(
            "TOON_WORKER_CLEANUP_TIMEOUT_SECONDS",
            30,
            1,
            3_600,
        )?);
        let scheduler_interval = Duration::from_millis(env_number(
            "TOON_WORKER_SCHEDULER_INTERVAL_MS",
            1_000,
            100,
            60_000,
        )?);
        anyhow::ensure!(
            cleanup_timeout < lease,
            "TOON_WORKER_CLEANUP_TIMEOUT_SECONDS must be shorter than TOON_WORKER_LEASE_SECONDS"
        );
        let drain_timeout = Duration::from_secs(env_number(
            "TOON_WORKER_DRAIN_TIMEOUT_SECONDS",
            45,
            1,
            3_600,
        )?);
        let instance_id = env::var("TOON_WORKER_INSTANCE_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_instance_id);
        let test_jobs = env_bool("TOON_WORKER_ENABLE_TEST_JOBS", false)?;
        Ok(Self {
            instance_id,
            concurrency,
            lease,
            heartbeat,
            dispatch_interval,
            publish_claim,
            republish_after,
            reaper_interval,
            cleanup_interval,
            cleanup_timeout,
            scheduler_interval,
            drain_timeout,
            test_jobs,
        })
    }
}

#[derive(Clone)]
struct HealthState {
    accepting: Arc<AtomicBool>,
    store: JobStore,
    broker: Broker,
    ffmpeg: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthBody {
    service: &'static str,
    status: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let telemetry = init_telemetry(SERVICE_NAME)?;
    let metrics = telemetry.metrics();
    let settings = WorkerSettings::from_env()?;
    let worker_dynamic = subscribe_json(
        NacosConfig::from_env(SERVICE_NAME)?,
        WorkerRuntimeConfig::new(
            settings.dispatch_interval,
            settings.scheduler_interval,
            settings.reaper_interval,
            settings.cleanup_interval,
        ),
        WorkerRuntimeConfig::validate,
    )
    .await?;
    let runtime_config = worker_dynamic.receiver();
    let pool = connect(&DatabaseConfig::from_env()?).await?;
    // The gateway/deployment migration job owns schema initialization. A
    // worker deliberately fails fast when the durable job schema is absent.
    ping(&pool).await?;
    let store = JobStore::new(pool);
    let mut handlers = HandlerRegistry::default();
    handlers
        .register(VideoQualityHandler)
        .map_err(anyhow::Error::msg)?;
    handlers
        .register(VideoExportHandler)
        .map_err(anyhow::Error::msg)?;
    handlers
        .register(ScheduledInfraHandler)
        .map_err(anyhow::Error::msg)?;
    if settings.test_jobs {
        handlers
            .register(TestNoopHandler)
            .map_err(anyhow::Error::msg)?;
    }

    let mut nats_config = NatsConfig::from_env()?;
    if env::var_os("NATS_CLIENT_NAME").is_none() {
        nats_config.client_name = format!("toon-worker-{}", settings.instance_id);
    }
    anyhow::ensure!(
        settings.heartbeat < nats_config.consumer_ack_wait,
        "TOON_WORKER_HEARTBEAT_SECONDS must be shorter than NATS_JOB_ACK_WAIT_SECONDS"
    );
    let broker = Broker::connect(nats_config).await?;
    rust_toon_toon_server::initialize_object_storage()
        .await
        .map_err(anyhow::Error::msg)
        .context("initialize object storage")?;
    match rust_toon_toon_server::cleanup_stale_export_workdirs().await {
        Ok(removed) if removed > 0 => info!(removed, "stale export work directories removed"),
        Ok(_) => {}
        Err(error) => warn!(%error, "failed to clean stale export work directories"),
    }
    let has_ffmpeg = ffmpeg_available().await;
    anyhow::ensure!(
        has_ffmpeg || settings.test_jobs,
        "FFmpeg and FFprobe are required by the Toon media worker"
    );

    store
        .register_worker(
            &settings.instance_id,
            settings.concurrency as i32,
            &json!({
                "pid": std::process::id(),
                "ffmpeg": has_ffmpeg,
                "testJobs": settings.test_jobs,
            }),
        )
        .await
        .context("register worker instance")?;

    let accepting = Arc::new(AtomicBool::new(true));
    let health = HealthState {
        accepting: accepting.clone(),
        store: store.clone(),
        broker: broker.clone(),
        ffmpeg: has_ffmpeg || settings.test_jobs,
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    // `TOON_WORKER_CONCURRENCY` is an instance-wide capacity limit. All job
    // kinds share it so adding a handler cannot silently multiply FFmpeg/CPU
    // load for the process.
    let execution_permits = Arc::new(tokio::sync::Semaphore::new(settings.concurrency));

    tasks.spawn(run_dispatcher(
        store.clone(),
        broker.clone(),
        settings.clone(),
        metrics.clone(),
        runtime_config.clone(),
        shutdown_rx.clone(),
    ));
    tasks.spawn(run_maintenance(
        store.clone(),
        settings.clone(),
        metrics.clone(),
        runtime_config.clone(),
        shutdown_rx.clone(),
    ));
    tasks.spawn(run_storage_cleanup(
        store.clone(),
        settings.clone(),
        metrics.clone(),
        runtime_config.clone(),
        shutdown_rx.clone(),
    ));
    tasks.spawn(run_scheduler(
        store.clone(),
        runtime_config,
        shutdown_rx.clone(),
    ));
    for handler in handlers.handlers() {
        tasks.spawn(run_consumer(
            store.clone(),
            broker.clone(),
            settings.clone(),
            handler,
            execution_permits.clone(),
            metrics.clone(),
            shutdown_rx.clone(),
        ));
    }
    tasks.spawn(serve_health(
        ServiceConfig::from_env(SERVICE_NAME, 8081),
        health,
        metrics,
        shutdown_rx,
    ));

    info!(
        instance_id = %settings.instance_id,
        concurrency = settings.concurrency,
        "distributed Toon worker is ready"
    );

    let mut fatal_error = None;
    tokio::select! {
        _ = shutdown_signal() => {
            info!(instance_id = %settings.instance_id, "worker shutdown requested");
        }
        outcome = tasks.join_next() => {
            fatal_error = Some(match outcome {
                Some(Ok(Ok(()))) => anyhow::anyhow!("worker subsystem stopped unexpectedly"),
                Some(Ok(Err(error))) => error,
                Some(Err(error)) => anyhow::Error::from(error),
                None => anyhow::anyhow!("worker has no running subsystems"),
            });
        }
    }

    accepting.store(false, Ordering::Release);
    if let Err(error) = store
        .set_worker_status(&settings.instance_id, "draining")
        .await
    {
        warn!(%error, "failed to persist worker draining state");
    }
    let _ = shutdown_tx.send(true);
    let drain_permits = execution_permits.clone();
    let permit_count = u32::try_from(settings.concurrency).unwrap_or(u32::MAX);
    let drain = async {
        // Detached delivery tasks each own one permit. Taking the entire
        // process-wide semaphore waits for every handler kind to finish.
        let _all_permits = drain_permits.acquire_many_owned(permit_count).await;
        while let Some(outcome) = tasks.join_next().await {
            if let Err(error) = outcome {
                warn!(%error, "worker subsystem did not shut down cleanly");
            } else if let Ok(Err(error)) = outcome {
                warn!(%error, "worker subsystem returned an error while draining");
            }
        }
    };
    if tokio::time::timeout(settings.drain_timeout, drain)
        .await
        .is_err()
    {
        warn!(
            timeout_seconds = settings.drain_timeout.as_secs(),
            "worker drain deadline elapsed; aborting remaining subsystems"
        );
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    if let Err(error) = store
        .set_worker_status(&settings.instance_id, "stopped")
        .await
    {
        warn!(%error, "failed to persist worker stopped state");
    }
    if let Some(error) = fatal_error {
        return Err(error);
    }
    Ok(())
}

async fn run_dispatcher(
    store: JobStore,
    broker: Broker,
    settings: WorkerSettings,
    metrics: Metrics,
    mut runtime: watch::Receiver<WorkerRuntimeConfig>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        match store
            .prepare_dispatch(
                &settings.instance_id,
                100,
                settings.publish_claim,
                settings.republish_after,
            )
            .await
        {
            Ok(jobs) => {
                for job in jobs {
                    let metric_kind = job.kind.clone();
                    let trace_context = serde_json::from_value::<TraceContext>(job.trace_context)
                        .unwrap_or_default();
                    let envelope = match JobEnvelope::new(
                        job.message_id,
                        job.id,
                        job.task_id,
                        job.kind,
                        u32::try_from(job.attempt).unwrap_or_default(),
                        job.trace_id,
                        job.payload,
                    )
                    .and_then(|envelope| envelope.with_trace_context(trace_context))
                    {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            let reason = format!("任务信封无法编码：{error}");
                            match store
                                .reject_dispatch(job.id, job.message_id, job.publish_token, &reason)
                                .await
                            {
                                Ok(true) => {
                                    metrics.record_worker_dispatch(&metric_kind, "invalid");
                                    error!(job_id = job.id, %error, "invalid outbox row failed without stopping dispatcher")
                                }
                                Ok(false) => {
                                    metrics.record_worker_dispatch(&metric_kind, "claim_lost");
                                    warn!(job_id = job.id, %error, "invalid outbox row lost its publish claim")
                                }
                                Err(store_error) => {
                                    metrics.record_worker_dispatch(&metric_kind, "store_error");
                                    warn!(job_id = job.id, %error, %store_error, "failed to quarantine invalid outbox row")
                                }
                            }
                            continue;
                        }
                    };
                    match broker.publish(&envelope).await {
                        Ok(receipt) => {
                            if store
                                .mark_published(job.id, job.message_id, job.publish_token)
                                .await?
                            {
                                metrics.record_worker_dispatch(
                                    &metric_kind,
                                    if receipt.duplicate {
                                        "duplicate"
                                    } else {
                                        "published"
                                    },
                                );
                                info!(
                                    job_id = job.id,
                                    task_id = job.task_id,
                                    sequence = receipt.sequence,
                                    duplicate = receipt.duplicate,
                                    "durable job dispatched"
                                );
                            } else {
                                metrics.record_worker_dispatch(&metric_kind, "claim_lost");
                            }
                        }
                        Err(error) => {
                            metrics.record_worker_dispatch(&metric_kind, "deferred");
                            warn!(job_id = job.id, %error, "durable job publish deferred");
                        }
                    }
                }
            }
            Err(error) => {
                metrics.record_worker_dispatch("all", "query_error");
                warn!(%error, "outbox dispatch query failed");
            }
        }
        let interval = runtime.borrow().dispatcher_interval();
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            changed = runtime.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn run_maintenance(
    store: JobStore,
    settings: WorkerSettings,
    metrics: Metrics,
    mut runtime: watch::Receiver<WorkerRuntimeConfig>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let heartbeat_interval = settings.heartbeat.min(Duration::from_secs(10));
    let mut heartbeat_tick = tokio::time::interval(heartbeat_interval);
    let mut next_reaper = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = heartbeat_tick.tick() => {
                if !store.heartbeat_worker(&settings.instance_id).await? {
                    anyhow::bail!("worker registration disappeared or is no longer ready");
                }
            }
            _ = tokio::time::sleep_until(next_reaper) => {
                let result = store.reap_expired(100).await?;
                metrics.record_worker_lease_reaps(result.retried, result.failed);
                if result.retried > 0 || result.failed > 0 {
                    warn!(retried = result.retried, failed = result.failed, "expired worker leases reaped");
                }
                next_reaper = tokio::time::Instant::now() + runtime.borrow().reaper_interval();
            }
            changed = runtime.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                next_reaper = tokio::time::Instant::now() + runtime.borrow().reaper_interval();
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn run_storage_cleanup(
    store: JobStore,
    settings: WorkerSettings,
    metrics: Metrics,
    mut runtime: watch::Receiver<WorkerRuntimeConfig>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut next_cleanup = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_cleanup) => {
                match rust_toon_toon_server::process_storage_cleanup_batch(
                    store.pool(),
                    &settings.instance_id,
                    8,
                    settings.lease,
                    settings.cleanup_timeout,
                ).await {
                    Ok(cleanup) if cleanup.completed > 0 || cleanup.deferred > 0 || cleanup.failed > 0 => {
                        metrics.record_worker_storage_cleanup(
                            cleanup.completed,
                            cleanup.deferred,
                            cleanup.failed,
                        );
                        info!(
                            completed = cleanup.completed,
                            deferred = cleanup.deferred,
                            failed = cleanup.failed,
                            "storage cleanup batch processed"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        metrics.record_worker_storage_cleanup(0, 0, 1);
                        warn!(%error, "storage cleanup batch failed");
                    }
                }
                next_cleanup = tokio::time::Instant::now() + runtime.borrow().cleanup_interval();
            }
            changed = runtime.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                next_cleanup = tokio::time::Instant::now() + runtime.borrow().cleanup_interval();
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn run_scheduler(
    store: JobStore,
    mut runtime: watch::Receiver<WorkerRuntimeConfig>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut next_scan = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_scan) => {
                match store.schedule_due_infra_jobs(100).await {
                    Ok(count) if count > 0 => info!(count, "scheduled durable infra jobs"),
                    Ok(_) => {}
                    Err(error) => warn!(%error, "distributed scheduler scan failed"),
                }
                next_scan = tokio::time::Instant::now() + runtime.borrow().scheduler_interval();
            }
            changed = runtime.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                next_scan = tokio::time::Instant::now() + runtime.borrow().scheduler_interval();
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn run_consumer(
    store: JobStore,
    broker: Broker,
    settings: WorkerSettings,
    handler: Arc<dyn JobHandler>,
    semaphore: Arc<tokio::sync::Semaphore>,
    metrics: Metrics,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let kind = handler.kind();
    let durable_name = durable_name(kind);
    loop {
        if *shutdown.borrow() {
            break;
        }
        let consumer = match broker.durable_consumer(&durable_name, kind).await {
            Ok(consumer) => consumer,
            Err(error) => {
                warn!(%error, kind, "failed to attach durable consumer");
                if wait_or_shutdown(Duration::from_secs(2), &mut shutdown).await {
                    break;
                }
                continue;
            }
        };
        let mut messages = match consumer.messages().await {
            Ok(messages) => messages,
            Err(error) => {
                warn!(%error, kind, "failed to open durable pull stream");
                if wait_or_shutdown(Duration::from_secs(2), &mut shutdown).await {
                    break;
                }
                continue;
            }
        };
        loop {
            let next = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        None
                    } else {
                        continue;
                    }
                }
                next = messages.next() => next,
            };
            let Some(next) = next else {
                break;
            };
            let message = match next {
                Ok(message) => message,
                Err(error) => {
                    warn!(%error, kind, "durable consumer stream error");
                    break;
                }
            };
            let permit = tokio::select! {
                permit = semaphore.clone().acquire_owned() => permit?,
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                    continue;
                }
            };
            let task_store = store.clone();
            let task_settings = settings.clone();
            let task_shutdown = shutdown.clone();
            let task_metrics = metrics.clone();
            let task_handler = handler.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let span = info_span!(
                    "worker_job_delivery",
                    "messaging.system" = "nats",
                    "messaging.destination.name" = kind,
                    "messaging.message.id" = tracing::field::Empty,
                    "job.id" = tracing::field::Empty,
                    "job.task_id" = tracing::field::Empty,
                    "job.trace_id" = tracing::field::Empty,
                    trace_id = tracing::field::Empty,
                    "otel.kind" = "consumer",
                );
                if let Some(headers) = &message.message.headers {
                    let mut carrier = TraceContext::new();
                    for key in ["traceparent", "tracestate"] {
                        if let Some(value) = headers.get(key) {
                            carrier.insert(key.to_string(), value.as_str().to_string());
                        }
                    }
                    set_parent_from_trace_context(&span, &carrier);
                }
                if let Err(error) = process_message(
                    task_store,
                    task_settings,
                    task_handler,
                    message,
                    task_metrics,
                    task_shutdown,
                )
                .instrument(span)
                .await
                {
                    error!(%error, kind, "job delivery processing failed");
                }
            });
        }
    }
    // In-flight jobs retain their permits and are drained by the process-level
    // shutdown path shared by all consumers.
    Ok(())
}

async fn process_message(
    store: JobStore,
    settings: WorkerSettings,
    handler: Arc<dyn JobHandler>,
    message: Message,
    metrics: Metrics,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let expected_kind = handler.kind();
    if *shutdown.borrow() {
        acknowledge(&message, AckKind::Nak(Some(settings.heartbeat))).await?;
        return Ok(());
    }
    let envelope = match JobEnvelope::<Value>::decode(&message.payload) {
        Ok(envelope) if envelope.kind == expected_kind => envelope,
        Ok(envelope) => {
            warn!(kind = %envelope.kind, expected_kind, "terminating misrouted job envelope");
            acknowledge(&message, AckKind::Term).await?;
            return Ok(());
        }
        Err(error) => {
            warn!(%error, expected_kind, "terminating invalid job envelope");
            acknowledge(&message, AckKind::Term).await?;
            return Ok(());
        }
    };
    let current_span = tracing::Span::current();
    set_parent_from_trace_context(&current_span, &envelope.trace_context);
    current_span.record("messaging.message.id", envelope.message_id.to_string());
    current_span.record("job.id", envelope.job_id);
    current_span.record("job.task_id", envelope.task_id);
    current_span.record("job.trace_id", envelope.trace_id.as_str());
    let claimed = match store
        .claim(
            envelope.job_id,
            envelope.message_id,
            envelope.task_id,
            &envelope.kind,
            &settings.instance_id,
            settings.lease,
        )
        .await?
    {
        ClaimResult::Claimed(job) => job,
        ClaimResult::Terminal => {
            acknowledge_confirmed(&message).await?;
            return Ok(());
        }
        ClaimResult::Missing => {
            warn!(job_id = envelope.job_id, "terminating orphaned job message");
            acknowledge(&message, AckKind::Term).await?;
            return Ok(());
        }
        ClaimResult::EnvelopeMismatch => {
            warn!(
                job_id = envelope.job_id,
                message_id = %envelope.message_id,
                "terminating stale or poisoned job envelope"
            );
            acknowledge(&message, AckKind::Term).await?;
            return Ok(());
        }
        ClaimResult::Busy => {
            acknowledge(&message, AckKind::Nak(Some(settings.heartbeat))).await?;
            return Ok(());
        }
    };
    info!(
        job_id = claimed.id,
        task_id = claimed.task_id,
        attempt = claimed.attempt,
        max_attempts = claimed.max_attempts,
        lease_until = %claimed.lease_until,
        trace_id = %claimed.trace_id,
        "durable job claimed"
    );
    let job_timer = metrics.worker_job_started(expected_kind);
    let (execution, shutdown_requeue) = {
        let execution = handler.execute(&store, &claimed);
        let heartbeat = run_job_heartbeat(
            store.clone(),
            settings.clone(),
            claimed.clone(),
            message.clone(),
        );
        tokio::pin!(execution);
        tokio::pin!(heartbeat);
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                let reason = if changed.is_err() || *shutdown.borrow() {
                    "Worker 正在关闭，当前尝试已取消并重新排队".to_string()
                } else {
                    "Worker 关闭信号异常，当前尝试已取消并重新排队".to_string()
                };
                (Err(reason), true)
            }
            heartbeat_result = &mut heartbeat => {
                let reason = match heartbeat_result {
                    Ok(()) => "任务心跳意外停止".to_string(),
                    Err(error) => format!("任务心跳失败，已取消执行：{error}"),
                };
                (Err(reason), false)
            }
            result = &mut execution => (result, false),
        }
    };

    let finalization: anyhow::Result<&'static str> = async {
        let metric_outcome = match execution {
            Ok(result) => match store
                .complete(claimed.id, claimed.lease_token, &result)
                .await?
            {
                CompletionDisposition::Completed => {
                    acknowledge_confirmed(&message).await?;
                    info!(
                        job_id = claimed.id,
                        task_id = claimed.task_id,
                        "durable job completed"
                    );
                    "completed"
                }
                CompletionDisposition::AlreadyCompleted => {
                    acknowledge_confirmed(&message).await?;
                    info!(
                        job_id = claimed.id,
                        task_id = claimed.task_id,
                        "handler transaction had already completed durable job"
                    );
                    "already_completed"
                }
                CompletionDisposition::LeaseLost => {
                    warn!(job_id = claimed.id, "job completion lost its lease fence");
                    acknowledge(&message, AckKind::Nak(Some(settings.heartbeat))).await?;
                    "lease_lost"
                }
            },
            Err(reason) => {
                if shutdown_requeue {
                    match store
                        .requeue_for_shutdown(claimed.id, claimed.lease_token, &reason)
                        .await?
                    {
                        FailureDisposition::Retry => {
                            info!(
                                job_id = claimed.id,
                                "shutdown handed the job to another worker without consuming an attempt"
                            );
                            acknowledge_confirmed(&message).await?;
                            "shutdown_requeued"
                        }
                        FailureDisposition::LeaseLost => {
                            warn!(job_id = claimed.id, "shutdown requeue lost its lease fence");
                            acknowledge(&message, AckKind::Nak(Some(settings.heartbeat))).await?;
                            "lease_lost"
                        }
                        FailureDisposition::Terminal => {
                            unreachable!("shutdown requeue never exhausts retries")
                        }
                    }
                } else {
                    let retry_after = configured_retry_delay(&claimed);
                    match store
                        .fail(claimed.id, claimed.lease_token, &reason, retry_after)
                        .await?
                    {
                        FailureDisposition::Retry => {
                            warn!(job_id = claimed.id, %reason, ?retry_after, "durable job scheduled for retry");
                            // PostgreSQL rotated message_id and reopened the outbox row.
                            // Remove this stale delivery; dispatcher will publish the
                            // replacement after available_at.
                            acknowledge_confirmed(&message).await?;
                            "retry"
                        }
                        FailureDisposition::Terminal => {
                            error!(job_id = claimed.id, %reason, "durable job exhausted retries");
                            acknowledge_confirmed(&message).await?;
                            "failed"
                        }
                        FailureDisposition::LeaseLost => {
                            warn!(job_id = claimed.id, "failed job lost its lease fence");
                            acknowledge(&message, AckKind::Nak(Some(settings.heartbeat))).await?;
                            "lease_lost"
                        }
                    }
                }
            }
        };
        Ok(metric_outcome)
    }
    .await;
    finish_job_metrics(job_timer, finalization)
}

fn finish_job_metrics(
    job_timer: rust_toon_framework_telemetry::WorkerJobTimer,
    finalization: anyhow::Result<&'static str>,
) -> anyhow::Result<()> {
    match finalization {
        Ok(outcome) => {
            job_timer.finish(outcome);
            Ok(())
        }
        Err(error) => {
            job_timer.finish("finalization_error");
            Err(error)
        }
    }
}

async fn run_job_heartbeat(
    store: JobStore,
    settings: WorkerSettings,
    job: job_store::ClaimedJob,
    message: Message,
) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(settings.heartbeat);
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if !store.heartbeat(job.id, job.lease_token, settings.lease).await? {
                    anyhow::bail!("database lease fence was lost");
                }
                acknowledge(&message, AckKind::Progress).await?;
            }
        }
    }
}

async fn acknowledge(message: &Message, kind: AckKind) -> anyhow::Result<()> {
    message
        .ack_with(kind)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn acknowledge_confirmed(message: &Message) -> anyhow::Result<()> {
    message
        .double_ack()
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn serve_health(
    config: ServiceConfig,
    state: HealthState,
    metrics: Metrics,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut app = Router::new()
        .route("/health", get(livez))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .with_state(state);
    if metrics.enabled() {
        app = app.merge(metrics.routes());
    }
    let app = app.layer(from_fn_with_state(metrics, record_http_metrics));
    let address = config.addr()?;
    let listener = TcpListener::bind(address).await?;
    info!(%address, "worker health server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
        })
        .await?;
    Ok(())
}

async fn livez() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(HealthBody {
            service: SERVICE_NAME,
            status: "up",
        }),
    )
}

async fn readyz(State(state): State<HealthState>) -> impl IntoResponse {
    let ready = state.accepting.load(Ordering::Acquire)
        && state.ffmpeg
        && ping(state.store.pool()).await.is_ok()
        && state.broker.readiness().await.is_ok()
        && rust_toon_toon_server::check_object_storage_readiness()
            .await
            .is_ok();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthBody {
            service: SERVICE_NAME,
            status: if ready { "ready" } else { "unavailable" },
        }),
    )
}

async fn ffmpeg_available() -> bool {
    tokio::task::spawn_blocking(|| {
        ["ffmpeg", "ffprobe"].into_iter().all(|binary| {
            std::process::Command::new(binary)
                .arg("-version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
    })
    .await
    .unwrap_or(false)
}

async fn wait_or_shutdown(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

fn durable_name(kind: &str) -> String {
    format!("toon_workers_{}", kind.replace('.', "_"))
}

fn retry_delay(attempt: i32) -> Duration {
    let exponent = u32::try_from(attempt.saturating_sub(1))
        .unwrap_or_default()
        .min(7);
    Duration::from_secs(2_u64.saturating_pow(exponent).min(300))
}

fn configured_retry_delay(job: &job_store::ClaimedJob) -> Duration {
    job.payload
        .get("retryIntervalMillis")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .map(|millis| Duration::from_millis(millis.min(86_400_000)))
        .unwrap_or_else(|| retry_delay(job.attempt))
}

fn default_instance_id() -> String {
    let host = env::var("HOSTNAME").unwrap_or_else(|_| "worker".to_string());
    format!("{host}-{}-{}", std::process::id(), Uuid::new_v4())
}

fn env_number(name: &str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
    let value = match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer"))?,
        Err(env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max}"
    );
    Ok(value)
}

fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match env::var(name) {
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes") => {
            Ok(true)
        }
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no") => {
            Ok(false)
        }
        Ok(_) => anyhow::bail!("{name} must be true or false"),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_bounded_exponential_backoff() {
        assert_eq!(retry_delay(1), Duration::from_secs(1));
        assert_eq!(retry_delay(2), Duration::from_secs(2));
        assert_eq!(retry_delay(20), Duration::from_secs(128));
    }

    #[test]
    fn durable_names_are_shared_by_replicas_and_valid() {
        assert_eq!(
            durable_name(rust_toon_toon_server::VIDEO_EXPORT_JOB_KIND),
            "toon_workers_toon_video_export"
        );
    }

    #[test]
    fn finalization_errors_are_not_recorded_as_cancelled_jobs() {
        let metrics = Metrics::new("worker-test", true);
        let result = finish_job_metrics(
            metrics.worker_job_started("video.merge"),
            Err(anyhow::anyhow!("message acknowledgement failed")),
        );

        assert!(result.is_err());
        let encoded = metrics.encode().expect("encode worker metrics");
        assert!(encoded.contains("outcome=\"finalization_error\""));
        assert!(!encoded.contains("outcome=\"cancelled\""));
    }
}
