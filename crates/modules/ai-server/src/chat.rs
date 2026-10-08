use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{Response, header},
    routing::{delete, get, post, put},
};
use futures_util::stream;
use rust_toon_ai_api::{ChatMessage, ChatRequest, ChatResponse};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::mpsc;

use crate::{AiState, require};

pub fn routes() -> Router<AiState> {
    Router::new()
        .route("/ai/chat/conversation/create-my", post(create_conversation))
        .route("/ai/chat/conversation/update-my", put(update_conversation))
        .route("/ai/chat/conversation/my-list", get(my_conversations))
        .route("/ai/chat/conversation/get-my", get(get_conversation))
        .route(
            "/ai/chat/conversation/delete-my",
            delete(delete_conversation),
        )
        .route(
            "/ai/chat/conversation/delete-by-unpinned",
            delete(delete_unpinned),
        )
        .route("/ai/chat/conversation/page", get(conversation_page))
        .route(
            "/ai/chat/conversation/delete-by-admin",
            delete(delete_conversation_admin),
        )
        .route("/ai/chat/message/list-by-conversation-id", get(messages))
        .route("/ai/chat/message/send", post(send))
        .route("/ai/chat/message/send-stream", post(send_stream))
        .route("/ai/chat/message/delete", delete(delete_message))
        .route(
            "/ai/chat/message/delete-by-conversation-id",
            delete(delete_messages),
        )
        .route("/ai/chat/message/page", get(message_page))
        .route(
            "/ai/chat/message/delete-by-admin",
            delete(delete_message_admin),
        )
}

fn id() -> i64 {
    chrono::Utc::now().timestamp_micros()
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveConversation {
    id: Option<i64>,
    title: Option<String>,
    pinned: Option<bool>,
    role_id: Option<i64>,
    model_id: Option<i64>,
    temperature: Option<f64>,
    max_tokens: Option<i32>,
    max_contexts: Option<i32>,
    system_message: Option<String>,
    #[serde(default)]
    tool_ids: Vec<i64>,
    #[serde(default)]
    knowledge_ids: Vec<i64>,
}
#[derive(Deserialize)]
struct Id {
    id: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConversationId {
    conversation_id: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageQuery {
    page_no: Option<i64>,
    page_size: Option<i64>,
    user_id: Option<String>,
    title: Option<String>,
    conversation_id: Option<i64>,
}
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SendRequest {
    conversation_id: i64,
    content: String,
    use_context: Option<bool>,
    use_search: Option<bool>,
    #[serde(default)]
    attachment_urls: Vec<String>,
}

async fn create_conversation(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(v): Json<SaveConversation>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    let id = id();
    let time = now();
    let role = if let Some(role_id) = v.role_id {
        sqlx::query("SELECT model_id,name,system_message,knowledge_ids,tool_ids FROM ai.chat_roles WHERE id=$1 AND status=1 AND (public_status=true OR user_id=$2)")
            .bind(role_id).bind(&user.user_id).fetch_optional(&state.pool).await
            .map_err(|_|AppError::internal("读取聊天角色失败"))?
    } else {
        None
    };
    if v.role_id.is_some() && role.is_none() {
        return Err(AppError::not_found("聊天角色不存在或不可用"));
    }
    let model_id = match v
        .model_id
        .or_else(|| role.as_ref().map(|r| r.get("model_id")))
    {
        Some(id) => id,
        None => sqlx::query_scalar(
            "SELECT id FROM ai.model_configs WHERE type='chat' AND status=0 ORDER BY id LIMIT 1",
        )
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| AppError::internal("读取聊天模型失败"))?
        .ok_or_else(|| AppError::bad_request("请先配置启用的聊天模型"))?,
    };
    let title = v
        .title
        .filter(|x| !x.trim().is_empty())
        .or_else(|| role.as_ref().map(|r| r.get("name")))
        .unwrap_or_else(|| "新对话".into());
    let system_message = v.system_message.or_else(|| {
        role.as_ref()
            .map(|r| r.get::<String, _>("system_message"))
            .filter(|x| !x.is_empty())
    });
    let tool_ids = if v.tool_ids.is_empty() {
        role.as_ref().map(|r| r.get("tool_ids")).unwrap_or_default()
    } else {
        v.tool_ids
    };
    let knowledge_ids = if v.knowledge_ids.is_empty() {
        role.as_ref()
            .map(|r| r.get("knowledge_ids"))
            .unwrap_or_default()
    } else {
        v.knowledge_ids
    };
    sqlx::query("INSERT INTO ai.chat_conversations(id,user_id,title,pinned,role_id,model_id,temperature,max_tokens,max_contexts,system_message,tool_ids,knowledge_ids,create_time,update_time) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$13)").bind(id).bind(&user.user_id).bind(title).bind(v.pinned.unwrap_or(false)).bind(v.role_id).bind(model_id).bind(v.temperature.unwrap_or(0.7)).bind(v.max_tokens.unwrap_or(4096)).bind(v.max_contexts.unwrap_or(20)).bind(system_message).bind(tool_ids).bind(knowledge_ids).bind(time).execute(&state.pool).await.map_err(|e|AppError::bad_request(format!("创建对话失败: {e}")))?;
    Ok(Json(ApiResponse::new(id)))
}
async fn update_conversation(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(v): Json<SaveConversation>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    let id =
        v.id.ok_or_else(|| AppError::bad_request("id is required"))?;
    let result=sqlx::query("UPDATE ai.chat_conversations SET title=COALESCE($3,title),pinned=COALESCE($4,pinned),role_id=COALESCE($5,role_id),model_id=COALESCE($6,model_id),temperature=COALESCE($7,temperature),max_tokens=COALESCE($8,max_tokens),max_contexts=COALESCE($9,max_contexts),system_message=COALESCE($10,system_message),tool_ids=CASE WHEN cardinality($11::bigint[])>0 THEN $11 ELSE tool_ids END,knowledge_ids=CASE WHEN cardinality($12::bigint[])>0 THEN $12 ELSE knowledge_ids END,update_time=$13 WHERE id=$1 AND user_id=$2").bind(id).bind(&user.user_id).bind(v.title).bind(v.pinned).bind(v.role_id).bind(v.model_id).bind(v.temperature).bind(v.max_tokens).bind(v.max_contexts).bind(v.system_message).bind(v.tool_ids).bind(v.knowledge_ids).bind(now()).execute(&state.pool).await.map_err(|_|AppError::internal("更新对话失败"))?;
    Ok(Json(ApiResponse::new(result.rows_affected() > 0)))
}

fn conversation(row: sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<i64,_>("id"),"userId":row.get::<String,_>("user_id"),"title":row.get::<String,_>("title"),"pinned":row.get::<bool,_>("pinned"),"roleId":row.get::<Option<i64>,_>("role_id"),"modelId":row.get::<i64,_>("model_id"),"model":row.get::<String,_>("model"),"modelName":row.get::<String,_>("model_name"),"temperature":row.get::<f64,_>("temperature"),"maxTokens":row.get::<i32,_>("max_tokens"),"maxContexts":row.get::<i32,_>("max_contexts"),"systemMessage":row.get::<Option<String>,_>("system_message"),"toolIds":row.get::<Vec<i64>,_>("tool_ids"),"knowledgeIds":row.get::<Vec<i64>,_>("knowledge_ids"),"createTime":row.get::<i64,_>("create_time")})
}
const CONVERSATION_SELECT: &str = "SELECT c.id,c.user_id,c.title,c.pinned,c.role_id,c.model_id,m.model,m.name model_name,c.temperature,c.max_tokens,c.max_contexts,c.system_message,c.tool_ids,c.knowledge_ids,c.create_time FROM ai.chat_conversations c JOIN ai.model_configs m ON m.id=c.model_id";
async fn my_conversations(
    user: CurrentUser,
    State(state): State<AiState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let rows = sqlx::query(&format!(
        "{CONVERSATION_SELECT} WHERE c.user_id=$1 ORDER BY c.pinned DESC,c.update_time DESC"
    ))
    .bind(&user.user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("读取对话失败"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter().map(conversation).collect(),
    )))
}
async fn get_conversation(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(v): Query<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let row = sqlx::query(&format!(
        "{CONVERSATION_SELECT} WHERE c.id=$1 AND c.user_id=$2"
    ))
    .bind(v.id)
    .bind(&user.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("读取对话失败"))?
    .ok_or_else(|| AppError::not_found("对话不存在"))?;
    Ok(Json(ApiResponse::new(conversation(row))))
}
async fn delete_conversation(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(v): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    let r = sqlx::query("DELETE FROM ai.chat_conversations WHERE id=$1 AND user_id=$2")
        .bind(v.id)
        .bind(&user.user_id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("删除对话失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn delete_unpinned(
    user: CurrentUser,
    State(state): State<AiState>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    sqlx::query("DELETE FROM ai.chat_conversations WHERE user_id=$1 AND pinned=false")
        .bind(&user.user_id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("删除对话失败"))?;
    Ok(Json(ApiResponse::new(true)))
}

fn message(row: &sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<i64,_>("id"),"conversationId":row.get::<i64,_>("conversation_id"),"type":row.get::<String,_>("type"),"userId":row.get::<String,_>("user_id"),"modelId":row.get::<Option<i64>,_>("model_id"),"content":row.get::<String,_>("content"),"reasoningContent":row.get::<Option<String>,_>("reasoning_content"),"tokens":row.get::<i32,_>("tokens"),"segmentIds":row.get::<Vec<i64>,_>("segment_ids"),"knowledgeStatus":row.get::<Value,_>("knowledge_status"),"attachmentUrls":row.get::<Vec<String>,_>("attachment_urls"),"createTime":row.get::<i64,_>("create_time")})
}
async fn messages(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(v): Query<ConversationId>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let rows=sqlx::query("SELECT m.* FROM ai.chat_messages m JOIN ai.chat_conversations c ON c.id=m.conversation_id WHERE m.conversation_id=$1 AND c.user_id=$2 ORDER BY m.id").bind(v.conversation_id).bind(&user.user_id).fetch_all(&state.pool).await.map_err(|_|AppError::internal("读取消息失败"))?;
    Ok(Json(ApiResponse::new(rows.iter().map(message).collect())))
}

async fn load_request(
    state: &AiState,
    user: &CurrentUser,
    v: &SendRequest,
) -> Result<(i64, i64, ChatRequest, Value, Vec<i64>, Value), AppError> {
    if v.content.trim().is_empty() {
        return Err(AppError::bad_request("聊天内容不能为空"));
    }
    let c=sqlx::query("SELECT model_id,temperature,max_tokens,max_contexts,system_message,tool_ids,knowledge_ids FROM ai.chat_conversations WHERE id=$1 AND user_id=$2").bind(v.conversation_id).bind(&user.user_id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("读取对话失败"))?.ok_or_else(||AppError::not_found("对话不存在"))?;
    let send_id = id();
    let receive_id = send_id + 1;
    let time = now();
    let model_id: i64 = c.get("model_id");
    sqlx::query("INSERT INTO ai.chat_messages(id,conversation_id,user_id,type,model_id,content,attachment_urls,create_time) VALUES($1,$2,$3,'user',$4,$5,$6,$7),($8,$2,$3,'assistant',$4,'','{}',$7)").bind(send_id).bind(v.conversation_id).bind(&user.user_id).bind(model_id).bind(&v.content).bind(&v.attachment_urls).bind(time).bind(receive_id).execute(&state.pool).await.map_err(|_|AppError::internal("保存消息失败"))?;
    let mut list = Vec::new();
    if let Some(system) = c
        .get::<Option<String>, _>("system_message")
        .filter(|x| !x.is_empty())
    {
        list.push(ChatMessage {
            role: "system".into(),
            content: system,
        })
    }
    if v.use_context.unwrap_or(true) {
        let max = c.get::<i32, _>("max_contexts").max(0) as i64;
        let rows=sqlx::query("SELECT type,content FROM ai.chat_messages WHERE conversation_id=$1 AND id<$2 AND type IN ('user','assistant') ORDER BY id DESC LIMIT $3").bind(v.conversation_id).bind(send_id).bind(max).fetch_all(&state.pool).await.map_err(|_|AppError::internal("读取上下文失败"))?;
        for row in rows.into_iter().rev() {
            list.push(ChatMessage {
                role: row.get("type"),
                content: row.get("content"),
            })
        }
    }
    let knowledge_ids = c.get::<Vec<i64>, _>("knowledge_ids");
    let mut knowledge_status = json!({"state":"disabled","segmentCount":0});
    if v.use_search.unwrap_or(true) && !knowledge_ids.is_empty() {
        let segments = crate::knowledge::retrieve(state, &knowledge_ids, &v.content).await?;
        knowledge_status = json!({"state":if segments.is_empty() { "no_results" } else { "ready" },"knowledgeCount":knowledge_ids.len(),"segmentCount":segments.len()});
        sqlx::query("UPDATE ai.chat_messages SET knowledge_status=$2 WHERE id=$1")
            .bind(receive_id)
            .bind(&knowledge_status)
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("保存知识检索状态失败"))?;
        if !segments.is_empty() {
            let segment_ids = segments
                .iter()
                .filter_map(|item| item["id"].as_i64())
                .collect::<Vec<_>>();
            let context = segments
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    format!(
                        "[知识片段 {}｜{}]\n{}",
                        index + 1,
                        item["documentName"].as_str().unwrap_or("未知文档"),
                        item["content"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            list.push(ChatMessage { role: "system".into(), content: format!("以下是与用户问题相关的知识库资料。请优先依据资料回答；资料不足时明确说明，不要编造。\n\n{context}") });
            sqlx::query("UPDATE ai.chat_messages SET segment_ids=$2 WHERE id=$1")
                .bind(receive_id)
                .bind(segment_ids)
                .execute(&state.pool)
                .await
                .map_err(|_| AppError::internal("保存知识引用失败"))?;
        }
    }
    list.push(ChatMessage {
        role: "user".into(),
        content: v.content.clone(),
    });
    let send = json!({"id":send_id,"conversationId":v.conversation_id,"type":"user","userId":user.user_id,"modelId":model_id,"content":v.content,"attachmentUrls":v.attachment_urls,"createTime":time});
    Ok((
        model_id,
        receive_id,
        ChatRequest {
            model: String::new(),
            messages: list,
            temperature: Some(c.get("temperature")),
            max_tokens: Some(c.get::<i32, _>("max_tokens") as u32),
        },
        send,
        c.get("tool_ids"),
        knowledge_status,
    ))
}
async fn generate(
    state: &AiState,
    model_id: i64,
    request: ChatRequest,
    tool_ids: &[i64],
) -> Result<(ChatResponse, Value), AppError> {
    let tools = crate::tools::load_definitions(&state.pool, tool_ids).await?;
    if tools.is_empty() {
        return state
            .factory
            .chat(model_id, request)
            .await
            .map(|r| (r, json!([])));
    }
    let allowed = tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut messages = request
        .messages
        .iter()
        .map(|m| json!({"role":m.role,"content":m.content}))
        .collect::<Vec<_>>();
    let mut records = Vec::new();
    for _ in 0..5 {
        let raw = state
            .factory
            .chat_tools(
                model_id,
                messages.clone(),
                tools.clone(),
                request.temperature,
                request.max_tokens,
            )
            .await?;
        let message = raw
            .pointer("/choices/0/message")
            .cloned()
            .ok_or_else(|| AppError::bad_request("模型工具响应缺少 message"))?;
        let calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if calls.is_empty() {
            let content = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if content.is_empty() {
                return Err(AppError::bad_request("模型工具响应缺少文本"));
            }
            return Ok((
                ChatResponse {
                    content,
                    reasoning: message
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    usage: raw.get("usage").cloned().unwrap_or_else(|| json!({})),
                },
                json!(records),
            ));
        }
        messages.push(message);
        for call in calls {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("tool_call 缺少 id"))?;
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("tool_call 缺少名称"))?;
            if !allowed.iter().any(|x| x == name) {
                return Err(AppError::forbidden("模型请求了未绑定的工具"));
            }
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let args: Value = serde_json::from_str(arguments)
                .map_err(|_| AppError::bad_request("工具参数不是合法 JSON"))?;
            let output = crate::tools::execute(&state.pool, name, &args)
                .await
                .map_err(AppError::bad_request)?;
            records.push(json!({"id":call_id,"name":name,"arguments":args,"output":output}));
            messages.push(json!({"role":"tool","tool_call_id":call_id,"content":output}));
        }
    }
    Err(AppError::bad_request("工具调用超过最大循环次数"))
}
async fn send(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(v): Json<SendRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let (model_id, receive_id, request, send, tool_ids, knowledge_status) =
        load_request(&state, &user, &v).await?;
    let (result, tool_calls) = generate(&state, model_id, request, &tool_ids).await?;
    sqlx::query(
        "UPDATE ai.chat_messages SET content=$2,reasoning_content=$3,tool_calls=$4 WHERE id=$1",
    )
    .bind(receive_id)
    .bind(&result.content)
    .bind(&result.reasoning)
    .bind(tool_calls)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("保存回复失败"))?;
    Ok(Json(ApiResponse::new(
        json!({"send":send,"receive":{"id":receive_id,"conversationId":v.conversation_id,"type":"assistant","modelId":model_id,"content":result.content,"reasoningContent":result.reasoning,"knowledgeStatus":knowledge_status}}),
    )))
}

async fn send_stream(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(v): Json<SendRequest>,
) -> Result<Response<Body>, AppError> {
    let (model_id, receive_id, request, send, tool_ids, knowledge_status) =
        load_request(&state, &user, &v).await?;
    let conversation_id = v.conversation_id;
    let direct_stream = tool_ids.is_empty();
    let (tx, rx) = mpsc::channel::<Result<String, std::convert::Infallible>>(32);
    let pool = state.pool.clone();
    let factory = state.factory.clone();
    let state_for_tools = state.clone();
    tokio::spawn(async move {
        let send_for_chunks = send.clone();
        let knowledge_status_for_chunks = knowledge_status.clone();
        let tx_chunks = tx.clone();
        let result = if direct_stream {
            factory.chat_stream(model_id,request,move|delta|{let tx=tx_chunks.clone();let send=send_for_chunks.clone();let knowledge_status=knowledge_status_for_chunks.clone();async move{let payload=json!({"code":0,"data":{"send":send,"receive":{"id":receive_id,"conversationId":conversation_id,"type":"assistant","modelId":model_id,"content":delta,"reasoningContent":null,"knowledgeStatus":knowledge_status}},"msg":""});tx.send(Ok(format!("data: {}\n\n",payload))).await.map_err(|_|"客户端已断开".to_string())}}).await.map(|r|(r,json!([])))
        } else {
            generate(&state_for_tools, model_id, request, &tool_ids).await
        };
        match result {
            Ok((response, tool_calls)) => {
                if !direct_stream {
                    let payload = json!({"code":0,"data":{"send":send,"receive":{"id":receive_id,"conversationId":conversation_id,"type":"assistant","modelId":model_id,"content":response.content,"reasoningContent":response.reasoning,"knowledgeStatus":knowledge_status}},"msg":""});
                    let _ = tx.send(Ok(format!("data: {}\n\n", payload))).await;
                }
                let _ = sqlx::query(
                    "UPDATE ai.chat_messages SET content=$2,reasoning_content=$3,tool_calls=$4 WHERE id=$1",
                )
                .bind(receive_id)
                .bind(&response.content)
                .bind(&response.reasoning)
                .bind(tool_calls)
                .execute(&pool)
                .await;
            }
            Err(error) => {
                let error = format!("{error:?}");
                let _ = sqlx::query("UPDATE ai.chat_messages SET content=$2 WHERE id=$1")
                    .bind(receive_id)
                    .bind(format!("生成失败: {error}"))
                    .execute(&pool)
                    .await;
                let payload = json!({"code":500,"data":null,"msg":error});
                let _ = tx.send(Ok(format!("data: {}\n\n", payload))).await;
            }
        }
    });
    let body = Body::from_stream(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .map_err(|_| AppError::internal("创建流失败"))
}
async fn delete_message(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(v): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    let r=sqlx::query("DELETE FROM ai.chat_messages m USING ai.chat_conversations c WHERE m.id=$1 AND m.conversation_id=c.id AND c.user_id=$2").bind(v.id).bind(&user.user_id).execute(&state.pool).await.map_err(|_|AppError::internal("删除消息失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn delete_messages(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(v): Query<ConversationId>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    sqlx::query("DELETE FROM ai.chat_messages m USING ai.chat_conversations c WHERE m.conversation_id=$1 AND m.conversation_id=c.id AND c.user_id=$2").bind(v.conversation_id).bind(&user.user_id).execute(&state.pool).await.map_err(|_|AppError::internal("清空消息失败"))?;
    Ok(Json(ApiResponse::new(true)))
}

async fn conversation_page(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:chat:query")?;
    let p = q.page_no.unwrap_or(1).max(1);
    let n = q.page_size.unwrap_or(10).clamp(1, 200);
    let rows=sqlx::query("SELECT c.id,c.user_id,c.title,c.pinned,c.role_id,c.model_id,m.model,m.name model_name,c.temperature,c.max_tokens,c.max_contexts,c.system_message,c.tool_ids,c.knowledge_ids,c.create_time,(SELECT count(*) FROM ai.chat_messages x WHERE x.conversation_id=c.id) message_count FROM ai.chat_conversations c JOIN ai.model_configs m ON m.id=c.model_id WHERE ($1::text IS NULL OR c.user_id=$1) AND ($2::text IS NULL OR c.title ILIKE '%'||$2||'%') ORDER BY c.id DESC LIMIT $3 OFFSET $4").bind(&q.user_id).bind(&q.title).bind(n).bind((p-1)*n).fetch_all(&state.pool).await.map_err(|_|AppError::internal("读取对话失败"))?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM ai.chat_conversations WHERE ($1::text IS NULL OR user_id=$1) AND ($2::text IS NULL OR title ILIKE '%'||$2||'%')").bind(&q.user_id).bind(&q.title).fetch_one(&state.pool).await.map_err(|_|AppError::internal("读取对话失败"))?;
    let list = rows
        .into_iter()
        .map(|r| {
            let count = r.get::<i64, _>("message_count");
            let mut v = conversation(r);
            v["messageCount"] = json!(count);
            v
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"list":list,"total":total}))))
}
async fn delete_conversation_admin(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&user, "ai:chat:delete")?;
    let r = sqlx::query("DELETE FROM ai.chat_conversations WHERE id=$1")
        .bind(q.id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("删除对话失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn message_page(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:chat:query")?;
    let p = q.page_no.unwrap_or(1).max(1);
    let n = q.page_size.unwrap_or(10).clamp(1, 200);
    let rows=sqlx::query("SELECT m.* FROM ai.chat_messages m WHERE ($1::bigint IS NULL OR m.conversation_id=$1) AND ($2::text IS NULL OR m.user_id=$2) ORDER BY m.id DESC LIMIT $3 OFFSET $4").bind(q.conversation_id).bind(&q.user_id).bind(n).bind((p-1)*n).fetch_all(&state.pool).await.map_err(|_|AppError::internal("读取消息失败"))?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM ai.chat_messages WHERE ($1::bigint IS NULL OR conversation_id=$1) AND ($2::text IS NULL OR user_id=$2)").bind(q.conversation_id).bind(&q.user_id).fetch_one(&state.pool).await.map_err(|_|AppError::internal("读取消息失败"))?;
    Ok(Json(ApiResponse::new(
        json!({"list":rows.iter().map(message).collect::<Vec<_>>(),"total":total}),
    )))
}
async fn delete_message_admin(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&user, "ai:chat:delete")?;
    let r = sqlx::query("DELETE FROM ai.chat_messages WHERE id=$1")
        .bind(q.id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("删除消息失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
