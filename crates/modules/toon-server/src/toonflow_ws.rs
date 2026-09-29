use crate::{ToonState, shared::require, toonflow_agent_history, toonflow_agents};
use axum::{
    extract::{
        Extension, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use rust_toon_framework_web::AppError;
use rust_toon_framework_security::{AuthenticatedSession, CurrentUser};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use tracing::warn;

// ---------------------------------------------------------------------------
// Connection auth params (query string)
// ---------------------------------------------------------------------------
#[derive(Deserialize)]
pub(crate) struct WsParams {
    #[serde(rename = "isolationKey")]
    isolation_key: String,
    #[serde(rename = "projectId")]
    project_id: i64,
    #[serde(rename = "scriptId")]
    script_id: Option<i64>,
    #[serde(default, rename = "historyMode")]
    history_mode: Option<String>,
    #[serde(default)]
    eio: Option<u8>,
    #[serde(default)]
    transport: Option<String>,
}

// ---------------------------------------------------------------------------
// Incoming client messages
// ---------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClientMessage {
    #[serde(rename = "chat")]
    Chat { content: String },
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "updateThinkConfig")]
    ThinkConfig {
        think: bool,
        #[serde(rename = "thinkLevel")]
        think_level: i32,
    },
    #[serde(rename = "history")]
    History {
        #[serde(rename = "beforeId")]
        before_id: Option<i64>,
        limit: Option<usize>,
    },
    #[serde(rename = "updateContext")]
    UpdateContext {
        #[serde(rename = "isolationKey")]
        isolation_key: String,
        #[serde(rename = "projectId")]
        project_id: i64,
        #[serde(rename = "scriptId")]
        script_id: Option<i64>,
    },
}

// ---------------------------------------------------------------------------
// Outgoing event builders (mirrors Toonflow-app socket protocol)
// ---------------------------------------------------------------------------

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn normalized_error(error: &str) -> Value {
    let Ok(mut details) = serde_json::from_str::<Value>(error) else {
        return json!({
            "error": error,
            "errorCode": "AGENT_EXECUTION_FAILED",
            "errorCategory": "agent"
        });
    };
    let message = details
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("AI 服务请求失败")
        .to_string();
    let code = details
        .get("code")
        .cloned()
        .unwrap_or_else(|| json!("AI_REQUEST_FAILED"));
    let category = details
        .get("category")
        .cloned()
        .unwrap_or_else(|| json!("unknown"));
    let Some(object) = details.as_object_mut() else {
        return json!({ "error": message, "errorCode": code, "errorCategory": category });
    };
    object.insert("error".into(), json!(message));
    object.insert("errorCode".into(), code);
    object.insert("errorCategory".into(), category);
    details
}

fn friendly_error(error: &str) -> String {
    normalized_error(error)
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or(error)
        .to_string()
}

#[cfg(test)]
mod error_tests {
    use super::{friendly_error, normalized_error};
    use serde_json::json;

    #[test]
    fn maps_structured_ai_errors_without_breaking_legacy_error_field() {
        let source = json!({
            "code":"AI_UPSTREAM_HTTP_429",
            "category":"upstream",
            "message":"AI 服务请求过于频繁，请稍后重试",
            "status":429,
            "responseData":{"summary":"quota exceeded"},
            "retryable":true
        })
        .to_string();
        let result = normalized_error(&source);
        assert_eq!(result["error"], json!("AI 服务请求过于频繁，请稍后重试"));
        assert_eq!(result["errorCode"], json!("AI_UPSTREAM_HTTP_429"));
        assert_eq!(result["status"], json!(429));
        assert_eq!(friendly_error(&source), "AI 服务请求过于频繁，请稍后重试");
    }

    #[test]
    fn keeps_plain_agent_errors_compatible() {
        let result = normalized_error("工具执行失败");
        assert_eq!(result["error"], json!("工具执行失败"));
        assert_eq!(result["errorCategory"], json!("agent"));
    }
}

/// Shared sender that agent execution uses to push events to the WebSocket.
#[derive(Clone)]
pub struct WsEmitter {
    tx: tokio::sync::mpsc::UnboundedSender<Message>,
    socket_io: bool,
}

impl WsEmitter {
    fn send_json(&self, value: &Value) {
        let text = if self.socket_io {
            format!("42[\"toonflow\",{}]", value)
        } else {
            value.to_string()
        };
        let _ = self.tx.send(Message::Text(text.into()));
    }

    /// Create a new message bubble. Returns (message_id, datetime).
    pub fn new_message(&self, name: &str, role: &str) -> (String, String) {
        let id = uuid();
        let datetime = chrono::Utc::now().to_rfc3339();
        self.send_json(&json!({
            "event": "message",
            "data": {
                "id": id,
                "role": role,
                "name": name,
                "status": "pending",
                "datetime": datetime,
                "content": []
            }
        }));
        (id, datetime)
    }

    /// Update a message's status.
    pub fn update_message(&self, id: &str, status: &str, error: Option<&str>) {
        let mut payload = json!({
            "event": "message:update",
            "data": { "id": id, "status": status }
        });
        if let Some(err) = error {
            payload["data"]["ext"] = normalized_error(err);
        }
        self.send_json(&payload);
    }

    /// Add a content block to a message. Returns content_id.
    pub fn add_content(&self, message_id: &str, content_type: &str, data: &Value) -> String {
        let content_id = uuid();
        let status = match content_type {
            "thinking" | "toolcall" => "pending",
            _ => "pending",
        };
        self.send_json(&json!({
            "event": "content:add",
            "data": {
                "messageId": message_id,
                "content": {
                    "type": content_type,
                    "id": content_id,
                    "data": data,
                    "status": status
                }
            }
        }));
        content_id
    }

    /// Stream-update a content block (append or merge).
    pub fn update_content(
        &self,
        message_id: &str,
        content_id: &str,
        content_type: &str,
        data: &Value,
        strategy: &str,
        status: &str,
    ) {
        self.send_json(&json!({
            "event": "content:update",
            "data": {
                "messageId": message_id,
                "contentId": content_id,
                "type": content_type,
                "data": data,
                "strategy": strategy,
                "status": status
            }
        }));
    }

    /// Convenience: append text delta to a text content block.
    pub fn text_delta(&self, message_id: &str, content_id: &str, text: &str) {
        self.update_content(
            message_id,
            content_id,
            "text",
            &json!(text),
            "append",
            "streaming",
        );
    }

    /// Convenience: complete a text content block.
    pub fn text_complete(&self, message_id: &str, content_id: &str) {
        self.update_content(
            message_id,
            content_id,
            "text",
            &json!(null),
            "append",
            "complete",
        );
    }

    /// Convenience: add and stream a toolcall content block. Returns content_id.
    pub fn tool_call_start(&self, message_id: &str, tool_call_id: &str, tool_name: &str) -> String {
        self.add_content(
            message_id,
            "toolcall",
            &json!({
                "toolCallId": tool_call_id,
                "toolCallName": tool_name,
                "parentMessageId": message_id
            }),
        )
    }

    /// Convenience: append tool call args chunk.
    pub fn tool_call_args(
        &self,
        message_id: &str,
        content_id: &str,
        tool_call_id: &str,
        chunk: &str,
    ) {
        self.update_content(
            message_id,
            content_id,
            "toolcall",
            &json!({ "toolCallId": tool_call_id, "args": chunk }),
            "append",
            "streaming",
        );
    }

    /// Convenience: finalize a tool call with success.
    pub fn tool_call_success(
        &self,
        message_id: &str,
        content_id: &str,
        tool_call_id: &str,
        result: &str,
    ) {
        self.update_content(
            message_id,
            content_id,
            "toolcall",
            &json!({ "toolCallId": tool_call_id, "result": result }),
            "merge",
            "complete",
        );
    }

    /// Convenience: finalize a tool call with error.
    pub fn tool_call_error(
        &self,
        message_id: &str,
        content_id: &str,
        tool_call_id: &str,
        error: &str,
    ) {
        self.update_content(
            message_id,
            content_id,
            "toolcall",
            &json!({ "toolCallId": tool_call_id, "result": error }),
            "merge",
            "error",
        );
    }
}

// ---------------------------------------------------------------------------
// WebSocket upgrade handler
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct AgentPath {
    agent: String,
}

pub async fn ws_handler(
    user: CurrentUser,
    Extension(session): Extension<AuthenticatedSession>,
    ws: WebSocketUpgrade,
    State(state): State<ToonState>,
    Path(path): Path<AgentPath>,
    Query(params): Query<WsParams>,
) -> Result<impl IntoResponse, AppError> {
    let agent_type = path.agent.as_str();

    // Validate agent type
    let _ = toonflow_agents::agent_key_for(agent_type).map_err(|e| AppError::bad_request(&e))?;

    require(&user, "toon:project:read")?;
    toonflow_agents::authorize_context(&state, &user, agent_type, &params.isolation_key, params.project_id, params.script_id).await?;
    let agent_type = agent_type.to_owned();

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, state, params, agent_type, session)
    }))
}

async fn handle_socket(
    socket: WebSocket,
    state: ToonState,
    mut params: WsParams,
    agent_type: String,
    session: AuthenticatedSession,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let socket_io = params.eio.is_some() || params.transport.as_deref() == Some("websocket");

    // Create a channel for the emitter
    let (emitter_tx, mut emitter_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // Spawn a task to forward emitter messages to the WebSocket
    let forward_handle = tokio::spawn(async move {
        while let Some(msg) = emitter_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let emitter = WsEmitter {
        tx: emitter_tx,
        socket_io,
    };
    if socket_io {
        let _ = emitter.tx.send(Message::Text(
            r#"0{"sid":"toonflow","upgrades":[],"pingInterval":30000,"pingTimeout":20000}"#.into(),
        ));
        let _ = emitter.tx.send(Message::Text("40".into()));
    }
    let heartbeat_tx = emitter.tx.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await;
        loop {
            interval.tick().await;
            let heartbeat = if socket_io {
                Message::Text("2".into())
            } else {
                Message::Ping(Vec::new().into())
            };
            if heartbeat_tx.send(heartbeat).is_err() {
                break;
            }
        }
    });

    // Resolve agent key
    let agent_key = match toonflow_agents::agent_key_for(&agent_type) {
        Ok(k) => k.to_string(),
        Err(_) => return,
    };

    // Think config defaults
    let mut think = true;
    let mut think_level: i32 = 1;

    // Abort controller
    let (mut abort_tx, _abort_rx) = watch::channel(false);
    let mut active_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_message: Option<(String, String)> = None;

    // New clients restore one bounded history page in a single frame. Keep the
    // legacy event stream for older deployed frontends during rolling updates.
    let batch_history = params.history_mode.as_deref() == Some("batch");
    let has_user_msg = if batch_history {
        match toonflow_agent_history::load_history_page(
            &state.pool,
            &agent_type,
            &params.isolation_key,
            None,
            None,
            false,
        )
        .await
        {
            Ok(page) => {
                let has_history = !page.messages.is_empty();
                emitter.send_json(&json!({ "event": "history", "data": page }));
                has_history
            }
            Err(error) => {
                warn!(%error, "failed to restore agent history page");
                emitter.send_json(&json!({
                    "event": "history:error",
                    "data": { "message": "历史会话加载失败" }
                }));
                false
            }
        }
    } else {
        let memories: Vec<toonflow_agents::MemoryRow> = sqlx::query_as(
            "SELECT id,role,content,memory_type,create_time FROM toonflow.agent_memories WHERE agent_type=$1 AND isolation_key=$2 AND memory_type='message' ORDER BY create_time",
        )
        .bind(&agent_type)
        .bind(&params.isolation_key)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

        for mem in &memories {
            let role = if mem.role.starts_with("user") {
                "user"
            } else {
                "assistant"
            };
            let name = if role == "user" {
                "你"
            } else if mem.role.contains("execution:storySkeleton")
                || mem.role.contains("execution:adaptationStrategy")
                || mem.role.contains("execution:script")
            {
                "编剧"
            } else if mem.role.contains("supervision") {
                "编辑"
            } else {
                "统筹"
            };
            let (mid, _dt) = emitter.new_message(name, role);
            let cid = emitter.add_content(&mid, "text", &json!(""));
            emitter.text_delta(&mid, &cid, &mem.content);
            emitter.text_complete(&mid, &cid);
            emitter.update_message(&mid, "complete", None);
        }
        memories
            .iter()
            .any(|memory| memory.role.starts_with("user"))
    };

    // If no history, send proactive greeting
    if !has_user_msg {
        let (greeting_id, _dt) = emitter.new_message("统筹", "assistant");
        let greeting_cid = emitter.add_content(&greeting_id, "text", &json!(""));
        let greeting = if agent_type == "scriptAgent" {
            "你好！我是剧本创作 Agent，我已读取当前项目的类型、画风等配置。\n\n需要我为你生成剧本吗？"
        } else {
            "你好！我是生产制作 Agent。我可以帮你进行分镜设计、视频生成等制片工作。\n\n请选择剧本后告诉我你的需求。"
        };
        emitter.text_delta(&greeting_id, &greeting_cid, greeting);
        emitter.text_complete(&greeting_id, &greeting_cid);
        emitter.update_message(&greeting_id, "complete", None);
    }

    // Revalidate idle connections too; each command also checks current access.
    let mut session_check = tokio::time::interval(std::time::Duration::from_secs(15));
    session_check.tick().await;
    let mut last_message = tokio::time::Instant::now();
    loop {
        let msg_result = tokio::select! {
            _ = session_check.tick() => {
                let Ok(user) = session.validate().await else { break; };
                if require(&user, "toon:project:read").is_err()
                    || (active_task.as_ref().is_some_and(|task| !task.is_finished()) && require(&user, "toon:project:update").is_err())
                    || toonflow_agents::authorize_context(&state, &user, &agent_type, &params.isolation_key, params.project_id, params.script_id).await.is_err() {
                    break;
                }
                continue;
            },
            _ = tokio::time::sleep_until(last_message + std::time::Duration::from_secs(90)) => break,
            message = ws_rx.next() => match message { Some(result) => result, None => break },
        };
        last_message = tokio::time::Instant::now();
        let msg = match msg_result {
            Ok(m) => m,
            Err(_) => break,
        };

        let mut text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        if socket_io {
            let raw = text.as_str();
            if raw == "2" {
                let _ = emitter.tx.send(Message::Text("3".into()));
                continue;
            }
            if let Some(payload) = raw.strip_prefix("42") {
                let packet: Value = match serde_json::from_str(payload) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                text = packet
                    .as_array()
                    .and_then(|items| items.get(1))
                    .cloned()
                    .unwrap_or(Value::Null)
                    .to_string()
                    .into();
            } else {
                continue;
            }
        }

        // Parse client message
        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => {
                warn!("invalid client ws message");
                continue;
            }
        };

        let Ok(user) = session.validate().await else { break; };
        let permission = if matches!(&client_msg, ClientMessage::Chat { .. } | ClientMessage::Stop) {
            "toon:project:update"
        } else { "toon:project:read" };
        if require(&user, permission).is_err()
            || toonflow_agents::authorize_context(&state, &user, &agent_type, &params.isolation_key, params.project_id, params.script_id).await.is_err() {
            break;
        }
        match client_msg {
            ClientMessage::Chat { content } => {
                if content.trim().is_empty() {
                    continue;
                }

                // Create user message bubble
                let (user_mid, _dt) = emitter.new_message("你", "user");
                let user_cid = emitter.add_content(&user_mid, "text", &json!(""));
                emitter.text_delta(&user_mid, &user_cid, &content);
                emitter.text_complete(&user_mid, &user_cid);
                emitter.update_message(&user_mid, "complete", None);

                // Create assistant message bubble
                let agent_name = "统筹";
                let (msg_id, _msg_dt) = emitter.new_message(agent_name, "assistant");
                let text_cid = emitter.add_content(&msg_id, "text", &json!(""));

                // Abort an in-flight HTTP/LLM request immediately. The watch
                // signal remains useful for cooperative cancellation inside
                // tool loops, while JoinHandle::abort handles the await that
                // is currently blocked on the upstream request.
                if let Some(task) = active_task.take() {
                    task.abort();
                }
                if let Some((old_message, old_content)) = active_message.take() {
                    emitter.update_message(
                        &old_message,
                        "canceled",
                        Some("用户开始了新的 Agent 运行"),
                    );
                    emitter.text_complete(&old_message, &old_content);
                }

                // Build the agent request
                let request = toonflow_agents::ChatRequest {
                    agent_type: agent_type.clone(),
                    isolation_key: params.isolation_key.clone(),
                    project_id: params.project_id,
                    script_id: params.script_id,
                    content: content.clone(),
                    think,
                    think_level,
                };

                // Create fresh abort channel for this run
                // Cancel the previous run, but keep this WebSocket session
                // alive so the user can immediately submit another message.
                let _ = abort_tx.send(true);
                let (new_abort_tx, new_abort_rx) = watch::channel(false);
                abort_tx = new_abort_tx;

                // Spawn agent execution
                let exec_emitter = emitter.clone();
                let exec_state = state.clone();
                let exec_agent_key = agent_key.clone();
                let exec_msg_id = msg_id.clone();
                let exec_text_cid = text_cid.clone();
                let exec_abort_rx = new_abort_rx;

                active_task = Some(tokio::spawn(async move {
                    let result = toonflow_agents::run_with_emitter(
                        &exec_state,
                        &request,
                        &exec_agent_key,
                        &exec_emitter,
                        &exec_msg_id,
                        &exec_text_cid,
                        exec_abort_rx,
                    )
                    .await;

                    match result {
                        Ok(_) => {
                            exec_emitter.text_complete(&exec_msg_id, &exec_text_cid);
                            exec_emitter.update_message(&exec_msg_id, "complete", None);
                        }
                        Err(error) => {
                            let friendly = friendly_error(&error);
                            exec_emitter.text_delta(
                                &exec_msg_id,
                                &exec_text_cid,
                                &format!("\n\n错误：{friendly}"),
                            );
                            exec_emitter.text_complete(&exec_msg_id, &exec_text_cid);
                            exec_emitter.update_message(&exec_msg_id, "error", Some(&error));
                        }
                    }
                }));
                active_message = Some((msg_id, text_cid));
            }

            ClientMessage::Stop => {
                let _ = abort_tx.send(true);
                if let Some(task) = active_task.take() {
                    task.abort();
                }
                if let Some((message_id, content_id)) = active_message.take() {
                    emitter.text_complete(&message_id, &content_id);
                    emitter.update_message(&message_id, "canceled", Some("用户已中止"));
                }
            }

            ClientMessage::History { before_id, limit } => {
                if !batch_history {
                    continue;
                }
                match toonflow_agent_history::load_history_page(
                    &state.pool,
                    &agent_type,
                    &params.isolation_key,
                    before_id,
                    limit,
                    true,
                )
                .await
                {
                    Ok(page) => {
                        emitter.send_json(&json!({ "event": "history", "data": page }));
                    }
                    Err(error) => {
                        warn!(%error, "failed to load older agent history");
                        emitter.send_json(&json!({
                            "event": "history:error",
                            "data": { "message": "更早的会话加载失败" }
                        }));
                    }
                }
            }

            ClientMessage::ThinkConfig {
                think: t,
                think_level: tl,
            } => {
                think = t;
                think_level = tl.clamp(0, 3);
            }
            ClientMessage::UpdateContext {
                isolation_key,
                project_id,
                script_id,
            } => {
                if toonflow_agents::authorize_context(&state, &user, &agent_type, &isolation_key, project_id, script_id).await.is_err() {
                    emitter.send_json(&json!({"event":"updateContext:ack","data":{"success":false,"message":"无权访问该 Agent 上下文"}}));
                    continue;
                }
                let _ = abort_tx.send(true);
                if let Some(task) = active_task.take() { task.abort(); }
                active_message = None;
                params.isolation_key = isolation_key;
                params.project_id = project_id;
                params.script_id = script_id;
                emitter.send_json(&json!({"event":"updateContext:ack","data":{"success":true}}));
            }
        }
    }

    // Cleanup
    let _ = abort_tx.send(true);
    if let Some(task) = active_task { task.abort(); }
    forward_handle.abort();
    heartbeat_handle.abort();
}
