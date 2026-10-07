use std::{
    collections::hash_map::DefaultHasher,
    env,
    hash::{Hash, Hasher},
    net::IpAddr,
    time::Duration,
};

use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use rust_toon_framework_common::{ApiResponse, is_health_probe_path};
use tokio::sync::watch;
use tracing::warn;

use crate::RedisClient;

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub namespace: String,
    pub max_requests: u64,
    pub window: Duration,
    pub trust_proxy_headers: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            namespace: "rate-limit".to_string(),
            max_requests: 300,
            window: Duration::from_secs(60),
            trust_proxy_headers: false,
        }
    }
}

impl RateLimitConfig {
    pub fn from_env() -> Self {
        Self {
            namespace: env::var("RATE_LIMIT_NAMESPACE").unwrap_or_else(|_| "rate-limit".into()),
            max_requests: env::var("RATE_LIMIT_MAX_REQUESTS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(300),
            window: Duration::from_secs(
                env::var("RATE_LIMIT_WINDOW_SECONDS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(60),
            ),
            trust_proxy_headers: env::var("RATE_LIMIT_TRUST_PROXY_HEADERS")
                .ok()
                .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE")),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitState {
    redis: RedisClient,
    config: watch::Receiver<RateLimitConfig>,
}

impl RateLimitState {
    pub fn new(redis: RedisClient, config: RateLimitConfig) -> Self {
        let (_sender, receiver) = watch::channel(config);
        Self {
            redis,
            config: receiver,
        }
    }

    pub fn with_receiver(redis: RedisClient, config: watch::Receiver<RateLimitConfig>) -> Self {
        Self { redis, config }
    }
}

pub async fn rate_limit(
    State(state): State<RateLimitState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    if is_health_probe_path(&path) {
        return next.run(request).await;
    }
    // Clone the complete snapshot before any await so one request never mixes
    // values from two configuration revisions.
    let config = state.config.borrow().clone();
    let actor = client_key(&request, config.trust_proxy_headers);
    let key = state
        .redis
        .key(&config.namespace, format!("{}:{}:{}", actor, method, path));

    match state.redis.increment_with_ttl(&key, config.window).await {
        Ok(count) if count > config.max_requests => {
            let body = ApiResponse {
                code: 429,
                data: (),
                message: "rate limit exceeded".to_string(),
            };
            (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response()
        }
        Ok(_) => next.run(request).await,
        Err(error) => {
            warn!(%error, "redis rate limit check failed");
            next.run(request).await
        }
    }
}

fn client_key(request: &Request, trust_proxy_headers: bool) -> String {
    let forwarded = if trust_proxy_headers {
        request
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    } else {
        None
    };
    forwarded
        .or_else(|| {
            request
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|connect_info| match connect_info.0.ip() {
                    IpAddr::V4(ip) => ip.to_string(),
                    IpAddr::V6(ip) => ip.to_string(),
                })
        })
        .or_else(|| {
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(|value| {
                    let mut hasher = DefaultHasher::new();
                    value.hash(&mut hasher);
                    format!("auth-{:x}", hasher.finish())
                })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};

    use super::client_key;

    #[test]
    fn uses_first_forwarded_for_ip() {
        let request = Request::builder()
            .header("x-forwarded-for", "10.0.0.1, 10.0.0.2")
            .body(Body::empty())
            .unwrap();

        assert_eq!(client_key(&request, true), "10.0.0.1");
        assert_eq!(client_key(&request, false), "unknown");
    }

    #[test]
    fn separates_authenticated_clients_without_connect_info() {
        let first = Request::builder()
            .header("authorization", "Bearer first")
            .body(Body::empty())
            .unwrap();
        let second = Request::builder()
            .header("authorization", "Bearer second")
            .body(Body::empty())
            .unwrap();
        assert_ne!(client_key(&first, false), client_key(&second, false));
        assert!(client_key(&first, false).starts_with("auth-"));
    }
}
