use axum::{
    Json,
    extract::{Path, State},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Row};

pub use crate::toonflow_project_crud::list_projects;
use crate::{
    ToonState,
    shared::{affected, require},
    toonflow_episode_renders::{ensure_project_access, ensure_script_in_project},
    toonflow_materials::save_asset_cover_data_url,
    toonflow_pagination::{PageData, default_limit, default_page},
    toonflow_project_helpers::{default_should_generate, ensure_project, next_id, now_ms},
    toonflow_scene_transitions::{
        apply_track_transition_defaults, normalize_persisted_scene_key,
        persist_work_data_with_transition_sync,
    },
    toonflow_storage::{delete_asset_file, enqueue_cleanup_paths},
};

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ToonflowProject {
    pub id: i64,
    pub project_type: String,
    pub chat_model: Option<i64>,
    pub image_model: Option<i64>,
    pub image_quality: String,
    pub video_model: Option<i64>,
    pub name: String,
    pub intro: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub art_style: String,
    pub director_manual: String,
    pub mode: String,
    pub video_ratio: String,
    pub create_time: i64,
    pub update_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProjectRequest {
    pub id: Option<i64>,
    #[serde(default)]
    pub project_type: String,
    #[serde(default)]
    pub chat_model: Option<i64>,
    #[serde(default)]
    pub image_model: Option<i64>,
    #[serde(default)]
    pub image_quality: String,
    #[serde(default)]
    pub video_model: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub intro: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub art_style: String,
    #[serde(default)]
    pub director_manual: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub video_ratio: String,
}

#[derive(Debug, Deserialize)]
pub struct IdRequest {
    pub id: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapter {
    pub id: i64,
    #[serde(rename = "index")]
    pub chapter_index: i32,
    pub reel: String,
    pub chapter: String,
    pub chapter_data: String,
    pub project_id: i64,
    pub event_state: i32,
    pub event: Option<String>,
    pub error_reason: Option<String>,
    pub create_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelItemRequest {
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub reel: String,
    pub chapter: String,
    pub chapter_data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddNovelRequest {
    pub project_id: i64,
    pub data: Vec<NovelItemRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListNovelRequest {
    pub project_id: i64,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    pub search: Option<String>,
}

pub async fn add_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<AddNovelRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    ensure_project(&state.pool, request.project_id).await?;
    let last: Option<(i32,)> = sqlx::query_as(
        "SELECT chapter_index FROM toonflow.novels WHERE project_id=$1 ORDER BY chapter_index DESC LIMIT 1",
    )
    .bind(request.project_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to read novel chapters"))?;
    let mut chapter_index = last.map(|row| row.0).unwrap_or(0);
    let mut ids = Vec::with_capacity(request.data.len());
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to add novel chapters"))?;
    for (idx, item) in request.data.into_iter().enumerate() {
        chapter_index += 1;
        let id = next_id(idx as i64);
        sqlx::query(
            r#"INSERT INTO toonflow.novels
               (id, chapter_index, reel, chapter, chapter_data, project_id, event_state, create_time)
               VALUES ($1,$2,$3,$4,$5,$6,0,$7)"#,
        )
        .bind(id)
        .bind(if item.index > 0 { item.index } else { chapter_index })
        .bind(item.reel)
        .bind(item.chapter)
        .bind(item.chapter_data)
        .bind(request.project_id)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to add novel chapter"))?;
        ids.push(id);
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to add novel chapters"))?;
    Ok(Json(ApiResponse::with_message(
        json!({ "ids": ids }),
        "新增原文成功",
    )))
}

pub async fn list_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ListNovelRequest>,
) -> Result<Json<ApiResponse<PageData<NovelChapter>>>, AppError> {
    require(&user, "toon:project:read")?;
    let search = request.search.unwrap_or_default();
    let search_pattern = format!("%{search}%");
    let limit = request.limit.clamp(1, 200);
    let offset = (request.page.max(1) - 1) * limit;
    let rows = sqlx::query_as::<_, NovelChapter>(
        r#"SELECT id, chapter_index, reel, chapter, chapter_data, project_id, event_state,
                  event, error_reason, create_time
           FROM toonflow.novels
           WHERE project_id=$1 AND ($2 = '' OR chapter ILIKE $3)
           ORDER BY chapter_index ASC LIMIT $4 OFFSET $5"#,
    )
    .bind(request.project_id)
    .bind(&search)
    .bind(&search_pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list novel chapters"))?;
    let total: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM toonflow.novels WHERE project_id=$1 AND ($2 = '' OR chapter ILIKE $3)",
    )
    .bind(request.project_id)
    .bind(&search)
    .bind(&search_pattern)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to count novel chapters"))?;
    Ok(Json(ApiResponse::new(PageData {
        data: rows,
        total: total.0,
    })))
}

pub async fn all_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectIdRequest>,
) -> Result<Json<ApiResponse<Vec<NovelChapter>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, NovelChapter>(
        r#"SELECT id, chapter_index, reel, chapter, chapter_data, project_id, event_state,
                  event, error_reason, create_time
           FROM toonflow.novels WHERE project_id=$1 ORDER BY chapter_index ASC"#,
    )
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list novel chapters"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Serialize, FromRow)]
pub struct NovelIndexRow {
    pub id: i64,
    #[serde(rename = "index")]
    pub chapter_index: i32,
    pub chapter: String,
}

pub async fn novel_index(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectIdRequest>,
) -> Result<Json<ApiResponse<Vec<NovelIndexRow>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as(
        "SELECT id,chapter_index,chapter FROM toonflow.novels WHERE project_id=$1 ORDER BY chapter_index",
    )
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list novel index"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
pub struct BatchIdsRequest {
    pub ids: Vec<i64>,
}

pub async fn batch_delete_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<BatchIdsRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.ids.is_empty() {
        return Err(AppError::bad_request("请先选择需要删除的内容"));
    }
    sqlx::query("DELETE FROM toonflow.novels WHERE id=ANY($1)")
        .bind(request.ids)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to batch delete novel chapters"))?;
    Ok(Json(ApiResponse::with_message((), "删除原文成功")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNovelRequest {
    pub id: i64,
    #[serde(alias = "index")]
    pub chapter_index: i32,
    pub reel: String,
    pub chapter: String,
    pub chapter_data: String,
    pub event: Option<String>,
}

pub async fn update_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UpdateNovelRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let result = sqlx::query(
        r#"UPDATE toonflow.novels
           SET chapter_index=$2, reel=$3, chapter=$4, chapter_data=$5, event=$6
           WHERE id=$1"#,
    )
    .bind(request.id)
    .bind(request.chapter_index)
    .bind(request.reel)
    .bind(request.chapter)
    .bind(request.chapter_data)
    .bind(request.event)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update novel chapter"))?;
    affected(result.rows_affected(), "novel")?;
    Ok(Json(ApiResponse::with_message((), "更新原文成功")))
}

pub async fn delete_novel(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<IdRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let result = sqlx::query("DELETE FROM toonflow.novels WHERE id=$1")
        .bind(request.id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to delete novel chapter"))?;
    affected(result.rows_affected(), "novel")?;
    Ok(Json(ApiResponse::with_message((), "删除原文成功")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdRequest {
    pub project_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSummary {
    pub id: i64,
    pub name: String,
    pub content: String,
    pub project_id: i64,
    pub extract_state: Option<i32>,
    pub error_reason: Option<String>,
    pub create_time: i64,
    pub related_assets: Vec<Value>,
}

#[derive(Debug, FromRow)]
struct ScriptRow {
    id: i64,
    name: String,
    content: String,
    project_id: i64,
    extract_state: Option<i32>,
    error_reason: Option<String>,
    create_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListScriptRequest {
    pub project_id: i64,
    pub name: Option<String>,
}

pub async fn list_scripts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ListScriptRequest>,
) -> Result<Json<ApiResponse<Vec<ScriptSummary>>>, AppError> {
    require(&user, "toon:episode:read")?;
    let name = request.name.unwrap_or_default();
    let pattern = format!("%{name}%");
    let scripts = sqlx::query_as::<_, ScriptRow>(
        r#"SELECT id, name, content, project_id, extract_state, error_reason, create_time
           FROM toonflow.scripts
           WHERE project_id=$1 AND ($2 = '' OR name ILIKE $3)
           ORDER BY create_time DESC"#,
    )
    .bind(request.project_id)
    .bind(&name)
    .bind(&pattern)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list scripts"))?;
    let ids: Vec<i64> = scripts.iter().map(|script| script.id).collect();
    let asset_rows = sqlx::query(
        r#"SELECT sa.script_id, a.id, a.name
           FROM toonflow.script_assets sa
           JOIN toonflow.assets a ON a.id = sa.asset_id
           WHERE sa.script_id = ANY($1)"#,
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list script assets"))?;
    let mut result = Vec::with_capacity(scripts.len());
    for script in scripts {
        let related_assets = asset_rows
            .iter()
            .filter(|row| row.get::<i64, _>("script_id") == script.id)
            .map(|row| json!({ "id": row.get::<i64, _>("id"), "name": row.get::<String, _>("name") }))
            .collect();
        result.push(ScriptSummary {
            id: script.id,
            name: script.name,
            content: script.content,
            project_id: script.project_id,
            extract_state: script.extract_state,
            error_reason: script.error_reason,
            create_time: script.create_time,
            related_assets,
        });
    }
    Ok(Json(ApiResponse::new(result)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveScriptRequest {
    pub id: Option<i64>,
    pub name: String,
    pub content: String,
    pub project_id: Option<i64>,
    pub assets: Option<Vec<i64>>,
}

pub async fn add_script(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveScriptRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:episode:create")?;
    let project_id = request
        .project_id
        .ok_or_else(|| AppError::bad_request("projectId is required"))?;
    let id = request.id.unwrap_or_else(|| next_id(0));
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to create script"))?;
    sqlx::query(
        "INSERT INTO toonflow.scripts (id, name, content, project_id, create_time) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(request.name)
    .bind(request.content)
    .bind(project_id)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to create script"))?;
    sync_script_assets(&mut tx, id, request.assets.as_deref().unwrap_or(&[])).await?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to create script"))?;
    Ok(Json(ApiResponse::with_message(
        json!({ "id": id }),
        "添加剧本成功",
    )))
}

pub async fn update_script(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveScriptRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:episode:update")?;
    let id = request
        .id
        .ok_or_else(|| AppError::bad_request("script id is required"))?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to update script"))?;
    let result = sqlx::query("UPDATE toonflow.scripts SET name=$2, content=$3 WHERE id=$1")
        .bind(id)
        .bind(request.name)
        .bind(request.content)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to update script"))?;
    affected(result.rows_affected(), "script")?;
    if let Some(assets) = request.assets.as_deref() {
        sync_script_assets(&mut tx, id, assets).await?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to update script"))?;
    Ok(Json(ApiResponse::with_message((), "编辑剧本成功")))
}

async fn sync_script_assets(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    script_id: i64,
    asset_ids: &[i64],
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM toonflow.script_assets WHERE script_id=$1")
        .bind(script_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::internal("failed to update script assets"))?;
    for asset_id in asset_ids {
        sqlx::query(
            "INSERT INTO toonflow.script_assets (script_id, asset_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(script_id)
        .bind(asset_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::internal("failed to update script assets"))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct DeleteScriptsRequest {
    pub ids: Vec<i64>,
}

pub async fn delete_scripts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<DeleteScriptsRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:episode:delete")?;
    if request.ids.is_empty() {
        return Err(AppError::bad_request("script ids are required"));
    }
    let mut script_ids = request.ids;
    script_ids.sort_unstable();
    script_ids.dedup();
    let scripts: Vec<(i64, i64)> =
        sqlx::query_as("SELECT id,project_id FROM toonflow.scripts WHERE id=ANY($1)")
            .bind(&script_ids)
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to authorize scripts"))?;
    if scripts.len() != script_ids.len() {
        return Err(AppError::not_found("script not found"));
    }
    let mut project_ids = scripts.iter().map(|script| script.1).collect::<Vec<_>>();
    project_ids.sort_unstable();
    project_ids.dedup();
    for project_id in project_ids {
        ensure_project_access(&state.pool, &user, project_id).await?;
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin script deletion"))?;
    sqlx::query("SELECT id FROM toonflow.videos WHERE script_id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&script_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock script videos"))?;
    let locked_scripts: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM toonflow.scripts WHERE id=ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(&script_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to lock scripts"))?;
    if locked_scripts.len() != script_ids.len() {
        return Err(AppError::not_found("script not found"));
    }
    // Re-scan after the parent locks close the window in which a concurrent
    // request could have inserted a new video between both statements.
    sqlx::query("SELECT id FROM toonflow.videos WHERE script_id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&script_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock script videos"))?;
    sqlx::query("SELECT id FROM toonflow.storyboards WHERE script_id=ANY($1) FOR UPDATE")
        .bind(&script_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock script storyboards"))?;
    sqlx::query("SELECT id FROM toonflow.episode_renders WHERE script_id=ANY($1) FOR UPDATE")
        .bind(&script_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock script renders"))?;
    sqlx::query(
        "SELECT frame.previous_video_id
         FROM toonflow.video_continuity_frames frame
         JOIN toonflow.videos video ON video.id=frame.previous_video_id
         WHERE video.script_id=ANY($1) FOR UPDATE OF frame",
    )
    .bind(&script_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to lock script continuity frames"))?;
    let has_active_work: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM toonflow.distributed_jobs job
           CROSS JOIN LATERAL unnest($1::bigint[]) requested(script_id)
           WHERE job.state IN ('queued','retry','running')
             AND job.payload->'scriptId'=to_jsonb(requested.script_id)
           UNION ALL
           SELECT 1 FROM toonflow.workflow_runs
           WHERE script_id=ANY($1) AND state IN ('pending','running')
           UNION ALL
           SELECT 1 FROM toonflow.tasks task
           CROSS JOIN LATERAL unnest($1::bigint[]) requested(script_id)
           WHERE task.state='running'
             AND (
               task.input->>'scriptId'=requested.script_id::text
               OR task.input->'scriptIds' @> jsonb_build_array(requested.script_id)
             )
           UNION ALL
           SELECT 1 FROM toonflow.agent_runs
           WHERE script_id=ANY($1) AND state='running'
           UNION ALL
           SELECT 1 FROM toonflow.scripts
           WHERE id=ANY($1) AND extract_state=2
           UNION ALL
           SELECT 1 FROM toonflow.videos
           WHERE script_id=ANY($1) AND state='生成中'
           UNION ALL
           SELECT 1 FROM toonflow.storyboards
           WHERE script_id=ANY($1) AND state='生成中'
         )",
    )
    .bind(&script_ids)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to inspect active script work"))?;
    if has_active_work {
        return Err(AppError::bad_request(
            "剧集仍有生成或合并任务运行，请等待完成或取消后再删除",
        ));
    }
    let paths: Vec<String> = sqlx::query_scalar(
        "SELECT file_path FROM toonflow.storyboards
           WHERE script_id=ANY($1) AND coalesce(file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.videos
           WHERE script_id=ANY($1) AND coalesce(file_path,'')<>''
         UNION ALL SELECT frame.file_path
           FROM toonflow.video_continuity_frames frame
           JOIN toonflow.videos video ON video.id=frame.previous_video_id
           WHERE video.script_id=ANY($1) AND coalesce(frame.file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.episode_renders
           WHERE script_id=ANY($1) AND coalesce(file_path,'')<>''
         UNION ALL SELECT cover_path FROM toonflow.episode_renders
           WHERE script_id=ANY($1) AND coalesce(cover_path,'')<>''
         UNION ALL SELECT job.result->>'stagingObjectPath'
           FROM toonflow.distributed_jobs job
           CROSS JOIN LATERAL unnest($1::bigint[]) requested(script_id)
           WHERE job.payload->'scriptId'=to_jsonb(requested.script_id)
             AND coalesce(job.result->>'stagingObjectPath','')<>''",
    )
    .bind(&script_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to collect script files"))?;
    enqueue_cleanup_paths(
        &mut tx,
        &paths,
        "script",
        None,
        "剧集记录已删除，等待引用感知对象清理",
    )
    .await
    .map_err(|_| AppError::internal("failed to enqueue script cleanup"))?;
    sqlx::query("DELETE FROM toonflow.script_assets WHERE script_id = ANY($1)")
        .bind(&script_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to clear script assets"))?;
    sqlx::query("UPDATE toonflow.assets SET script_id=NULL WHERE script_id = ANY($1)")
        .bind(&script_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to detach script assets"))?;
    let deleted = sqlx::query("DELETE FROM toonflow.scripts WHERE id = ANY($1)")
        .bind(&script_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete scripts"))?;
    if deleted.rows_affected() != script_ids.len() as u64 {
        return Err(AppError::not_found("script not found"));
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit script deletion"))?;
    Ok(Json(ApiResponse::with_message((), "删除剧本成功")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchAddScriptRequest {
    pub project_id: i64,
    pub data: Vec<BatchScriptItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchScriptItem {
    pub script_name: String,
    pub script_data: String,
}

pub async fn batch_add_scripts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<BatchAddScriptRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:episode:create")?;
    let mut ids = Vec::with_capacity(request.data.len());
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to create scripts"))?;
    for (idx, item) in request.data.into_iter().enumerate() {
        let id = next_id(idx as i64);
        sqlx::query(
            "INSERT INTO toonflow.scripts (id, name, content, project_id, create_time) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind(item.script_name)
        .bind(item.script_data)
        .bind(request.project_id)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to create scripts"))?;
        ids.push(id);
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to create scripts"))?;
    Ok(Json(ApiResponse::with_message(
        json!({ "ids": ids }),
        "添加剧本成功",
    )))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AssetRow {
    pub id: i64,
    pub name: String,
    pub prompt: String,
    pub remark: Option<String>,
    #[serde(rename = "type")]
    pub type_: String,
    pub description: String,
    pub script_id: Option<i64>,
    pub image_id: Option<i64>,
    pub image_file_path: Option<String>,
    pub parent_asset_id: Option<i64>,
    pub project_id: i64,
    pub flow_id: Option<i64>,
    pub prompt_state: Option<String>,
    pub audio_bind_state: Option<i32>,
    pub prompt_error_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAssetRequest {
    pub id: Option<i64>,
    pub project_id: i64,
    pub name: String,
    pub prompt: Option<String>,
    pub remark: Option<String>,
    pub r#type: Option<String>,
    #[serde(alias = "desc", alias = "describe")]
    pub description: Option<String>,
    pub script_id: Option<i64>,
    pub parent_asset_id: Option<i64>,
    pub image_id: Option<i64>,
    #[serde(alias = "base64Data")]
    pub base64: Option<String>,
}

pub async fn list_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectIdRequest>,
) -> Result<Json<ApiResponse<Vec<AssetRow>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, AssetRow>(
        r#"SELECT a.id, a.name, a.prompt, a.remark, a.type as type_, a.description,
                  a.script_id, a.image_id, i.file_path as image_file_path,
                  a.parent_asset_id, a.project_id, a.flow_id, a.prompt_state,
                  a.audio_bind_state, a.prompt_error_reason
           FROM toonflow.assets a
           LEFT JOIN toonflow.images i ON i.id = a.image_id
           WHERE a.project_id = $1 AND a.parent_asset_id IS NULL
           ORDER BY a.id DESC"#,
    )
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list assets"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatAssetListRequest {
    pub project_id: i64,
    #[serde(alias = "pageNo", alias = "current")]
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub r#type: Option<String>,
}

/// Compatibility shape for the legacy getAssetsApi contract.
/// The newer Toonflow UI continues using list_assets' flat array response.
pub async fn list_assets_compat(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<CompatAssetListRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, AssetRow>(
        r#"SELECT a.id, a.name, a.prompt, a.remark, a.type as type_, a.description,
                  a.script_id, a.image_id, i.file_path as image_file_path,
                  a.parent_asset_id, a.project_id, a.flow_id, a.prompt_state,
                  a.audio_bind_state, a.prompt_error_reason
           FROM toonflow.assets a
           LEFT JOIN toonflow.images i ON i.id = a.image_id
           WHERE a.project_id = $1 AND a.parent_asset_id IS NULL
             AND ($2::text IS NULL OR a.type=$2)
           ORDER BY a.id DESC"#,
    )
    .bind(request.project_id)
    .bind(request.r#type.as_deref())
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list compatible assets"))?;
    let total = rows.len() as i64;
    let parent_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let child_rows = sqlx::query_as::<_, AssetRow>(
        r#"SELECT a.id, a.name, a.prompt, a.remark, a.type as type_, a.description,
                  a.script_id, a.image_id, i.file_path as image_file_path,
                  a.parent_asset_id, a.project_id, a.flow_id, a.prompt_state,
                  a.audio_bind_state, a.prompt_error_reason
           FROM toonflow.assets a
           LEFT JOIN toonflow.images i ON i.id = a.image_id
           WHERE a.project_id = $1 AND a.parent_asset_id = ANY($2)
           ORDER BY a.id DESC"#,
    )
    .bind(request.project_id)
    .bind(&parent_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list compatible child assets"))?;
    let mut son_assets = std::collections::HashMap::<i64, Vec<Value>>::new();
    for child in child_rows {
        let parent_id = child.parent_asset_id;
        let image_url = child.image_file_path.clone();
        let mut item = serde_json::to_value(child).unwrap_or_else(|_| json!({}));
        if let Some(object) = item.as_object_mut() {
            object.insert("sonAssets".into(), json!([]));
            object.insert("sex".into(), Value::Null);
            object.insert(
                "imageUrl".into(),
                image_url.map(Value::String).unwrap_or(Value::Null),
            );
        }
        if let Some(parent_id) = parent_id {
            son_assets.entry(parent_id).or_default().push(item);
        }
    }
    let page = request.page.unwrap_or(1).max(1);
    let page_size = request.page_size.unwrap_or(20).clamp(1, 200);
    let start = ((page - 1) * page_size) as usize;
    let list = rows
        .into_iter()
        .skip(start)
        .take(page_size as usize)
        .map(|row| {
            let asset_id = row.id;
            let image_url = row.image_file_path.clone();
            let mut item = serde_json::to_value(row).unwrap_or_else(|_| json!({}));
            if let Some(object) = item.as_object_mut() {
                object.insert(
                    "sonAssets".into(),
                    Value::Array(son_assets.get(&asset_id).cloned().unwrap_or_default()),
                );
                object.insert("sex".into(), Value::Null);
                object.insert(
                    "imageUrl".into(),
                    image_url.map(Value::String).unwrap_or(Value::Null),
                );
            }
            item
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({
        "data": list.clone(),
        "list": list,
        "total": total,
        "page": page,
        "pageSize": page_size,
    }))))
}

pub async fn save_asset(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveAssetRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let id = request.id.unwrap_or_else(|| next_id(0));
    if let Some(image_id) = request.image_id {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM toonflow.images i JOIN toonflow.assets a ON a.id=i.assets_id WHERE i.id=$1 AND a.project_id=$2)",
        )
        .bind(image_id)
        .bind(request.project_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to validate asset cover"))?;
        if !valid {
            return Err(AppError::bad_request("imageId 不属于当前项目"));
        }
    }
    let uploaded_cover = match request.base64.as_deref() {
        Some(data) if !data.trim().is_empty() => {
            Some(save_asset_cover_data_url(data, request.project_id).await?)
        }
        _ => None,
    };
    let uploaded_image_id = uploaded_cover.as_ref().map(|_| next_id(1));
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin asset transaction"))?;
    let prompt_changed: bool = if request.id.is_some() && request.prompt.is_some() {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM toonflow.assets WHERE id=$1 AND project_id=$2 AND prompt IS DISTINCT FROM $3)",
        )
        .bind(id)
        .bind(request.project_id)
        .bind(request.prompt.as_deref().unwrap_or_default())
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to compare asset prompt"))?
    } else {
        false
    };
    sqlx::query(
        r#"INSERT INTO toonflow.assets
           (id, project_id, name, prompt, remark, type, description, script_id, parent_asset_id, image_id)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           ON CONFLICT (id) DO UPDATE SET
             name=excluded.name,
             prompt=coalesce(excluded.prompt, toonflow.assets.prompt),
             remark=coalesce(excluded.remark, toonflow.assets.remark),
             type=coalesce(nullif(excluded.type,''), toonflow.assets.type),
             description=coalesce(excluded.description, toonflow.assets.description),
             script_id=coalesce(excluded.script_id, toonflow.assets.script_id),
             parent_asset_id=coalesce(excluded.parent_asset_id, toonflow.assets.parent_asset_id),
             image_id=CASE
               WHEN toonflow.assets.prompt IS DISTINCT FROM excluded.prompt THEN excluded.image_id
               ELSE coalesce(excluded.image_id, toonflow.assets.image_id)
             END"#,
    )
    .bind(id)
    .bind(request.project_id)
    .bind(request.name)
    .bind(request.prompt.unwrap_or_default())
    .bind(request.remark)
    .bind(request.r#type.unwrap_or_default())
    .bind(request.description.unwrap_or_default())
    .bind(request.script_id)
    .bind(request.parent_asset_id)
    .bind(request.image_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to save asset"))?;
    if prompt_changed {
        sqlx::query(
            "UPDATE toonflow.images SET state='已取消',error_reason='资产提示词已修改'
             WHERE assets_id=$1 AND state='生成中'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to invalidate outdated image generation"))?;
    }
    sqlx::query(
        "INSERT INTO toonflow.project_assets(project_id,asset_id,linked_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
    )
    .bind(request.project_id)
    .bind(id)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to link saved asset"))?;
    if let (Some(image_id), Some(file_path)) = (uploaded_image_id, uploaded_cover) {
        sqlx::query("INSERT INTO toonflow.images(id,file_path,type,assets_id,state) VALUES($1,$2,'asset',$3,'已完成')")
            .bind(image_id)
            .bind(file_path)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to save asset cover"))?;
        sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
            .bind(id)
            .bind(image_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to bind asset cover"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit asset"))?;
    Ok(Json(ApiResponse::new(json!({ "id": id }))))
}

#[derive(Debug, Deserialize)]
pub struct DeleteIdsRequest {
    pub ids: Option<Vec<i64>>,
    pub id: Option<Vec<i64>>,
}

pub async fn delete_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<DeleteIdsRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let ids = request.ids.or(request.id).unwrap_or_default();
    if ids.is_empty() {
        return Err(AppError::bad_request("ids is required"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin asset deletion"))?;
    let file_paths: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT i.file_path
         FROM toonflow.images i
         JOIN toonflow.assets a ON a.id=i.assets_id
         WHERE (a.id=ANY($1) OR a.parent_asset_id=ANY($1))
           AND nullif(i.file_path,'') IS NOT NULL",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to collect asset files"))?;
    let all_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM toonflow.assets WHERE id=ANY($1) OR parent_asset_id=ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to resolve asset descendants"))?;
    sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE asset_id=ANY($1)")
        .bind(&all_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to clear storyboard assets"))?;
    sqlx::query("DELETE FROM toonflow.project_assets WHERE asset_id=ANY($1)")
        .bind(&all_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to clear project assets"))?;
    sqlx::query("UPDATE toonflow.assets SET image_id=NULL WHERE id=ANY($1)")
        .bind(&all_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to detach asset images"))?;
    sqlx::query("DELETE FROM toonflow.images WHERE assets_id=ANY($1)")
        .bind(&all_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete asset images"))?;
    let result = sqlx::query("DELETE FROM toonflow.assets WHERE id=ANY($1)")
        .bind(&all_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete assets"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit asset deletion"))?;
    for path in file_paths {
        let _ = delete_asset_file(&path).await;
    }
    affected(result.rows_affected(), "asset")?;
    Ok(Json(ApiResponse::with_message((), "删除资产成功")))
}

pub async fn delete_asset(
    user: CurrentUser,
    state: State<ToonState>,
    Json(request): Json<IdRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    delete_assets(
        user,
        state,
        Json(DeleteIdsRequest {
            ids: Some(vec![request.id]),
            id: None,
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowRequest {
    pub project_id: i64,
    #[serde(alias = "episodesId")]
    pub episodes_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFlowRequest {
    pub project_id: i64,
    #[serde(alias = "episodesId")]
    pub episodes_id: i64,
    pub data: Value,
}

pub async fn get_flow_data(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<FlowRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_access(&state.pool, &user, request.project_id).await?;
    ensure_script_in_project(&state.pool, request.project_id, request.episodes_id).await?;
    let flow: Option<(Value,)> = sqlx::query_as(
        "SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'",
    )
    .bind(request.project_id)
    .bind(request.episodes_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get flow data"))?;
    let (script, assets) = crate::toonflow_asset_context::load_script_context(
        &state.pool,
        request.project_id,
        request.episodes_id,
    )
    .await
    .map_err(|_| AppError::internal("failed to build production asset context"))?;
    if let Some((mut data,)) = flow {
        data["script"] = json!(script);
        data["assets"] = assets;
        if data.get("workflow").is_none() {
            data["workflow"] =
                serde_json::to_value(crate::toonflow_workflow::default_production_workflow())
                    .map_err(|_| AppError::internal("failed to build workflow definition"))?;
        }
        return Ok(Json(ApiResponse::new(data)));
    }
    Ok(Json(ApiResponse::new(json!({
        "script": script,
        "scriptPlan": "",
        "assets": assets,
        "storyboardTable": "",
        "storyboard": [],
        "workbench": { "videoList": [] },
        "workflow": crate::toonflow_workflow::default_production_workflow()
    }))))
}

pub async fn save_flow_data(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveFlowRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:update")?;
    ensure_project_access(&state.pool, &user, request.project_id).await?;
    ensure_script_in_project(&state.pool, request.project_id, request.episodes_id).await?;
    let time = now_ms();
    let workflow = crate::toonflow_workflow::workflow_from_data(&request.data)?;
    crate::toonflow_workflow::persist_definition(
        &state.pool,
        request.project_id,
        request.episodes_id,
        &workflow,
        time,
    )
    .await?;
    let mut data = request.data;
    data["workflow"] = serde_json::to_value(workflow)
        .map_err(|_| AppError::internal("failed to serialize workflow definition"))?;
    persist_work_data_with_transition_sync(
        &state.pool,
        request.project_id,
        request.episodes_id,
        &data,
        time,
    )
    .await?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardRow {
    pub id: i64,
    pub script_id: i64,
    pub prompt: String,
    pub file_path: Option<String>,
    pub duration: Option<String>,
    pub state: Option<String>,
    pub track_id: Option<i64>,
    pub reason: Option<String>,
    pub track: Option<String>,
    pub video_desc: Option<String>,
    pub shot_size: Option<String>,
    pub camera_move: Option<String>,
    pub time_of_day: Option<String>,
    pub scene_key: Option<String>,
    pub scene_state_id: Option<i64>,
    pub generated_scene_state_id: Option<i64>,
    pub scene_generation_context: Value,
    pub scene_master_id: Option<i64>,
    pub scene_master_name: Option<String>,
    pub scene_master_status: Option<String>,
    pub scene_master_revision: Option<i32>,
    pub scene_state_key: Option<String>,
    pub scene_state_name: Option<String>,
    pub scene_state_revision: Option<i32>,
    pub should_generate_image: i32,
    pub project_id: i64,
    pub flow_id: Option<i64>,
    pub index: Option<i32>,
    pub create_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardListRequest {
    pub script_id: i64,
    pub project_id: i64,
}

pub async fn get_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<StoryboardListRequest>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    let rows = sqlx::query_as::<_, StoryboardRow>(
        r#"SELECT storyboard.id,storyboard.script_id,storyboard.prompt,storyboard.file_path,
                  storyboard.duration,storyboard.state,storyboard.track_id,storyboard.reason,
                  storyboard.track,storyboard.video_desc,storyboard.shot_size,storyboard.camera_move,storyboard.time_of_day,storyboard.scene_key,
                  storyboard.scene_state_id,storyboard.generated_scene_state_id,
                  storyboard.scene_generation_context,master.id AS scene_master_id,
                  master.name AS scene_master_name,
                  CASE WHEN master.status='ready' AND (
                         master_image.id IS NULL OR master_image.state<>'已完成'
                         OR coalesce(master_image.file_path,'')=''
                       ) THEN 'missing_reference' ELSE master.status END AS scene_master_status,
                  master.revision AS scene_master_revision,scene_state.state_key AS scene_state_key,
                  scene_state.name AS scene_state_name,scene_state.revision AS scene_state_revision,
                  storyboard.should_generate_image,storyboard.project_id,storyboard.flow_id,
                  storyboard.index,storyboard.create_time
           FROM toonflow.storyboards storyboard
           LEFT JOIN toonflow.scene_states scene_state ON scene_state.id=storyboard.scene_state_id
           LEFT JOIN toonflow.scene_masters master ON master.id=scene_state.scene_master_id
           LEFT JOIN toonflow.images master_image ON master_image.id=master.pinned_image_id
           WHERE storyboard.script_id=$1 AND storyboard.project_id=$2
           ORDER BY storyboard.index ASC NULLS LAST,storyboard.id ASC"#,
    )
    .bind(request.script_id)
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list storyboards"))?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let asset_ids: Vec<(i64,)> = sqlx::query_as(
            "SELECT asset_id FROM toonflow.assets_storyboards WHERE storyboard_id=$1 ORDER BY sort_order,asset_id",
        )
        .bind(row.id)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to list storyboard assets"))?;
        let scene_consistency_status = storyboard_scene_consistency_status(&row);
        result.push(json!({
            "id": row.id,
            "scriptId": row.script_id,
            "projectId": row.project_id,
            "prompt": row.prompt,
            "filePath": row.file_path,
            "src": row.file_path,
            "duration": row.duration.and_then(|value| value.parse::<i64>().ok()),
            "state": row.state,
            "trackId": row.track_id,
            "reason": row.reason,
            "track": row.track,
            "videoDesc": row.video_desc,
            "shotSize": row.shot_size,
            "cameraMove": row.camera_move,
            "timeOfDay": row.time_of_day,
            "sceneKey": row.scene_key,
            "sceneMasterId": row.scene_master_id,
            "sceneMasterName": row.scene_master_name,
            "sceneMasterStatus": row.scene_master_status,
            "sceneStateId": row.scene_state_id,
            "sceneStateKey": row.scene_state_key,
            "sceneStateName": row.scene_state_name,
            "generatedSceneStateId": row.generated_scene_state_id,
            "sceneConsistencyStatus": scene_consistency_status,
            "shouldGenerateImage": row.should_generate_image,
            "flowId": row.flow_id,
            "index": row.index,
            "createTime": row.create_time,
            "associateAssetsIds": asset_ids.into_iter().map(|id| id.0).collect::<Vec<_>>()
        }));
    }
    Ok(Json(ApiResponse::new(result)))
}

fn storyboard_scene_consistency_status(row: &StoryboardRow) -> &'static str {
    crate::toonflow_scene_consistency::storyboard_consistency_status(
        row.scene_key.as_deref(),
        row.scene_state_id,
        row.file_path.as_deref(),
        row.generated_scene_state_id,
        &row.scene_generation_context,
        row.scene_master_status.as_deref(),
        row.scene_master_revision,
        row.scene_state_revision,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStoryboardRequest {
    pub id: Option<i64>,
    pub prompt: String,
    pub duration: Option<i64>,
    #[serde(default)]
    pub state: String,
    pub video_desc: Option<String>,
    pub shot_size: Option<String>,
    pub camera_move: Option<String>,
    pub time_of_day: Option<String>,
    pub scene_key: Option<String>,
    pub scene_state_id: Option<i64>,
    pub scene_state_key: Option<String>,
    pub scene_state_parent_key: Option<String>,
    pub scene_state_description: Option<String>,
    #[serde(default = "default_should_generate")]
    pub should_generate_image: i32,
    #[serde(alias = "src")]
    pub file_path: Option<String>,
    pub script_id: Option<i64>,
    pub project_id: Option<i64>,
    pub track: Option<String>,
    #[serde(default)]
    pub associate_assets_ids: Vec<i64>,
}

async fn validate_storyboard_prompt_inputs(
    pool: &sqlx::PgPool,
    project_id: i64,
    prompt: &str,
    asset_ids: &[i64],
) -> Result<(), AppError> {
    let prompt_assets =
        crate::toonflow_asset_context::load_storyboard_prompt_assets(pool, project_id, asset_ids)
            .await?;
    crate::toonflow_storyboard_prompt_validation::validate_storyboard_prompt(prompt, &prompt_assets)
        .map_err(AppError::bad_request)
}

/// P0.2 结构化镜头：景别与运镜的合法值域。新增值前先确认生成链路
/// （结构化编译、供应商提示词）能消费它。
pub(crate) const SHOT_SIZES: &[&str] = &["特写", "近景", "中景", "全景", "远景", "大远景"];
pub(crate) const CAMERA_MOVES: &[&str] =
    &["固定", "推镜", "拉镜", "摇镜", "移镜", "跟镜", "升降", "环绕"];

/// 对齐方案 P1 场景日夜状态：镜头的昼夜时间。空值表示未指定。
pub(crate) const TIMES_OF_DAY: &[&str] = &["日", "夜", "晨", "黄昏"];

pub(crate) fn validate_shot_framing(
    shot_size: &Option<String>,
    camera_move: &Option<String>,
) -> Result<(), AppError> {
    if let Some(size) = shot_size.as_deref().filter(|value| !value.is_empty()) {
        if !SHOT_SIZES.contains(&size) {
            return Err(AppError::bad_request(format!(
                "景别必须是 {SHOT_SIZES:?} 之一，收到“{size}”"
            )));
        }
    }
    if let Some(movement) = camera_move.as_deref().filter(|value| !value.is_empty()) {
        if !CAMERA_MOVES.contains(&movement) {
            return Err(AppError::bad_request(format!(
                "运镜必须是 {CAMERA_MOVES:?} 之一，收到“{movement}”"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_time_of_day(value: &Option<String>) -> Result<(), AppError> {
    if let Some(time) = value.as_deref().filter(|value| !value.is_empty()) {
        if !TIMES_OF_DAY.contains(&time) {
            return Err(AppError::bad_request(format!(
                "昼夜时间必须是 {TIMES_OF_DAY:?} 之一，收到“{time}”"
            )));
        }
    }
    Ok(())
}

pub async fn add_storyboard(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveStoryboardRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:create")?;
    validate_shot_framing(&request.shot_size, &request.camera_move)?;
    validate_time_of_day(&request.time_of_day)?;
    let script_id = request
        .script_id
        .ok_or_else(|| AppError::bad_request("scriptId is required"))?;
    let project_id = request
        .project_id
        .ok_or_else(|| AppError::bad_request("projectId is required"))?;
    crate::toonflow_storyboard_asset_validation::validate_storyboard_asset_ids(
        &state.pool,
        project_id,
        &request.associate_assets_ids,
    )
    .await?;
    if request.should_generate_image != 0 {
        validate_storyboard_prompt_inputs(
            &state.pool,
            project_id,
            &request.prompt,
            &request.associate_assets_ids,
        )
        .await?;
    }
    let id = request.id.unwrap_or_else(|| next_id(0));
    let track_id = next_id(1);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to create storyboard"))?;
    sqlx::query(
        "INSERT INTO toonflow.video_tracks(id,script_id,project_id,duration,sort_order)
         VALUES($1,$2,$3,$4,coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$3 AND script_id=$2),0))",
    )
    .bind(track_id)
    .bind(script_id)
    .bind(project_id)
    .bind(request.duration.map(|duration| duration as i32))
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to create storyboard track"))?;
    insert_storyboard(
        &mut tx,
        id,
        Some(track_id),
        &request,
        0,
        script_id,
        project_id,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to create storyboard"))?;
    apply_track_transition_defaults(&state.pool, project_id, script_id).await?;
    Ok(Json(ApiResponse::new(json!({ "id": id }))))
}

async fn insert_storyboard(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    track_id: Option<i64>,
    request: &SaveStoryboardRequest,
    index: i32,
    script_id: i64,
    project_id: i64,
) -> Result<(), AppError> {
    let scene_key = normalize_persisted_scene_key(request.scene_key.as_deref())?;
    let scene_state_id = crate::toonflow_scene_consistency::resolve_storyboard_scene_state(
        &mut **tx,
        project_id,
        script_id,
        scene_key.as_deref(),
        request.scene_state_id,
        request.scene_state_key.as_deref(),
        request.scene_state_parent_key.as_deref(),
        request.scene_state_description.as_deref(),
        &request.associate_assets_ids,
    )
    .await?;
    sqlx::query(
        r#"INSERT INTO toonflow.storyboards
           (id, script_id, prompt, file_path, duration, state, track_id, track, video_desc,
            shot_size,camera_move,time_of_day,scene_key,scene_state_id,should_generate_image,project_id,index,create_time)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)"#,
    )
    .bind(id)
    .bind(script_id)
    .bind(&request.prompt)
    .bind(&request.file_path)
    .bind(request.duration.map(|value| value.to_string()))
    .bind(&request.state)
    .bind(track_id)
    .bind(&request.track)
    .bind(&request.video_desc)
    .bind(&request.shot_size)
    .bind(&request.camera_move)
    .bind(&request.time_of_day)
    .bind(scene_key)
    .bind(scene_state_id)
    .bind(request.should_generate_image)
    .bind(project_id)
    .bind(index)
    .bind(now_ms())
    .execute(&mut **tx)
    .await
    .map_err(|_| AppError::internal("failed to create storyboard"))?;
    sync_storyboard_assets(tx, id, &request.associate_assets_ids).await?;
    Ok(())
}

async fn sync_storyboard_assets(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    storyboard_id: i64,
    asset_ids: &[i64],
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=$1")
        .bind(storyboard_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::internal("failed to update storyboard assets"))?;
    for (sort_order, asset_id) in asset_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO toonflow.assets_storyboards (storyboard_id, asset_id, sort_order) VALUES ($1,$2,$3) ON CONFLICT(storyboard_id,asset_id) DO UPDATE SET sort_order=excluded.sort_order",
        )
        .bind(storyboard_id)
        .bind(asset_id)
        .bind(sort_order as i32)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::internal("failed to update storyboard assets"))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct BatchStoryboardRequest {
    pub data: Vec<SaveStoryboardRequest>,
    #[serde(rename = "scriptId")]
    pub script_id: i64,
    #[serde(rename = "projectId")]
    pub project_id: i64,
}

pub async fn batch_add_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<BatchStoryboardRequest>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:create")?;
    if request.data.is_empty() {
        return Err(AppError::bad_request("data is required"));
    }
    let associated_asset_ids = request
        .data
        .iter()
        .flat_map(|item| item.associate_assets_ids.iter().copied())
        .collect::<Vec<_>>();
    crate::toonflow_storyboard_asset_validation::validate_storyboard_asset_ids(
        &state.pool,
        request.project_id,
        &associated_asset_ids,
    )
    .await?;
    for item in &request.data {
        if item.should_generate_image != 0 {
            validate_storyboard_prompt_inputs(
                &state.pool,
                request.project_id,
                &item.prompt,
                &item.associate_assets_ids,
            )
            .await?;
        }
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to create storyboards"))?;
    let mut storyboard_groups = std::collections::BTreeMap::<String, Vec<i64>>::new();
    for (idx, item) in request.data.iter().enumerate() {
        let id = item.id.unwrap_or_else(|| next_id(idx as i64));
        let track = item
            .track
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "main".to_string());
        insert_storyboard(
            &mut tx,
            id,
            None,
            item,
            idx as i32,
            request.script_id,
            request.project_id,
        )
        .await?;
        storyboard_groups.entry(track).or_default().push(id);
    }
    for (group_index, (track, storyboard_ids)) in storyboard_groups.iter().enumerate() {
        let existing_track_id: Option<i64> = sqlx::query_scalar(
            "SELECT track_id FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track=$3 AND track_id IS NOT NULL LIMIT 1",
        )
        .bind(request.project_id)
        .bind(request.script_id)
        .bind(track)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to find storyboard track"))?;
        let track_id = existing_track_id.unwrap_or_else(|| next_id(10_000 + group_index as i64));
        let duration: i32 = sqlx::query_scalar(
            "SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track=$3",
        )
        .bind(request.project_id)
        .bind(request.script_id)
        .bind(track)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to total storyboard track duration"))?;
        if existing_track_id.is_some() {
            sqlx::query("UPDATE toonflow.video_tracks SET duration=$2 WHERE id=$1")
                .bind(track_id)
                .bind(duration)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to update storyboard track"))?;
        } else {
            sqlx::query(
                "INSERT INTO toonflow.video_tracks(id,project_id,script_id,state,duration,sort_order)
                 VALUES($1,$2,$3,'未生成',$4,coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$2 AND script_id=$3),0))",
            )
            .bind(track_id)
            .bind(request.project_id)
            .bind(request.script_id)
            .bind(duration)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to create storyboard track"))?;
        }
        sqlx::query("UPDATE toonflow.storyboards SET track_id=$2 WHERE id=ANY($1)")
            .bind(storyboard_ids)
            .bind(track_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to bind storyboard track"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to create storyboards"))?;
    apply_track_transition_defaults(&state.pool, request.project_id, request.script_id).await?;
    get_storyboards(
        user,
        State(state),
        Json(StoryboardListRequest {
            script_id: request.script_id,
            project_id: request.project_id,
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditStoryboardInfoRequest {
    pub id: i64,
    pub prompt: String,
    pub video_desc: String,
    pub shot_size: Option<String>,
    pub camera_move: Option<String>,
    pub time_of_day: Option<String>,
    pub scene_key: Option<String>,
    pub scene_state_id: Option<i64>,
    pub scene_state_key: Option<String>,
    pub scene_state_parent_key: Option<String>,
    pub scene_state_description: Option<String>,
    pub duration: Option<i64>,
    pub track: Option<String>,
    pub should_generate_image: Option<i32>,
    pub associate_assets_ids: Option<Vec<i64>>,
}

type StoryboardEditRow = (
    i64,
    i64,
    Option<i64>,
    Option<String>,
    i32,
    Option<String>,
    Option<i64>,
);

pub async fn edit_storyboard_info(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<EditStoryboardInfoRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:update")?;
    validate_shot_framing(&request.shot_size, &request.camera_move)?;
    validate_time_of_day(&request.time_of_day)?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to update storyboard"))?;
    let current: Option<StoryboardEditRow> = sqlx::query_as(
        "SELECT project_id,script_id,track_id,track,should_generate_image,scene_key,scene_state_id
         FROM toonflow.storyboards WHERE id=$1 FOR UPDATE",
    )
    .bind(request.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard"))?;
    let Some((
        project_id,
        script_id,
        old_track_id,
        old_track,
        current_should_generate,
        current_scene_key,
        current_scene_state_id,
    )) = current
    else {
        return Err(AppError::not_found("storyboard not found"));
    };
    if let Some(asset_ids) = request.associate_assets_ids.as_deref() {
        crate::toonflow_storyboard_asset_validation::validate_storyboard_asset_ids(
            &state.pool,
            project_id,
            asset_ids,
        )
        .await?;
    }
    let effective_asset_ids = if let Some(asset_ids) = request.associate_assets_ids.as_ref() {
        asset_ids.clone()
    } else {
        sqlx::query_scalar(
            "SELECT asset_id FROM toonflow.assets_storyboards WHERE storyboard_id=$1 ORDER BY sort_order,asset_id",
        )
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to load storyboard assets"))?
    };
    if request
        .should_generate_image
        .unwrap_or(current_should_generate)
        != 0
    {
        validate_storyboard_prompt_inputs(
            &state.pool,
            project_id,
            &request.prompt,
            &effective_asset_ids,
        )
        .await?;
    }
    let track = request.track.as_deref().unwrap_or("main").trim();
    let track = if track.is_empty() { "main" } else { track };
    let target_track_id: Option<i64> = sqlx::query_scalar(
        "SELECT track_id FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track=$3 AND id<>$4 AND track_id IS NOT NULL LIMIT 1",
    )
    .bind(project_id)
    .bind(script_id)
    .bind(track)
    .bind(request.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to find storyboard track"))?;
    let track_id = match target_track_id {
        Some(id) => id,
        None if old_track_id.is_some() && old_track.as_deref().unwrap_or("main") == track => {
            old_track_id.unwrap()
        }
        None => {
            let id = next_id(2);
            sqlx::query("INSERT INTO toonflow.video_tracks(id,project_id,script_id,state,duration,sort_order) VALUES($1,$2,$3,'未生成',0,coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$2 AND script_id=$3),0))")
                .bind(id).bind(project_id).bind(script_id).execute(&mut *tx).await
                .map_err(|_| AppError::internal("failed to create storyboard track"))?;
            id
        }
    };
    let scene_key_was_provided = request.scene_key.is_some();
    let scene_key = normalize_persisted_scene_key(request.scene_key.as_deref())?;
    let effective_scene_key = if scene_key_was_provided {
        scene_key.as_deref()
    } else {
        current_scene_key.as_deref()
    };
    let scene_changed = scene_key_was_provided && scene_key != current_scene_key;
    let state_was_provided = request.scene_state_id.is_some() || request.scene_state_key.is_some();
    let scene_state_id = if scene_changed || state_was_provided {
        crate::toonflow_scene_consistency::resolve_storyboard_scene_state(
            &mut *tx,
            project_id,
            script_id,
            effective_scene_key,
            request.scene_state_id,
            request.scene_state_key.as_deref(),
            request.scene_state_parent_key.as_deref(),
            request.scene_state_description.as_deref(),
            &effective_asset_ids,
        )
        .await?
    } else {
        current_scene_state_id
    };
    let result = sqlx::query("UPDATE toonflow.storyboards SET prompt=$2,video_desc=$3,duration=$4,track=$5,track_id=$6,should_generate_image=$7,scene_key=CASE WHEN $8 THEN $9 ELSE scene_key END,scene_state_id=$10,shot_size=$11,camera_move=$12,time_of_day=$13,state='未生成',reason='分镜描述或参考资产已更新，请重新生成图片',generated_scene_state_id=NULL,scene_generation_context='{}'::jsonb WHERE id=$1")
        .bind(request.id).bind(request.prompt).bind(request.video_desc)
        .bind(request.duration.map(|value| value.to_string())).bind(track).bind(track_id)
        .bind(request.should_generate_image.unwrap_or(current_should_generate))
        .bind(scene_key_was_provided).bind(scene_key).bind(scene_state_id)
        .bind(request.shot_size).bind(request.camera_move).bind(request.time_of_day).execute(&mut *tx).await
        .map_err(|_| AppError::internal("failed to update storyboard"))?;
    if let Some(asset_ids) = request.associate_assets_ids {
        sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=$1")
            .bind(request.id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to update storyboard assets"))?;
        for (sort_order, asset_id) in asset_ids.iter().enumerate() {
            sqlx::query("INSERT INTO toonflow.assets_storyboards(storyboard_id,asset_id,sort_order) VALUES($1,$2,$3)")
                .bind(request.id).bind(asset_id).bind(sort_order as i32).execute(&mut *tx).await
                .map_err(|_| AppError::internal("failed to update storyboard assets"))?;
        }
    }
    let mut affected_track_ids = vec![track_id];
    if let Some(old_track_id) = old_track_id.filter(|id| *id != track_id) {
        affected_track_ids.push(old_track_id);
    }
    for affected_track_id in affected_track_ids {
        let storyboard_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM toonflow.storyboards WHERE track_id=$1")
                .bind(affected_track_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to inspect storyboard track"))?;
        let duration: i32 = sqlx::query_scalar("SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE track_id=$1")
            .bind(affected_track_id).fetch_one(&mut *tx).await
            .map_err(|_| AppError::internal("failed to total storyboard track duration"))?;
        if storyboard_count == 0 {
            sqlx::query("DELETE FROM toonflow.video_tracks WHERE id=$1")
                .bind(affected_track_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to delete empty storyboard track"))?;
        } else {
            sqlx::query("UPDATE toonflow.video_tracks SET duration=$2,state='未生成',reason='分镜已更新，请重新生成视频' WHERE id=$1")
                .bind(affected_track_id)
                .bind(duration)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to update storyboard track"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to update storyboard"))?;
    apply_track_transition_defaults(&state.pool, project_id, script_id).await?;
    affected(result.rows_affected(), "storyboard")?;
    Ok(Json(ApiResponse::with_message((), "更新分镜成功")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderStoryboardsRequest {
    pub project_id: i64,
    pub script_id: i64,
    pub storyboard_ids: Vec<i64>,
}

pub async fn reorder_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ReorderStoryboardsRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:update")?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2",
    )
    .bind(request.project_id)
    .bind(request.script_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to validate storyboards"))?;
    let matched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND id=ANY($3)",
    )
    .bind(request.project_id)
    .bind(request.script_id)
    .bind(&request.storyboard_ids)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to validate storyboards"))?;
    if total != request.storyboard_ids.len() as i64 || matched != total {
        return Err(AppError::bad_request("分镜排序必须包含当前剧本的全部分镜"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    for (index, id) in request.storyboard_ids.into_iter().enumerate() {
        sqlx::query("UPDATE toonflow.storyboards SET index=$2 WHERE id=$1")
            .bind(id)
            .bind(index as i32)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to reorder storyboards"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed transaction"))?;
    apply_track_transition_defaults(&state.pool, request.project_id, request.script_id).await?;
    Ok(Json(ApiResponse::with_message((), "分镜排序已保存")))
}

pub async fn remove_storyboard(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<IdRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:scene:delete")?;
    let row: Option<(Option<i64>, Option<i64>, i64, i64)> = sqlx::query_as(
        "SELECT track_id,flow_id,project_id,script_id FROM toonflow.storyboards WHERE id=$1",
    )
    .bind(request.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard"))?;
    let Some((track_id, flow_id, project_id, script_id)) = row else {
        return Err(AppError::not_found("storyboard not found"));
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin transaction"))?;
    sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=$1")
        .bind(request.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete storyboard assets"))?;
    sqlx::query("DELETE FROM toonflow.storyboards WHERE id=$1")
        .bind(request.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete storyboard"))?;
    if let Some(flow_id) = flow_id {
        sqlx::query("DELETE FROM toonflow.image_flows WHERE id=$1")
            .bind(flow_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to delete storyboard image flow"))?;
    }
    if let Some(track_id) = track_id {
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM toonflow.storyboards WHERE track_id=$1")
                .bind(track_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to inspect storyboard track"))?;
        if remaining == 0 {
            sqlx::query("DELETE FROM toonflow.video_tracks WHERE id=$1")
                .bind(track_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to delete storyboard track"))?;
        } else {
            sqlx::query(
                "UPDATE toonflow.video_tracks SET duration=(SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE track_id=$1) WHERE id=$1",
            )
            .bind(track_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to update storyboard track"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit storyboard deletion"))?;
    apply_track_transition_defaults(&state.pool, project_id, script_id).await?;
    Ok(Json(ApiResponse::with_message((), "视频删除成功")))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AgentDeployment {
    pub id: i64,
    pub key: String,
    pub description: String,
    pub name: String,
    pub temperature: i32,
    pub max_output_tokens: i32,
    pub disabled: bool,
    pub model_config_id: Option<i64>,
    pub model_type: String,
    pub prompt_source_key: Option<String>,
    pub memory_scope: String,
}

pub async fn list_agent_deployments(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<AgentDeployment>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, AgentDeployment>(
        r#"SELECT d.id,d.key,d.description,d.name,
                  d.temperature,d.max_output_tokens,d.disabled,d.model_config_id,d.model_type,
                  d.prompt_source_key,d.memory_scope
           FROM toonflow.agent_deployments d ORDER BY d.id"#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list agent deployments"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAgentDeploymentRequest {
    pub id: i64,
    pub temperature: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub disabled: Option<bool>,
    pub model_config_id: Option<i64>,
    pub prompt_source_key: Option<String>,
    pub memory_scope: Option<String>,
}

pub async fn update_agent_deployment(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UpdateAgentDeploymentRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let model_id = request
        .model_config_id
        .ok_or_else(|| AppError::bad_request("必须绑定统一 AI 模型"))?;
    let valid_model: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1
               FROM toonflow.agent_deployments d
               JOIN ai.model_configs m ON m.id=$2
               WHERE d.id=$1 AND m.status=0 AND m.type=d.model_type
           )"#,
    )
    .bind(request.id)
    .bind(model_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to validate agent model"))?;
    if !valid_model {
        return Err(AppError::bad_request("模型未启用或类型与当前用途不匹配"));
    }
    let result = sqlx::query(r#"UPDATE toonflow.agent_deployments SET temperature=coalesce($2,temperature),max_output_tokens=coalesce($3,max_output_tokens),disabled=coalesce($4,disabled),model_config_id=$5,prompt_source_key=coalesce($6,prompt_source_key),memory_scope=coalesce($7,memory_scope) WHERE id=$1"#)
    .bind(request.id)
    .bind(request.temperature)
    .bind(request.max_output_tokens)
    .bind(request.disabled)
    .bind(model_id)
    .bind(request.prompt_source_key)
    .bind(request.memory_scope)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update agent deployment"))?;
    affected(result.rows_affected(), "agent deployment")?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(Debug, Serialize, FromRow)]
pub struct SettingRow {
    pub key: String,
    pub value: String,
}

pub async fn list_settings(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<SettingRow>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows =
        sqlx::query_as::<_, SettingRow>("SELECT key, value FROM toonflow.settings ORDER BY key")
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to list toonflow settings"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
pub struct SaveSettingRequest {
    pub key: String,
    pub value: String,
}

pub async fn save_setting(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveSettingRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    sqlx::query(
        r#"INSERT INTO toonflow.settings (key, value) VALUES ($1,$2)
           ON CONFLICT (key) DO UPDATE SET value=excluded.value"#,
    )
    .bind(request.key)
    .bind(request.value)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to save toonflow setting"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn get_agent_use_mode(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let mode = sqlx::query_scalar::<_, String>(
        "SELECT value FROM toonflow.settings WHERE key='agentUseMode'",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load agent use mode"))?
    .unwrap_or_else(|| "workflow".into());
    Ok(Json(ApiResponse::new(json!({ "mode": mode }))))
}

pub async fn update_agent_use_mode(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveSettingRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.value != "workflow" && request.value != "direct" && request.value != "hybrid" {
        return Err(AppError::bad_request(
            "agentUseMode 仅支持 workflow、direct、hybrid",
        ));
    }
    sqlx::query("INSERT INTO toonflow.settings(key,value) VALUES('agentUseMode',$1) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(request.value)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to save agent use mode"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn get_project_by_path(
    user: CurrentUser,
    State(state): State<ToonState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<ToonflowProject>>, AppError> {
    require(&user, "toon:project:read")?;
    let row = sqlx::query_as::<_, ToonflowProject>(
        r#"SELECT id, project_type, chat_model, image_model, image_quality, video_model, name, intro,
                  type as type_, art_style, director_manual, mode, video_ratio, create_time, update_time
           FROM toonflow.projects WHERE id=$1"#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get project"))?
    .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(Json(ApiResponse::new(row)))
}
