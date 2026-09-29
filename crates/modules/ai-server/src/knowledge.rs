use crate::{AiState, require, vector};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{delete, get, post, put},
};
use rust_toon_ai_api::EmbeddingRequest;
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
pub fn routes() -> Router<AiState> {
    Router::new()
        .route("/ai/knowledge/page", get(k_page))
        .route("/ai/knowledge/simple-list", get(k_simple))
        .route("/ai/knowledge/get", get(k_get))
        .route("/ai/knowledge/create", post(k_create))
        .route("/ai/knowledge/update", put(k_update))
        .route("/ai/knowledge/delete", delete(k_delete))
        .route("/ai/knowledge/document/page", get(d_page))
        .route("/ai/knowledge/document/get", get(d_get))
        .route("/ai/knowledge/document/create-list", post(d_create_list))
        .route("/ai/knowledge/document/update", put(d_update))
        .route("/ai/knowledge/document/update-status", put(d_status))
        .route("/ai/knowledge/document/delete", delete(d_delete))
        .route("/ai/knowledge/segment/page", get(s_page))
        .route("/ai/knowledge/segment/get", get(s_get))
        .route("/ai/knowledge/segment/create", post(s_create))
        .route("/ai/knowledge/segment/update", put(s_update))
        .route("/ai/knowledge/segment/update-status", put(s_status))
        .route("/ai/knowledge/segment/delete", delete(s_delete))
        .route("/ai/knowledge/segment/split", get(s_split))
        .route("/ai/knowledge/segment/get-process-list", get(s_process))
        .route("/ai/knowledge/segment/search", get(s_search))
}
fn id() -> i64 {
    chrono::Utc::now().timestamp_micros()
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
#[derive(Deserialize)]
struct Id {
    id: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    page_no: Option<i64>,
    page_size: Option<i64>,
    name: Option<String>,
    knowledge_id: Option<i64>,
    document_id: Option<i64>,
    status: Option<i32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Knowledge {
    id: Option<i64>,
    name: String,
    description: Option<String>,
    embedding_model_id: i64,
    top_k: Option<i32>,
    similarity_threshold: Option<f64>,
}
fn kv(r: sqlx::postgres::PgRow) -> Value {
    json!({"id":r.get::<i64,_>("id"),"name":r.get::<String,_>("name"),"description":r.get::<String,_>("description"),"embeddingModelId":r.get::<i64,_>("embedding_model_id"),"topK":r.get::<i32,_>("top_k"),"similarityThreshold":r.get::<f64,_>("similarity_threshold")})
}
async fn k_page(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Page>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    let (p, n) = (
        q.page_no.unwrap_or(1).max(1),
        q.page_size.unwrap_or(10).clamp(1, 100),
    );
    let rows=sqlx::query("SELECT * FROM ai.knowledge_bases WHERE ($1::text IS NULL OR name ILIKE '%'||$1||'%') ORDER BY id DESC LIMIT $2 OFFSET $3").bind(&q.name).bind(n).bind((p-1)*n).fetch_all(&s.pool).await.map_err(|_|AppError::internal("读取知识库失败"))?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM ai.knowledge_bases WHERE ($1::text IS NULL OR name ILIKE '%'||$1||'%')").bind(&q.name).fetch_one(&s.pool).await.map_err(|_|AppError::internal("读取知识库失败"))?;
    Ok(Json(ApiResponse::new(
        json!({"list":rows.into_iter().map(kv).collect::<Vec<_>>(),"total":total}),
    )))
}
async fn k_simple(
    u: CurrentUser,
    State(s): State<AiState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    let rows = sqlx::query("SELECT * FROM ai.knowledge_bases ORDER BY name")
        .fetch_all(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取知识库失败"))?;
    Ok(Json(ApiResponse::new(rows.into_iter().map(kv).collect())))
}
async fn k_get(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    let r = sqlx::query("SELECT * FROM ai.knowledge_bases WHERE id=$1")
        .bind(q.id)
        .fetch_optional(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取知识库失败"))?
        .ok_or_else(|| AppError::not_found("知识库不存在"))?;
    Ok(Json(ApiResponse::new(kv(r))))
}
async fn k_create(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Knowledge>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&u, "ai:knowledge:create")?;
    s.factory
        .embedding(
            v.embedding_model_id,
            EmbeddingRequest {
                inputs: vec!["连接测试".into()],
            },
        )
        .await?;
    let id = id();
    sqlx::query("INSERT INTO ai.knowledge_bases(id,name,description,embedding_model_id,top_k,similarity_threshold,create_time,update_time)VALUES($1,$2,$3,$4,$5,$6,$7,$7)").bind(id).bind(v.name).bind(v.description.unwrap_or_default()).bind(v.embedding_model_id).bind(v.top_k.unwrap_or(5)).bind(v.similarity_threshold.unwrap_or(0.5)).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("创建知识库失败"))?;
    Ok(Json(ApiResponse::new(id)))
}
async fn k_update(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Knowledge>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:update")?;
    let id =
        v.id.ok_or_else(|| AppError::bad_request("id is required"))?;
    let r=sqlx::query("UPDATE ai.knowledge_bases SET name=$2,description=$3,embedding_model_id=$4,top_k=$5,similarity_threshold=$6,update_time=$7 WHERE id=$1").bind(id).bind(v.name).bind(v.description.unwrap_or_default()).bind(v.embedding_model_id).bind(v.top_k.unwrap_or(5)).bind(v.similarity_threshold.unwrap_or(0.5)).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("更新知识库失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn k_delete(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:delete")?;
    let r = sqlx::query("DELETE FROM ai.knowledge_bases WHERE id=$1")
        .bind(q.id)
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("删除知识库失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
fn dv(r: sqlx::postgres::PgRow) -> Value {
    json!({"id":r.get::<i64,_>("id"),"knowledgeId":r.get::<i64,_>("knowledge_id"),"name":r.get::<String,_>("name"),"url":r.get::<String,_>("url"),"contentLength":r.get::<i32,_>("content_length"),"tokens":r.get::<i32,_>("tokens"),"segmentMaxTokens":r.get::<i32,_>("segment_max_tokens"),"retrievalCount":r.get::<i32,_>("retrieval_count"),"status":r.get::<i32,_>("status")})
}
async fn d_page(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Page>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    page_table(
        &s,
        "ai.knowledge_documents",
        q.knowledge_id,
        "knowledge_id",
        q.status,
        dv,
        &q,
    )
    .await
}
async fn d_get(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    let r = sqlx::query("SELECT * FROM ai.knowledge_documents WHERE id=$1")
        .bind(q.id)
        .fetch_optional(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取文档失败"))?
        .ok_or_else(|| AppError::not_found("文档不存在"))?;
    Ok(Json(ApiResponse::new(dv(r))))
}
#[derive(Deserialize)]
struct DocItem {
    name: String,
    url: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocList {
    knowledge_id: i64,
    segment_max_tokens: i32,
    list: Vec<DocItem>,
}
async fn d_create_list(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<DocList>,
) -> Result<Json<ApiResponse<Vec<i64>>>, AppError> {
    require(&u, "ai:knowledge:create")?;
    let model_id: i64 =
        sqlx::query_scalar("SELECT embedding_model_id FROM ai.knowledge_bases WHERE id=$1")
            .bind(v.knowledge_id)
            .fetch_optional(&s.pool)
            .await
            .map_err(|_| AppError::internal("读取知识库失败"))?
            .ok_or_else(|| AppError::not_found("知识库不存在"))?;
    let mut ids = Vec::new();
    for item in v.list {
        let content = crate::knowledge_download::download_text(&s.pool, &item.url).await?;
        let document_id = id();
        let parts = vector::split(&content, v.segment_max_tokens.max(1) as usize);
        sqlx::query("INSERT INTO ai.knowledge_documents(id,knowledge_id,name,url,content,content_length,tokens,segment_max_tokens,create_time,update_time)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9)").bind(document_id).bind(v.knowledge_id).bind(item.name).bind(item.url).bind(&content).bind(content.chars().count() as i32).bind((content.chars().count()/4) as i32).bind(v.segment_max_tokens).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("保存文档失败"))?;
        if !parts.is_empty() {
            let embeddings = s
                .factory
                .embedding(
                    model_id,
                    EmbeddingRequest {
                        inputs: parts.clone(),
                    },
                )
                .await?;
            for (part, embedding) in parts.into_iter().zip(embeddings.embeddings) {
                let segment_id = id();
                sqlx::query("INSERT INTO ai.knowledge_segments(id,document_id,knowledge_id,vector_id,content,content_length,tokens,embedding,create_time,update_time)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9)").bind(segment_id).bind(document_id).bind(v.knowledge_id).bind(segment_id.to_string()).bind(&part).bind(part.chars().count() as i32).bind((part.chars().count()/4) as i32).bind(embedding).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("保存文档分段失败"))?;
            }
        }
        ids.push(document_id)
    }
    Ok(Json(ApiResponse::new(ids)))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocUpdate {
    id: i64,
    segment_max_tokens: Option<i32>,
    status: Option<i32>,
}
async fn d_update(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<DocUpdate>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:update")?;
    let r=sqlx::query("UPDATE ai.knowledge_documents SET segment_max_tokens=COALESCE($2,segment_max_tokens),update_time=$3 WHERE id=$1").bind(v.id).bind(v.segment_max_tokens).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("更新文档失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn d_status(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<DocUpdate>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:update")?;
    let r = sqlx::query("UPDATE ai.knowledge_documents SET status=$2,update_time=$3 WHERE id=$1")
        .bind(v.id)
        .bind(v.status.unwrap_or(1))
        .bind(now())
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("更新文档失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn d_delete(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:delete")?;
    let r = sqlx::query("DELETE FROM ai.knowledge_documents WHERE id=$1")
        .bind(q.id)
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("删除文档失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
fn sv(r: sqlx::postgres::PgRow) -> Value {
    json!({"id":r.get::<i64,_>("id"),"documentId":r.get::<i64,_>("document_id"),"knowledgeId":r.get::<i64,_>("knowledge_id"),"vectorId":r.get::<String,_>("vector_id"),"content":r.get::<String,_>("content"),"contentLength":r.get::<i32,_>("content_length"),"tokens":r.get::<i32,_>("tokens"),"retrievalCount":r.get::<i32,_>("retrieval_count"),"status":r.get::<i32,_>("status"),"createTime":r.get::<i64,_>("create_time")})
}
async fn page_table<F>(
    s: &AiState,
    table: &str,
    parent: Option<i64>,
    column: &str,
    status: Option<i32>,
    map: F,
    q: &Page,
) -> Result<Json<ApiResponse<Value>>, AppError>
where
    F: Fn(sqlx::postgres::PgRow) -> Value,
{
    let p = q.page_no.unwrap_or(1).max(1);
    let n = q.page_size.unwrap_or(10).clamp(1, 100);
    let query = format!(
        "SELECT * FROM {table} WHERE ($1::bigint IS NULL OR {column}=$1) AND ($2::int IS NULL OR status=$2) ORDER BY id DESC LIMIT $3 OFFSET $4"
    );
    let count = format!(
        "SELECT count(*) FROM {table} WHERE ($1::bigint IS NULL OR {column}=$1) AND ($2::int IS NULL OR status=$2)"
    );
    let rows = sqlx::query(&query)
        .bind(parent)
        .bind(status)
        .bind(n)
        .bind((p - 1) * n)
        .fetch_all(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取分页数据失败"))?;
    let total: i64 = sqlx::query_scalar(&count)
        .bind(parent)
        .bind(status)
        .fetch_one(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取分页数据失败"))?;
    Ok(Json(ApiResponse::new(
        json!({"list":rows.into_iter().map(map).collect::<Vec<_>>(),"total":total}),
    )))
}
async fn s_page(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Page>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    page_table(
        &s,
        "ai.knowledge_segments",
        q.document_id,
        "document_id",
        q.status,
        sv,
        &q,
    )
    .await
}
async fn s_get(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&u, "ai:knowledge:query")?;
    let r = sqlx::query("SELECT * FROM ai.knowledge_segments WHERE id=$1")
        .bind(q.id)
        .fetch_optional(&s.pool)
        .await
        .map_err(|_| AppError::internal("读取分段失败"))?
        .ok_or_else(|| AppError::not_found("分段不存在"))?;
    Ok(Json(ApiResponse::new(sv(r))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    id: Option<i64>,
    document_id: i64,
    knowledge_id: i64,
    content: String,
    status: Option<i32>,
}
async fn embed_segment(s: &AiState, v: &Segment) -> Result<Vec<f32>, AppError> {
    let model: i64 =
        sqlx::query_scalar("SELECT embedding_model_id FROM ai.knowledge_bases WHERE id=$1")
            .bind(v.knowledge_id)
            .fetch_one(&s.pool)
            .await
            .map_err(|_| AppError::not_found("知识库不存在"))?;
    s.factory
        .embedding(
            model,
            EmbeddingRequest {
                inputs: vec![v.content.clone()],
            },
        )
        .await?
        .embeddings
        .pop()
        .ok_or_else(|| AppError::bad_request("Embedding 未返回向量"))
}
async fn s_create(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Segment>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&u, "ai:knowledge:create")?;
    let embedding = embed_segment(&s, &v).await?;
    let id = id();
    sqlx::query("INSERT INTO ai.knowledge_segments(id,document_id,knowledge_id,vector_id,content,content_length,tokens,status,embedding,create_time,update_time)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10)").bind(id).bind(v.document_id).bind(v.knowledge_id).bind(id.to_string()).bind(&v.content).bind(v.content.chars().count()as i32).bind((v.content.chars().count()/4)as i32).bind(v.status.unwrap_or(1)).bind(embedding).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("创建分段失败"))?;
    Ok(Json(ApiResponse::new(id)))
}
async fn s_update(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Segment>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:update")?;
    let id =
        v.id.ok_or_else(|| AppError::bad_request("id is required"))?;
    let embedding = embed_segment(&s, &v).await?;
    let r=sqlx::query("UPDATE ai.knowledge_segments SET content=$2,content_length=$3,tokens=$4,status=$5,embedding=$6,update_time=$7 WHERE id=$1").bind(id).bind(&v.content).bind(v.content.chars().count()as i32).bind((v.content.chars().count()/4)as i32).bind(v.status.unwrap_or(1)).bind(embedding).bind(now()).execute(&s.pool).await.map_err(|_|AppError::internal("更新分段失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
#[derive(Deserialize)]
struct Status {
    id: i64,
    status: i32,
}
async fn s_status(
    u: CurrentUser,
    State(s): State<AiState>,
    Json(v): Json<Status>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:update")?;
    let r = sqlx::query("UPDATE ai.knowledge_segments SET status=$2,update_time=$3 WHERE id=$1")
        .bind(v.id)
        .bind(v.status)
        .bind(now())
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("更新分段失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
async fn s_delete(
    u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Id>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&u, "ai:knowledge:delete")?;
    let r = sqlx::query("DELETE FROM ai.knowledge_segments WHERE id=$1")
        .bind(q.id)
        .execute(&s.pool)
        .await
        .map_err(|_| AppError::internal("删除分段失败"))?;
    Ok(Json(ApiResponse::new(r.rows_affected() > 0)))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Split {
    url: String,
    segment_max_tokens: usize,
}
async fn s_split(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(q): Query<Split>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "ai:knowledge:create")?;
    if !(1..=32_768).contains(&q.segment_max_tokens) {
        return Err(AppError::bad_request("segmentMaxTokens 必须在 1 到 32768 之间"));
    }
    let content = crate::knowledge_download::download_text(&state.pool, &q.url).await?;
    Ok(Json(ApiResponse::new(vector::split(&content,q.segment_max_tokens).into_iter().map(|content|json!({"contentLength":content.chars().count(),"tokens":content.chars().count()/4,"content":content})).collect())))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Process {
    document_ids: String,
}
async fn s_process(
    _u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Process>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let ids = q
        .document_ids
        .split(',')
        .filter_map(|x| x.parse().ok())
        .collect::<Vec<i64>>();
    let rows=sqlx::query("SELECT d.id document_id,count(s.id)::bigint count,count(s.embedding)::bigint embedding_count FROM ai.knowledge_documents d LEFT JOIN ai.knowledge_segments s ON s.document_id=d.id WHERE d.id=ANY($1) GROUP BY d.id").bind(ids).fetch_all(&s.pool).await.map_err(|_|AppError::internal("读取处理进度失败"))?;
    Ok(Json(ApiResponse::new(rows.into_iter().map(|r|json!({"documentId":r.get::<i64,_>("document_id"),"count":r.get::<i64,_>("count"),"embeddingCount":r.get::<i64,_>("embedding_count")})).collect())))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search {
    knowledge_id: i64,
    content: String,
    top_k: Option<usize>,
    similarity_threshold: Option<f32>,
}
pub(crate) async fn retrieve(
    state: &AiState,
    knowledge_ids: &[i64],
    content: &str,
) -> Result<Vec<Value>, AppError> {
    let mut matches = Vec::new();
    for knowledge_id in knowledge_ids {
        let base = sqlx::query("SELECT embedding_model_id,top_k,similarity_threshold FROM ai.knowledge_bases WHERE id=$1")
            .bind(knowledge_id).fetch_optional(&state.pool).await
            .map_err(|_| AppError::internal("读取知识库失败"))?
            .ok_or_else(|| AppError::not_found("知识库不存在"))?;
        let query = state
            .factory
            .embedding(
                base.get("embedding_model_id"),
                EmbeddingRequest {
                    inputs: vec![content.to_string()],
                },
            )
            .await?
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| AppError::bad_request("Embedding 未返回向量"))?;
        let rows = sqlx::query("SELECT s.*,d.name document_name FROM ai.knowledge_segments s JOIN ai.knowledge_documents d ON d.id=s.document_id WHERE s.knowledge_id=$1 AND s.status=1 AND d.status=1 AND s.embedding IS NOT NULL")
            .bind(knowledge_id).fetch_all(&state.pool).await.map_err(|_| AppError::internal("读取知识分段失败"))?;
        let threshold = base.get::<f64, _>("similarity_threshold") as f32;
        let mut scored = rows
            .into_iter()
            .filter_map(|row| {
                let score = vector::cosine(&query, &row.get::<Vec<f32>, _>("embedding"));
                (score >= threshold).then(|| json!({
                "id":row.get::<i64,_>("id"),"documentId":row.get::<i64,_>("document_id"),
                "documentName":row.get::<String,_>("document_name"),"knowledgeId":knowledge_id,
                "content":row.get::<String,_>("content"),"similarity":score
            }))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| {
            b["similarity"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["similarity"].as_f64().unwrap_or(0.0))
        });
        scored.truncate(base.get::<i32, _>("top_k").max(0) as usize);
        matches.extend(scored);
    }
    matches.sort_by(|a, b| {
        b["similarity"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["similarity"].as_f64().unwrap_or(0.0))
    });
    let ids = matches
        .iter()
        .filter_map(|v| v["id"].as_i64())
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        sqlx::query(
            "UPDATE ai.knowledge_segments SET retrieval_count=retrieval_count+1 WHERE id=ANY($1)",
        )
        .bind(ids)
        .execute(&state.pool)
        .await
        .ok();
    }
    Ok(matches)
}
async fn s_search(
    _u: CurrentUser,
    State(s): State<AiState>,
    Query(q): Query<Search>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let k = sqlx::query(
        "SELECT embedding_model_id,top_k,similarity_threshold FROM ai.knowledge_bases WHERE id=$1",
    )
    .bind(q.knowledge_id)
    .fetch_optional(&s.pool)
    .await
    .map_err(|_| AppError::internal("读取知识库失败"))?
    .ok_or_else(|| AppError::not_found("知识库不存在"))?;
    let query = s
        .factory
        .embedding(
            k.get("embedding_model_id"),
            EmbeddingRequest {
                inputs: vec![q.content],
            },
        )
        .await?
        .embeddings
        .into_iter()
        .next()
        .ok_or_else(|| AppError::bad_request("Embedding 未返回向量"))?;
    let rows=sqlx::query("SELECT * FROM ai.knowledge_segments WHERE knowledge_id=$1 AND status=1 AND embedding IS NOT NULL").bind(q.knowledge_id).fetch_all(&s.pool).await.map_err(|_|AppError::internal("读取分段失败"))?;
    let threshold = q
        .similarity_threshold
        .unwrap_or(k.get::<f64, _>("similarity_threshold") as f32);
    let mut scored = rows
        .into_iter()
        .filter_map(|r| {
            let score = vector::cosine(&query, &r.get::<Vec<f32>, _>("embedding"));
            (score >= threshold).then(|| {
                let id = r.get::<i64, _>("id");
                let mut v = sv(r);
                v["similarity"] = json!(score);
                (id, score, v)
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(q.top_k.unwrap_or(k.get::<i32, _>("top_k") as usize));
    let ids = scored.iter().map(|x| x.0).collect::<Vec<_>>();
    if !ids.is_empty() {
        sqlx::query(
            "UPDATE ai.knowledge_segments SET retrieval_count=retrieval_count+1 WHERE id=ANY($1)",
        )
        .bind(ids)
        .execute(&s.pool)
        .await
        .ok();
    }
    Ok(Json(ApiResponse::new(
        scored.into_iter().map(|x| x.2).collect(),
    )))
}
