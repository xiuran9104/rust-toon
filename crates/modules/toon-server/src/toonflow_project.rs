use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::{
    ToonState,
    shared::{affected, require},
    toonflow::ToonflowProject,
};

#[derive(Debug, Deserialize)]
pub struct ProjectIdRequest {
    id: i64,
}

pub async fn get_single_project(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectIdRequest>,
) -> Result<Json<ApiResponse<Vec<ToonflowProject>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, ToonflowProject>(r#"SELECT id,project_type,chat_model,image_model,image_quality,video_model,name,intro,type as type_,art_style,director_manual,mode,video_ratio,create_time,update_time FROM toonflow.projects WHERE id=$1"#)
        .bind(request.id).fetch_all(&state.pool).await.map_err(|_| AppError::internal("failed to get project"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRefRequest {
    project_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatistics {
    role_count: i64,
    script_count: i64,
    video_count: i64,
    storyboard_count: i64,
}

pub async fn general_statistics(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectRefRequest>,
) -> Result<Json<ApiResponse<ProjectStatistics>>, AppError> {
    require(&user, "toon:project:read")?;
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT
        (SELECT count(*) FROM toonflow.assets WHERE project_id=$1 AND type='role'),
        (SELECT count(*) FROM toonflow.scripts WHERE project_id=$1),
        (SELECT count(*) FROM toonflow.videos WHERE project_id=$1),
        (SELECT count(*) FROM toonflow.storyboards WHERE project_id=$1)"#,
    )
    .bind(request.project_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get project statistics"))?;
    Ok(Json(ApiResponse::new(ProjectStatistics {
        role_count: counts.0,
        script_count: counts.1,
        video_count: counts.2,
        storyboard_count: counts.3,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectProfile {
    id: i64,
    intro: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
    art_style: Option<String>,
    video_ratio: Option<String>,
    project_type: Option<String>,
}

pub async fn update_project_profile(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UpdateProjectProfile>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let result=sqlx::query("UPDATE toonflow.projects SET intro=coalesce($2,intro),type=coalesce($3,type),art_style=coalesce($4,art_style),video_ratio=coalesce($5,video_ratio),project_type=coalesce($6,project_type),update_time=$7 WHERE id=$1")
        .bind(request.id).bind(request.intro).bind(request.type_).bind(request.art_style).bind(request.video_ratio).bind(request.project_type).bind(chrono::Utc::now().timestamp_millis())
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to update project"))?;
    affected(result.rows_affected(), "project")?;
    Ok(Json(ApiResponse::new(
        serde_json::json!({"message":"修改成功"}),
    )))
}

#[derive(Debug, Deserialize)]
pub struct ModelDetailsRequest {
    key: String,
}

pub async fn get_model_details(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ModelDetailsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    if request.key != "scriptAgent" && request.key != "productionAgent" {
        return Err(AppError::bad_request("invalid agent key"));
    }
    let deployment: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT model_config_id FROM toonflow.agent_deployments WHERE key=$1")
            .bind(request.key)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get agent model"))?;
    let model_id = deployment
        .and_then(|x| x.0)
        .ok_or_else(|| AppError::not_found("Agent 尚未绑定统一模型"))?;
    let row = sqlx::query(
        "SELECT id,name,platform,type,model,status,config FROM ai.model_configs WHERE id=$1",
    )
    .bind(model_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get AI model"))?
    .ok_or_else(|| AppError::not_found("未找到模型"))?;
    let model = serde_json::json!({"id":row.get::<i64,_>("id"),"name":row.get::<String,_>("name"),"modelName":row.get::<String,_>("model"),"model":row.get::<String,_>("model"),"platform":row.get::<String,_>("platform"),"type":row.get::<String,_>("type"),"status":row.get::<i32,_>("status"),"config":row.get::<Value,_>("config")});
    Ok(Json(ApiResponse::new(model)))
}
