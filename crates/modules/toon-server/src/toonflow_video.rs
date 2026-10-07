use crate::{
    ToonState, ai_client,
    shared::require,
    toonflow_episode_renders::{ensure_project_access, ensure_script_in_project},
    toonflow_video_continuity::{
        FRAME_POLICY_OWN, FRAME_POLICY_PREVIOUS_TAIL, FrameApplication, TrackTransitionSettings,
        apply_frame_policy, load_track_settings, persist_generation_context,
        prompt_with_transition_context, resolve_previous_track_id, validate_continuity_mode,
        validate_frame_policy, validate_transition_type,
    },
};
use axum::{Json, extract::State, http::StatusCode};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};
use tokio::{sync::OwnedSemaphorePermit, task::JoinSet};

type StoryboardAssetMediaRow = (i64, i64, String, String, Option<String>, Option<String>);

static PROVIDER_VIDEO_SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
static VIDEO_GENERATION_TASK_SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn provider_video_semaphore() -> Arc<tokio::sync::Semaphore> {
    PROVIDER_VIDEO_SEMAPHORE
        .get_or_init(|| {
            let concurrency = std::env::var("TOON_PROVIDER_VIDEO_CONCURRENCY")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| (1..=32).contains(value))
                .unwrap_or(2);
            Arc::new(tokio::sync::Semaphore::new(concurrency))
        })
        .clone()
}

async fn acquire_provider_video_slot() -> Option<OwnedSemaphorePermit> {
    provider_video_semaphore().acquire_owned().await.ok()
}

fn video_generation_task_semaphore() -> Arc<tokio::sync::Semaphore> {
    VIDEO_GENERATION_TASK_SEMAPHORE
        .get_or_init(|| {
            let concurrency = std::env::var("TOON_VIDEO_GENERATION_TASK_CONCURRENCY")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| (1..=64).contains(value))
                .unwrap_or(4);
            Arc::new(tokio::sync::Semaphore::new(concurrency))
        })
        .clone()
}

fn try_acquire_video_generation_task() -> Result<OwnedSemaphorePermit, AppError> {
    video_generation_task_semaphore()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                429,
                "视频生成准备任务已达到当前节点上限，请稍后重试",
            )
        })
}

async fn acquire_video_generation_task() -> Option<OwnedSemaphorePermit> {
    video_generation_task_semaphore().acquire_owned().await.ok()
}

async fn ensure_project_script_access(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: i64,
    script_id: i64,
) -> Result<(), AppError> {
    ensure_project_access(pool, user, project_id).await?;
    ensure_script_in_project(pool, project_id, script_id).await
}

async fn ensure_track_access(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    track_id: i64,
) -> Result<(i64, Option<i64>), AppError> {
    let track: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT project_id,script_id FROM toonflow.video_tracks WHERE id=$1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| AppError::internal("failed to authorize video track"))?;
    let (project_id, script_id) =
        track.ok_or_else(|| AppError::not_found("video track not found"))?;
    ensure_project_access(pool, user, project_id).await?;
    if let Some(script_id) = script_id {
        ensure_script_in_project(pool, project_id, script_id).await?;
    }
    Ok((project_id, script_id))
}

async fn ensure_track_in_context(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: i64,
    script_id: i64,
    track_id: i64,
) -> Result<(), AppError> {
    ensure_project_script_access(pool, user, project_id, script_id).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM toonflow.video_tracks
           WHERE id=$1 AND project_id=$2 AND script_id=$3
         )",
    )
    .bind(track_id)
    .bind(project_id)
    .bind(script_id)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::internal("failed to authorize video track"))?;
    if !exists {
        return Err(AppError::not_found("video track not found"));
    }
    Ok(())
}

async fn ensure_resource_projects_access(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    requested_ids: &[i64],
    rows: Vec<(i64, i64)>,
    resource_name: &str,
) -> Result<(), AppError> {
    let mut requested_ids = requested_ids.to_vec();
    requested_ids.sort_unstable();
    requested_ids.dedup();
    let mut found_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
    found_ids.sort_unstable();
    found_ids.dedup();
    if requested_ids != found_ids {
        return Err(AppError::not_found(format!("{resource_name} not found")));
    }
    let mut project_ids = rows.into_iter().map(|row| row.1).collect::<Vec<_>>();
    project_ids.sort_unstable();
    project_ids.dedup();
    for project_id in project_ids {
        ensure_project_access(pool, user, project_id).await?;
    }
    Ok(())
}

fn ensure_internal_references_in_project(value: &Value, project_id: i64) -> Result<(), AppError> {
    let expected_prefix = format!("toonflow/{project_id}/assets/");
    let references = value.as_array().into_iter().flatten().filter_map(|item| {
        item.as_str()
            .or_else(|| item.get("src").and_then(Value::as_str))
    });
    if references
        .filter_map(crate::toonflow_storage::asset_object_key)
        .any(|key| !key.starts_with(&expected_prefix))
    {
        return Err(AppError::bad_request("视频参考素材包含其他项目的对象路径"));
    }
    Ok(())
}

async fn store_generated_video(
    pool: &sqlx::PgPool,
    video_id: i64,
    project_id: i64,
    provider_url: &str,
) -> bool {
    match crate::toonflow_storage::persist_remote_video_for_row(
        pool,
        provider_url,
        project_id,
        video_id,
    )
    .await
    {
        Ok(file_path) => {
            let updated = crate::toonflow_video_quality::enqueue(
                pool, video_id, project_id, &file_path, false,
            )
            .await;
            match updated {
                Ok(_) => crate::toonflow_video_quality::wait_for_result(pool, video_id).await,
                Err(error) => {
                    // An autocommit acknowledgement can be lost after the row
                    // update became visible. The cleanup worker rechecks live
                    // references before DELETE, so queue instead of deleting
                    // the possibly committed file here.
                    crate::toonflow_storage::record_cleanup_failure(
                        pool,
                        &file_path,
                        "video_generation_update",
                        Some(video_id),
                        &error.to_string(),
                    )
                    .await;
                    tracing::error!(video_id, %error, "failed to finalize persisted video");
                    // If the commit acknowledgement was lost, the durable job
                    // may already own this video. Do not overwrite its verdict.
                    let _ = sqlx::query("UPDATE toonflow.videos SET state='生成失败',error_reason=$2 WHERE id=$1 AND state='生成中' AND NOT EXISTS(SELECT 1 FROM toonflow.distributed_jobs j WHERE j.kind='toon.video_quality' AND j.payload->>'videoId'=$1::text AND j.state IN ('queued','running','retry','succeeded'))")
                        .bind(video_id).bind(format!("无法提交视频质检：{error}")).execute(pool).await;
                    false
                }
            }
        }
        Err(reason) => {
            let _ = sqlx::query(
                "UPDATE toonflow.videos
                 SET state='生成失败',error_reason=$2
                 WHERE id=$1 AND state='生成中'",
            )
            .bind(video_id)
            .bind(format!("归档生成视频失败：{reason}"))
            .execute(pool)
            .await;
            false
        }
    }
}

/// Resume provider polling for videos that were mid-generation when the
/// Gateway stopped. The provider task handle was persisted at submit time, so
/// a restart continues the paid task instead of failing or resubmitting it.
/// Called once during Gateway startup after `repair_gateway_interrupted_state`.
pub async fn resume_interrupted_video_generations(pool: &sqlx::PgPool) -> u64 {
    let rows = sqlx::query_as::<_, (i64, i64, String, String)>(
        "SELECT id,project_id,
                generation_context->'provider'->>'model',
                generation_context->'provider'->>'taskId'
         FROM toonflow.videos
         WHERE state='生成中'
           AND generation_context->'provider'->>'taskId' IS NOT NULL
           AND generation_context->'provider'->>'model' IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let resumed = rows.len() as u64;
    for (video_id, project_id, model, task_id) in rows {
        let pool = pool.clone();
        tokio::spawn(async move {
            let Some(_generation_task_permit) = acquire_video_generation_task().await else {
                mark_video_generation_failed(&pool, video_id, "视频生成任务控制不可用".into())
                    .await;
                return;
            };
            match ai_client::video_poll_task(&pool, &model, &task_id).await {
                Ok(url) => {
                    store_generated_video(&pool, video_id, project_id, &url).await;
                }
                Err(reason) => {
                    mark_video_generation_failed(&pool, video_id, reason).await;
                }
            }
        });
    }
    resumed
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAssetsRequest {
    assets_ids: Vec<i64>,
}

pub async fn audio_bind_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<AudioAssetsRequest>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    let resource_projects = sqlx::query_as::<_, (i64, i64)>(
        "SELECT id,project_id FROM toonflow.assets WHERE id=ANY($1)",
    )
    .bind(&req.assets_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to authorize bound audio assets"))?;
    ensure_resource_projects_access(
        &state.pool,
        &user,
        &req.assets_ids,
        resource_projects,
        "asset",
    )
    .await?;
    let rows=sqlx::query_as::<_,(i64,String,String,Option<String>,i64)>("SELECT audio.id,audio.prompt,audio.type,i.file_path,b.asset_role_id FROM toonflow.asset_audio_bindings b JOIN toonflow.assets role ON role.id=b.asset_role_id JOIN toonflow.assets audio ON audio.id=b.asset_audio_id AND audio.project_id=role.project_id LEFT JOIN toonflow.images i ON i.id=audio.image_id WHERE b.asset_role_id=ANY($1) ORDER BY audio.id").bind(req.assets_ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list bound audio assets"))?;
    Ok(Json(ApiResponse::new(rows.into_iter().map(|row|json!({"fileType":"audio","sources":"assets","src":row.3,"id":row.0,"prompt":row.1,"type":row.2,"assetsRoleId":row.4})).collect())))
}

#[derive(Deserialize)]
pub struct FileItem {
    id: i64,
    sources: String,
}

#[derive(Deserialize)]
pub struct FileUrlRequest {
    items: Vec<FileItem>,
}

pub async fn file_urls(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<FileUrlRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:read")?;
    let storyboard_ids = req
        .items
        .iter()
        .filter(|item| item.sources == "storyboard")
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let asset_ids = req
        .items
        .iter()
        .filter(|item| item.sources == "assets")
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let mut result = serde_json::Map::new();
    if !storyboard_ids.is_empty() {
        let resource_projects = sqlx::query_as::<_, (i64, i64)>(
            "SELECT id,project_id FROM toonflow.storyboards WHERE id=ANY($1)",
        )
        .bind(&storyboard_ids)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to authorize storyboard files"))?;
        ensure_resource_projects_access(
            &state.pool,
            &user,
            &storyboard_ids,
            resource_projects,
            "storyboard",
        )
        .await?;
        let rows = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT id,file_path FROM toonflow.storyboards WHERE id=ANY($1)",
        )
        .bind(storyboard_ids)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to load storyboard files"))?;
        for (id, path) in rows {
            result.insert(format!("{id}:storyboard"), json!(path.unwrap_or_default()));
        }
    }
    if !asset_ids.is_empty() {
        let resource_projects = sqlx::query_as::<_, (i64, i64)>(
            "SELECT id,project_id FROM toonflow.assets WHERE id=ANY($1)",
        )
        .bind(&asset_ids)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to authorize asset files"))?;
        ensure_resource_projects_access(&state.pool, &user, &asset_ids, resource_projects, "asset")
            .await?;
        let rows=sqlx::query_as::<_,(i64,Option<String>)>("SELECT a.id,i.file_path FROM toonflow.assets a LEFT JOIN toonflow.images i ON i.id=a.image_id WHERE a.id=ANY($1)").bind(asset_ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to load asset files"))?;
        for (id, path) in rows {
            result.insert(format!("{id}:assets"), json!(path.unwrap_or_default()));
        }
    }
    Ok(Json(ApiResponse::new(json!({"data":result}))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackRequest {
    project_id: i64,
    script_id: i64,
    duration: Option<i32>,
}
pub async fn add_track(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<TrackRequest>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let id = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO toonflow.video_tracks(id,project_id,script_id,duration,state,sort_order) VALUES($1,$2,$3,$4,'未生成',coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$2 AND script_id=$3),0))").bind(id).bind(req.project_id).bind(req.script_id).bind(req.duration).execute(&state.pool).await.map_err(|_|AppError::internal("failed to add video track"))?;
    Ok(Json(ApiResponse::new(id)))
}
#[derive(Deserialize)]
pub struct Id {
    pub(crate) id: i64,
}
pub async fn delete_track(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:delete")?;
    let (project_id, script_id) = ensure_track_access(&state.pool, &user, req.id).await?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    sqlx::query(
        "UPDATE toonflow.storyboards SET track_id=NULL WHERE track_id=$1 AND project_id=$2",
    )
    .bind(req.id)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to unbind track"))?;
    let deleted = sqlx::query("DELETE FROM toonflow.video_tracks WHERE id=$1 AND project_id=$2")
        .bind(req.id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete track"))?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("video track not found"));
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    if let Some(script_id) = script_id {
        crate::toonflow_scene_transitions::apply_track_transition_defaults(
            &state.pool,
            project_id,
            script_id,
        )
        .await?;
    }
    Ok(Json(ApiResponse::new(json!({"message":"视频段删除成功"}))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workbench {
    project_id: i64,
    script_id: i64,
}

#[derive(Debug, FromRow)]
struct WorkbenchStoryboardRow {
    id: i64,
    track_id: Option<i64>,
    file_path: Option<String>,
    prompt: String,
    video_desc: Option<String>,
    index: i32,
    flow_id: Option<i64>,
    scene_key: Option<String>,
    scene_master_name: Option<String>,
    scene_state_name: Option<String>,
    scene_state_key: Option<String>,
    scene_state_id: Option<i64>,
    generated_scene_state_id: Option<i64>,
    scene_generation_context: Value,
    scene_master_status: Option<String>,
    scene_master_revision: Option<i32>,
    scene_state_revision: Option<i32>,
}

impl WorkbenchStoryboardRow {
    fn consistency_status(&self) -> &'static str {
        crate::toonflow_scene_consistency::storyboard_consistency_status(
            self.scene_key.as_deref(),
            self.scene_state_id,
            self.file_path.as_deref(),
            self.generated_scene_state_id,
            &self.scene_generation_context,
            self.scene_master_status.as_deref(),
            self.scene_master_revision,
            self.scene_state_revision,
        )
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "trackId": self.track_id,
            "src": self.file_path,
            "prompt": self.prompt,
            "videoDesc": self.video_desc,
            "index": self.index,
            "flowId": self.flow_id,
            "sceneKey": self.scene_key,
            "sceneMasterName": self.scene_master_name,
            "sceneStateName": self.scene_state_name,
            "sceneStateKey": self.scene_state_key,
            "sceneStateId": self.scene_state_id,
            "generatedSceneStateId": self.generated_scene_state_id,
            "sceneConsistencyStatus": self.consistency_status(),
        })
    }
}
pub async fn video_list(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Workbench>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let rows=sqlx::query_as::<_,(i64,Option<String>,String,Option<String>,Option<i64>,Value)>("SELECT id,file_path,coalesce(state,''),error_reason,video_track_id,generation_context FROM toonflow.videos WHERE project_id=$1 AND script_id=$2 ORDER BY id DESC").bind(req.project_id).bind(req.script_id).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list videos"))?;
    Ok(Json(ApiResponse::new(rows.into_iter().map(|r|json!({"id":r.0,"filePath":r.1,"src":r.1,"state":r.2,"errorReason":r.3,"videoTrackId":r.4,"generationContext":r.5})).collect())))
}
#[derive(Deserialize)]
pub struct Prompt {
    id: i64,
    prompt: Option<String>,
}
pub async fn update_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Prompt>,
) -> Result<Json<ApiResponse<&'static str>>, AppError> {
    require(&user, "toon:scene:update")?;
    let (project_id, _) = ensure_track_access(&state.pool, &user, req.id).await?;
    let updated =
        sqlx::query("UPDATE toonflow.video_tracks SET prompt=$2 WHERE id=$1 AND project_id=$3")
            .bind(req.id)
            .bind(req.prompt)
            .bind(project_id)
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to update prompt"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("video track not found"));
    }
    Ok(Json(ApiResponse::new("更新成功")))
}
#[derive(Deserialize)]
pub struct Duration {
    id: i64,
    duration: Option<i32>,
}
pub async fn update_duration(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Duration>,
) -> Result<Json<ApiResponse<&'static str>>, AppError> {
    require(&user, "toon:scene:update")?;
    let (project_id, _) = ensure_track_access(&state.pool, &user, req.id).await?;
    let updated =
        sqlx::query("UPDATE toonflow.video_tracks SET duration=$2 WHERE id=$1 AND project_id=$3")
            .bind(req.id)
            .bind(req.duration)
            .bind(project_id)
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to update duration"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("video track not found"));
    }
    Ok(Json(ApiResponse::new("更新成功")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuityMode {
    id: i64,
    continuity_mode: String,
}

pub async fn update_continuity_mode(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ContinuityMode>,
) -> Result<Json<ApiResponse<&'static str>>, AppError> {
    require(&user, "toon:scene:update")?;
    if !matches!(req.continuity_mode.as_str(), "auto" | "always" | "never") {
        return Err(AppError::bad_request("无效的视频衔接模式"));
    }
    let (project_id, script_id) = ensure_track_access(&state.pool, &user, req.id).await?;
    let frame_policy = match req.continuity_mode.as_str() {
        "always" => FRAME_POLICY_PREVIOUS_TAIL,
        // The legacy "auto" mode was heuristic and could silently connect unrelated shots.
        // Keep accepting it for old clients, but use the safe per-track storyboard frame.
        "auto" | "never" => FRAME_POLICY_OWN,
        _ => unreachable!(),
    };
    let previous_track_id = if frame_policy == FRAME_POLICY_PREVIOUS_TAIL {
        let script_id = script_id.ok_or_else(|| AppError::bad_request("视频轨道未关联剧本"))?;
        resolve_previous_track_id(&state.pool, project_id, script_id, req.id, None)
            .await
            .map_err(AppError::bad_request)?
    } else {
        None
    };
    let updated = sqlx::query(
        "UPDATE toonflow.video_tracks
         SET continuity_mode=$2,frame_policy=$3,previous_track_id=$4,transition_source='manual'
         WHERE id=$1 AND project_id=$5",
    )
    .bind(req.id)
    .bind(req.continuity_mode)
    .bind(frame_policy)
    .bind(previous_track_id)
    .bind(project_id)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update continuity mode"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("video track not found"));
    }
    Ok(Json(ApiResponse::new("更新成功")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionSettingsRequest {
    id: i64,
    transition_type: String,
    frame_policy: String,
    previous_track_id: Option<i64>,
    /// Edit-time transition length; only dissolve/audio_bridge render an
    /// overlap. Missing values keep the column default.
    #[serde(default)]
    transition_duration_ms: Option<i32>,
    #[serde(default)]
    trim_start_ms: Option<i32>,
    #[serde(default)]
    trim_end_ms: Option<i32>,
}

pub async fn update_transition_settings(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<TransitionSettingsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    if !validate_transition_type(&req.transition_type) {
        return Err(AppError::bad_request("无效的入场过渡类型"));
    }
    if !validate_frame_policy(&req.frame_policy) {
        return Err(AppError::bad_request("无效的首帧来源策略"));
    }
    let transition_duration_ms = req.transition_duration_ms.unwrap_or(600);
    if !(0..=10_000).contains(&transition_duration_ms) {
        return Err(AppError::bad_request("过渡时长必须在 0-10000 毫秒之间"));
    }
    let trim_start_ms = req.trim_start_ms.unwrap_or(0);
    if trim_start_ms < 0 {
        return Err(AppError::bad_request("裁切起点不能为负数"));
    }
    if let Some(trim_end_ms) = req.trim_end_ms {
        if trim_end_ms <= trim_start_ms {
            return Err(AppError::bad_request("裁切终点必须大于裁切起点"));
        }
    }
    let (project_id, script_id) = ensure_track_access(&state.pool, &user, req.id).await?;
    let script_id = script_id.ok_or_else(|| AppError::bad_request("视频轨道未关联剧本"))?;
    let previous_track_id = if req.frame_policy == FRAME_POLICY_PREVIOUS_TAIL {
        resolve_previous_track_id(
            &state.pool,
            project_id,
            script_id,
            req.id,
            req.previous_track_id,
        )
        .await
        .map_err(AppError::bad_request)?
        .ok_or_else(|| AppError::bad_request("当前轨道前没有可用于续接的视频轨道"))?
        .into()
    } else {
        None
    };
    let updated = sqlx::query(
        "UPDATE toonflow.video_tracks
         SET transition_type=$2,frame_policy=$3,previous_track_id=$4,
             transition_duration_ms=$5,trim_start_ms=$6,trim_end_ms=$7,
             transition_source='manual',continuity_mode=CASE WHEN $3='previous_tail' THEN 'always' ELSE 'never' END
         WHERE id=$1 AND project_id=$8 AND script_id=$9",
    )
    .bind(req.id)
    .bind(&req.transition_type)
    .bind(&req.frame_policy)
    .bind(previous_track_id)
    .bind(transition_duration_ms)
    .bind(trim_start_ms)
    .bind(req.trim_end_ms)
    .bind(project_id)
    .bind(script_id)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update video transition settings"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("video track not found"));
    }
    Ok(Json(ApiResponse::new(json!({
        "id": req.id,
        "transitionType": req.transition_type,
        "framePolicy": req.frame_policy,
        "previousTrackId": previous_track_id,
        "transitionDurationMs": transition_duration_ms,
        "trimStartMs": trim_start_ms,
        "trimEndMs": req.trim_end_ms,
        "transitionSource": "manual",
    }))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Select {
    pub(crate) track_id: i64,
    pub(crate) video_id: i64,
}
pub async fn select_video(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Select>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let project_id: i64 =
        sqlx::query_scalar("SELECT project_id FROM toonflow.video_tracks WHERE id=$1")
            .bind(req.track_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load video track"))?
            .ok_or_else(|| AppError::not_found("video track not found"))?;
    ensure_project_access(&state.pool, &user, project_id).await?;
    let selected = sqlx::query(
        "UPDATE toonflow.video_tracks track
         SET video_id=video.id
         FROM toonflow.videos video
         WHERE track.id=$1 AND video.id=$2 AND track.project_id=$3
           AND video.video_track_id=track.id
           AND video.project_id=track.project_id
           AND video.script_id=track.script_id
           AND video.state='生成成功'
           AND coalesce(video.file_path,'')<>''",
    )
    .bind(req.track_id)
    .bind(req.video_id)
    .bind(project_id)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to select video"))?;
    if selected.rows_affected() != 1 {
        return Err(AppError::bad_request(
            "视频不存在、尚未生成成功或不属于当前轨道",
        ));
    }
    Ok(Json(ApiResponse::new(json!({"message":"视频选择成功"}))))
}
pub async fn delete_video(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:delete")?;
    let project_id: i64 = sqlx::query_scalar("SELECT project_id FROM toonflow.videos WHERE id=$1")
        .bind(req.id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to authorize video"))?
        .ok_or_else(|| AppError::not_found("video not found"))?;
    ensure_project_access(&state.pool, &user, project_id).await?;

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    let file_path: Option<String> = sqlx::query_scalar(
        "SELECT file_path FROM toonflow.videos WHERE id=$1 AND project_id=$2 FOR UPDATE",
    )
    .bind(req.id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load video"))?
    .ok_or_else(|| AppError::not_found("video not found"))?;
    sqlx::query(
        "UPDATE toonflow.video_tracks SET video_id=NULL WHERE video_id=$1 AND project_id=$2",
    )
    .bind(req.id)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to unbind video"))?;
    let cached_frames: Vec<String> = sqlx::query_scalar(
        "SELECT file_path FROM toonflow.video_continuity_frames
         WHERE previous_video_id=$1 AND project_id=$2",
    )
    .bind(req.id)
    .bind(project_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load continuity frame cache"))?;
    sqlx::query(
        "DELETE FROM toonflow.video_continuity_frames
         WHERE previous_video_id=$1 AND project_id=$2",
    )
    .bind(req.id)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to delete continuity frame cache"))?;
    let mut cleanup_paths = cached_frames
        .iter()
        .map(|path| (path.as_str(), "deleted_video_continuity_frame"))
        .collect::<Vec<_>>();
    if let Some(file_path) = file_path
        .as_deref()
        .filter(|file_path| !file_path.trim().is_empty())
    {
        cleanup_paths.push((file_path, "deleted_video"));
    }
    for (object_path, resource_type) in cleanup_paths {
        sqlx::query(
            "INSERT INTO toonflow.storage_cleanup_tasks(
               object_path,resource_type,resource_id,error_reason,attempts,state,create_time,update_time
             ) VALUES(
               $1,$2,$3,'视频记录已删除，等待引用感知清理',1,'pending',
               (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
               (extract(epoch FROM clock_timestamp()) * 1000)::bigint
             )",
        )
        .bind(object_path)
        .bind(resource_type)
        .bind(req.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to enqueue video object cleanup"))?;
    }
    let deleted = sqlx::query("DELETE FROM toonflow.videos WHERE id=$1 AND project_id=$2")
        .bind(req.id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete video"))?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("video not found"));
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    Ok(Json(ApiResponse::new(json!({"message":"视频删除成功"}))))
}

pub async fn generate_data(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Workbench>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    Ok(Json(ApiResponse::new(
        load_generate_data(&state.pool, req.project_id, req.script_id).await?,
    )))
}

pub(crate) async fn load_generate_data(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<Value, AppError> {
    let boards = sqlx::query_as::<_, WorkbenchStoryboardRow>(
        r#"SELECT storyboard.id,storyboard.track_id,storyboard.file_path,storyboard.prompt,
                  storyboard.video_desc,coalesce(storyboard.index,0) AS index,storyboard.flow_id,
                  storyboard.scene_key,master.name AS scene_master_name,
                  scene_state.name AS scene_state_name,scene_state.state_key AS scene_state_key,
                  storyboard.scene_state_id,storyboard.generated_scene_state_id,
                  storyboard.scene_generation_context,
                  CASE WHEN master.status='ready' AND (
                         master_image.id IS NULL OR master_image.state<>'已完成'
                         OR coalesce(master_image.file_path,'')=''
                       ) THEN 'missing_reference' ELSE master.status END AS scene_master_status,
                  master.revision AS scene_master_revision,
                  scene_state.revision AS scene_state_revision
           FROM toonflow.storyboards storyboard
           LEFT JOIN toonflow.scene_states scene_state ON scene_state.id=storyboard.scene_state_id
           LEFT JOIN toonflow.scene_masters master ON master.id=scene_state.scene_master_id
           LEFT JOIN toonflow.images master_image ON master_image.id=master.pinned_image_id
           WHERE storyboard.project_id=$1 AND storyboard.script_id=$2
           ORDER BY storyboard.index,storyboard.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to list storyboards"))?;
    let tracks=sqlx::query_as::<_,(i64,Option<String>,Option<String>,Option<i32>,Option<i64>,i32,String,String,Option<i64>,String,i32,i32,Option<i32>)>("SELECT track.id,track.prompt,track.state,track.duration,track.video_id,track.sort_order,track.transition_type,track.frame_policy,track.previous_track_id,track.transition_source,track.transition_duration_ms,track.trim_start_ms,track.trim_end_ms FROM toonflow.video_tracks track WHERE track.project_id=$1 AND track.script_id=$2 ORDER BY coalesce((SELECT min(board.index) FROM toonflow.storyboards board WHERE board.track_id=track.id),2147483647),track.sort_order,track.id").bind(project_id).bind(script_id).fetch_all(pool).await.map_err(|_|AppError::internal("failed to list tracks"))?;
    let videos=sqlx::query_as::<_,(i64,Option<String>,String,Option<String>,Option<i64>,Value)>("SELECT id,file_path,coalesce(state,''),error_reason,video_track_id,generation_context FROM toonflow.videos WHERE project_id=$1 AND script_id=$2 ORDER BY time DESC,id DESC").bind(project_id).bind(script_id).fetch_all(pool).await.map_err(|_|AppError::internal("failed to list videos"))?;
    let transitions=sqlx::query_as::<_,(String,String,String,String,String)>("SELECT from_scene_key,to_scene_key,transition_type,description,frame_policy FROM toonflow.scene_transitions WHERE project_id=$1 AND script_id=$2 ORDER BY from_scene_key,to_scene_key").bind(project_id).bind(script_id).fetch_all(pool).await.map_err(|_|AppError::internal("failed to list scene transitions"))?;
    let asset_media: Vec<StoryboardAssetMediaRow> = sqlx::query_as("SELECT ast.storyboard_id,a.id,a.name,a.type,img.file_path,audio.file_path FROM toonflow.assets_storyboards ast JOIN toonflow.assets a ON a.id=ast.asset_id LEFT JOIN toonflow.images img ON img.id=a.image_id LEFT JOIN LATERAL (SELECT aa_img.file_path FROM toonflow.asset_audio_bindings b JOIN toonflow.assets aa ON aa.id=b.asset_audio_id LEFT JOIN toonflow.images aa_img ON aa_img.id=aa.image_id WHERE b.asset_role_id=a.id ORDER BY b.create_time DESC LIMIT 1) audio ON true WHERE a.project_id=$1 AND ast.storyboard_id IN (SELECT id FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2) ORDER BY ast.storyboard_id,ast.sort_order").bind(project_id).bind(script_id).fetch_all(pool).await.map_err(|_|AppError::internal("failed to list storyboard asset media"))?;
    let list = tracks.into_iter().map(|t| {
        let mut medias = Vec::new();
        for board in boards.iter().filter(|board| board.track_id == Some(t.0)) {
            let mut storyboard_media = board.to_json();
            storyboard_media["fileType"] = json!("image");
            storyboard_media["sources"] = json!("storyboard");
            medias.push(storyboard_media);
            for asset in asset_media.iter().filter(|asset| asset.0 == board.id) {
                if let Some(src) = &asset.4 { medias.push(json!({"id":asset.1,"name":asset.2,"type":asset.3,"src":src,"fileType":"image","sources":"assets","storyboardId":board.id})); }
                if let Some(src) = &asset.5 { medias.push(json!({"id":asset.1,"name":asset.2,"type":asset.3,"src":src,"fileType":"audio","sources":"assets","storyboardId":board.id})); }
            }
        }
        json!({"id":t.0,"prompt":t.1,"state":t.2,"duration":t.3,"selectVideoId":t.4,"sortOrder":t.5,"transitionType":t.6,"framePolicy":t.7,"previousTrackId":t.8,"transitionSource":t.9,"transitionDurationMs":t.10,"trimStartMs":t.11,"trimEndMs":t.12,"medias":medias,"videoList":videos.iter().filter(|v|v.4==Some(t.0)).map(|v|json!({"id":v.0,"src":v.1,"state":v.2,"errorReason":v.3,"generationContext":v.5})).collect::<Vec<_>>()})
    }).collect::<Vec<_>>();
    Ok(
        json!({"storyboardList":boards.iter().map(WorkbenchStoryboardRow::to_json).collect::<Vec<_>>(),"trackList":list,"sceneTransitions":transitions.into_iter().map(|transition|json!({"fromSceneKey":transition.0,"toSceneKey":transition.1,"transitionType":transition.2,"description":transition.3,"framePolicy":transition.4})).collect::<Vec<_>>() }),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Generate {
    project_id: i64,
    script_id: i64,
    prompt: String,
    #[serde(deserialize_with = "deserialize_model")]
    model: String,
    mode: Value,
    resolution: String,
    duration: i32,
    audio: Option<bool>,
    track_id: i64,
    upload_data: Value,
    #[serde(default)]
    retry_of_id: Option<i64>,
}

/// Combines canonical asset images with caller-provided frames. Assets stay first so the
const VIDEO_REFERENCE_CAP: usize = 4;

/// Asset references arrive pre-selected by the P0.4 priority policy, so only
/// trailing upload references can overflow here; the cut drops the newest
/// extras and reports them instead of failing the whole generation.
fn merge_references(
    upload_data: Value,
    asset_references: Vec<String>,
) -> Result<(Value, Vec<String>), String> {
    let mut references = asset_references
        .into_iter()
        .filter(|reference| !reference.is_empty())
        .map(Value::String)
        .collect::<Vec<_>>();
    references.extend(
        upload_data
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| item.get("src").and_then(Value::as_str).map(str::to_string))
            })
            .filter(|reference| !reference.is_empty())
            .map(Value::String)
            .collect::<Vec<_>>(),
    );
    let mut seen = HashSet::new();
    references.retain(|reference| seen.insert(reference.as_str().unwrap_or_default().to_string()));
    let dropped_uploads = if references.len() > VIDEO_REFERENCE_CAP {
        references
            .split_off(VIDEO_REFERENCE_CAP)
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    } else {
        Vec::new()
    };
    Ok((json!(references), dropped_uploads))
}

fn references_for_mode(
    upload_data: Value,
    asset_references: Vec<String>,
    mode: &Value,
) -> Result<(Value, Vec<String>), String> {
    let mode = mode.as_str().unwrap_or("text");
    if mode == "text" {
        return merge_references(upload_data, asset_references);
    }
    // Frame modes deliberately select endpoints from the complete storyboard
    // sequence; never truncate before selecting its last frame.
    let frames = upload_data
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("src").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
        })
        .collect::<Vec<_>>();
    if mode == "startEndRequired" && frames.len() < 2 {
        return Err("首尾帧模式需要首帧和尾帧两张图片".into());
    }
    if frames.is_empty()
        && matches!(
            mode,
            "singleImage" | "endFrameOptional" | "startFrameOptional"
        )
    {
        return Err("当前视频模式需要至少一张分镜图片".into());
    }
    if matches!(
        mode,
        "startEndRequired" | "endFrameOptional" | "startFrameOptional"
    ) && frames.len() > 1
    {
        return Ok((
            json!([
                frames.first().cloned().unwrap(),
                frames.last().cloned().unwrap()
            ]),
            Vec::new(),
        ));
    }
    Ok((json!(frames.into_iter().take(1).collect::<Vec<_>>()), Vec::new()))
}

fn validate_prompt_references(prompt: &str, count: usize) -> Result<(), String> {
    for suffix in prompt.split("@图").skip(1) {
        let digits = suffix
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        let index = digits
            .parse::<usize>()
            .map_err(|_| "参考图编号必须是 @图1 这样的数字编号")?;
        if index == 0 || index > count {
            return Err(format!(
                "提示词引用 @图{index}，但实际只发送 {count} 张参考图"
            ));
        }
    }
    Ok(())
}

fn compile_video_prompt_with_manifest(
    prompt: &str,
    references: &[crate::toonflow_asset_context::TrackAssetReference],
) -> Result<String, String> {
    validate_prompt_references(prompt, references.len())?;
    if references.is_empty() {
        return Ok(prompt.trim().to_string());
    }
    for index in 1..=references.len() {
        let marker = format!("@图{index}");
        if !prompt.contains(&marker) {
            return Err(format!("视频提示词缺少服务端参考清单中的 {marker}"));
        }
    }
    let declarations = references
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            format!(
                "@图{} 为{}（{}，资产ID={}，图片ID={}）",
                index + 1,
                reference.asset_name,
                reference.asset_type,
                reference.asset_id,
                reference.image_id,
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    Ok(format!("参考图清单：{declarations}\n{}", prompt.trim()))
}

#[derive(Clone, Debug, FromRow)]
struct StructuredStoryboardRow {
    id: i64,
    index: Option<i32>,
    scene_key: Option<String>,
    scene_state_key: Option<String>,
    video_desc: Option<String>,
    prompt: String,
    duration: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StructuredShotReference {
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    asset_id: i64,
    asset_type: String,
    name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StructuredShotDescription {
    storyboard_id: i64,
    sequence: i32,
    scene_key: Option<String>,
    scene_state_key: Option<String>,
    duration_seconds: f64,
    description: String,
    references: Vec<StructuredShotReference>,
}

fn parse_shot_duration(value: &str, position: usize) -> Result<f64, String> {
    let duration = value
        .trim()
        .trim_end_matches(|character| character == 's' || character == '秒')
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("第 {position} 个分镜时长无效"))?;
    if !duration.is_finite() || duration <= 0.0 {
        return Err(format!("第 {position} 个分镜时长必须大于 0"));
    }
    Ok(duration)
}

async fn load_structured_shot_descriptions(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
    include_reference_indices: bool,
) -> Result<Vec<StructuredShotDescription>, String> {
    let manifest = if include_reference_indices {
        crate::toonflow_asset_context::load_track_asset_reference_manifest(
            pool, project_id, script_id, track_id,
        )
        .await
        .map_err(|error| format!("加载轨道参考清单失败：{error}"))?
    } else {
        Vec::new()
    };
    let manifest_indices = manifest
        .iter()
        .enumerate()
        .map(|(index, reference)| (reference.asset_id, index + 1))
        .collect::<HashMap<_, _>>();
    let rows = sqlx::query_as::<_, StructuredStoryboardRow>(
        r#"SELECT storyboard.id,storyboard.index,storyboard.scene_key,
                  scene_state.state_key AS scene_state_key,
                  storyboard.video_desc,storyboard.prompt,storyboard.duration
           FROM toonflow.storyboards storyboard
           LEFT JOIN toonflow.scene_states scene_state ON scene_state.id=storyboard.scene_state_id
           WHERE storyboard.project_id=$1 AND storyboard.script_id=$2
             AND storyboard.track_id=$3
           ORDER BY storyboard.index,storyboard.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("加载结构化镜头失败：{error}"))?;
    if rows.is_empty() {
        return Err("视频轨道没有可编译的分镜".into());
    }
    let asset_rows = sqlx::query_as::<_, (i64, i64, String, String)>(
        r#"SELECT binding.storyboard_id,asset.id,asset.type,asset.name
           FROM toonflow.assets_storyboards binding
           JOIN toonflow.storyboards storyboard ON storyboard.id=binding.storyboard_id
           JOIN toonflow.assets asset ON asset.id=binding.asset_id AND asset.project_id=$1
           WHERE storyboard.project_id=$1 AND storyboard.script_id=$2
             AND storyboard.track_id=$3
           ORDER BY storyboard.index,storyboard.id,binding.sort_order,binding.asset_id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("加载镜头资产绑定失败：{error}"))?;
    let mut assets_by_storyboard: HashMap<i64, Vec<StructuredShotReference>> = HashMap::new();
    for (storyboard_id, asset_id, asset_type, name) in asset_rows {
        let index = manifest_indices.get(&asset_id).copied();
        if include_reference_indices && index.is_none() {
            return Err(format!(
                "分镜 {storyboard_id} 的资产 {asset_id} 未进入最终参考清单"
            ));
        }
        assets_by_storyboard
            .entry(storyboard_id)
            .or_default()
            .push(StructuredShotReference {
                index,
                asset_id,
                asset_type,
                name,
            });
    }
    rows.into_iter()
        .enumerate()
        .map(|(position, row)| {
            let description = row
                .video_desc
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(row.prompt)
                .trim()
                .to_string();
            if description.is_empty() {
                return Err(format!("第 {} 个分镜缺少画面描述", position + 1));
            }
            let duration =
                parse_shot_duration(row.duration.as_deref().unwrap_or_default(), position + 1)?;
            Ok(StructuredShotDescription {
                storyboard_id: row.id,
                sequence: row.index.unwrap_or(position as i32 + 1),
                scene_key: row.scene_key.filter(|value| !value.trim().is_empty()),
                scene_state_key: row
                    .scene_state_key
                    .filter(|value| !value.trim().is_empty()),
                duration_seconds: duration,
                description,
                references: assets_by_storyboard.remove(&row.id).unwrap_or_default(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, FromRow)]
struct VideoReferenceAsset {
    file_path: String,
    asset_id: i64,
    asset_type: String,
    asset_name: String,
    image_id: i64,
}

fn reference_kind(mode: &str, index: usize, count: usize) -> &'static str {
    if mode == "text" {
        "reference_image"
    } else if mode == "startFrameOptional" && count == 1 {
        "last_frame"
    } else if index == 0 {
        "first_frame"
    } else {
        "last_frame"
    }
}

fn asset_reference_role(asset_type: &str) -> &'static str {
    match asset_type {
        "scene" => "environment_reference",
        "role" | "character" => "required_subject",
        _ => "required_subject",
    }
}

async fn build_reference_manifest(
    pool: &sqlx::PgPool,
    video_id: i64,
    mode: &str,
    references: &[Value],
) -> Result<Vec<Value>, String> {
    let assets = sqlx::query_as::<_, VideoReferenceAsset>(
        r#"SELECT DISTINCT ON (i.file_path)
                  i.file_path,a.id AS asset_id,a.type AS asset_type,
                  a.name AS asset_name,i.id AS image_id
           FROM toonflow.videos v
           JOIN toonflow.assets a ON a.project_id=v.project_id
           JOIN toonflow.images i ON i.id=a.image_id
           WHERE v.id=$1 AND i.file_path=ANY($2::text[])
           ORDER BY i.file_path,a.id"#,
    )
    .bind(video_id)
    .bind(
        references
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("加载视频参考资产清单失败：{e}"))?
    .into_iter()
    .map(|asset| (asset.file_path.clone(), asset))
    .collect::<HashMap<_, _>>();

    Ok(references
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            let kind = reference_kind(mode, index, references.len());
            let asset = reference.as_str().and_then(|path| assets.get(path));
            // P0.2：文本模式下未命中资产的附加参考是“非主体身份参考”，
            // 不与镜头主体的 required_subject 混用；帧模式的角色保持不变。
            let fallback_role = if mode == "text" {
                "identity_reference"
            } else {
                kind
            };
            json!({
                "index": index + 1,
                "kind": kind,
                "role": asset.map(|item| asset_reference_role(&item.asset_type)).unwrap_or(fallback_role),
                "url": reference,
                "reference": reference,
                "assetId": asset.map(|item| item.asset_id),
                "assetType": asset.map(|item| item.asset_type.as_str()),
                "name": asset.map(|item| item.asset_name.as_str()),
                "imageId": asset.map(|item| item.image_id),
            })
        })
        .collect())
}

/// P0.5：供应商提交前确认参考图 URL 可访问，避免付费任务因供应商拉取
/// 失败而作废。只检查 http(s) 引用；本地相对路径与模拟供应商不受影响。
/// HEAD 被拒（405/403）说明资源存在但服务器限制方法，视为可达。
async fn preflight_reference_reachability(
    references: Option<&Vec<Value>>,
) -> Result<(), String> {
    let Some(references) = references else {
        return Ok(());
    };
    let urls = references
        .iter()
        .filter_map(Value::as_str)
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .map_err(|error| error.to_string())?;
    let mut checks = JoinSet::new();
    for url in urls {
        let client = client.clone();
        checks.spawn(async move {
            let outcome = client.head(&url).send().await;
            (url, outcome)
        });
    }
    let mut failures = Vec::new();
    while let Some(joined) = checks.join_next().await {
        let Ok((url, outcome)) = joined else {
            continue;
        };
        match outcome {
            Ok(response)
                if response.status().is_success()
                    || response.status().is_redirection()
                    || response.status().as_u16() == 405 =>
            {
                continue;
            }
            Ok(response) => {
                failures.push(format!("{url}（HTTP {}）", response.status().as_u16()));
            }
            Err(error) => failures.push(format!("{url}（{error}）")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "参考图无法访问，供应商将读取失败：{}",
            failures.join("；")
        ))
    }
}

async fn generate_with_snapshot(
    pool: &sqlx::PgPool,
    video_id: i64,
    model: &str,
    payload: Value,
    dropped_references: Vec<Value>,
) -> Result<String, String> {
    let references = payload["references"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    validate_prompt_references(
        payload["prompt"].as_str().unwrap_or_default(),
        references.len(),
    )?;
    let mode = payload["mode"].as_str().unwrap_or("text");
    let manifest = build_reference_manifest(pool, video_id, mode, &references).await?;
    let context = sqlx::query_as::<_, (i64, i64, Option<i64>)>(
        "SELECT project_id,script_id,video_track_id FROM toonflow.videos WHERE id=$1",
    )
    .bind(video_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("加载视频镜头上下文失败：{error}"))?
    .ok_or_else(|| "视频任务不存在".to_string())?;
    let track_id = context
        .2
        .ok_or_else(|| "视频任务未关联视频轨道".to_string())?;
    let structured_shots =
        load_structured_shot_descriptions(
            pool,
            context.0,
            context.1,
            track_id,
            mode == "text",
        )
        .await?;
    let updated = sqlx::query("UPDATE toonflow.videos SET generation_context=generation_context || jsonb_build_object('request',$2::jsonb) WHERE id=$1 AND state='生成中'")
        .bind(video_id).bind(json!({
            "version":2,
            "model":model,
            "payload":payload,
            "structuredShots":structured_shots,
            "referenceManifest":manifest.clone(),
            // Keep the old key during the snapshot schema transition so existing
            // task-detail consumers can continue to render reference entries.
            "references":manifest,
            "droppedReferences":dropped_references,
        }))
        .execute(pool).await.map_err(|e| format!("保存视频生成快照失败：{e}"))?;
    if updated.rows_affected() != 1 {
        return Err("视频任务已取消或删除".into());
    }
    preflight_reference_reachability(payload["references"].as_array()).await?;
    let submission = ai_client::video_submit(pool, model, payload).await?;
    if let Some(url) = submission.url {
        return Ok(url);
    }
    let task_id = submission
        .task_id
        .ok_or_else(|| "视频响应缺少 URL 或任务 ID".to_string())?;
    // Persist the provider handle before polling so a Gateway restart resumes
    // this paid task instead of failing it or submitting a duplicate.
    let updated = sqlx::query("UPDATE toonflow.videos SET generation_context=generation_context || jsonb_build_object('provider',$2::jsonb) WHERE id=$1 AND state='生成中'")
        .bind(video_id)
        .bind(json!({"model":model,"taskId":task_id,"submittedAt":chrono::Utc::now().timestamp_millis()}))
        .execute(pool).await.map_err(|e| format!("保存供应商任务标识失败：{e}"))?;
    if updated.rows_affected() != 1 {
        return Err("视频任务已取消或删除".into());
    }
    ai_client::video_poll_task(pool, model, &task_id).await
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowVideoInput {
    #[serde(default)]
    pub track_ids: Vec<i64>,
    #[serde(default = "workflow_video_concurrency")]
    pub concurrent_count: usize,
    #[serde(default = "workflow_video_resolution")]
    pub resolution: String,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub video_ids: Vec<i64>,
}

fn workflow_video_concurrency() -> usize {
    2
}

fn workflow_video_resolution() -> String {
    "1080p".into()
}

#[derive(Clone)]
pub(crate) struct WorkflowVideoJob {
    previous_video_id: Option<i64>,
    id: i64,
    script_id: i64,
    track_id: i64,
    prompt: String,
    duration: i32,
    model: String,
    mode: String,
    ratio: String,
    resolution: String,
    audio: bool,
    references: Value,
    dropped_references: Vec<Value>,
    transition_settings: TrackTransitionSettings,
}

#[derive(Default)]
pub(crate) struct WorkflowVideoSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub video_ids: Vec<i64>,
}

pub(crate) async fn prepare_workflow_video_generation(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    input: &mut WorkflowVideoInput,
) -> Result<Vec<WorkflowVideoJob>, AppError> {
    let settings: Option<(Option<i64>, String, String)> =
        sqlx::query_as("SELECT video_model,mode,video_ratio FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| AppError::internal("failed to load project video settings"))?;
    let (model, mode, ratio) = settings
        .and_then(|(model, mode, ratio)| model.map(|model| (model.to_string(), mode, ratio)))
        .ok_or_else(|| AppError::bad_request("请先配置当前项目的视频模型"))?;
    let mut tracks = sqlx::query_as::<
        _,
        (
            i64,
            Option<String>,
            Option<i32>,
            String,
            String,
            Option<i64>,
            String,
        ),
    >(
        "SELECT id,prompt,duration,transition_type,frame_policy,previous_track_id,transition_source
         FROM toonflow.video_tracks
         WHERE project_id=$1 AND script_id=$2
         ORDER BY coalesce((
                    SELECT min(board.index)
                    FROM toonflow.storyboards board
                    WHERE board.track_id=toonflow.video_tracks.id
                  ),2147483647),sort_order,id",
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load workflow video tracks"))?;
    if !input.track_ids.is_empty() {
        tracks.retain(|track| input.track_ids.contains(&track.0));
    }
    if tracks.is_empty() {
        return Err(AppError::bad_request("当前没有可生成的视频轨道"));
    }
    input.track_ids = tracks.iter().map(|track| track.0).collect();
    crate::toonflow_scene_consistency::ensure_track_storyboard_images_current(
        pool,
        project_id,
        script_id,
        &input.track_ids,
    )
    .await?;
    let mut prepared_jobs = Vec::with_capacity(tracks.len());
    for (
        track_id,
        prompt,
        duration,
        transition_type,
        frame_policy,
        previous_track_id,
        transition_source,
    ) in tracks
    {
        validate_continuity_mode(&mode, &frame_policy).map_err(AppError::bad_request)?;
        let previous_track_id = if mode != "text"
            && frame_policy == FRAME_POLICY_PREVIOUS_TAIL
            && previous_track_id.is_none()
        {
            resolve_previous_track_id(pool, project_id, script_id, track_id, None)
                .await
                .map_err(AppError::internal)?
        } else {
            previous_track_id
        };
        let frames: Vec<String> = sqlx::query_scalar(
            "SELECT file_path FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track_id=$3 AND file_path IS NOT NULL AND file_path<>'' ORDER BY index,id",
        )
        .bind(project_id)
        .bind(script_id)
        .bind(track_id)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to load workflow video frames"))?;
        let (asset_references, dropped_references) = if mode == "text" {
            let (manifest, drops) =
                crate::toonflow_asset_context::load_track_asset_reference_selection(
                    pool,
                    project_id,
                    script_id,
                    track_id,
                    VIDEO_REFERENCE_CAP,
                )
                .await
                .map_err(|_| AppError::internal("failed to load video asset references"))?;
            (
                manifest
                    .into_iter()
                    .map(|reference| reference.file_path)
                    .collect::<Vec<_>>(),
                drops,
            )
        } else {
            (
                crate::toonflow_asset_context::load_track_asset_references(
                    pool, project_id, script_id, track_id,
                )
                .await
                .map_err(|_| AppError::internal("failed to load video asset references"))?,
                Vec::new(),
            )
        };
        let (references, dropped_uploads) =
            references_for_mode(json!(frames), asset_references, &json!(mode))
                .map_err(AppError::bad_request)?;
        let mut dropped_references = dropped_references;
        dropped_references.extend(dropped_uploads.into_iter().map(|file_path| {
            json!({"filePath": file_path, "reason": "参考图超过上限，优先保留资产参考，舍弃附加参考图"})
        }));
        prepared_jobs.push(WorkflowVideoJob {
            previous_video_id: None,
            id: 0,
            script_id,
            track_id,
            prompt: prompt.unwrap_or_default(),
            duration: duration.unwrap_or(5),
            model: model.clone(),
            mode: mode.clone(),
            ratio: if ratio.trim().is_empty() {
                "16:9".into()
            } else {
                ratio.clone()
            },
            resolution: input.resolution.clone(),
            audio: input.audio,
            references,
            dropped_references,
            transition_settings: TrackTransitionSettings {
                transition_type,
                frame_policy,
                previous_track_id,
                transition_source,
            },
        });
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin workflow video creation"))?;
    let mut jobs = Vec::with_capacity(prepared_jobs.len());
    for mut job in prepared_jobs {
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO toonflow.videos(state,script_id,project_id,video_track_id,time)
             SELECT '生成中',$1,$2,$3,$4
             WHERE EXISTS(
               SELECT 1 FROM toonflow.video_tracks
               WHERE id=$3 AND project_id=$2 AND script_id=$1
             )
             RETURNING id",
        )
        .bind(script_id)
        .bind(project_id)
        .bind(job.track_id)
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to create workflow video"))?;
        let Some(id) = id else {
            transaction
                .rollback()
                .await
                .map_err(|_| AppError::internal("failed to roll back workflow video creation"))?;
            return Err(AppError::not_found("video track not found"));
        };
        job.id = id;
        jobs.push(job);
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit workflow video creation"))?;
    input.video_ids = jobs.iter().map(|job| job.id).collect();
    Ok(jobs)
}

fn workflow_job_is_ready(
    job: &WorkflowVideoJob,
    track_ranks: &HashMap<i64, usize>,
    completed_tracks: &HashSet<i64>,
) -> bool {
    if job.mode == "text" || job.transition_settings.frame_policy != FRAME_POLICY_PREVIOUS_TAIL {
        return true;
    }
    let Some(previous_track_id) = job.transition_settings.previous_track_id else {
        return true;
    };
    let (Some(previous_rank), Some(current_rank)) = (
        track_ranks.get(&previous_track_id),
        track_ranks.get(&job.track_id),
    ) else {
        // A dependency outside this workflow run can use its already persisted
        // successful video without blocking this batch.
        return true;
    };
    previous_rank < current_rank && completed_tracks.contains(&previous_track_id)
}

async fn mark_video_generation_failed(pool: &sqlx::PgPool, video_id: i64, reason: String) {
    let _ = sqlx::query(
        "UPDATE toonflow.videos
         SET state='生成失败',error_reason=$2
         WHERE id=$1 AND state='生成中'",
    )
    .bind(video_id)
    .bind(reason)
    .execute(pool)
    .await;
}

async fn run_workflow_video_job(
    pool: sqlx::PgPool,
    project_id: i64,
    mut job: WorkflowVideoJob,
) -> (i64, bool) {
    let track_id = job.track_id;
    let Some(_generation_task_permit) = acquire_video_generation_task().await else {
        mark_video_generation_failed(&pool, job.id, "视频生成任务控制不可用".into()).await;
        return (track_id, false);
    };
    let prompt = if job.prompt.trim().is_empty() {
        match create_prompt(&pool, job.track_id, project_id, &job.model, &job.mode).await {
            Ok(prompt) => prompt,
            Err(reason) => {
                mark_video_generation_failed(&pool, job.id, reason).await;
                return (track_id, false);
            }
        }
    } else {
        job.prompt.clone()
    };
    let frame_application = if job.mode != "text" {
        match apply_frame_policy(
            &pool,
            project_id,
            job.script_id,
            job.track_id,
            &job.transition_settings,
            job.references,
            job.previous_video_id,
            &job.mode,
        )
        .await
        {
            Ok((references, frame_application)) => {
                job.references = references;
                frame_application
            }
            Err(reason) => {
                mark_video_generation_failed(&pool, job.id, reason).await;
                return (track_id, false);
            }
        }
    } else {
        FrameApplication::own()
    };
    if let Err(reason) =
        persist_generation_context(&pool, job.id, &job.transition_settings, &frame_application)
            .await
    {
        mark_video_generation_failed(&pool, job.id, reason).await;
        return (track_id, false);
    }
    let prompt =
        prompt_with_transition_context(&prompt, &job.transition_settings, &frame_application);
    let payload = json!({
        "prompt": prompt,
        "mode": job.mode,
        "resolution": job.resolution,
        "duration": job.duration,
        "audio": job.audio,
        "aspect_ratio": job.ratio,
        "references": job.references,
    });
    let provider_result = {
        let Some(_provider_permit) = acquire_provider_video_slot().await else {
            mark_video_generation_failed(&pool, job.id, "视频生成并发控制不可用".into()).await;
            return (track_id, false);
        };
        generate_with_snapshot(&pool, job.id, &job.model, payload, job.dropped_references.clone())
            .await
    };
    let succeeded = match provider_result {
        Ok(url) => store_generated_video(&pool, job.id, project_id, &url).await,
        Err(reason) => {
            mark_video_generation_failed(&pool, job.id, reason).await;
            false
        }
    };
    (track_id, succeeded)
}

pub(crate) async fn run_workflow_video_generation(
    pool: sqlx::PgPool,
    project_id: i64,
    jobs: Vec<WorkflowVideoJob>,
    concurrent_count: usize,
    node_run_id: i64,
) -> WorkflowVideoSummary {
    let mut summary = WorkflowVideoSummary {
        total: jobs.len(),
        video_ids: jobs.iter().map(|job| job.id).collect(),
        ..Default::default()
    };
    let concurrency = concurrent_count.clamp(1, 10);
    let track_ranks = jobs
        .iter()
        .enumerate()
        .map(|(rank, job)| (job.track_id, rank))
        .collect::<HashMap<_, _>>();
    let mut completed_tracks = HashSet::new();
    let video_ids_by_track = jobs
        .iter()
        .map(|job| (job.track_id, job.id))
        .collect::<HashMap<_, _>>();
    let mut pending = jobs;
    let mut running = JoinSet::new();
    loop {
        while running.len() < concurrency {
            let Some(ready_index) = pending
                .iter()
                .position(|job| workflow_job_is_ready(job, &track_ranks, &completed_tracks))
            else {
                break;
            };
            let mut job = pending.remove(ready_index);
            job.previous_video_id = job
                .transition_settings
                .previous_track_id
                .and_then(|id| video_ids_by_track.get(&id).copied());
            running.spawn(run_workflow_video_job(pool.clone(), project_id, job));
        }
        if running.is_empty() && !pending.is_empty() {
            // Never run a dependent shot against an older successful candidate
            // after its requested predecessor failed in this run.
            for job in pending.drain(..) {
                mark_video_generation_failed(
                    &pool,
                    job.id,
                    "前镜头未通过质检或依赖顺序无效，连续镜头已停止；请修复前镜头后重试".into(),
                )
                .await;
                summary.failed += 1;
            }
            let _ = sqlx::query("UPDATE toonflow.workflow_node_runs SET progress_current=$2 WHERE id=$1 AND state='running'")
                .bind(node_run_id).bind((summary.succeeded + summary.failed) as i32).execute(&pool).await;
            break;
        }
        let Some(result) = running.join_next().await else {
            break;
        };
        match result {
            Ok((track_id, true)) => {
                completed_tracks.insert(track_id);
                summary.succeeded += 1;
            }
            Ok((_, false)) => {
                summary.failed += 1;
            }
            Err(_) => summary.failed += 1,
        }
        let completed = summary.succeeded + summary.failed;
        let _ = sqlx::query("UPDATE toonflow.workflow_node_runs SET progress_current=$2 WHERE id=$1 AND state='running'")
            .bind(node_run_id).bind(completed as i32).execute(&pool).await;
    }
    summary
}

pub async fn generate_video(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Generate>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_track_in_context(
        &state.pool,
        &user,
        req.project_id,
        req.script_id,
        req.track_id,
    )
    .await?;
    crate::toonflow_scene_consistency::ensure_track_storyboard_images_current(
        &state.pool,
        req.project_id,
        req.script_id,
        &[req.track_id],
    )
    .await?;
    ensure_internal_references_in_project(&req.upload_data, req.project_id)?;
    if let Some(retry_of_id) = req.retry_of_id {
        let valid_retry: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM toonflow.videos
               WHERE id=$1 AND project_id=$2 AND script_id=$3 AND video_track_id=$4
                 AND state IN ('生成失败','已取消')
             )",
        )
        .bind(retry_of_id)
        .bind(req.project_id)
        .bind(req.script_id)
        .bind(req.track_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to authorize retry video"))?;
        if !valid_retry {
            return Err(AppError::not_found("retry video not found"));
        }
    }
    let transition_settings =
        load_track_settings(&state.pool, req.project_id, req.script_id, req.track_id)
            .await
            .map_err(AppError::bad_request)?;
    validate_continuity_mode(
        req.mode.as_str().unwrap_or("text"),
        &transition_settings.frame_policy,
    )
    .map_err(AppError::bad_request)?;
    let generation_mode = req.mode.as_str().unwrap_or("text").to_string();
    let (asset_references, mut dropped_references) = if generation_mode == "text" {
        let (manifest, drops) =
            crate::toonflow_asset_context::load_track_asset_reference_selection(
                &state.pool,
                req.project_id,
                req.script_id,
                req.track_id,
                VIDEO_REFERENCE_CAP,
            )
            .await
            .map_err(|_| AppError::internal("failed to load video asset references"))?;
        (
            manifest
                .into_iter()
                .map(|reference| reference.file_path)
                .collect::<Vec<_>>(),
            drops,
        )
    } else {
        (
            crate::toonflow_asset_context::load_track_asset_references(
                &state.pool,
                req.project_id,
                req.script_id,
                req.track_id,
            )
            .await
            .map_err(|_| AppError::internal("failed to load video asset references"))?,
            Vec::new(),
        )
    };
    let (references, dropped_uploads) =
        references_for_mode(req.upload_data, asset_references, &req.mode)
            .map_err(AppError::bad_request)?;
    dropped_references.extend(dropped_uploads.into_iter().map(|file_path| {
        json!({"filePath": file_path, "reason": "参考图超过上限，优先保留资产参考，舍弃附加参考图"})
    }));
    let generation_task_permit = try_acquire_video_generation_task()?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO toonflow.videos(
           state,script_id,project_id,video_track_id,time,retry_of_id
         )
         SELECT '生成中',$1,$2,$3,$4,$5
         WHERE EXISTS(
           SELECT 1 FROM toonflow.video_tracks
           WHERE id=$3 AND project_id=$2 AND script_id=$1
         )
         RETURNING id",
    )
    .bind(req.script_id)
    .bind(req.project_id)
    .bind(req.track_id)
    .bind(chrono::Utc::now().timestamp_millis())
    .bind(req.retry_of_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to create video"))?;
    let id = id.ok_or_else(|| AppError::not_found("video track not found"))?;
    let pool = state.pool.clone();
    tokio::spawn(async move {
        // This admission permit bounds the complete background task, including
        // tail preparation and any wait for provider capacity.
        let _generation_task_permit = generation_task_permit;
        // Tail-frame extraction can download/materialize media and invoke
        // FFmpeg. Keep it behind the durable video row so the request returns
        // immediately and every failure reaches a terminal state.
        let (references, frame_application) = if req.mode.as_str() != Some("text") {
            match apply_frame_policy(
                &pool,
                req.project_id,
                req.script_id,
                req.track_id,
                &transition_settings,
                references,
                None,
                req.mode.as_str().unwrap_or("text"),
            )
            .await
            {
                Ok(result) => result,
                Err(reason) => {
                    mark_video_generation_failed(&pool, id, reason).await;
                    return;
                }
            }
        } else {
            (references, FrameApplication::own())
        };
        if let Err(reason) =
            persist_generation_context(&pool, id, &transition_settings, &frame_application).await
        {
            mark_video_generation_failed(&pool, id, reason).await;
            return;
        }
        let prompt =
            prompt_with_transition_context(&req.prompt, &transition_settings, &frame_application);
        let ratio: Option<(String,)> =
            sqlx::query_as("SELECT video_ratio FROM toonflow.projects WHERE id=$1")
                .bind(req.project_id)
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten();
        let payload = json!({"prompt":prompt,"mode":req.mode,"resolution":req.resolution,"duration":req.duration,"audio":req.audio.unwrap_or(false),"aspect_ratio":ratio.map(|r|r.0).unwrap_or_else(||"16:9".into()),"references":references});
        let provider_result = {
            let Some(_provider_permit) = acquire_provider_video_slot().await else {
                mark_video_generation_failed(&pool, id, "视频生成并发控制不可用".into()).await;
                return;
            };
            generate_with_snapshot(&pool, id, &req.model, payload, dropped_references).await
        };
        match provider_result {
            Ok(url) => {
                store_generated_video(&pool, id, req.project_id, &url).await;
            }
            Err(reason) => {
                mark_video_generation_failed(&pool, id, reason).await;
            }
        }
    });
    Ok(Json(ApiResponse::new(id)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderRequest {
    project_id: i64,
    script_id: i64,
    track_ids: Vec<i64>,
}

pub async fn reorder_tracks(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ReorderRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let (total, matched): (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(*) FILTER (WHERE id=ANY($3))
         FROM toonflow.video_tracks WHERE project_id=$1 AND script_id=$2",
    )
    .bind(req.project_id)
    .bind(req.script_id)
    .bind(&req.track_ids)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to validate tracks"))?;
    if total != req.track_ids.len() as i64 || matched != total {
        return Err(AppError::bad_request(
            "轨道排序必须且只能包含当前剧本的全部轨道",
        ));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    for (index, id) in req.track_ids.into_iter().enumerate() {
        let updated = sqlx::query(
            "UPDATE toonflow.video_tracks SET sort_order=$2
             WHERE id=$1 AND project_id=$3 AND script_id=$4",
        )
        .bind(id)
        .bind(index as i32)
        .bind(req.project_id)
        .bind(req.script_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to reorder tracks"))?;
        if updated.rows_affected() != 1 {
            return Err(AppError::not_found("video track not found"));
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    crate::toonflow_scene_transitions::apply_track_transition_defaults(
        &state.pool,
        req.project_id,
        req.script_id,
    )
    .await?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindStoryboardsRequest {
    track_id: i64,
    storyboard_ids: Vec<i64>,
}

fn normalize_storyboard_ids(mut ids: Vec<i64>) -> Vec<i64> {
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub async fn bind_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<BindStoryboardsRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:update")?;
    let storyboard_ids = normalize_storyboard_ids(req.storyboard_ids);
    let track: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT project_id,script_id FROM toonflow.video_tracks WHERE id=$1")
            .bind(req.track_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load track"))?;
    let (project_id, script_id) = track.ok_or_else(|| AppError::not_found("track not found"))?;
    ensure_project_access(&state.pool, &user, project_id).await?;
    let script_id = script_id.ok_or_else(|| AppError::not_found("script not found"))?;
    ensure_script_in_project(&state.pool, project_id, script_id).await?;
    let valid_storyboard_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM toonflow.storyboards WHERE id=ANY($1) AND project_id=$2 AND script_id=$3",
    )
    .bind(&storyboard_ids)
    .bind(project_id)
    .bind(script_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to validate storyboards"))?;
    if valid_storyboard_count != storyboard_ids.len() as i64 {
        return Err(AppError::bad_request("部分分镜不存在或不属于当前轨道项目"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    let mut affected_track_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT track_id FROM toonflow.storyboards WHERE id=ANY($1) AND track_id IS NOT NULL",
    )
    .bind(&storyboard_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load previous tracks"))?;
    affected_track_ids.push(req.track_id);
    affected_track_ids.sort_unstable();
    affected_track_ids.dedup();
    sqlx::query("UPDATE toonflow.storyboards SET track_id=$1 WHERE id=ANY($2) AND project_id=$3 AND script_id=$4").bind(req.track_id).bind(&storyboard_ids).bind(project_id).bind(script_id).execute(&mut *tx).await.map_err(|_|AppError::internal("failed to bind storyboards"))?;
    sqlx::query(
        "UPDATE toonflow.video_tracks vt
         SET duration=coalesce((
           SELECT sum(CASE WHEN s.duration ~ '^[0-9]+$' THEN s.duration::integer ELSE 0 END)::integer
           FROM toonflow.storyboards s
           WHERE s.track_id=vt.id AND s.project_id=$2 AND s.script_id=$3
         ),0)
         WHERE vt.id=ANY($1) AND vt.project_id=$2 AND vt.script_id=$3",
    )
    .bind(&affected_track_ids)
    .bind(project_id)
    .bind(script_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to update track durations"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    crate::toonflow_scene_transitions::apply_track_transition_defaults(
        &state.pool,
        project_id,
        script_id,
    )
    .await?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn cancel_video(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Id>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let video: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT project_id,script_id FROM toonflow.videos WHERE id=$1")
            .bind(req.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to authorize video"))?;
    let (project_id, script_id) = video.ok_or_else(|| AppError::not_found("video not found"))?;
    ensure_project_access(&state.pool, &user, project_id).await?;
    if let Some(script_id) = script_id {
        ensure_script_in_project(&state.pool, project_id, script_id).await?;
    }
    let result=sqlx::query("UPDATE toonflow.videos SET state='已取消',error_reason='用户取消生成' WHERE id=$1 AND project_id=$2 AND state='生成中'").bind(req.id).bind(project_id).execute(&state.pool).await.map_err(|_|AppError::internal("failed to cancel video"))?;
    Ok(Json(ApiResponse::new(
        json!({"canceled":result.rows_affected()>0}),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryVideoRequest {
    id: i64,
    #[serde(deserialize_with = "deserialize_model")]
    model: String,
    mode: Value,
    resolution: String,
    audio: Option<bool>,
    upload_data: Value,
}

type RetryVideoSource = (i64, i64, Option<i64>, Option<String>, Option<i32>);

pub async fn retry_video(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<RetryVideoRequest>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&user, "toon:scene:update")?;
    let source: Option<RetryVideoSource> = sqlx::query_as("SELECT project_id,script_id,video_track_id,(SELECT prompt FROM toonflow.video_tracks WHERE id=video_track_id),(SELECT duration FROM toonflow.video_tracks WHERE id=video_track_id) FROM toonflow.videos WHERE id=$1 AND state IN('生成失败','已取消')").bind(req.id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to load retry video"))?;
    let (project_id, script_id, track_id, prompt, duration) =
        source.ok_or_else(|| AppError::bad_request("只有失败或已取消的视频可以重试"))?;
    let track_id = track_id.ok_or_else(|| AppError::bad_request("视频未关联轨道"))?;
    ensure_track_in_context(&state.pool, &user, project_id, script_id, track_id).await?;
    generate_video(
        user,
        State(state),
        Json(Generate {
            project_id,
            script_id,
            prompt: prompt.unwrap_or_default(),
            model: req.model,
            mode: req.mode,
            resolution: req.resolution,
            duration: duration.unwrap_or(5),
            audio: req.audio,
            track_id,
            upload_data: req.upload_data,
            retry_of_id: Some(req.id),
        }),
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    project_id: i64,
    script_id: i64,
    video_ids: Vec<i64>,
}
pub async fn check_states(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Check>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let rows=sqlx::query_as::<_,(i64,String,Option<String>,Option<String>,Option<i64>,Value)>("SELECT id,state,error_reason,file_path,retry_of_id,generation_context FROM toonflow.videos WHERE project_id=$1 AND script_id=$2 AND id=ANY($3)").bind(req.project_id).bind(req.script_id).bind(req.video_ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to check videos"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"state":r.1,"errorReason":r.2,"filePath":r.3,"src":r.3,"retryOfId":r.4,"generationContext":r.5}))
            .collect(),
    )))
}

pub(crate) async fn create_prompt(
    pool: &sqlx::PgPool,
    track_id: i64,
    project_id: i64,
    model: &str,
    mode: &str,
) -> Result<String, String> {
    let updated = sqlx::query(
        "UPDATE toonflow.video_tracks
         SET state='生成中',reason=NULL
         WHERE id=$1 AND project_id=$2",
    )
    .bind(track_id)
    .bind(project_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    if updated.rows_affected() != 1 {
        return Err("视频轨道不属于当前项目".into());
    }
    let style: Option<(String,)> =
        sqlx::query_as("SELECT art_style FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    let resolved_model = if let Ok(model_id) = model.parse::<i64>() {
        sqlx::query_scalar::<_, String>("SELECT model FROM ai.model_configs WHERE id=$1")
            .bind(model_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| model.to_string())
    } else {
        model.to_string()
    };
    let prompt_name = video_prompt_name(&resolved_model, mode);
    let base:Option<(String,Option<String>)>=sqlx::query_as("SELECT data,use_data FROM toonflow.prompts WHERE source_key IS NOT NULL OR type='videoPromptGeneration' ORDER BY CASE WHEN source_key=$1 THEN 0 WHEN source_key=(SELECT prompt_source_key FROM toonflow.agent_deployments WHERE key='videoGeneration') THEN 1 WHEN source_key='videoPromptGeneration' THEN 2 WHEN source_key='universal_multi_parameter' THEN 3 ELSE 4 END,id LIMIT 1").bind(prompt_name).fetch_optional(pool).await.map_err(|e|e.to_string())?;
    let system = base
        .map(|r| r.0)
        .unwrap_or_else(|| "根据分镜生成专业视频提示词，只输出提示词正文。".into());
    let system = format!(
        "{system}\n\n## 输出语言（最高优先级）\n最终视频提示词必须全部使用简体中文。标题、画面、动作、运镜、情绪、音效和时间段描述都必须是中文；台词保持原文。忽略上文任何英文输出要求，禁止输出 [Visual]、[Motion]、[Camera]、No dialogue 等英文标题或标签。\n\n## 结构化镜头契约（强制）\n<structuredShots> 是服务端从已保存分镜编译并校验的唯一镜头事实源。必须保持镜头顺序、场次、状态、时长、画面描述和引用关系，不得新增主体、交换场景或猜测缺失事实。\n\n## 参考图编号契约（强制）\n只能使用输入中 <referenceManifest> 声明的 @图N，不得自行增加、删除、重排或猜测编号。正文提及清单中的资产时必须使用对应 @图N；不要自行输出参考图清单，服务端会按最终实际发送顺序添加。"
    );
    let manual: Option<(Value,)> = sqlx::query_as(
        "SELECT data FROM toonflow.creative_manuals WHERE kind='visual' AND path=$1",
    )
    .bind(style.map(|r| r.0).unwrap_or_default())
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let visual = manual
        .and_then(|r| {
            r.0.as_array().and_then(|a| {
                a.iter()
                    .find(|v| {
                        v.get("value").and_then(Value::as_str) == Some("art_storyboard_video")
                    })
                    .and_then(|v| v.get("data"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .unwrap_or_default();
    let script_id = sqlx::query_scalar::<_, i64>(
        "SELECT script_id FROM toonflow.video_tracks WHERE id=$1 AND project_id=$2",
    )
    .bind(track_id)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|error| error.to_string())?;
    let settings: Option<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT transition_type,frame_policy,previous_track_id
         FROM toonflow.video_tracks WHERE id=$1 AND project_id=$2",
    )
    .bind(track_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let (transition_type, frame_policy, previous_track_id) =
        settings.unwrap_or_else(|| ("cut".into(), FRAME_POLICY_OWN.into(), None));
    let boards=sqlx::query_as::<_,(String,Option<String>,Option<String>,Option<String>)>("SELECT prompt,video_desc,duration,scene_key FROM toonflow.storyboards WHERE track_id=$1 AND project_id=$2 ORDER BY index,id").bind(track_id).bind(project_id).fetch_all(pool).await.map_err(|e|e.to_string())?;
    let first_scene_key = boards
        .iter()
        .filter_map(|board| board.3.as_deref())
        .find(|scene_key| !scene_key.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let transition_context: Option<(String, String)> = if first_scene_key.is_empty() {
        None
    } else {
        sqlx::query_as(
            "SELECT transition.from_scene_key,transition.description
             FROM toonflow.scene_transitions transition
             WHERE transition.project_id=$1
               AND transition.script_id=(
                 SELECT script_id FROM toonflow.video_tracks WHERE id=$2 AND project_id=$1
               )
               AND transition.to_scene_key=$3
             ORDER BY transition.update_time DESC,transition.from_scene_key
             LIMIT 1",
        )
        .bind(project_id)
        .bind(track_id)
        .bind(&first_scene_key)
        .fetch_optional(pool)
        .await
        .map_err(|error| error.to_string())?
    };
    let (from_scene_key, transition_description) =
        transition_context.unwrap_or_else(|| (String::new(), String::new()));
    let reference_manifest = if mode == "text" {
        crate::toonflow_asset_context::load_track_asset_reference_selection(
            pool,
            project_id,
            script_id,
            track_id,
            VIDEO_REFERENCE_CAP,
        )
        .await
        .map_err(|error| error.to_string())?
        .0
    } else {
        Vec::new()
    };
    let reference_manifest_xml = reference_manifest
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            format!(
                "<reference index=\"{}\" marker=\"@图{}\" role=\"{}\" assetId=\"{}\" imageId=\"{}\" name=\"{}\" url=\"{}\"></reference>",
                index + 1,
                index + 1,
                asset_reference_role(&reference.asset_type),
                reference.asset_id,
                reference.image_id,
                xml_attribute(&reference.asset_name),
                xml_attribute(&reference.file_path),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let structured_shots =
        load_structured_shot_descriptions(
            pool,
            project_id,
            script_id,
            track_id,
            mode == "text",
        )
        .await?;
    let structured_shots_json = serde_json::to_string(&structured_shots)
        .map_err(|error| format!("序列化结构化镜头失败：{error}"))?;
    let storyboard_items = boards
        .into_iter()
        .map(|(prompt, video_desc, duration, scene_key)| {
            let video_desc = video_desc
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| prompt.clone());
            format!(
                "<storyboardItem sceneKey=\"{}\" videoDesc=\"{}\" prompt=\"{}\" duration=\"{}\"></storyboardItem>",
                xml_attribute(scene_key.as_deref().unwrap_or_default()),
                xml_attribute(&video_desc),
                xml_attribute(&prompt),
                xml_attribute(duration.as_deref().unwrap_or_default()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let content = format!(
        "模型名称：{resolved_model}\n模式：{mode}\n视觉规范：{visual}\n<referenceManifest>\n{reference_manifest_xml}\n</referenceManifest>\n<structuredShots>{structured_shots_json}</structuredShots>\n<transition type=\"{}\" framePolicy=\"{}\" previousTrackId=\"{}\" fromSceneKey=\"{}\" toSceneKey=\"{}\" description=\"{}\"></transition>\n<storyboardItems>\n{}\n</storyboardItems>",
        xml_attribute(&transition_type),
        xml_attribute(&frame_policy),
        previous_track_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        xml_attribute(&from_scene_key),
        xml_attribute(&first_scene_key),
        xml_attribute(&transition_description),
        storyboard_items,
    );
    match ai_client::project_text(pool, "universalAi", project_id, &system, &content).await {
        Ok(text) => {
            let text = match compile_video_prompt_with_manifest(&text, &reference_manifest) {
                Ok(text) => text,
                Err(reason) => {
                    let _ = sqlx::query(
                        "UPDATE toonflow.video_tracks SET state='生成失败',reason=$2
                         WHERE id=$1 AND project_id=$3",
                    )
                    .bind(track_id)
                    .bind(&reason)
                    .bind(project_id)
                    .execute(pool)
                    .await;
                    return Err(reason);
                }
            };
            sqlx::query(
                "UPDATE toonflow.video_tracks SET prompt=$2,state='已完成',reason=NULL
                 WHERE id=$1 AND project_id=$3",
            )
            .bind(track_id)
            .bind(&text)
            .bind(project_id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
            Ok(text)
        }
        Err(reason) => {
            let _ = sqlx::query(
                "UPDATE toonflow.video_tracks SET state='生成失败',reason=$2
                 WHERE id=$1 AND project_id=$3",
            )
            .bind(track_id)
            .bind(&reason)
            .bind(project_id)
            .execute(pool)
            .await;
            Err(reason)
        }
    }
}

fn xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn video_prompt_name(model: &str, mode: &str) -> &'static str {
    let model = model.to_ascii_lowercase();
    if model.contains("wan") && model.contains("2.6") {
        "wan_2_6_single_image_first_frame"
    } else if model.contains("seedance")
        && (model.contains("2.0") || model.contains("2-0") || model.contains("2_0"))
    {
        "seedance_2_multi_parameter"
    } else if matches!(
        mode,
        "startEndRequired" | "endFrameOptional" | "startFrameOptional"
    ) {
        "universal_first_last_frame"
    } else {
        "universal_multi_parameter"
    }
}

fn model_parameter(value: &Value) -> Result<String, AppError> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .ok_or_else(|| AppError::bad_request("视频模型参数必须是模型 ID 或名称"))
}

#[cfg(test)]
mod prompt_tests {
    use super::{
        FRAME_POLICY_OWN, FRAME_POLICY_PREVIOUS_TAIL, Generate, TrackTransitionSettings,
        WorkflowVideoJob, asset_reference_role, compile_video_prompt_with_manifest,
        merge_references, model_parameter, normalize_storyboard_ids, parse_shot_duration,
        reference_kind, references_for_mode, validate_prompt_references, video_prompt_name,
        workflow_job_is_ready, xml_attribute,
    };
    use crate::toonflow_asset_context::TrackAssetReference;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    fn workflow_job(
        track_id: i64,
        previous_track_id: Option<i64>,
        frame_policy: &str,
    ) -> WorkflowVideoJob {
        WorkflowVideoJob {
            previous_video_id: None,
            id: track_id + 100,
            script_id: 1,
            track_id,
            prompt: String::new(),
            duration: 5,
            model: "video-model".into(),
            mode: "startEndRequired".into(),
            ratio: "16:9".into(),
            resolution: "1080p".into(),
            audio: false,
            references: json!([]),
            dropped_references: Vec::new(),
            transition_settings: TrackTransitionSettings {
                transition_type: "continuous".into(),
                frame_policy: frame_policy.into(),
                previous_track_id,
                transition_source: "director".into(),
            },
        }
    }

    #[test]
    fn selects_the_original_toonflow_video_prompt_variants() {
        assert_eq!(
            video_prompt_name("doubao-seedance-2-0-260128", "text"),
            "seedance_2_multi_parameter"
        );
        assert_eq!(
            video_prompt_name("wan2.6", "text"),
            "wan_2_6_single_image_first_frame"
        );
        assert_eq!(
            video_prompt_name("doubao-seedance-1-5-pro", "startEndRequired"),
            "universal_first_last_frame"
        );
        assert_eq!(
            video_prompt_name("doubao-seedance-1-5-pro", "text"),
            "universal_multi_parameter"
        );
    }

    #[test]
    fn accepts_numeric_and_string_model_parameters() {
        assert_eq!(model_parameter(&json!(123)).unwrap(), "123");
        assert_eq!(model_parameter(&json!("seedance")).unwrap(), "seedance");
        assert!(model_parameter(&json!({})).is_err());
    }

    #[test]
    fn workflow_only_blocks_real_in_batch_tail_dependencies() {
        let ranks = HashMap::from([(1, 0), (2, 1), (3, 2)]);
        let mut completed = HashSet::new();
        let dependent = workflow_job(2, Some(1), FRAME_POLICY_PREVIOUS_TAIL);
        assert!(!workflow_job_is_ready(&dependent, &ranks, &completed));
        completed.insert(1);
        assert!(workflow_job_is_ready(&dependent, &ranks, &completed));

        assert!(workflow_job_is_ready(
            &workflow_job(3, None, FRAME_POLICY_OWN),
            &ranks,
            &HashSet::new(),
        ));
        assert!(workflow_job_is_ready(
            &workflow_job(2, Some(99), FRAME_POLICY_PREVIOUS_TAIL),
            &ranks,
            &HashSet::new(),
        ));
        assert!(!workflow_job_is_ready(
            &workflow_job(2, Some(3), FRAME_POLICY_PREVIOUS_TAIL),
            &ranks,
            &HashSet::new(),
        ));
    }

    #[test]
    fn escapes_structured_storyboard_prompt_attributes() {
        assert_eq!(
            xml_attribute("A&B <镜头> \"推进\""),
            "A&amp;B &lt;镜头&gt; &quot;推进&quot;"
        );
    }

    #[test]
    fn normalizes_duplicate_storyboard_ids_before_binding() {
        assert_eq!(normalize_storyboard_ids(vec![3, 1, 3, 2, 1]), vec![1, 2, 3]);
    }

    #[test]
    fn video_generation_accepts_numeric_model_id() {
        let request: Generate = serde_json::from_value(json!({
            "projectId": 1,
            "scriptId": 2,
            "prompt": "镜头提示词",
            "model": 1784249635985_i64,
            "mode": "startEndRequired",
            "resolution": "1080p",
            "duration": 5,
            "trackId": 3,
            "uploadData": []
        }))
        .unwrap();
        assert_eq!(request.model, "1784249635985");
    }

    #[test]
    fn places_unique_canonical_assets_before_storyboard_media() {
        let (references, dropped) = merge_references(
            json!([
                {"id": 1, "src": "https://example.com/storyboard.png"},
                "https://example.com/direct.png"
            ]),
            vec![
                "https://example.com/role.png".into(),
                "https://example.com/direct.png".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            references,
            json!([
                "https://example.com/role.png",
                "https://example.com/direct.png",
                "https://example.com/storyboard.png",
            ])
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn drops_excess_upload_references_and_reports_them() {
        let (references, dropped) = merge_references(
            json!(["frame-1", "frame-2"]),
            vec!["role-1".into(), "scene-1".into(), "tool-1".into()],
        )
        .unwrap();
        assert_eq!(
            references,
            json!(["role-1", "scene-1", "tool-1", "frame-1"])
        );
        assert_eq!(dropped, vec!["frame-2".to_string()]);
    }

    fn track_reference(name: &str, asset_type: &str) -> TrackAssetReference {
        TrackAssetReference {
            asset_id: 0,
            asset_name: name.into(),
            asset_type: asset_type.into(),
            image_id: 0,
            file_path: format!("{name}.png"),
        }
    }

    #[test]
    fn selects_references_by_priority_and_keeps_first_use_order() {
        let references = vec![
            track_reference("S1", "scene"),
            track_reference("R1", "role"),
            track_reference("P1", "prop"),
            track_reference("S2", "scene"),
            track_reference("R2", "character"),
            track_reference("P2", "prop"),
        ];
        let (selected, dropped) =
            crate::toonflow_asset_context::select_references_within_cap(references, 4);
        let names = selected
            .iter()
            .map(|reference| reference.asset_name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["R1", "P1", "R2", "P2"]);
        let dropped_names = dropped
            .iter()
            .map(|entry| entry["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(dropped_names, vec!["S1", "S2"]);
        assert!(dropped
            .iter()
            .all(|entry| entry["reason"].as_str().unwrap().contains("上限")));
    }

    #[test]
    fn keeps_all_references_within_the_cap() {
        let references = vec![
            track_reference("R1", "role"),
            track_reference("S1", "scene"),
            track_reference("P1", "prop"),
            track_reference("R2", "role"),
        ];
        let (selected, dropped) =
            crate::toonflow_asset_context::select_references_within_cap(references, 4);
        assert_eq!(selected.len(), 4);
        assert!(dropped.is_empty());
    }

    #[test]
    fn separates_frame_modes_from_multi_reference_mode() {
        let frames = json!(["first", "middle", "last"]);
        let assets = vec!["role".into(), "scene".into()];
        assert_eq!(
            references_for_mode(frames.clone(), assets.clone(), &json!("startEndRequired"))
                .unwrap()
                .0,
            json!(["first", "last"])
        );
        assert_eq!(
            references_for_mode(frames.clone(), assets.clone(), &json!("endFrameOptional"))
                .unwrap()
                .0,
            json!(["first", "last"])
        );
        assert_eq!(
            references_for_mode(frames.clone(), assets.clone(), &json!("startFrameOptional"))
                .unwrap()
                .0,
            json!(["first", "last"])
        );
        assert_eq!(
            references_for_mode(frames.clone(), assets.clone(), &json!("singleImage"))
                .unwrap()
                .0,
            json!(["first"])
        );
        // Text mode keeps assets ahead of frame media and reports the cut.
        let (references, dropped) =
            references_for_mode(frames, assets, &json!("text")).unwrap();
        assert_eq!(references, json!(["role", "scene", "first", "middle"]));
        assert_eq!(dropped, vec!["last".to_string()]);
        assert_eq!(
            references_for_mode(json!(["only"]), Vec::new(), &json!("startFrameOptional"))
                .unwrap()
                .0,
            json!(["only"])
        );
    }

    #[test]
    fn uses_actual_last_frame_and_checks_prompt_indices() {
        assert_eq!(
            references_for_mode(
                json!(["1", "2", "3", "4", "5"]),
                vec![],
                &json!("startEndRequired")
            )
            .unwrap()
            .0,
            json!(["1", "5"])
        );
        assert!(references_for_mode(json!(["1"]), vec![], &json!("startEndRequired")).is_err());
        assert!(validate_prompt_references("@图1 人物参考 @图2 场景", 2).is_ok());
        assert!(validate_prompt_references("@图3", 2).is_err());
        assert!(validate_prompt_references("@图0", 2).is_err());
        assert_eq!(reference_kind("text", 0, 1), "reference_image");
        assert_eq!(reference_kind("startEndRequired", 0, 2), "first_frame");
        assert_eq!(reference_kind("startEndRequired", 1, 2), "last_frame");
        assert_eq!(reference_kind("startFrameOptional", 0, 1), "last_frame");
        assert_eq!(asset_reference_role("role"), "required_subject");
        assert_eq!(asset_reference_role("scene"), "environment_reference");
    }

    #[test]
    fn compiles_server_owned_reference_declarations() {
        let references = vec![
            TrackAssetReference {
                asset_id: 11,
                asset_name: "沈辞".into(),
                asset_type: "role".into(),
                image_id: 101,
                file_path: "role.png".into(),
            },
            TrackAssetReference {
                asset_id: 22,
                asset_name: "城楼".into(),
                asset_type: "scene".into(),
                image_id: 202,
                file_path: "scene.png".into(),
            },
        ];
        let prompt = compile_video_prompt_with_manifest(
            "@图1 走向 @图2，镜头缓慢推进。",
            &references,
        )
        .unwrap();
        assert!(prompt.starts_with(
            "参考图清单：@图1 为沈辞（role，资产ID=11，图片ID=101）；@图2 为城楼（scene，资产ID=22，图片ID=202）"
        ));
        assert!(prompt.ends_with("@图1 走向 @图2，镜头缓慢推进。"));
        assert!(compile_video_prompt_with_manifest("@图1 独自行走", &references)
            .unwrap_err()
            .contains("缺少服务端参考清单中的 @图2"));
        assert!(compile_video_prompt_with_manifest("@图3 出现", &references).is_err());
    }

    #[test]
    fn parses_positive_structured_shot_durations_without_coercion() {
        assert_eq!(parse_shot_duration("4", 1).unwrap(), 4.0);
        assert_eq!(parse_shot_duration("2.5s", 2).unwrap(), 2.5);
        assert_eq!(parse_shot_duration("3秒", 3).unwrap(), 3.0);
        assert!(parse_shot_duration("0", 4).unwrap_err().contains("必须大于 0"));
        assert!(parse_shot_duration("未知", 5).unwrap_err().contains("时长无效"));
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptGenerate {
    track_id: i64,
    project_id: i64,
    info: Value,
    model: Value,
    mode: String,
}
pub async fn generate_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<PromptGenerate>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:scene:update")?;
    let (project_id, _) = ensure_track_access(&state.pool, &user, req.track_id).await?;
    if project_id != req.project_id {
        return Err(AppError::not_found("video track not found"));
    }
    let _ = req.info;
    let model = model_parameter(&req.model)?;
    let text = create_prompt(&state.pool, req.track_id, req.project_id, &model, &req.mode)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(ApiResponse::new(text)))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckPrompt {
    project_id: i64,
    script_id: i64,
    track_ids: Vec<i64>,
}
pub async fn check_prompts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<CheckPrompt>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let rows=sqlx::query_as::<_,(i64,String,Option<String>,Option<String>)>("SELECT id,coalesce(state,''),reason,prompt FROM toonflow.video_tracks WHERE project_id=$1 AND script_id=$2 AND id=ANY($3) AND state IN('已完成','生成失败')").bind(req.project_id).bind(req.script_id).bind(req.track_ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to check prompts"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"state":r.1,"reason":r.2,"prompt":r.3}))
            .collect(),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackPrompt {
    track_id: i64,
    info: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPrompts {
    project_id: i64,
    track_data: Vec<TrackPrompt>,
    mode: String,
    model: Value,
    concurrent_count: Option<usize>,
}
pub async fn batch_prompts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<BatchPrompts>,
) -> Result<Json<ApiResponse<&'static str>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_project_access(&state.pool, &user, req.project_id).await?;
    let mut track_ids = req
        .track_data
        .iter()
        .map(|track| track.track_id)
        .collect::<Vec<_>>();
    let requested_track_count = track_ids.len();
    track_ids.sort_unstable();
    track_ids.dedup();
    if track_ids.is_empty() {
        return Err(AppError::bad_request("请选择至少一个视频轨道"));
    }
    if track_ids.len() != requested_track_count {
        return Err(AppError::bad_request("批量视频轨道不能重复"));
    }
    let track_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM toonflow.video_tracks
         WHERE project_id=$1 AND id=ANY($2)",
    )
    .bind(req.project_id)
    .bind(&track_ids)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to authorize prompt tracks"))?;
    if track_count != track_ids.len() as i64 {
        return Err(AppError::not_found("video track not found"));
    }
    let model = model_parameter(&req.model)?;
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(
            req.concurrent_count.unwrap_or(5).clamp(1, 20),
        ));
        for track in req.track_data {
            let _ = track.info;
            let permit = sem.clone().acquire_owned().await;
            let pool = pool.clone();
            let model = model.clone();
            let mode = req.mode.clone();
            let project_id = req.project_id;
            tokio::spawn(async move {
                if permit.is_ok() {
                    let _ = create_prompt(&pool, track.track_id, project_id, &model, &mode).await;
                }
            });
        }
    });
    Ok(Json(ApiResponse::new("开始生成提示词")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoBatchItem {
    upload_data: Value,
    track_id: i64,
    prompt: String,
    duration: i32,
    #[serde(skip)]
    transition_settings: Option<TrackTransitionSettings>,
    #[serde(skip)]
    dropped_references: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchVideos {
    project_id: i64,
    script_id: i64,
    track_data: Vec<VideoBatchItem>,
    #[serde(deserialize_with = "deserialize_model")]
    model: String,
    mode: Value,
    resolution: String,
    audio: Option<bool>,
}

fn deserialize_model<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .ok_or_else(|| D::Error::custom("视频模型参数必须是模型 ID 或名称"))
}
pub async fn batch_videos(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(mut req): Json<BatchVideos>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_project_script_access(&state.pool, &user, req.project_id, req.script_id).await?;
    let mut track_ids = req
        .track_data
        .iter()
        .map(|track| track.track_id)
        .collect::<Vec<_>>();
    let requested_track_count = track_ids.len();
    track_ids.sort_unstable();
    track_ids.dedup();
    if track_ids.is_empty() {
        return Err(AppError::bad_request("请选择至少一个视频轨道"));
    }
    if track_ids.len() != requested_track_count {
        return Err(AppError::bad_request("批量视频轨道不能重复"));
    }
    let ordered_track_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT track.id FROM toonflow.video_tracks track
         WHERE track.project_id=$1 AND track.script_id=$2 AND track.id=ANY($3)
         ORDER BY coalesce((
                    SELECT min(board.index)
                    FROM toonflow.storyboards board
                    WHERE board.track_id=track.id
                  ),2147483647),track.sort_order,track.id",
    )
    .bind(req.project_id)
    .bind(req.script_id)
    .bind(&track_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to authorize video tracks"))?;
    if ordered_track_ids.len() != track_ids.len() {
        return Err(AppError::not_found("video track not found"));
    }
    crate::toonflow_scene_consistency::ensure_track_storyboard_images_current(
        &state.pool,
        req.project_id,
        req.script_id,
        &track_ids,
    )
    .await?;
    req.track_data.sort_by_key(|track| {
        ordered_track_ids
            .iter()
            .position(|track_id| *track_id == track.track_id)
            .unwrap_or(usize::MAX)
    });
    for track in &req.track_data {
        ensure_internal_references_in_project(&track.upload_data, req.project_id)?;
    }
    let mut prepared_tracks = Vec::with_capacity(req.track_data.len());
    for mut track in req.track_data {
        track.transition_settings = Some(
            load_track_settings(&state.pool, req.project_id, req.script_id, track.track_id)
                .await
                .map_err(AppError::bad_request)?,
        );
        if let Some(settings) = track.transition_settings.as_mut() {
            validate_continuity_mode(req.mode.as_str().unwrap_or("text"), &settings.frame_policy)
                .map_err(AppError::bad_request)?;
            if settings.frame_policy == FRAME_POLICY_PREVIOUS_TAIL
                && settings.previous_track_id.is_none()
            {
                settings.previous_track_id = resolve_previous_track_id(
                    &state.pool,
                    req.project_id,
                    req.script_id,
                    track.track_id,
                    None,
                )
                .await
                .map_err(AppError::bad_request)?;
            }
        }
        let (asset_references, mut dropped_references) = if req.mode.as_str() == Some("text") {
            let (manifest, drops) =
                crate::toonflow_asset_context::load_track_asset_reference_selection(
                    &state.pool,
                    req.project_id,
                    req.script_id,
                    track.track_id,
                    VIDEO_REFERENCE_CAP,
                )
                .await
                .map_err(|_| AppError::internal("failed to load video asset references"))?;
            (
                manifest
                    .into_iter()
                    .map(|reference| reference.file_path)
                    .collect::<Vec<_>>(),
                drops,
            )
        } else {
            (
                crate::toonflow_asset_context::load_track_asset_references(
                    &state.pool,
                    req.project_id,
                    req.script_id,
                    track.track_id,
                )
                .await
                .map_err(|_| AppError::internal("failed to load video asset references"))?,
                Vec::new(),
            )
        };
        let (references, dropped_uploads) =
            references_for_mode(track.upload_data, asset_references, &req.mode)
                .map_err(AppError::bad_request)?;
        dropped_references.extend(dropped_uploads.into_iter().map(|file_path| {
            json!({"filePath": file_path, "reason": "参考图超过上限，优先保留资产参考，舍弃附加参考图"})
        }));
        track.upload_data = references;
        track.dropped_references = dropped_references;
        prepared_tracks.push(track);
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    let mut jobs = Vec::with_capacity(prepared_tracks.len());
    for track in prepared_tracks {
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO toonflow.videos(state,script_id,project_id,video_track_id,time)
             SELECT '生成中',$1,$2,$3,$4
             WHERE EXISTS(
               SELECT 1 FROM toonflow.video_tracks
               WHERE id=$3 AND project_id=$2 AND script_id=$1
             )
             RETURNING id",
        )
        .bind(req.script_id)
        .bind(req.project_id)
        .bind(track.track_id)
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to create video"))?;
        let Some(id) = id else {
            tx.rollback()
                .await
                .map_err(|_| AppError::internal("failed transaction"))?;
            return Err(AppError::not_found("video track not found"));
        };
        jobs.push((id, track));
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    let response = jobs
        .iter()
        .map(|j| json!({"videoId":j.0,"trackId":j.1.track_id}))
        .collect();
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let ratio: Option<(String,)> =
            sqlx::query_as("SELECT video_ratio FROM toonflow.projects WHERE id=$1")
                .bind(req.project_id)
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten();
        let ratio = ratio.map(|r| r.0).unwrap_or_else(|| "16:9".into());
        let jobs = jobs
            .into_iter()
            .map(|(id, track)| WorkflowVideoJob {
                id,
                previous_video_id: None,
                script_id: req.script_id,
                track_id: track.track_id,
                prompt: track.prompt,
                duration: track.duration,
                model: req.model.clone(),
                mode: req.mode.as_str().unwrap_or("text").into(),
                ratio: ratio.clone(),
                resolution: req.resolution.clone(),
                audio: req.audio.unwrap_or(false),
                references: track.upload_data,
                dropped_references: track.dropped_references,
                transition_settings: track.transition_settings.expect("prepared track settings"),
            })
            .collect();
        // Both entrypoints use the same dependency and acceptance rules. ID 0
        // has no workflow row; batch progress is carried by the video rows.
        run_workflow_video_generation(pool, req.project_id, jobs, 2, 0).await;
    });
    Ok(Json(ApiResponse::new(response)))
}
