use std::{
    collections::BTreeMap,
    env,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use rust_toon_framework_database::{PgPool, ping as ping_database};
use rust_toon_framework_redis::RedisClient;
use serde::Serialize;

const SERVICE_NAME: &str = "gateway";

#[derive(Clone)]
pub struct ReadinessState {
    database: PgPool,
    redis: Option<RedisClient>,
    redis_required: bool,
    object_storage_configured: bool,
    object_storage_required: bool,
    ffmpeg_required: bool,
    drain: DrainHandle,
}

#[derive(Clone)]
pub struct DrainHandle {
    accepting_traffic: Arc<AtomicBool>,
}

impl DrainHandle {
    fn new() -> Self {
        Self {
            accepting_traffic: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn begin_draining(&self) {
        self.accepting_traffic.store(false, Ordering::Release);
    }

    fn is_accepting_traffic(&self) -> bool {
        self.accepting_traffic.load(Ordering::Acquire)
    }
}

impl ReadinessState {
    pub fn new(database: PgPool, redis: Option<RedisClient>, redis_configured: bool) -> Self {
        let production = env::var("RUST_ENV")
            .map(|value| value.eq_ignore_ascii_case("production"))
            .unwrap_or(false);
        let object_storage_configured = env::var_os("S3_ENDPOINT").is_some();
        Self {
            database,
            redis,
            redis_required: env_bool("READINESS_REQUIRE_REDIS", redis_configured),
            object_storage_configured,
            object_storage_required: env_bool(
                "READINESS_REQUIRE_OBJECT_STORAGE",
                production || object_storage_configured,
            ),
            ffmpeg_required: env_bool("READINESS_REQUIRE_FFMPEG", false),
            drain: DrainHandle::new(),
        }
    }

    pub fn drain_handle(&self) -> DrainHandle {
        self.drain.clone()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    status: &'static str,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl Check {
    fn ok(required: bool) -> Self {
        Self {
            status: "ok",
            required,
            detail: None,
        }
    }

    fn skipped() -> Self {
        Self {
            status: "skipped",
            required: false,
            detail: None,
        }
    }

    fn failed(required: bool, detail: impl Into<String>) -> Self {
        Self {
            status: "failed",
            required,
            detail: Some(detail.into()),
        }
    }

    fn blocks_readiness(&self) -> bool {
        self.required && self.status != "ok"
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeResponse {
    service: &'static str,
    status: &'static str,
    checked_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    checks: BTreeMap<&'static str, Check>,
}

pub fn routes(state: ReadinessState) -> Router {
    Router::new()
        .route("/livez", get(live))
        .route("/readyz", get(ready))
        .with_state(state)
}

async fn live() -> Json<ProbeResponse> {
    Json(ProbeResponse {
        service: SERVICE_NAME,
        status: "ok",
        checked_at: chrono::Utc::now(),
        checks: BTreeMap::new(),
    })
}

async fn ready(State(state): State<ReadinessState>) -> Response {
    let traffic = check_traffic(&state);
    let database = check_database(&state);
    let redis = check_redis(&state);
    let object_storage = check_object_storage(&state);
    let ffmpeg = check_ffmpeg(state.ffmpeg_required);
    let (database, redis, object_storage, ffmpeg) =
        tokio::join!(database, redis, object_storage, ffmpeg);

    let mut checks = BTreeMap::new();
    checks.insert("database", database);
    checks.insert("ffmpeg", ffmpeg);
    checks.insert("objectStorage", object_storage);
    checks.insert("redis", redis);
    checks.insert("traffic", traffic);
    let ready = !checks.values().any(Check::blocks_readiness);
    let response = ProbeResponse {
        service: SERVICE_NAME,
        status: if ready { "ok" } else { "unavailable" },
        checked_at: chrono::Utc::now(),
        checks,
    };
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(response),
    )
        .into_response()
}

fn check_traffic(state: &ReadinessState) -> Check {
    if state.drain.is_accepting_traffic() {
        Check::ok(true)
    } else {
        Check::failed(true, "gateway is draining")
    }
}

async fn check_database(state: &ReadinessState) -> Check {
    match tokio::time::timeout(Duration::from_secs(3), ping_database(&state.database)).await {
        Ok(Ok(())) => Check::ok(true),
        Ok(Err(error)) => Check::failed(true, error.to_string()),
        Err(_) => Check::failed(true, "database check timed out"),
    }
}

async fn check_redis(state: &ReadinessState) -> Check {
    let Some(redis) = state.redis.as_ref() else {
        return if state.redis_required {
            Check::failed(true, "Redis is configured but the gateway is disconnected")
        } else {
            Check::skipped()
        };
    };
    match tokio::time::timeout(Duration::from_secs(3), redis.ping()).await {
        Ok(Ok(())) => Check::ok(state.redis_required),
        Ok(Err(error)) => Check::failed(state.redis_required, error.to_string()),
        Err(_) => Check::failed(state.redis_required, "Redis check timed out"),
    }
}

async fn check_object_storage(state: &ReadinessState) -> Check {
    if !state.object_storage_required && !state.object_storage_configured {
        return Check::skipped();
    }
    match tokio::time::timeout(
        Duration::from_secs(3),
        rust_toon_toon_server::check_object_storage_readiness(),
    )
    .await
    {
        Ok(Ok(())) => Check::ok(state.object_storage_required),
        Ok(Err(error)) => Check::failed(state.object_storage_required, error),
        Err(_) => Check::failed(state.object_storage_required, "object storage check timed out"),
    }
}

async fn check_ffmpeg(required: bool) -> Check {
    if !required {
        return Check::skipped();
    }
    let (ffmpeg, ffprobe) = tokio::join!(check_media_tool("ffmpeg"), check_media_tool("ffprobe"));
    match (ffmpeg, ffprobe) {
        (Ok(()), Ok(())) => Check::ok(true),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Check::failed(true, error),
        (Err(ffmpeg_error), Err(ffprobe_error)) => {
            Check::failed(true, format!("{ffmpeg_error}; {ffprobe_error}"))
        }
    }
}

async fn check_media_tool(name: &'static str) -> Result<(), String> {
    let mut command = tokio::process::Command::new(name);
    command.arg("-version").kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(3), command.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(format!("{name} exited with status {}", output.status)),
        Ok(Err(error)) => Err(format!("failed to run {name}: {error}")),
        Err(_) => Err(format!("{name} check timed out")),
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{DrainHandle, env_bool};

    #[test]
    fn readiness_boolean_defaults_are_stable() {
        assert!(!env_bool("RUST_TOON_TEST_MISSING_READINESS_FLAG", false));
        assert!(env_bool("RUST_TOON_TEST_MISSING_READINESS_FLAG", true));
    }

    #[test]
    fn drain_handle_is_shared_and_one_way() {
        let handle = DrainHandle::new();
        let observer = handle.clone();
        assert!(observer.is_accepting_traffic());

        handle.begin_draining();

        assert!(!observer.is_accepting_traffic());
    }
}
