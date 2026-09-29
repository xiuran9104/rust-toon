#![recursion_limit = "256"]

use std::{env, time::Duration};

use axum::{Json, Router, middleware::from_fn_with_state, routing::get};
use rust_toon_framework_common::{ApiResponse, ServiceConfig, health_route};
use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
use rust_toon_framework_dynamic_config::{
    DynamicConfig, GatewayRuntimeConfig, NacosConfig, subscribe_json,
};
use rust_toon_framework_redis::{RateLimitConfig, RateLimitState, RedisClient, RedisConfig};
use rust_toon_framework_security::{SecurityConfig, TokenService};
use rust_toon_framework_telemetry::{init_telemetry, record_http_metrics};
use rust_toon_framework_web::{AppError, WebConfig, apply_web_layers};
use serde::Serialize;
use tokio::{net::TcpListener, sync::watch};
use tracing::{info, warn};

mod audit;
mod openapi;
mod readiness;

const SERVICE_NAME: &str = "gateway";

#[derive(Debug, Serialize)]
struct GatewayIndex {
    service: &'static str,
    modules: [&'static str; 5],
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let telemetry = init_telemetry(SERVICE_NAME)?;
    let metrics = telemetry.metrics();

    let rate_limit_defaults = RateLimitConfig::from_env();
    let gateway_dynamic = subscribe_json(
        NacosConfig::from_env(SERVICE_NAME)?,
        GatewayRuntimeConfig::new(
            rate_limit_defaults.max_requests,
            rate_limit_defaults.window.as_secs(),
        ),
        GatewayRuntimeConfig::validate,
    )
    .await?;
    let rate_limit_config = project_rate_limit_config(&gateway_dynamic, rate_limit_defaults);

    let database = connect(&DatabaseConfig::from_env()?).await?;
    migrate(&database).await?;
    let repaired = rust_toon_toon_server::repair_gateway_interrupted_state(&database).await?;
    if repaired > 0 {
        warn!(repaired, "marked interrupted Gateway-owned work as failed");
    }
    let removed_provider_downloads =
        rust_toon_toon_server::cleanup_provider_video_temp_on_startup()
            .await
            .map_err(anyhow::Error::msg)?;
    if removed_provider_downloads > 0 {
        warn!(
            removed_provider_downloads,
            "removed interrupted provider video downloads"
        );
    }
    let redis_configured = std::env::var_os("REDIS_URL").is_some();
    let redis = connect_redis().await;
    let tokens = TokenService::new(SecurityConfig::from_env()?);
    let system_state = rust_toon_system_server::SystemState::with_cache(
        database.clone(),
        tokens.clone(),
        redis.clone(),
    );
    let infra_state = rust_toon_infra_server::InfraState::new(database.clone(), tokens.clone());
    let ai_state = rust_toon_ai_server::AiState::new(database.clone(), tokens.clone());
    if std::env::var_os("S3_ENDPOINT").is_some() {
        rust_toon_toon_server::initialize_object_storage()
            .await
            .map_err(anyhow::Error::msg)?;
    }
    let toon_state = rust_toon_toon_server::ToonState::new(database.clone(), tokens.clone());
    let resumed_videos =
        rust_toon_toon_server::resume_interrupted_video_generations(&database).await;
    let resumed_runs =
        rust_toon_toon_server::resume_interrupted_workflow_runs(&toon_state).await;
    if resumed_videos > 0 || resumed_runs > 0 {
        info!(
            resumed_videos,
            resumed_runs, "resumed interrupted video generations and workflow runs"
        );
    }
    let media_state = rust_toon_media_server::MediaState::new(database.clone(), tokens);
    system_state.bootstrap().await?;
    let database_auth = system_state.database_auth_state();
    let readiness_state =
        readiness::ReadinessState::new(database.clone(), redis.clone(), redis_configured);
    let drain = readiness_state.drain_handle();

    let mut app = Router::new()
        .route("/", get(index))
        .route("/openapi.json", get(openapi::document))
        .merge(rust_toon_system_server::routes(system_state))
        .merge(rust_toon_infra_server::routes(infra_state))
        .merge(rust_toon_ai_server::routes(ai_state))
        .merge(rust_toon_toon_server::routes(toon_state))
        .merge(rust_toon_media_server::routes(media_state))
        .merge(readiness::routes(readiness_state))
        .merge(health_route(SERVICE_NAME))
        .fallback(not_found)
        .layer(from_fn_with_state(
            audit::AuditState::new(database),
            audit::record,
        ))
        .layer(from_fn_with_state(
            database_auth,
            rust_toon_system_server::authenticate_from_database,
        ));

    if let Some(redis) = redis {
        app = app.layer(from_fn_with_state(
            RateLimitState::with_receiver(redis, rate_limit_config),
            rust_toon_framework_redis::rate_limit,
        ));
    }

    // Merge the scrape endpoint after auth/audit/rate-limit layers so routine
    // Prometheus collection does not create audit rows or consume user quota.
    if metrics.enabled() {
        app = app.merge(metrics.routes());
    }
    let app = app.layer(from_fn_with_state(metrics.clone(), record_http_metrics));

    let app = apply_web_layers(app, WebConfig::from_env());

    serve_gateway(ServiceConfig::from_env(SERVICE_NAME, 8080), app, drain).await
}

fn project_rate_limit_config(
    dynamic: &DynamicConfig<GatewayRuntimeConfig>,
    defaults: RateLimitConfig,
) -> watch::Receiver<RateLimitConfig> {
    let namespace = defaults.namespace;
    let current = dynamic.current();
    let (sender, receiver) = watch::channel(RateLimitConfig {
        namespace: namespace.clone(),
        max_requests: current.rate_limit.max_requests,
        window: Duration::from_secs(current.rate_limit.window_seconds),
    });
    let mut source = dynamic.receiver();
    tokio::spawn(async move {
        while source.changed().await.is_ok() {
            let current = source.borrow().clone();
            sender.send_replace(RateLimitConfig {
                namespace: namespace.clone(),
                max_requests: current.rate_limit.max_requests,
                window: Duration::from_secs(current.rate_limit.window_seconds),
            });
        }
    });
    receiver
}

async fn serve_gateway(
    config: ServiceConfig,
    app: Router,
    drain: readiness::DrainHandle,
) -> anyhow::Result<()> {
    let addr = config.addr()?;
    let listener = TcpListener::bind(addr).await?;
    info!(service = %config.name, address = %addr, "service listening");

    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal(drain))
        .await?;

    Ok(())
}

async fn shutdown_signal(drain: readiness::DrainHandle) {
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
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    drain.begin_draining();
    let delay = drain_delay();
    info!(delay_ms = delay.as_millis(), "gateway is draining");
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

fn drain_delay() -> Duration {
    let default_seconds = if env::var("RUST_ENV")
        .map(|value| value.eq_ignore_ascii_case("production"))
        .unwrap_or(false)
    {
        5
    } else {
        0
    };
    let seconds = env::var("GATEWAY_DRAIN_DELAY_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default_seconds)
        .min(300);
    Duration::from_secs(seconds)
}

async fn connect_redis() -> Option<RedisClient> {
    let config = RedisConfig::from_env()?;
    match RedisClient::connect(&config).await {
        Ok(client) => Some(client),
        Err(error) => {
            warn!(%error, "redis is configured but unavailable; cache and rate limit disabled");
            None
        }
    }
}

async fn not_found() -> AppError {
    AppError::not_found("route not found")
}

async fn index() -> Json<ApiResponse<GatewayIndex>> {
    Json(ApiResponse::new(GatewayIndex {
        service: SERVICE_NAME,
        modules: ["system", "infra", "ai", "toon", "media"],
    }))
}
