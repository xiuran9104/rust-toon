use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{SystemState, management::shared::require};

#[derive(Debug, Serialize)]
pub struct Page<T> {
    list: Vec<T>,
    total: i64,
}

#[derive(Debug, Deserialize)]
pub struct QueryParams {
    #[serde(default, rename = "pageNo")]
    page_no: Option<i64>,
    #[serde(default, rename = "pageSize")]
    page_size: Option<i64>,
}

pub async fn my_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM system_notify_message
         WHERE user_id=$1 AND tenant_id=$2 AND deleted=0",
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to count notify messages"))?;
    let list = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object(
             'id', id,
             'userId', user_id,
             'userType', user_type,
             'templateId', template_id,
             'templateCode', template_code,
             'templateNickname', template_nickname,
             'templateContent', template_content,
             'templateType', template_type,
             'templateParams', template_params,
             'readStatus', read_status,
             'readTime', read_time,
             'createTime', create_time
         )
         FROM system_notify_message
         WHERE user_id=$1 AND tenant_id=$2 AND deleted=0
         ORDER BY create_time DESC, id DESC
         LIMIT $3 OFFSET $4",
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list notify messages"))?;
    Ok(Json(ApiResponse::new(Page { list, total })))
}

pub async fn my_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    let list = message_list(&state.pool, user_id, tenant_id, false, 20).await?;
    Ok(Json(ApiResponse::new(list)))
}

pub async fn unread_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    let list = message_list(&state.pool, user_id, tenant_id, true, 20).await?;
    Ok(Json(ApiResponse::new(list)))
}

pub async fn unread_count(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM system_notify_message
         WHERE user_id=$1 AND tenant_id=$2 AND read_status=false AND deleted=0",
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to count unread notify messages"))?;
    Ok(Json(ApiResponse::new(count)))
}

pub async fn update_read(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let ids = split_i64_ids(&params);
    if ids.is_empty() {
        return Ok(Json(ApiResponse::new(())));
    }
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    sqlx::query(
        "UPDATE system_notify_message
         SET read_status=true, read_time=now(), updater=$4, update_time=now()
         WHERE id = ANY($1) AND user_id=$2 AND tenant_id=$3 AND deleted=0",
    )
    .bind(&ids)
    .bind(user_id)
    .bind(tenant_id)
    .bind(&user.username)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update notify messages"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn update_all_read(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let user_id = yudao_user_id(&state.pool, &user).await?;
    let tenant_id = current_tenant_id(&user)?;
    sqlx::query(
        "UPDATE system_notify_message
         SET read_status=true, read_time=now(), updater=$3, update_time=now()
         WHERE user_id=$1 AND tenant_id=$2 AND read_status=false AND deleted=0",
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&user.username)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update notify messages"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn send_notify(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:notify-template:send-notify")?;
    let tenant_id = current_tenant_id(&user)?;
    let user_id = parse_i64(&payload, "userId")?;
    let user_type = payload
        .get("userType")
        .and_then(Value::as_i64)
        .and_then(|value| i16::try_from(value).ok())
        .unwrap_or(2);
    let template_code = payload
        .get("templateCode")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request("templateCode is required"))?;
    let params = payload
        .get("templateParams")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let template = sqlx::query_as::<_, NotifyTemplateRow>(
        "SELECT id, code, nickname, content, type AS type_
         FROM system_notify_template
         WHERE code=$1 AND status=0 AND deleted=0
         ORDER BY id
         LIMIT 1",
    )
    .bind(template_code)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get notify template"))?
    .ok_or_else(|| AppError::not_found("notify template not found"))?;
    let content = render_template(&template.content, &params);
    sqlx::query(
        "INSERT INTO system_notify_message
         (id, user_id, user_type, template_id, template_code, template_nickname,
          template_content, template_type, template_params, read_status, creator,
          create_time, updater, update_time, deleted, tenant_id)
         VALUES (nextval('system_notify_message_seq'), $1, $2, $3, $4, $5,
                 $6, $7, $8, false, $9, now(), $9, now(), 0, $10)",
    )
    .bind(user_id)
    .bind(user_type)
    .bind(template.id)
    .bind(template.code)
    .bind(template.nickname)
    .bind(content)
    .bind(template.type_)
    .bind(params.to_string())
    .bind(&user.username)
    .bind(tenant_id)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to create notify message"))?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(sqlx::FromRow)]
struct NotifyTemplateRow {
    id: i64,
    code: String,
    nickname: String,
    content: String,
    type_: i16,
}

async fn message_list(
    pool: &PgPool,
    user_id: i64,
    tenant_id: i64,
    unread_only: bool,
    limit: i64,
) -> Result<Vec<Value>, AppError> {
    let read_filter = if unread_only {
        "AND read_status=false"
    } else {
        ""
    };
    let sql = format!(
        "SELECT jsonb_build_object(
             'id', id,
             'userId', user_id,
             'userType', user_type,
             'templateId', template_id,
             'templateCode', template_code,
             'templateNickname', template_nickname,
             'templateContent', template_content,
             'templateType', template_type,
             'templateParams', template_params,
             'readStatus', read_status,
             'readTime', read_time,
             'createTime', create_time
         )
         FROM system_notify_message
         WHERE user_id=$1 AND tenant_id=$2 AND deleted=0 {read_filter}
         ORDER BY create_time DESC, id DESC
         LIMIT $3"
    );
    // read_filter 是内部二选一的静态片段（未读过滤），非用户输入；
    // sqlx 0.9 要求动态 SQL 显式声明已审计。
    sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list notify messages"))
}

async fn yudao_user_id(pool: &PgPool, user: &CurrentUser) -> Result<i64, AppError> {
    let user_uuid =
        Uuid::parse_str(&user.user_id).map_err(|_| AppError::bad_request("invalid user id"))?;
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM system_users
         WHERE md5('yudao-user:' || id::text)::uuid = $1 AND deleted = 0",
    )
    .bind(user_uuid)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::not_found("user not found"))
}

fn current_tenant_id(user: &CurrentUser) -> Result<i64, AppError> {
    user.tenant_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("tenant is required"))
}

fn split_i64_ids(params: &HashMap<String, String>) -> Vec<i64> {
    params
        .iter()
        .filter(|(key, _)| key.as_str() == "id" || key.as_str() == "ids" || key.as_str() == "ids[]")
        .flat_map(|(_, value)| value.split(','))
        .filter_map(|value| value.trim().parse::<i64>().ok())
        .collect()
}

fn parse_i64(value: &Value, key: &str) -> Result<i64, AppError> {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        })
        .ok_or_else(|| AppError::bad_request(format!("{key} is required")))
}

fn render_template(template: &str, params: &Value) -> String {
    let Some(object) = params.as_object() else {
        return template.to_owned();
    };
    object
        .iter()
        .fold(template.to_owned(), |content, (key, value)| {
            let rendered = render_value(value);
            content
                .replace(&format!("{{{key}}}"), &rendered)
                .replace(&format!("${{{key}}}"), &rendered)
        })
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => compact_json(value),
    }
}

fn compact_json(value: &Value) -> String {
    match value {
        Value::Object(object) => Value::Object(Map::from_iter(object.clone())).to_string(),
        _ => value.to_string(),
    }
}
