use axum::{Json, Router, extract::State, routing::get};
use chrono::{DateTime, Utc};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::{CurrentUser, Permission};
use rust_toon_framework_web::AppError;
use rust_toon_system_api::AuditLogSummary;
use serde_json::Value;
use sqlx::FromRow;
use tracing::warn;
use uuid::Uuid;

use crate::SystemState;

pub fn routes() -> Router<SystemState> {
    Router::new().route("/system/audit-logs", get(list_audit_logs))
}

pub async fn record(
    state: &SystemState,
    actor: Option<&CurrentUser>,
    action: &str,
    target_type: &str,
    target_id: Option<String>,
    detail: Value,
) {
    let (actor_user_id, actor_username, tenant_id) = actor
        .map(|user| {
            (
                Uuid::parse_str(&user.user_id).ok(),
                Some(user.username.clone()),
                user.tenant_id
                    .as_deref()
                    .and_then(|id| id.parse::<i64>().ok()),
            )
        })
        .unwrap_or((None, None, None));

    let trace_id = Uuid::new_v4().to_string();
    let biz_id = target_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
        .unwrap_or(0);
    let action_detail = detail.to_string();

    if let Err(error) = sqlx::query(
        "INSERT INTO system_operate_log
         (id, trace_id, user_id, user_type, type, sub_type, biz_id, action, success,
          extra, request_method, request_url, user_ip, user_agent, creator,
          create_time, updater, update_time, deleted, tenant_id)
         VALUES (
             nextval('system_operate_log_seq'), $1,
             COALESCE((SELECT id FROM system_users
                       WHERE identity_uuid = $2 AND deleted = 0), 0),
             2, $3, $4, $5, $6, true, '', '', '', '', '', COALESCE($7, 'system'),
             now(), COALESCE($7, 'system'), now(), 0, COALESCE($8, 0)
         )",
    )
    .bind(trace_id)
    .bind(actor_user_id)
    .bind(target_type)
    .bind(action)
    .bind(biz_id)
    .bind(action_detail)
    .bind(actor_username)
    .bind(tenant_id)
    .execute(&state.pool)
    .await
    {
        warn!(%error, action, target_type, "failed to write audit log");
    }
}

pub async fn record_login(
    state: &SystemState,
    user_id: Option<Uuid>,
    username: &str,
    log_type: i64,
    result: i16,
) {
    if let Err(error) = sqlx::query(
        "INSERT INTO system_login_log
         (id, log_type, trace_id, user_id, user_type, username, result, user_ip,
          user_agent, creator, create_time, updater, update_time, deleted, tenant_id)
         SELECT nextval('system_login_log_seq'), $1, $2, COALESCE(users.id, 0), 2,
                COALESCE(NULLIF($3, ''), users.username, ''), $4, '', '',
                COALESCE(users.username, $3, ''), now(), COALESCE(users.username, $3, ''),
                now(), 0, COALESCE(users.tenant_id, 0)
         FROM (SELECT 1) seed
         LEFT JOIN system_users users
           ON users.identity_uuid = $5 AND users.deleted = 0",
    )
    .bind(log_type)
    .bind(Uuid::new_v4().to_string())
    .bind(username)
    .bind(result)
    .bind(user_id)
    .execute(&state.pool)
    .await
    {
        warn!(%error, log_type, username, "failed to write login log");
    }
}

#[derive(FromRow)]
struct AuditLogRow {
    id: i64,
    actor_user_id: Option<i64>,
    actor_username: Option<String>,
    action: String,
    target_type: String,
    target_id: Option<String>,
    detail: Value,
    created_at: DateTime<Utc>,
}

async fn list_audit_logs(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<AuditLogSummary>>>, AppError> {
    let permission = Permission::new("system:operate-log:query")
        .map_err(|_| AppError::internal("invalid policy"))?;
    if !user.role_codes.iter().any(|role| role == "super_admin") && !user.can(&permission) {
        return Err(AppError::forbidden("permission denied"));
    }

    let rows = sqlx::query_as::<_, AuditLogRow>(
        "SELECT logs.id,
                NULLIF(logs.user_id, 0) AS actor_user_id,
                users.username AS actor_username,
                logs.sub_type AS action,
                logs.type AS target_type,
                NULLIF(logs.biz_id, 0)::text AS target_id,
                CASE WHEN logs.action ~ '^\\s*[\\[{]'
                     THEN logs.action::jsonb ELSE to_jsonb(logs.action) END AS detail,
                logs.create_time AT TIME ZONE 'UTC' AS created_at
         FROM system_operate_log logs
         LEFT JOIN system_users users ON users.id = logs.user_id AND users.deleted = 0
         WHERE logs.deleted = 0
         ORDER BY logs.create_time DESC
         LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list audit logs"))?;

    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|row| AuditLogSummary {
                id: row.id.to_string(),
                actor_user_id: row.actor_user_id.map(|id| id.to_string()),
                actor_username: row.actor_username,
                action: row.action,
                target_type: row.target_type,
                target_id: row.target_id,
                detail: row.detail,
                created_at: row.created_at.to_rfc3339(),
            })
            .collect(),
    )))
}
