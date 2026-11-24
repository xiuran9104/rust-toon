use crate::{
    ToonState,
    shared::{affected, current_user_id, require},
    toonflow::{SaveProjectRequest, ToonflowProject},
    toonflow_episode_renders::ensure_project_access,
    toonflow_project_helpers::{now_ms, validate_models, video_mode, video_ratio},
    toonflow_storage::enqueue_cleanup_paths,
};
use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde_json::{Value, json};

pub async fn list_projects(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<ToonflowProject>>>, AppError> {
    require(&user, "toon:project:read")?;
    let user_id = current_user_id(&user)?;
    let is_super_admin = user.role_codes.iter().any(|role| role == "super_admin");
    let rows = sqlx::query_as::<_, ToonflowProject>(
        r#"SELECT id, project_type, chat_model, image_model, image_quality, video_model, name, intro,
                  type as type_, art_style, director_manual, mode, video_ratio, create_time, update_time
           FROM toonflow.projects
           WHERE $1 OR user_id=$2
           ORDER BY create_time DESC"#,
    )
    .bind(is_super_admin)
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list toonflow projects"))?;
    Ok(Json(ApiResponse::new(rows)))
}

pub async fn create_project(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveProjectRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:create")?;
    if request.name.trim().is_empty() {
        return Err(AppError::bad_request("project name is required"));
    }
    validate_models(
        &state.pool,
        request.chat_model,
        request.image_model,
        request.video_model,
    )
    .await?;
    let time = now_ms();
    // New projects get their identifier from PostgreSQL; a pinned identifier is
    // only honoured when the caller replays an existing project.
    let id: i64 = sqlx::query_scalar("INSERT INTO toonflow.projects (id, project_type, chat_model, image_model, image_quality, video_model, name, intro, type, art_style, director_manual, mode, video_ratio, user_id, create_time, update_time) VALUES (coalesce($1::bigint, nextval('toonflow.projects_id_seq')),$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$15) RETURNING id")
        .bind(request.id).bind(request.project_type).bind(request.chat_model).bind(request.image_model).bind(request.image_quality).bind(request.video_model).bind(request.name.trim()).bind(request.intro).bind(request.r#type).bind(request.art_style).bind(request.director_manual).bind(video_mode(&request.mode)).bind(video_ratio(&request.video_ratio)).bind(current_user_id(&user)?).bind(time)
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to create toonflow project"))?;
    Ok(Json(ApiResponse::with_message(
        json!({"id": id}),
        "新增项目成功",
    )))
}

pub async fn update_project(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveProjectRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let id = request
        .id
        .ok_or_else(|| AppError::bad_request("project id is required"))?;
    ensure_project_access(&state.pool, &user, id).await?;
    validate_models(
        &state.pool,
        request.chat_model,
        request.image_model,
        request.video_model,
    )
    .await?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin project update"))?;
    let generation_changed: Option<bool> = sqlx::query_scalar(
        r#"WITH previous AS MATERIALIZED (
             SELECT project_type,type,intro,art_style,image_model,image_quality
             FROM toonflow.projects WHERE id=$1 FOR UPDATE
           ), updated AS (
             UPDATE toonflow.projects project
             SET project_type=$2,chat_model=$3,image_model=$4,image_quality=$5,
                 video_model=$6,name=$7,intro=$8,type=$9,art_style=$10,
                 director_manual=$11,mode=$12,video_ratio=$13,update_time=$14
             FROM previous WHERE project.id=$1
             RETURNING previous.project_type IS DISTINCT FROM $2
                    OR previous.type IS DISTINCT FROM $9
                    OR previous.intro IS DISTINCT FROM $8
                    OR previous.art_style IS DISTINCT FROM $10
                    OR previous.image_model IS DISTINCT FROM $4
                    OR previous.image_quality IS DISTINCT FROM $5 AS generation_changed
           ) SELECT generation_changed FROM updated"#,
    )
    .bind(id)
    .bind(&request.project_type)
    .bind(request.chat_model)
    .bind(request.image_model)
    .bind(&request.image_quality)
    .bind(request.video_model)
    .bind(request.name.trim())
    .bind(&request.intro)
    .bind(&request.r#type)
    .bind(&request.art_style)
    .bind(&request.director_manual)
    .bind(video_mode(&request.mode))
    .bind(video_ratio(&request.video_ratio))
    .bind(now_ms())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to update toonflow project"))?;
    affected(u64::from(generation_changed.is_some()), "project")?;
    if generation_changed == Some(true) {
        sqlx::query(
            r#"UPDATE toonflow.images image SET state='已取消',error_reason='项目生成配置已修改'
               FROM toonflow.assets asset
               WHERE image.assets_id=asset.id AND asset.project_id=$1
                 AND image.state='生成中' AND image.input_hash IS NOT NULL"#,
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to cancel stale image generation"))?;
        sqlx::query(
            r#"UPDATE toonflow.assets asset SET image_id=NULL
               WHERE asset.project_id=$1 AND EXISTS (
                 SELECT 1 FROM toonflow.images image
                 WHERE image.id=asset.image_id AND image.input_hash IS NOT NULL
               )"#,
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to invalidate generated images"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit project update"))?;
    Ok(Json(ApiResponse::with_message((), "编辑项目成功")))
}

pub async fn delete_project(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<crate::toonflow::IdRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:delete")?;
    ensure_project_access(&state.pool, &user, request.id).await?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin project deletion"))?;

    // Existing media transactions freeze video sources before inserting rows
    // that reference their project. Keep deletion in the same child-to-parent
    // order to avoid a video/project FK lock inversion.
    sqlx::query("SELECT id FROM toonflow.videos WHERE project_id=$1 ORDER BY id FOR UPDATE")
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock project videos"))?;
    sqlx::query("SELECT id FROM toonflow.scripts WHERE project_id=$1 ORDER BY id FOR UPDATE")
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock project scripts"))?;
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM toonflow.projects WHERE id=$1 FOR UPDATE")
            .bind(request.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to lock project"))?;
    if locked.is_none() {
        return Err(AppError::not_found("project not found"));
    }
    // Child rows may have committed during the initial scan. Holding the
    // project now blocks new scripts; re-scan children to close that window.
    sqlx::query("SELECT id FROM toonflow.videos WHERE project_id=$1 ORDER BY id FOR UPDATE")
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock project videos"))?;
    sqlx::query("SELECT id FROM toonflow.scripts WHERE project_id=$1 ORDER BY id FOR UPDATE")
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock project scripts"))?;
    sqlx::query(
        "SELECT image.id FROM toonflow.images image
         JOIN toonflow.assets asset ON asset.id=image.assets_id
         WHERE asset.project_id=$1 FOR UPDATE OF image",
    )
    .bind(request.id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to lock project images"))?;
    for (table, id_column) in [
        ("storyboards", "id"),
        ("video_continuity_frames", "previous_video_id"),
        ("episode_renders", "id"),
    ] {
        // (table, id_column) 均为上方静态清单的字面量，非用户输入；
        // sqlx 0.9 要求动态 SQL 显式声明已审计。
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {id_column} FROM toonflow.{table} WHERE project_id=$1 FOR UPDATE"
        )))
        .bind(request.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to lock project media"))?;
    }
    let has_active_work: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM toonflow.distributed_jobs job
           JOIN toonflow.tasks task ON task.id=job.task_id
           WHERE task.project_id=$1 AND job.state IN ('queued','retry','running')
           UNION ALL
           SELECT 1 FROM toonflow.workflow_runs
           WHERE project_id=$1 AND state IN ('pending','running')
           UNION ALL
           SELECT 1 FROM toonflow.tasks
           WHERE project_id=$1 AND state='running'
           UNION ALL
           SELECT 1 FROM toonflow.agent_runs
           WHERE project_id=$1 AND state='running'
           UNION ALL
           SELECT 1 FROM toonflow.scripts
           WHERE project_id=$1 AND extract_state=2
           UNION ALL
           SELECT 1 FROM toonflow.videos
           WHERE project_id=$1 AND state='生成中'
           UNION ALL
           SELECT 1 FROM toonflow.storyboards
           WHERE project_id=$1 AND state='生成中'
           UNION ALL
           SELECT 1 FROM toonflow.images image
           JOIN toonflow.assets asset ON asset.id=image.assets_id
           WHERE asset.project_id=$1 AND image.state='生成中'
         )",
    )
    .bind(request.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to inspect active project work"))?;
    if has_active_work {
        return Err(AppError::bad_request(
            "项目仍有生成或合并任务运行，请等待完成或取消后再删除",
        ));
    }
    let paths: Vec<String> = sqlx::query_scalar(
        "SELECT image.file_path FROM toonflow.images image
           JOIN toonflow.assets asset ON asset.id=image.assets_id
           WHERE asset.project_id=$1 AND coalesce(image.file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.storyboards
           WHERE project_id=$1 AND coalesce(file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.videos
           WHERE project_id=$1 AND coalesce(file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.video_continuity_frames
           WHERE project_id=$1 AND coalesce(file_path,'')<>''
         UNION ALL SELECT file_path FROM toonflow.episode_renders
           WHERE project_id=$1 AND coalesce(file_path,'')<>''
         UNION ALL SELECT cover_path FROM toonflow.episode_renders
           WHERE project_id=$1 AND coalesce(cover_path,'')<>''
         UNION ALL SELECT job.result->>'stagingObjectPath'
           FROM toonflow.distributed_jobs job
           JOIN toonflow.tasks task ON task.id=job.task_id
           WHERE task.project_id=$1 AND coalesce(job.result->>'stagingObjectPath','')<>''",
    )
    .bind(request.id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to collect project files"))?;
    enqueue_cleanup_paths(
        &mut tx,
        &paths,
        "project",
        Some(request.id),
        "项目记录已删除，等待引用感知对象清理",
    )
    .await
    .map_err(|_| AppError::internal("failed to enqueue project cleanup"))?;
    let result = sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
        .bind(request.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete toonflow project"))?;
    affected(result.rows_affected(), "project")?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit project deletion"))?;
    Ok(Json(ApiResponse::with_message((), "删除项目成功")))
}
