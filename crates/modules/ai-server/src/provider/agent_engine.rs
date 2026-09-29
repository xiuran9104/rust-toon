use async_trait::async_trait;
use futures_util::StreamExt;
use rust_toon_ai_api::{ChatRequest, ChatResponse, ModelConfig};
use rust_toon_framework_security::{
    CurrentUser, DataScope, PermissionSet, SecurityConfig, TokenService,
};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

use super::ChatProvider;

const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:7750";
const SIDECAR_TURN_PATH: &str = "/sidecar/v1/turn";
const SIDECAR_TOKEN_TTL: Duration = Duration::from_secs(300);
const SIDECAR_ISSUER: &str = "rust-toon";
const SIDECAR_AUDIENCE: &str = "piren-sidecar";

pub struct AgentEngineProvider;

fn config_error(message: &str) -> String {
    super::ProviderError {
        code: "AI_AGENT_ENGINE_CONFIG".into(),
        category: "config".into(),
        message: format!("AgentEngine 配置错误：{message}"),
        status: None,
        response_data: None,
        retryable: false,
    }
    .encoded()
}

fn stream_error(message: &str, summary: &str, retryable: bool) -> String {
    super::ProviderError {
        code: "AI_AGENT_ENGINE_STREAM".into(),
        category: "response".into(),
        message: message.into(),
        status: None,
        response_data: Some(json!({"summary": super::truncate(summary, 1000)})),
        retryable,
    }
    .encoded()
}

fn malformed_event(data: &str) -> String {
    stream_error("AgentEngine 返回了无法解析的流式事件", data, false)
}

/// AgentEngine calls are proxied to the piren sidecar at this address; the
/// env var overrides the default, matching the fixed sidecar contract.
fn sidecar_base_url() -> String {
    std::env::var("PIREN_SIDECAR_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SIDECAR_URL.to_string())
}

/// The proxy JWT is signed with the dedicated `PIREN_SIDECAR_SECRET`, never
/// with the user-facing `JWT_SECRET`, so it gets its own `TokenService`.
/// The instance is built lazily and cached: a missing or weak secret fails
/// every AgentEngine call at first use with a clear config error.
fn sidecar_token_service() -> Result<&'static TokenService, String> {
    static SERVICE: OnceLock<Result<TokenService, String>> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let secret = std::env::var("PIREN_SIDECAR_SECRET")
                .map_err(|_| config_error("环境变量 PIREN_SIDECAR_SECRET 未设置"))?;
            let config =
                SecurityConfig::new(secret, SIDECAR_ISSUER, SIDECAR_AUDIENCE, SIDECAR_TOKEN_TTL)
                    .map_err(|error| {
                        config_error(&format!("PIREN_SIDECAR_SECRET 无效：{error}"))
                    })?;
            Ok(TokenService::new(config))
        })
        .as_ref()
        .map_err(|error| error.clone())
}

fn mint_sidecar_token(
    service: &TokenService,
    user_id: &str,
    tenant_id: Option<String>,
) -> Result<String, String> {
    let user = CurrentUser {
        user_id: user_id.to_string(),
        username: user_id.to_string(),
        tenant_id,
        role_codes: Vec::new(),
        permissions: PermissionSet::new([]),
        data_scope: DataScope::SelfOnly,
    };
    service
        .issue_access_token(user)
        .map_err(|_| config_error("AgentEngine 代理令牌签发失败"))
}

/// One sidecar turn. The engine owns the full conversation and its built-in
/// tools, so rust-toon only forwards the identity, the conversation system
/// prompt, and the latest user message per the fixed contract.
#[derive(Debug)]
struct SidecarTurn {
    conversation_key: String,
    user_id: String,
    tenant_id: Option<String>,
    system_message: String,
    user_content: String,
}

impl SidecarTurn {
    fn finish(
        user_id: Option<String>,
        tenant_id: Option<String>,
        conversation_id: Option<i64>,
        system_message: String,
        user_content: String,
    ) -> Result<Self, String> {
        let user_id = user_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| config_error("缺少已认证用户身份，请通过聊天接口调用 AgentEngine"))?;
        let conversation_id = conversation_id
            .ok_or_else(|| config_error("缺少会话标识，请通过聊天接口调用 AgentEngine"))?;
        Ok(Self {
            conversation_key: format!("u{user_id}-c{conversation_id}"),
            user_id,
            tenant_id,
            system_message,
            user_content,
        })
    }

    fn from_request(request: &ChatRequest) -> Result<Self, String> {
        // Only the latest user message is forwarded; the sidecar replays the
        // conversation history itself.
        let user_content = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .ok_or_else(|| config_error("AgentEngine 请求缺少用户消息"))?;
        Self::finish(
            request.user_id.clone(),
            request.tenant_id.clone(),
            request.conversation_id,
            request.system_message.clone().unwrap_or_default(),
            user_content,
        )
    }

    fn from_messages(messages: &[Value]) -> Result<Self, String> {
        // The factory tools API only carries an OpenAI-style message list, so
        // the system prompt and the latest user turn are recovered from it.
        let system_message = messages
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("system"))
            .and_then(|message| message.get("content").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let user_content = messages
            .iter()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|message| message.get("content").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        if user_content.is_empty() {
            return Err(config_error("AgentEngine 消息列表缺少用户消息"));
        }
        Self::finish(None, None, None, system_message, user_content)
    }
}

#[derive(Debug)]
enum ParsedEvent {
    Delta(String),
    Reasoning(String),
    Done(Value),
    Error(String),
    Ignored,
}

fn parse_sidecar_event(name: &str, data: &str) -> Result<ParsedEvent, String> {
    match name {
        "delta" | "reasoning" => {
            let value: Value = serde_json::from_str(data).map_err(|_| malformed_event(data))?;
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed_event(data))?;
            Ok(if name == "delta" {
                ParsedEvent::Delta(text.to_string())
            } else {
                ParsedEvent::Reasoning(text.to_string())
            })
        }
        "done" => {
            let value: Value = serde_json::from_str(data).map_err(|_| malformed_event(data))?;
            Ok(ParsedEvent::Done(value))
        }
        "error" => {
            let value: Value = serde_json::from_str(data).map_err(|_| malformed_event(data))?;
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("AgentEngine 返回错误")
                .to_string();
            Ok(ParsedEvent::Error(message))
        }
        _ => Ok(ParsedEvent::Ignored),
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_event<F, Fut>(
    name: &str,
    data: &str,
    content: &mut String,
    reasoning: &mut String,
    usage: &mut Value,
    finished: bool,
    on_delta: &mut F,
) -> Result<bool, String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    match parse_sidecar_event(name, data)? {
        ParsedEvent::Delta(text) => {
            content.push_str(&text);
            on_delta(text).await?;
        }
        ParsedEvent::Reasoning(text) => reasoning.push_str(&text),
        ParsedEvent::Done(value) => {
            if let Some(summary) = value.get("usage") {
                *usage = summary.clone();
            }
            return Ok(true);
        }
        ParsedEvent::Error(message) => {
            return Err(super::ProviderError {
                code: "AI_AGENT_ENGINE_ERROR".into(),
                category: "upstream".into(),
                message,
                status: None,
                response_data: None,
                retryable: false,
            }
            .encoded());
        }
        ParsedEvent::Ignored => {}
    }
    Ok(finished)
}

impl AgentEngineProvider {
    /// Drives one engine turn and folds the SSE event stream into a
    /// `ChatResponse`. Each `delta` text is forwarded to `on_delta` 1:1 for
    /// streaming callers; reasoning events accumulate onto the final response.
    async fn run_turn<F, Fut>(
        &self,
        config: &ModelConfig,
        turn: &SidecarTurn,
        mut on_delta: F,
    ) -> Result<ChatResponse, String>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let token = mint_sidecar_token(
            sidecar_token_service()?,
            &turn.user_id,
            turn.tenant_id.clone(),
        )?;
        let body = json!({
            "conversation_key": turn.conversation_key,
            "user_id": turn.user_id,
            "tenant_id": turn.tenant_id,
            "model": config.model,
            "system_message": turn.system_message,
            "messages": [{"role":"user","content":turn.user_content}],
        });
        let response = super::send_with_retry(
            super::http_client()
                .post(format!(
                    "{}{}",
                    sidecar_base_url().trim_end_matches('/'),
                    SIDECAR_TURN_PATH
                ))
                .bearer_auth(token)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(&body),
        )
        .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            return Err(super::upstream_error(
                status,
                &value,
                "AgentEngine 请求失败",
            ));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut usage = json!({});
        let mut finished = false;
        let mut event_name = String::new();
        let mut event_data = String::new();
        while let Some(chunk) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(
                &chunk.map_err(|error| super::transport_error(&error))?,
            ));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);
                if line.is_empty() {
                    finished = apply_event(
                        &event_name,
                        &event_data,
                        &mut content,
                        &mut reasoning,
                        &mut usage,
                        finished,
                        &mut on_delta,
                    )
                    .await?;
                    event_name.clear();
                    event_data.clear();
                    continue;
                }
                if let Some(name) = line.strip_prefix("event:") {
                    event_name = name.trim().to_string();
                } else if let Some(data) = line.strip_prefix("data:") {
                    if !event_data.is_empty() {
                        event_data.push('\n');
                    }
                    event_data.push_str(data.trim());
                }
            }
        }
        if !event_data.is_empty() {
            finished = apply_event(
                &event_name,
                &event_data,
                &mut content,
                &mut reasoning,
                &mut usage,
                finished,
                &mut on_delta,
            )
            .await?;
        }
        if !finished {
            return Err(stream_error(
                "AgentEngine 流在未完成的情况下结束",
                &content,
                true,
            ));
        }
        if content.is_empty() {
            return Err("AgentEngine 未返回文本".into());
        }
        Ok(ChatResponse {
            content,
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            usage,
        })
    }

    pub async fn chat_stream<F, Fut>(
        &self,
        config: &ModelConfig,
        request: &ChatRequest,
        on_delta: F,
    ) -> Result<ChatResponse, String>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let turn = SidecarTurn::from_request(request)?;
        self.run_turn(config, &turn, on_delta).await
    }

    /// rust-toon tool definitions never reach the sidecar: the engine runs
    /// its own built-in tools, so the factory tools arms reuse the plain
    /// engine call with the turn derived from the OpenAI-style message list.
    pub async fn chat_tools(
        &self,
        config: &ModelConfig,
        messages: Vec<Value>,
    ) -> Result<Value, String> {
        let turn = SidecarTurn::from_messages(&messages)?;
        let response = self
            .run_turn(config, &turn, |_| async { Ok::<(), String>(()) })
            .await?;
        Ok(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": response.content,
                    "reasoning_content": response.reasoning,
                },
                "finish_reason": "stop",
            }],
            "usage": response.usage,
        }))
    }

    pub async fn chat_tools_stream<F, Fut>(
        &self,
        config: &ModelConfig,
        messages: Vec<Value>,
        mut on_delta: F,
    ) -> Result<Value, String>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let turn = SidecarTurn::from_messages(&messages)?;
        let response = self
            .run_turn(config, &turn, |text| on_delta(json!({"content": text})))
            .await?;
        Ok(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": response.content,
                    "reasoning_content": response.reasoning,
                },
                "finish_reason": "stop",
            }],
            "usage": response.usage,
        }))
    }
}

#[async_trait]
impl ChatProvider for AgentEngineProvider {
    async fn chat(
        &self,
        config: &ModelConfig,
        request: &ChatRequest,
    ) -> Result<ChatResponse, String> {
        let turn = SidecarTurn::from_request(request)?;
        self.run_turn(config, &turn, |_| async { Ok::<(), String>(()) })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use futures_util::stream;

    const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn test_token_service() -> TokenService {
        TokenService::new(
            SecurityConfig::new(
                TEST_SECRET,
                SIDECAR_ISSUER,
                SIDECAR_AUDIENCE,
                SIDECAR_TOKEN_TTL,
            )
            .unwrap(),
        )
    }

    fn decode_claims(token: &str) -> Value {
        let payload = token.split('.').nth(1).expect("token payload segment");
        let bytes = URL_SAFE_NO_PAD.decode(payload).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn model_config(model: &str) -> ModelConfig {
        ModelConfig {
            id: 1,
            name: "agent".into(),
            key: "agent".into(),
            platform: "AgentEngine".into(),
            type_: "chat".into(),
            model: model.into(),
            api_key: String::new(),
            url: String::new(),
            status: 0,
            config: json!({}),
        }
    }

    fn chat_request(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![
                rust_toon_ai_api::ChatMessage {
                    role: "system".into(),
                    content: "ignored-knowledge-context".into(),
                },
                rust_toon_ai_api::ChatMessage {
                    role: "user".into(),
                    content: "earlier".into(),
                },
                rust_toon_ai_api::ChatMessage {
                    role: "assistant".into(),
                    content: "reply".into(),
                },
                rust_toon_ai_api::ChatMessage {
                    role: "user".into(),
                    content: "hello".into(),
                },
            ],
            temperature: None,
            max_tokens: None,
            conversation_id: Some(7),
            user_id: Some("42".into()),
            tenant_id: Some("t9".into()),
            system_message: Some("sys".into()),
        }
    }

    #[test]
    fn mints_proxy_token_with_fixed_issuer_audience_and_ttl() {
        let service = test_token_service();
        let token = mint_sidecar_token(&service, "42", Some("t9".into())).unwrap();
        let claims = decode_claims(&token);
        assert_eq!(claims["sub"], json!("42"));
        assert_eq!(claims["iss"], json!("rust-toon"));
        assert_eq!(claims["aud"], json!("piren-sidecar"));
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            300
        );
        assert_eq!(claims["user"]["user_id"], json!("42"));
        assert_eq!(claims["user"]["tenant_id"], json!("t9"));
        // The token verifies against the sidecar secret and audience.
        let verified = service.verify_access_token(&token).unwrap();
        assert_eq!(verified.sub, "42");
        assert_eq!(verified.user.tenant_id, Some("t9".to_string()));
    }

    #[test]
    fn builds_turn_from_request_using_only_the_last_user_message() {
        let turn = SidecarTurn::from_request(&chat_request("m")).unwrap();
        assert_eq!(turn.conversation_key, "u42-c7");
        assert_eq!(turn.user_id, "42");
        assert_eq!(turn.tenant_id, Some("t9".to_string()));
        assert_eq!(turn.system_message, "sys");
        assert_eq!(turn.user_content, "hello");
    }

    #[test]
    fn turn_requires_authenticated_identity() {
        let mut request = chat_request("m");
        request.user_id = None;
        assert!(
            SidecarTurn::from_request(&request)
                .unwrap_err()
                .contains("AI_AGENT_ENGINE_CONFIG")
        );
        let mut request = chat_request("m");
        request.conversation_id = None;
        assert!(SidecarTurn::from_request(&request).is_err());
        let mut request = chat_request("m");
        request.messages = vec![];
        assert!(SidecarTurn::from_request(&request).is_err());
    }

    #[test]
    fn parses_stream_events() {
        assert!(matches!(
            parse_sidecar_event("delta", r#"{"text":"a"}"#).unwrap(),
            ParsedEvent::Delta(text) if text == "a"
        ));
        assert!(matches!(
            parse_sidecar_event("reasoning", r#"{"text":"b"}"#).unwrap(),
            ParsedEvent::Reasoning(text) if text == "b"
        ));
        match parse_sidecar_event(
            "done",
            r#"{"session_id":"s","usage":{"input":1,"output":2}}"#,
        )
        .unwrap()
        {
            ParsedEvent::Done(value) => assert_eq!(value["usage"]["output"], json!(2)),
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(matches!(
            parse_sidecar_event("error", r#"{"message":"boom"}"#).unwrap(),
            ParsedEvent::Error(message) if message == "boom"
        ));
        assert!(matches!(
            parse_sidecar_event("unknown", "{}").unwrap(),
            ParsedEvent::Ignored
        ));
        let malformed = parse_sidecar_event("delta", "not-json").unwrap_err();
        assert!(malformed.contains("AI_AGENT_ENGINE_STREAM"));
        let missing_text = parse_sidecar_event("delta", r#"{"nope":1}"#).unwrap_err();
        assert!(missing_text.contains("AI_AGENT_ENGINE_STREAM"));
    }

    #[tokio::test]
    async fn adapter_handles_sse_success_error_and_http_failure() {
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::Response;
        use axum::{Router, routing::post};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        let app = Router::new().route(
            "/sidecar/v1/turn",
            post(move |headers: HeaderMap, body: String| {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .is_some_and(|value| value.starts_with("Bearer "))
                    );
                    let value: Value = serde_json::from_str(&body).unwrap();
                    match value["model"].as_str().unwrap_or_default() {
                        "reject" => Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .body(axum::body::Body::from(r#"{"message":"bad turn"}"#))
                            .unwrap(),
                        "engine-error" => sse(vec![Ok::<_, std::convert::Infallible>(
                            "event: error\ndata: {\"message\":\"engine died\"}\n\n",
                        )]),
                        "garbage" => sse(vec![Ok::<_, std::convert::Infallible>(
                            "event: delta\ndata: {broken\n\n",
                        )]),
                        "truncated" => sse(vec![Ok::<_, std::convert::Infallible>(
                            "event: delta\ndata: {\"text\":\"partial\"}\n\n",
                        )]),
                        _ => {
                            assert_eq!(value["conversation_key"], json!("u42-c7"));
                            assert_eq!(value["user_id"], json!("42"));
                            assert_eq!(value["tenant_id"], json!("t9"));
                            assert_eq!(value["system_message"], json!("sys"));
                            assert_eq!(
                                value["messages"],
                                json!([{"role":"user","content":"hello"}])
                            );
                            sse(vec![
                                Ok::<_, std::convert::Infallible>(
                                    "event: reasoning\ndata: {\"text\":\"think \"}\n\n",
                                ),
                                Ok("event: delta\ndata: {\"text\":\"Hel\"}\n\n"),
                                Ok("event: delta\ndata: {\"text\":\"lo\"}\n\n"),
                                Ok("event: done\ndata: {\"session_id\":\"s-1\",\"usage\":{\"input\":3,\"output\":2}}\n\n"),
                            ])
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // Safety: this test binary runs in a single process, no other test
        // reads or writes these two variables, and every env-using test in
        // this module uses identical values.
        unsafe {
            std::env::set_var("PIREN_SIDECAR_URL", format!("http://{address}"));
            std::env::set_var("PIREN_SIDECAR_SECRET", TEST_SECRET);
        }

        let provider = AgentEngineProvider;

        // Non-streaming chat folds the stream into one response.
        let response = ChatProvider::chat(
            &provider,
            &model_config("piren-agent"),
            &chat_request("piren-agent"),
        )
        .await
        .unwrap();
        assert_eq!(response.content, "Hello");
        assert_eq!(response.reasoning.as_deref(), Some("think "));
        assert_eq!(response.usage, json!({"input":3,"output":2}));

        // Streaming forwards each delta 1:1 and keeps the accumulated text.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_deltas = seen.clone();
        let streamed = provider
            .chat_stream(
                &model_config("piren-agent"),
                &chat_request("piren-agent"),
                move |text| {
                    let seen_deltas = seen_deltas.clone();
                    async move {
                        seen_deltas.lock().unwrap().push(text);
                        Ok::<(), String>(())
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["Hel".to_string(), "lo".to_string()]
        );
        assert_eq!(streamed.content, "Hello");
        assert_eq!(streamed.reasoning.as_deref(), Some("think "));

        // `error` events surface as encoded provider errors.
        let error = ChatProvider::chat(
            &provider,
            &model_config("engine-error"),
            &chat_request("engine-error"),
        )
        .await
        .unwrap_err();
        let details: crate::provider::ProviderError = serde_json::from_str(&error).unwrap();
        assert_eq!(details.code, "AI_AGENT_ENGINE_ERROR");
        assert_eq!(details.message, "engine died");

        // Non-2xx responses map to the shared upstream error shape.
        let rejected =
            ChatProvider::chat(&provider, &model_config("reject"), &chat_request("reject"))
                .await
                .unwrap_err();
        let details: crate::provider::ProviderError = serde_json::from_str(&rejected).unwrap();
        assert_eq!(details.code, "AI_UPSTREAM_HTTP_400");

        // Malformed stream payloads are reported, not silently dropped.
        let garbage = ChatProvider::chat(
            &provider,
            &model_config("garbage"),
            &chat_request("garbage"),
        )
        .await
        .unwrap_err();
        assert!(garbage.contains("AI_AGENT_ENGINE_STREAM"));

        // A stream that ends before `done` fails instead of returning partial text.
        let truncated = ChatProvider::chat(
            &provider,
            &model_config("truncated"),
            &chat_request("truncated"),
        )
        .await
        .unwrap_err();
        assert!(truncated.contains("AI_AGENT_ENGINE_STREAM"));
    }

    fn sse(
        events: Vec<Result<&'static str, std::convert::Infallible>>,
    ) -> axum::response::Response {
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream::iter(events)))
            .unwrap()
    }
}
