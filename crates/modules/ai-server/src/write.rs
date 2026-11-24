use crate::{AiState, require};
use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{Response, header},
    routing::{delete, get, post},
};
use futures_util::stream;
use rust_toon_ai_api::{ChatMessage, ChatRequest};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::mpsc;
pub fn routes() -> Router<AiState> {
    Router::new()
        .route("/ai/write/generate-stream", post(generate_stream))
        .route("/ai/write/page", get(page))
        .route("/ai/write/delete", delete(remove))
}
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Generate {
    model_id: Option<i64>,
    #[serde(rename = "type")]
    type_: i32,
    prompt: String,
    original_content: Option<String>,
    length: Option<i32>,
    format: Option<i32>,
    tone: Option<i32>,
    language: Option<i32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    page_no: Option<i64>,
    page_size: Option<i64>,
    user_id: Option<String>,
    #[serde(rename = "type")]
    type_: Option<i32>,
    platform: Option<String>,
}
#[derive(Deserialize)]
struct Id {
    id: i64,
}
fn instruction(v: &Generate) -> String {
    let action = if v.type_ == 2 {
        "根据原文撰写回复"
    } else {
        "按要求撰写内容"
    };
    format!(
        "{action}。要求：长度级别 {}，格式级别 {}，语气级别 {}，语言级别 {}。\n原文：{}\n写作要求：{}",
        v.length.unwrap_or(1),
        v.format.unwrap_or(1),
        v.tone.unwrap_or(1),
        v.language.unwrap_or(1),
        v.original_content.as_deref().unwrap_or(""),
        v.prompt
    )
}
async fn generate_stream(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Generate>,
) -> Result<Response<Body>, AppError> {
    if v.prompt.trim().is_empty() {
        return Err(AppError::bad_request("写作提示不能为空"));
    }
    let model_id = match v.model_id {
        Some(id) => id,
        None => sqlx::query_scalar(
            "SELECT id FROM ai.model_configs WHERE type='chat' AND status=0 ORDER BY id LIMIT 1",
        )
        .fetch_optional(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取写作模型失败"))?
        .ok_or_else(|| AppError::bad_request("请先配置启用的聊天模型"))?,
    };
    let config = s.factory.config(model_id).await?;
    let id = chrono::Utc::now().timestamp_micros();
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO ai.writes(id,user_id,model_id,type,prompt,original_content,length,format,tone,language,platform,model,create_time)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)").bind(id).bind(&u.user_id).bind(model_id).bind(v.type_).bind(&v.prompt).bind(v.original_content.clone().unwrap_or_default()).bind(v.length.unwrap_or(1)).bind(v.format.unwrap_or(1)).bind(v.tone.unwrap_or(1)).bind(v.language.unwrap_or(1)).bind(config.platform).bind(config.model).bind(now).execute(&s.pool).await.map_err(|_|AppError::internal("创建写作记录失败"))?;
    let request = ChatRequest {
        model: String::new(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: instruction(&v),
        }],
        temperature: Some(0.7),
        max_tokens: Some(4096),
    };
    let (tx, rx) = mpsc::channel::<Result<String, std::convert::Infallible>>(32);
    let pool = s.pool.clone();
    let factory = s.factory.clone();
    tokio::spawn(async move {
        let chunks = tx.clone();
        let result = factory
            .chat_stream(model_id, request, move |delta| {
                let tx = chunks.clone();
                async move {
                    tx.send(Ok(format!(
                        "data: {}\n\n",
                        json!({"code":0,"data":delta,"msg":""})
                    )))
                    .await
                    .map_err(|_| "客户端已断开".to_string())
                }
            })
            .await;
        match result {
            Ok(r) => {
                sqlx::query("UPDATE ai.writes SET generated_content=$2,finish_time=$3 WHERE id=$1")
                    .bind(id)
                    .bind(r.content)
                    .bind(chrono::Utc::now().timestamp_millis())
                    .execute(&pool)
                    .await
                    .ok();
            }
            Err(e) => {
                let error = format!("{e:?}");
                sqlx::query("UPDATE ai.writes SET error_message=$2,finish_time=$3 WHERE id=$1")
                    .bind(id)
                    .bind(&error)
                    .bind(chrono::Utc::now().timestamp_millis())
                    .execute(&pool)
                    .await
                    .ok();
                let _ = tx
                    .send(Ok(format!(
                        "data: {}\n\n",
                        json!({"code":500,"data":null,"msg":error})
                    )))
                    .await;
            }
        }
    });
    let body = Body::from_stream(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|x| (x, rx))
    }));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .map_err(|_| AppError::internal("创建写作流失败"))
}
fn value(r: sqlx::postgres::PgRow) -> Value {
    json!({"id":r.get::<i64,_>("id"),"userId":r.get::<String,_>("user_id"),"type":r.get::<i32,_>("type"),"prompt":r.get::<String,_>("prompt"),"originalContent":r.get::<String,_>("original_content"),"length":r.get::<i32,_>("length"),"format":r.get::<i32,_>("format"),"tone":r.get::<i32,_>("tone"),"language":r.get::<i32,_>("language"),"platform":r.get::<String,_>("platform"),"model":r.get::<String,_>("model"),"generatedContent":r.get::<String,_>("generated_content"),"errorMessage":r.get::<Option<String>,_>("error_message"),"createTime":r.get::<i64,_>("create_time")})
}
async fn page(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Page>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:write:query")?;
    let p = q.page_no.unwrap_or(1).max(1);
    let n = q.page_size.unwrap_or(10).clamp(1, 100);
    let rows=sqlx::query("SELECT * FROM ai.writes WHERE ($1::text IS NULL OR user_id=$1) AND ($2::int IS NULL OR type=$2) AND ($3::text IS NULL OR platform=$3) ORDER BY id DESC LIMIT $4 OFFSET $5").bind(&q.user_id).bind(q.type_).bind(&q.platform).bind(n).bind((p-1)*n).fetch_all(&s.pool).await.map_err(|_|AppError::internal("读取写作记录失败"))?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM ai.writes WHERE ($1::text IS NULL OR user_id=$1) AND ($2::int IS NULL OR type=$2) AND ($3::text IS NULL OR platform=$3)").bind(&q.user_id).bind(q.type_).bind(&q.platform).fetch_one(&s.pool).await.map_err(|_|AppError::internal("读取写作记录失败"))?;
    Ok(Json(ApiResponse::new(
        json!({"list":rows.into_iter().map(value).collect::<Vec<_>>(),"total":total}),
    )))
}
async fn remove(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:write:delete")?;
    let r = sqlx::query("DELETE FROM ai.writes WHERE id=$1")
        .bind(q.id)
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("删除写作记录失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
