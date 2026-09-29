use std::{net::SocketAddr, time::Instant};

use axum::{
    body::{Body, Bytes, HttpBody, to_bytes},
    extract::{ConnectInfo, Query, Request, State},
    http::{HeaderMap, Uri},
    middleware::Next,
    response::Response,
};
use rust_toon_framework_common::is_health_probe_path;
use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::CurrentUser;
use serde_json::Value;

const MAX_AUDIT_BODY_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub struct AuditState {
    pool: PgPool,
}

impl AuditState {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

pub async fn record(
    State(state): State<AuditState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if is_health_probe_path(request.uri().path()) {
        return next.run(request).await;
    }
    let started_at = chrono::Utc::now().naive_utc();
    let timer = Instant::now();
    let method = request.method().to_string();
    let uri = request.uri().clone();
    let path = audit_path(&uri);
    let authenticated_id = request.extensions().get::<CurrentUser>()
        .map(|user| user.user_id.clone());
    let peer_ip = request.extensions().get::<ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip());
    let request_headers = request.headers().clone();
    let user_agent = request_headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .chars()
        .take(500)
        .collect::<String>();
    let request_trace_id = request_headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let user_ip = peer_ip.map(|ip| ip.to_string());
    let (request, request_body) = capture_request(request, &request_headers, &uri).await;
    let request_params = request_preview(&method, &uri, &request_headers, &request_body);
    let response = next.run(request).await;
    let status = response.status().as_u16() as i32;
    let trace_id = request_trace_id
        .or_else(|| {
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let (response, response_body) = capture_response(response, status, &uri).await;
    let duration = timer.elapsed().as_millis().min(i32::MAX as u128) as i32;
    let ended_at = chrono::Utc::now().naive_utc();
    let module = uri
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("gateway")
        .to_string();
    let (operate_name, operate_type) = operation_metadata(&method, uri.path());
    let pool = state.pool.clone();
    tokio::spawn(async move {
        // CurrentUser uses the stable UUID identity; logs use system_users.id.
        let user_id: Option<i64> = match authenticated_id {
            Some(id) => sqlx::query_scalar(
                "SELECT id FROM system_users WHERE md5('yudao-user:' || id::text)::uuid::text=$1",
            ).bind(id).fetch_optional(&pool).await.unwrap_or(None),
            None => None,
        };
        let user_type: Option<i16> = user_id.map(|_| 2);
        let _ = sqlx::query(
            "INSERT INTO infra_api_access_log(
                trace_id, application_name, request_method, request_url, request_params,
                response_body, user_ip, user_agent, operate_module, operate_name, operate_type,
                begin_time, end_time, duration, result_code, result_msg, user_id, user_type
             ) VALUES($1,'rust-toon-gateway',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(trace_id)
        .bind(method)
        .bind(path)
        .bind(request_params)
        .bind(response_body)
        .bind(user_ip)
        .bind(user_agent)
        .bind(module)
        .bind(operate_name)
        .bind(operate_type)
        .bind(started_at)
        .bind(ended_at)
        .bind(duration)
        .bind(status)
        .bind(if status >= 400 { "failed" } else { "ok" })
        .bind(user_id)
        .bind(user_type)
        .execute(&pool)
        .await;
    });
    response
}

fn audit_path(uri: &Uri) -> String {
    let mut path = uri.path().to_owned();
    if uri.query().is_some() {
        // Query values can themselves be URLs, JSON, or arbitrary secrets.
        // Keep only numeric pagination/resource selectors in the audit target.
        match Query::<Vec<(String, String)>>::try_from_uri(uri) {
            Ok(Query(pairs)) => {
                let query = pairs.into_iter().map(|(key, value)| {
                    let name = normalized_key(&key);
                    let safe = matches!(name.as_str(), "id" | "projectid" | "scriptid" | "pageno" | "pagesize" | "page" | "limit" | "afterid" | "beforeid")
                        && !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
                    format!("{}={}", encode_query_key(&key), if safe { value.as_str() } else { "[REDACTED]" })
                }).collect::<Vec<_>>().join("&");
                path.push('?');
                path.push_str(&query);
            }
            Err(_) => path.push_str("?[query omitted]"),
        }
    }
    path.chars().take(1024).collect()
}

fn encode_query_key(key: &str) -> String {
    key.bytes().map(|byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            (byte as char).to_string()
        } else { format!("%{byte:02X}") }
    }).collect()
}

fn metadata_only(uri: &Uri) -> bool {
    let path = uri.path();
    ["/system/", "/ai/model", "/setting/", "/infra/config/", "/infra/data-source-config/", "/infra/file-config/", "/infra/api-access-log/", "/infra/api-error-log/"]
        .iter().any(|prefix| path.starts_with(prefix))
}

async fn capture_request(
    request: Request<Body>,
    headers: &HeaderMap,
    uri: &Uri,
) -> (Request<Body>, String) {
    let (parts, body) = request.into_parts();
    if metadata_only(uri) || !can_capture_body(headers, &body) {
        return (Request::from_parts(parts, body), "[body omitted]".into());
    }
    match to_bytes(body, MAX_AUDIT_BODY_BYTES).await {
        Ok(bytes) => (
            Request::from_parts(parts, Body::from(bytes.clone())),
            bytes_preview(&bytes),
        ),
        Err(error) => (
            Request::from_parts(parts, failed_body(error)),
            "[request body could not be read]".into(),
        ),
    }
}

fn request_preview(method: &str, uri: &Uri, headers: &HeaderMap, body: &str) -> String {
    let mut output = format!("{method} {}\n{}", audit_path(uri), headers_preview(headers));
    if !body.is_empty() {
        output.push_str("\n");
        output.push_str(body);
    }
    output
}

async fn capture_response(response: Response, status: i32, uri: &Uri) -> (Response, String) {
    let (parts, body) = response.into_parts();
    let headers = parts.headers.clone();
    let status_line = format!("HTTP/1.1 {status}\n{}", headers_preview(&headers));
    if metadata_only(uri) || !can_capture_body(&headers, &body) {
        return (
            Response::from_parts(parts, body),
            format!("{status_line}\n[body omitted]"),
        );
    }
    match to_bytes(body, MAX_AUDIT_BODY_BYTES).await {
        Ok(bytes) => {
            let text = bytes_preview(&bytes);
            let preview = if text.is_empty() {
                status_line
            } else {
                format!("{status_line}\n{text}")
            };
            (Response::from_parts(parts, Body::from(bytes)), preview)
        }
        Err(error) => (
            Response::from_parts(parts, failed_body(error)),
            format!("{status_line}\n[response body could not be read]"),
        ),
    }
}

fn failed_body(error: axum::Error) -> Body {
    Body::from_stream(futures_util::stream::once(async move { Err::<Bytes, _>(error) }))
}

fn can_capture_body(headers: &HeaderMap, body: &Body) -> bool {
    // Never buffer an unknown-length stream just for logging. HTTP extractors
    // retain responsibility for enforcing the actual request size limit.
    if !body.size_hint().upper().is_some_and(|length| length <= MAX_AUDIT_BODY_BYTES as u64) {
        return false;
    }
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    media_type == "application/json" || media_type.ends_with("+json")
}

fn bytes_preview(bytes: &[u8]) -> String {
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return "[invalid JSON body omitted]".into();
    };
    redact_json(&mut value);
    let text = value.to_string();
    text.chars().take(MAX_AUDIT_BODY_BYTES).collect()
}

fn normalized_key(key: &str) -> String {
    key.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

fn sensitive_key(key: &str) -> bool {
    let key = normalized_key(key);
    key == "key" || key == "url" || key == "dsn" || ["password", "passwd", "secret", "token", "credential", "authorization", "cookie", "apikey", "accesskey", "privatekey", "connectionstring"].iter().any(|part| key.contains(part))
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(object) => for (key, value) in object {
            if sensitive_key(key) { *value = Value::String("[REDACTED]".into()); }
            else { redact_json(value); }
        },
        Value::Array(items) => for item in items { redact_json(item); },
        // Free text may contain nested JSON, provider errors or a pasted URL.
        // Store string lengths rather than trying to guess every secret format.
        Value::String(text) => *text = format!("[text omitted: {} chars]", text.chars().count()),
        _ => {}
    }
}

fn headers_preview(headers: &HeaderMap) -> String {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let preview = if matches!(name.as_str(), "content-type" | "content-length" | "accept") {
                value.to_str().ok()?.to_owned()
            } else { "[REDACTED]".into() };
            Some(format!("{name}: {preview}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn operation_metadata(method: &str, path: &str) -> (String, i16) {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let action = segments.last().copied().unwrap_or("request");
    let resource = if action.parse::<i64>().is_ok()
        || matches!(
        action,
        "page" | "list" | "get" | "create" | "update" | "delete" | "export-excel"
            | "import" | "login" | "logout" | "trigger" | "retry"
    ) {
        segments.iter().rev().nth(1).copied().unwrap_or("gateway")
    } else {
        action
    };
    let (label, operate_type) = match action {
        "page" => ("分页查询", 1),
        "list" => ("列表查询", 1),
        "get" => ("查询详情", 1),
        "create" => ("新增", 2),
        "update" => ("修改", 3),
        "delete" => ("删除", 4),
        "export-excel" => ("导出", 5),
        "import" => ("导入", 6),
        "login" => ("登录", 0),
        "logout" => ("退出登录", 0),
        "trigger" => ("触发", 3),
        "retry" => ("重试", 3),
        _ => match method {
            "POST" => ("提交", 2),
            "PUT" | "PATCH" => ("修改", 3),
            "DELETE" => ("删除", 4),
            _ => ("查询", 1),
        },
    };
    (format!("{resource} · {label}"), operate_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Uri;

    #[test]
    fn audit_path_redacts_query_credentials() {
        let uri: Uri = "/toonflow/ws?token=super-secret-jwt&projectId=42"
            .parse()
            .expect("valid URI");

        let path = audit_path(&uri);

        assert_eq!(path, "/toonflow/ws?token=[REDACTED]&projectId=42");
    }

    #[test]
    fn redacts_encoded_duplicate_credentials_headers_and_nested_values() {
        let uri: Uri = "/socket/scriptAgent?%74oken=first-secret&token=second-secret&redirect=https%3A%2F%2Fx%2F%3Ftoken%3Dnested-secret&projectId=42".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer header-secret".parse().unwrap());
        headers.insert("cookie", "session=cookie-secret".parse().unwrap());
        headers.insert("set-cookie", "session=response-secret".parse().unwrap());
        headers.insert("x-custom-key", "custom-secret".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let body = serde_json::json!({
            "id": 42, "config": {"api_key": "api-secret", "privateKey": "private-secret"},
            "items": [{"password": "password-secret", "accessToken":"access-secret"}],
            "content":"https://example.com/?token=text-secret", "ok":true
        });
        let preview = request_preview("POST", &uri, &headers, &bytes_preview(body.to_string().as_bytes()));
        for secret in ["first-secret", "second-secret", "nested-secret", "header-secret", "cookie-secret", "response-secret", "custom-secret", "api-secret", "private-secret", "password-secret", "access-secret", "text-secret"] {
            assert!(!preview.contains(secret), "leaked {secret}");
        }
        assert!(preview.contains("projectId=42"));
        assert!(preview.contains("\"id\":42"));
        assert!(!bytes_preview(br#"{"password":"incomplete-secret"#).contains("incomplete-secret"));
    }

    #[tokio::test]
    async fn audit_preserves_business_body_and_omits_sensitive_routes() {
        let uri: Uri = "/system/auth/login?token=query-secret".parse().unwrap();
        let payload = r#"{"password":"body-secret","username":"admin"}"#;
        let request = Request::builder().uri(uri.clone()).header("content-type", "application/json")
            .body(Body::from(payload)).unwrap();
        let headers = request.headers().clone();
        let (request, preview) = capture_request(request, &headers, &uri).await;
        assert!(!preview.contains("body-secret"));
        assert_eq!(to_bytes(request.into_body(), 1024).await.unwrap(), payload);
        let response = Response::builder().header("content-type", "application/json")
            .body(Body::from(r#"{"accessToken":"response-secret"}"#)).unwrap();
        let (response, preview) = capture_response(response, 200, &uri).await;
        assert!(!preview.contains("response-secret"));
        assert!(String::from_utf8(to_bytes(response.into_body(), 1024).await.unwrap().to_vec()).unwrap().contains("response-secret"));
    }

    #[tokio::test]
    async fn audit_does_not_read_unknown_length_or_oversized_streams() {
        let uri = "/example".parse().unwrap();
        let stream = futures_util::stream::once(async { Ok::<_, std::convert::Infallible>("{\"value\":1}") });
        let response = Response::builder().header("content-type", "application/json")
            .body(Body::from_stream(stream)).unwrap();
        let (response, preview) = capture_response(response, 200, &uri).await;
        assert!(preview.contains("body omitted"));
        assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "{\"value\":1}");
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        assert!(!can_capture_body(&headers, &Body::from(vec![b'x'; MAX_AUDIT_BODY_BYTES + 1])));
    }

    #[test]
    fn operation_metadata_uses_resource_and_action_semantics() {
        assert_eq!(
            operation_metadata("GET", "/infra/api-access-log/page"),
            ("api-access-log · 分页查询".to_string(), 1)
        );
        assert_eq!(
            operation_metadata("POST", "/system/auth/login"),
            ("auth · 登录".to_string(), 0)
        );
        assert_eq!(
            operation_metadata("DELETE", "/toonflow/projects/42"),
            ("projects · 删除".to_string(), 4)
        );
    }
}
