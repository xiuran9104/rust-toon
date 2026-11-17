use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ToonState, shared::require};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAgentRequest {
    project_id: i64,
    agent_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePlanRequest {
    project_id: i64,
    agent_type: String,
    data: Value,
}

#[derive(Deserialize)]
pub struct UpdatePlanRequest {
    id: i64,
    data: Value,
}

fn validate_script_agent(agent: &str) -> Result<(), AppError> {
    if agent == "scriptAgent" {
        Ok(())
    } else {
        Err(AppError::bad_request("agentType 必须是 scriptAgent"))
    }
}

async fn scripts(pool: &sqlx::PgPool, project_id: i64) -> Result<Value, AppError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id,name,content FROM toonflow.scripts WHERE project_id=$1 ORDER BY create_time,id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load scripts"))?;
    Ok(json!(
        rows.into_iter()
            .map(|(id, name, content)| json!({"id":id,"name":name,"content":content}))
            .collect::<Vec<_>>()
    ))
}

pub(crate) async fn load_plan_data(
    pool: &sqlx::PgPool,
    project_id: i64,
) -> Result<Value, AppError> {
    let row: Option<(i64, Value)> = sqlx::query_as(
        "SELECT id,data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id IS NULL AND key='scriptAgent'",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to load plan data"))?;
    let (id, mut data) = if let Some(row) = row {
        row
    } else {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.agent_work_data(project_id,episodes_id,key,data,create_time,update_time) VALUES($1,NULL,'scriptAgent',$2,$3,$3) RETURNING id",
        )
        .bind(project_id)
        .bind(json!({"storySkeleton":"","adaptationStrategy":""}))
        .bind(now_ms())
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to create plan data"))?;
        (id, json!({"storySkeleton":"","adaptationStrategy":""}))
    };
    data["script"] = scripts(pool, project_id).await?;
    Ok(json!({"id": id, "data": data}))
}

pub async fn get_plan(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectAgentRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    validate_script_agent(&request.agent_type)?;
    Ok(Json(ApiResponse::new(
        load_plan_data(&state.pool, request.project_id).await?,
    )))
}

pub async fn set_plan(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SavePlanRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    validate_script_agent(&request.agent_type)?;
    let skeleton = request
        .data
        .get("storySkeleton")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let strategy = request
        .data
        .get("adaptationStrategy")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id:i64=sqlx::query_scalar("INSERT INTO toonflow.agent_work_data(project_id,episodes_id,key,data,create_time,update_time)VALUES($1,NULL,$2,$3,$4,$4) ON CONFLICT(project_id,key) WHERE episodes_id IS NULL DO UPDATE SET data=excluded.data,update_time=excluded.update_time RETURNING id").bind(request.project_id).bind(&request.agent_type).bind(json!({"storySkeleton":skeleton,"adaptationStrategy":strategy})).bind(now_ms()).fetch_one(&state.pool).await.map_err(|_|AppError::internal("failed to save plan data"))?;
    if let Some(items) = request.data.get("script").and_then(Value::as_array) {
        for item in items.iter() {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("未命名剧本");
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // A script identifier carried by the plan is honoured so the plan
            // can update its own scripts; otherwise PostgreSQL allocates one.
            let script_id = item.get("id").and_then(Value::as_i64);
            if let Some(script_id) = script_id
                && let Some(existing_project_id) =
                    sqlx::query_scalar::<_, i64>(
                        "SELECT project_id FROM toonflow.scripts WHERE id=$1",
                    )
                    .bind(script_id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to validate agent script"))?
                && existing_project_id != request.project_id
            {
                return Err(AppError::bad_request("script 不属于当前项目"));
            }
            sqlx::query("INSERT INTO toonflow.scripts(id,name,content,project_id,create_time)VALUES(coalesce($1::bigint, nextval('toonflow.scripts_id_seq')),$2,$3,$4,$5) ON CONFLICT(id) DO UPDATE SET name=excluded.name,content=excluded.content WHERE toonflow.scripts.project_id=excluded.project_id")
                .bind(script_id).bind(name).bind(content).bind(request.project_id).bind(now_ms()).execute(&state.pool).await.map_err(|_|AppError::internal("failed to save agent script"))?;
        }
    }
    Ok(Json(ApiResponse::new(json!({"id":id}))))
}

pub async fn update_plan(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UpdatePlanRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT project_id,key FROM toonflow.agent_work_data WHERE id=$1")
            .bind(request.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load plan data"))?;
    let (project_id, key) = row.ok_or_else(|| AppError::not_found("plan data not found"))?;
    validate_script_agent(&key)?;
    sqlx::query("UPDATE toonflow.agent_work_data SET data=$2,update_time=$3 WHERE id=$1")
        .bind(request.id)
        .bind(&request.data)
        .bind(now_ms())
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to update plan data"))?;
    if let Some(items) = request.data.get("script").and_then(Value::as_array) {
        for item in items {
            if let (Some(id), Some(content)) = (
                item.get("id").and_then(Value::as_i64),
                item.get("content").and_then(Value::as_str),
            ) {
                sqlx::query("UPDATE toonflow.scripts SET content=$3 WHERE id=$1 AND project_id=$2")
                    .bind(id)
                    .bind(project_id)
                    .bind(content)
                    .execute(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to update script"))?;
            }
        }
    }
    Ok(Json(ApiResponse::new(json!(true))))
}
