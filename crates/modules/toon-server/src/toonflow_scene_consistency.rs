use std::collections::{BTreeMap, HashSet};

use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgConnection, PgPool};

use crate::{
    ToonState, shared::require, toonflow_project_helpers::now_ms,
    toonflow_scene_transitions::normalize_persisted_scene_key,
};

const BASE_STATE_KEY: &str = "base";

#[allow(clippy::too_many_arguments)]
pub(crate) fn storyboard_consistency_status(
    scene_key: Option<&str>,
    scene_state_id: Option<i64>,
    file_path: Option<&str>,
    generated_scene_state_id: Option<i64>,
    generation_context: &Value,
    scene_master_status: Option<&str>,
    scene_master_revision: Option<i32>,
    scene_state_revision: Option<i32>,
) -> &'static str {
    if scene_key.is_none() {
        return "missing_scene_key";
    }
    if scene_state_id.is_none() {
        return "unconfigured";
    }
    if scene_master_status != Some("ready") {
        return "missing_master";
    }
    if file_path.is_none_or(|path| path.trim().is_empty()) {
        return "ready";
    }
    if generated_scene_state_id != scene_state_id {
        return "stale";
    }
    let generated_master_revision = generation_context
        .get("masterRevision")
        .and_then(Value::as_i64);
    let generated_state_revision = generation_context
        .get("stateRevision")
        .and_then(Value::as_i64);
    if generated_master_revision != scene_master_revision.map(i64::from)
        || generated_state_revision != scene_state_revision.map(i64::from)
    {
        "stale"
    } else {
        "ready"
    }
}

#[derive(Debug, FromRow)]
struct TrackStoryboardConsistencyRow {
    id: i64,
    scene_key: Option<String>,
    scene_state_id: Option<i64>,
    file_path: Option<String>,
    generated_scene_state_id: Option<i64>,
    scene_generation_context: Value,
    scene_master_status: Option<String>,
    scene_master_revision: Option<i32>,
    scene_state_revision: Option<i32>,
}

pub(crate) async fn ensure_track_storyboard_images_current(
    pool: &PgPool,
    project_id: i64,
    script_id: i64,
    track_ids: &[i64],
) -> Result<(), AppError> {
    if track_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, TrackStoryboardConsistencyRow>(
        r#"SELECT storyboard.id,storyboard.scene_key,storyboard.scene_state_id,
                  storyboard.file_path,storyboard.generated_scene_state_id,
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
             AND storyboard.track_id=ANY($3)
             AND coalesce(storyboard.file_path,'')<>''
           ORDER BY storyboard.index,storyboard.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to validate video storyboard scene states"))?;
    let stale = rows
        .iter()
        .filter_map(|row| {
            let status = storyboard_consistency_status(
                row.scene_key.as_deref(),
                row.scene_state_id,
                row.file_path.as_deref(),
                row.generated_scene_state_id,
                &row.scene_generation_context,
                row.scene_master_status.as_deref(),
                row.scene_master_revision,
                row.scene_state_revision,
            );
            (status != "ready").then_some(format!("{}（{}）", row.id, status_label(status)))
        })
        .collect::<Vec<_>>();
    if stale.is_empty() {
        Ok(())
    } else {
        Err(AppError::bad_request(format!(
            "以下分镜图片的场景母版或状态已过期，必须先重新生成图片再生成视频：{}",
            stale.join("、")
        )))
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "missing_scene_key" => "缺少场次",
        "unconfigured" => "未绑定状态",
        "missing_master" => "母版未就绪",
        "stale" => "状态已变更",
        _ => "未知状态",
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneCatalogRequest {
    project_id: i64,
    script_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSceneMasterRequest {
    project_id: i64,
    script_id: i64,
    scene_key: String,
    #[serde(default)]
    name: String,
    scene_asset_id: Option<i64>,
    /// Keeps an already selected mother-image version stable while the user
    /// edits metadata. Omitting it intentionally selects the asset's current
    /// ready image, which is also the behavior for older clients.
    pinned_image_id: Option<i64>,
    #[serde(default)]
    spatial_prompt: String,
    #[serde(default = "empty_object")]
    layout_spec: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSceneStateRequest {
    id: Option<i64>,
    scene_master_id: i64,
    state_key: String,
    #[serde(default)]
    name: String,
    parent_state_id: Option<i64>,
    sequence: Option<i32>,
    #[serde(default)]
    change_summary: String,
    #[serde(default)]
    state_prompt: String,
    #[serde(default = "empty_object")]
    object_states: Value,
    #[serde(default)]
    reference_asset_ids: Vec<i64>,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, FromRow)]
struct SceneMasterRecord {
    id: i64,
    project_id: i64,
    script_id: i64,
    scene_key: String,
    name: String,
    scene_asset_id: Option<i64>,
    scene_asset_name: Option<String>,
    pinned_image_id: Option<i64>,
    reference_url: Option<String>,
    spatial_prompt: String,
    layout_spec: Value,
    status: String,
    source: String,
    revision: i32,
    create_time: i64,
    update_time: i64,
}

#[derive(Debug, FromRow)]
struct SceneStateRecord {
    id: i64,
    scene_master_id: i64,
    state_key: String,
    name: String,
    parent_state_id: Option<i64>,
    sequence: i32,
    change_summary: String,
    state_prompt: String,
    object_states: Value,
    source: String,
    revision: i32,
    create_time: i64,
    update_time: i64,
    storyboard_count: i64,
}

#[derive(Debug, FromRow)]
struct SceneStateReferenceRecord {
    scene_state_id: i64,
    sort_order: i32,
    role: String,
    asset_id: i64,
    asset_name: String,
    image_id: i64,
    reference_url: String,
    prompt_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneStateReferenceResponse {
    sort_order: i32,
    role: String,
    asset_id: i64,
    asset_name: String,
    image_id: i64,
    reference_url: String,
    prompt_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneStateResponse {
    id: i64,
    scene_master_id: i64,
    state_key: String,
    name: String,
    parent_state_id: Option<i64>,
    sequence: i32,
    change_summary: String,
    state_prompt: String,
    object_states: Value,
    source: String,
    revision: i32,
    create_time: i64,
    update_time: i64,
    storyboard_count: i64,
    references: Vec<SceneStateReferenceResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneMasterResponse {
    id: i64,
    project_id: i64,
    script_id: i64,
    scene_key: String,
    name: String,
    scene_asset_id: Option<i64>,
    scene_asset_name: Option<String>,
    pinned_image_id: Option<i64>,
    reference_url: Option<String>,
    spatial_prompt: String,
    layout_spec: Value,
    status: String,
    source: String,
    revision: i32,
    create_time: i64,
    update_time: i64,
    states: Vec<SceneStateResponse>,
}

pub async fn list_scene_catalog(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SceneCatalogRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:read")?;
    crate::toonflow_episode_renders::ensure_project_access(&state.pool, &user, request.project_id)
        .await?;
    ensure_script_scope(&state.pool, request.project_id, request.script_id).await?;
    Ok(Json(ApiResponse::new(json!({
        "scenes": load_catalog(&state.pool, request.project_id, request.script_id).await?,
    }))))
}

pub async fn save_scene_master(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveSceneMasterRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    crate::toonflow_episode_renders::ensure_project_access(&state.pool, &user, request.project_id)
        .await?;
    ensure_script_scope(&state.pool, request.project_id, request.script_id).await?;
    let scene_key = normalize_persisted_scene_key(Some(&request.scene_key))?
        .ok_or_else(|| AppError::bad_request("场次键不能为空"))?;
    ensure_json_object(&request.layout_spec, "layoutSpec")?;

    let asset = load_scene_asset(&state.pool, request.project_id, request.scene_asset_id).await?;
    let (scene_asset_id, pinned_image_id, status) = match asset {
        Some(asset) => {
            let pinned_image_id = if let Some(pinned_image_id) = request.pinned_image_id {
                validate_pinned_scene_image(&state.pool, asset.id, pinned_image_id).await?;
                Some(pinned_image_id)
            } else {
                asset.image_id.filter(|_| asset.reference_url.is_some())
            };
            (
                Some(asset.id),
                pinned_image_id,
                if pinned_image_id.is_some() {
                    "ready"
                } else {
                    "missing_reference"
                },
            )
        }
        None => {
            if request.pinned_image_id.is_some() {
                return Err(AppError::bad_request(
                    "未选择场景资产时不能指定母版图片版本",
                ));
            }
            (None, None, "missing_reference")
        }
    };
    let name = non_empty_or(&request.name, &scene_key);
    let timestamp = now_ms();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to save scene master"))?;
    let master_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO toonflow.scene_masters(
             project_id,script_id,scene_key,name,scene_asset_id,pinned_image_id,
             spatial_prompt,layout_spec,status,source,create_time,update_time
           ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'manual',$10,$10)
           ON CONFLICT(project_id,script_id,scene_key) DO UPDATE SET
             name=excluded.name,
             scene_asset_id=excluded.scene_asset_id,
             pinned_image_id=excluded.pinned_image_id,
             spatial_prompt=excluded.spatial_prompt,
             layout_spec=excluded.layout_spec,
             status=excluded.status,
             source='manual',
             revision=toonflow.scene_masters.revision + CASE WHEN
               ROW(
                 toonflow.scene_masters.name,
                 toonflow.scene_masters.scene_asset_id,
                 toonflow.scene_masters.pinned_image_id,
                 toonflow.scene_masters.spatial_prompt,
                 toonflow.scene_masters.layout_spec,
                 toonflow.scene_masters.status
               ) IS DISTINCT FROM ROW(
                 excluded.name,
                 excluded.scene_asset_id,
                 excluded.pinned_image_id,
                 excluded.spatial_prompt,
                 excluded.layout_spec,
                 excluded.status
               ) THEN 1 ELSE 0 END,
             update_time=CASE WHEN
               ROW(
                 toonflow.scene_masters.name,
                 toonflow.scene_masters.scene_asset_id,
                 toonflow.scene_masters.pinned_image_id,
                 toonflow.scene_masters.spatial_prompt,
                 toonflow.scene_masters.layout_spec,
                 toonflow.scene_masters.status
               ) IS DISTINCT FROM ROW(
                 excluded.name,
                 excluded.scene_asset_id,
                 excluded.pinned_image_id,
                 excluded.spatial_prompt,
                 excluded.layout_spec,
                 excluded.status
               ) THEN excluded.update_time ELSE toonflow.scene_masters.update_time END
           RETURNING id"#,
    )
    .bind(request.project_id)
    .bind(request.script_id)
    .bind(&scene_key)
    .bind(name)
    .bind(scene_asset_id)
    .bind(pinned_image_id)
    .bind(request.spatial_prompt.trim())
    .bind(request.layout_spec)
    .bind(status)
    .bind(timestamp)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to save scene master"))?;
    ensure_base_state(&mut tx, master_id, timestamp).await?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to save scene master"))?;

    Ok(Json(ApiResponse::new(json!({ "id": master_id }))))
}

pub async fn save_scene_state(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveSceneStateRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let state_key = normalize_state_key(&request.state_key)?;
    ensure_json_object(&request.object_states, "objectStates")?;
    ensure_unique_ids(&request.reference_asset_ids)?;
    if state_key != BASE_STATE_KEY
        && request.change_summary.trim().is_empty()
        && request.state_prompt.trim().is_empty()
    {
        return Err(AppError::bad_request(
            "非基础状态必须填写变化说明或累计状态约束",
        ));
    }

    let master_scope: Option<(i64, i64)> =
        sqlx::query_as("SELECT project_id,script_id FROM toonflow.scene_masters WHERE id=$1")
            .bind(request.scene_master_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load scene master"))?;
    let (project_id, _script_id) =
        master_scope.ok_or_else(|| AppError::not_found("scene master not found"))?;
    crate::toonflow_episode_renders::ensure_project_access(&state.pool, &user, project_id).await?;

    let references =
        load_reference_assets(&state.pool, project_id, &request.reference_asset_ids).await?;
    let timestamp = now_ms();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to save scene state"))?;
    let base_state_id = ensure_base_state(&mut tx, request.scene_master_id, timestamp).await?;
    let parent_state_id = if state_key == BASE_STATE_KEY {
        if request.parent_state_id.is_some() {
            return Err(AppError::bad_request("基础状态不能设置前置状态"));
        }
        None
    } else {
        Some(request.parent_state_id.unwrap_or(base_state_id))
    };
    let sequence = if state_key == BASE_STATE_KEY {
        0
    } else if let Some(sequence) = request.sequence {
        sequence.max(1)
    } else {
        sqlx::query_scalar(
            "SELECT coalesce(max(sequence),0)+1 FROM toonflow.scene_states WHERE scene_master_id=$1",
        )
        .bind(request.scene_master_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to order scene state"))?
    };
    validate_parent_state(
        &mut tx,
        request.scene_master_id,
        request.id,
        parent_state_id,
        sequence,
    )
    .await?;
    let name = non_empty_or(&request.name, &state_key);
    let existing_state_id = request.id.or_else(|| {
        if state_key == BASE_STATE_KEY {
            Some(base_state_id)
        } else {
            None
        }
    });
    let existing_state = if let Some(id) = existing_state_id {
        sqlx::query_as::<_, EditableSceneState>(
            r#"SELECT id,state_key,name,parent_state_id,sequence,change_summary,
                      state_prompt,object_states
               FROM toonflow.scene_states
               WHERE id=$1 AND scene_master_id=$2"#,
        )
        .bind(id)
        .bind(request.scene_master_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to load scene state"))?
        .ok_or_else(|| AppError::not_found("scene state not found"))?
        .into()
    } else {
        None
    };
    if state_key == BASE_STATE_KEY
        && existing_state
            .as_ref()
            .is_some_and(|state| state.id != base_state_id)
    {
        return Err(AppError::bad_request("基础状态键只能用于系统基础状态"));
    }

    let change_summary = request.change_summary.trim();
    let state_prompt = request.state_prompt.trim();
    let metadata_changed = existing_state.as_ref().is_some_and(|existing| {
        existing.state_key != state_key
            || existing.name != name
            || existing.parent_state_id != parent_state_id
            || existing.sequence != sequence
            || existing.change_summary != change_summary
            || existing.state_prompt != state_prompt
            || existing.object_states != request.object_states
    });
    let state_id = if let Some(existing) = existing_state.as_ref() {
        sqlx::query_scalar(
            r#"UPDATE toonflow.scene_states SET
                 state_key=$3,name=$4,parent_state_id=$5,sequence=$6,change_summary=$7,
                 state_prompt=$8,object_states=$9,source='manual',
                 revision=revision+CASE WHEN $10 THEN 1 ELSE 0 END,
                 update_time=CASE WHEN $10 THEN $11 ELSE update_time END
               WHERE id=$1 AND scene_master_id=$2
               RETURNING id"#,
        )
        .bind(existing.id)
        .bind(request.scene_master_id)
        .bind(&state_key)
        .bind(name)
        .bind(parent_state_id)
        .bind(sequence)
        .bind(change_summary)
        .bind(state_prompt)
        .bind(&request.object_states)
        .bind(metadata_changed)
        .bind(timestamp)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::bad_request("状态键或顺序已存在，请换一个值"))?
    } else {
        sqlx::query_scalar(
            r#"INSERT INTO toonflow.scene_states(
                 scene_master_id,state_key,name,parent_state_id,sequence,change_summary,
                 state_prompt,object_states,source,create_time,update_time
               ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'manual',$9,$9)
               RETURNING id"#,
        )
        .bind(request.scene_master_id)
        .bind(&state_key)
        .bind(name)
        .bind(parent_state_id)
        .bind(sequence)
        .bind(change_summary)
        .bind(state_prompt)
        .bind(&request.object_states)
        .bind(timestamp)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| AppError::bad_request("状态键或顺序已存在，请换一个值"))?
    };

    if metadata_changed {
        touch_descendant_states(&mut tx, state_id, timestamp).await?;
    }

    let existing_references = sqlx::query_as::<_, StateReferenceBinding>(
        r#"SELECT sort_order,role,asset_id,image_id,prompt_label
           FROM toonflow.scene_state_references
           WHERE scene_state_id=$1
           ORDER BY sort_order"#,
    )
    .bind(state_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to inspect scene state references"))?;
    let desired_references = references
        .iter()
        .enumerate()
        .map(|(sort_order, reference)| StateReferenceBinding {
            sort_order: sort_order as i32,
            role: if reference.type_ == "scene" {
                "state".to_string()
            } else {
                "object_detail".to_string()
            },
            asset_id: reference.id,
            image_id: reference.image_id.expect("validated image id"),
            prompt_label: reference.name.clone(),
        })
        .collect::<Vec<_>>();
    if existing_references != desired_references {
        sqlx::query("DELETE FROM toonflow.scene_state_references WHERE scene_state_id=$1")
            .bind(state_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to update scene state references"))?;
        for reference in &desired_references {
            sqlx::query(
                r#"INSERT INTO toonflow.scene_state_references(
                     scene_state_id,sort_order,role,asset_id,image_id,prompt_label
                   ) VALUES($1,$2,$3,$4,$5,$6)"#,
            )
            .bind(state_id)
            .bind(reference.sort_order)
            .bind(&reference.role)
            .bind(reference.asset_id)
            .bind(reference.image_id)
            .bind(&reference.prompt_label)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to update scene state references"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to save scene state"))?;
    Ok(Json(ApiResponse::new(json!({ "id": state_id }))))
}

async fn ensure_script_scope(
    pool: &PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM toonflow.scripts WHERE id=$1 AND project_id=$2)",
    )
    .bind(script_id)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::internal("failed to validate script"))?;
    if exists {
        Ok(())
    } else {
        Err(AppError::not_found("script not found"))
    }
}

async fn load_catalog(
    pool: &PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<Vec<SceneMasterResponse>, AppError> {
    let masters = sqlx::query_as::<_, SceneMasterRecord>(
        r#"SELECT master.id,master.project_id,master.script_id,master.scene_key,master.name,
                  master.scene_asset_id,asset.name AS scene_asset_name,master.pinned_image_id,
                  image.file_path AS reference_url,master.spatial_prompt,master.layout_spec,
                  CASE WHEN master.status='ready' AND (
                         image.id IS NULL OR image.state<>'已完成' OR coalesce(image.file_path,'')=''
                       ) THEN 'missing_reference' ELSE master.status END AS status,
                  master.source,master.revision,master.create_time,master.update_time
           FROM toonflow.scene_masters master
           LEFT JOIN toonflow.assets asset ON asset.id=master.scene_asset_id
           LEFT JOIN toonflow.images image ON image.id=master.pinned_image_id
           WHERE master.project_id=$1 AND master.script_id=$2
           ORDER BY nullif(regexp_replace(master.scene_key,'[^0-9]','','g'),'')::numeric NULLS LAST,
                    master.scene_key,master.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to list scene masters"))?;
    let master_ids = masters.iter().map(|master| master.id).collect::<Vec<_>>();
    let states = if master_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, SceneStateRecord>(
            r#"SELECT scene_state.id,scene_state.scene_master_id,scene_state.state_key,
                      scene_state.name,scene_state.parent_state_id,scene_state.sequence,
                      scene_state.change_summary,scene_state.state_prompt,scene_state.object_states,
                      scene_state.source,scene_state.revision,scene_state.create_time,
                      scene_state.update_time,count(storyboard.id)::bigint AS storyboard_count
               FROM toonflow.scene_states scene_state
               LEFT JOIN toonflow.storyboards storyboard ON storyboard.scene_state_id=scene_state.id
               WHERE scene_state.scene_master_id=ANY($1)
               GROUP BY scene_state.id
               ORDER BY scene_state.scene_master_id,scene_state.sequence,scene_state.id"#,
        )
        .bind(&master_ids)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list scene states"))?
    };
    let state_ids = states
        .iter()
        .map(|scene_state| scene_state.id)
        .collect::<Vec<_>>();
    let references = if state_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, SceneStateReferenceRecord>(
            r#"SELECT reference.scene_state_id,reference.sort_order,reference.role,
                      reference.asset_id,asset.name AS asset_name,reference.image_id,
                      image.file_path AS reference_url,reference.prompt_label
               FROM toonflow.scene_state_references reference
               JOIN toonflow.assets asset ON asset.id=reference.asset_id
               JOIN toonflow.images image ON image.id=reference.image_id
               WHERE reference.scene_state_id=ANY($1)
               ORDER BY reference.scene_state_id,reference.sort_order"#,
        )
        .bind(&state_ids)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list scene state references"))?
    };

    let mut references_by_state = BTreeMap::<i64, Vec<SceneStateReferenceResponse>>::new();
    for reference in references {
        references_by_state
            .entry(reference.scene_state_id)
            .or_default()
            .push(SceneStateReferenceResponse {
                sort_order: reference.sort_order,
                role: reference.role,
                asset_id: reference.asset_id,
                asset_name: reference.asset_name,
                image_id: reference.image_id,
                reference_url: reference.reference_url,
                prompt_label: reference.prompt_label,
            });
    }
    let mut states_by_master = BTreeMap::<i64, Vec<SceneStateResponse>>::new();
    for scene_state in states {
        let state_id = scene_state.id;
        states_by_master
            .entry(scene_state.scene_master_id)
            .or_default()
            .push(SceneStateResponse {
                id: state_id,
                scene_master_id: scene_state.scene_master_id,
                state_key: scene_state.state_key,
                name: scene_state.name,
                parent_state_id: scene_state.parent_state_id,
                sequence: scene_state.sequence,
                change_summary: scene_state.change_summary,
                state_prompt: scene_state.state_prompt,
                object_states: scene_state.object_states,
                source: scene_state.source,
                revision: scene_state.revision,
                create_time: scene_state.create_time,
                update_time: scene_state.update_time,
                storyboard_count: scene_state.storyboard_count,
                references: references_by_state.remove(&state_id).unwrap_or_default(),
            });
    }
    Ok(masters
        .into_iter()
        .map(|master| {
            let master_id = master.id;
            SceneMasterResponse {
                id: master_id,
                project_id: master.project_id,
                script_id: master.script_id,
                scene_key: master.scene_key,
                name: master.name,
                scene_asset_id: master.scene_asset_id,
                scene_asset_name: master.scene_asset_name,
                pinned_image_id: master.pinned_image_id,
                reference_url: master.reference_url,
                spatial_prompt: master.spatial_prompt,
                layout_spec: master.layout_spec,
                status: master.status,
                source: master.source,
                revision: master.revision,
                create_time: master.create_time,
                update_time: master.update_time,
                states: states_by_master.remove(&master_id).unwrap_or_default(),
            }
        })
        .collect())
}

#[derive(Debug, FromRow)]
struct AssetReference {
    id: i64,
    name: String,
    type_: String,
    image_id: Option<i64>,
    reference_url: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct CandidateSceneAsset {
    id: i64,
    name: String,
    image_id: Option<i64>,
    ready: bool,
}

#[derive(Debug, FromRow)]
struct SceneMasterBinding {
    id: i64,
    scene_asset_id: Option<i64>,
    pinned_image_id: Option<i64>,
    status: String,
    pinned_image_ready: bool,
    scene_asset_image_id: Option<i64>,
    scene_asset_image_ready: bool,
}

#[derive(Debug, FromRow)]
struct AgentSceneState {
    id: i64,
    scene_master_id: i64,
    state_key: String,
    parent_state_id: Option<i64>,
    change_summary: String,
    state_prompt: String,
}

#[derive(Debug, FromRow)]
struct EditableSceneState {
    id: i64,
    state_key: String,
    name: String,
    parent_state_id: Option<i64>,
    sequence: i32,
    change_summary: String,
    state_prompt: String,
    object_states: Value,
}

#[derive(Debug, Eq, FromRow, PartialEq)]
struct StateReferenceBinding {
    sort_order: i32,
    role: String,
    asset_id: i64,
    image_id: i64,
    prompt_label: String,
}

async fn load_scene_asset(
    pool: &PgPool,
    project_id: i64,
    asset_id: Option<i64>,
) -> Result<Option<AssetReference>, AppError> {
    let Some(asset_id) = asset_id else {
        return Ok(None);
    };
    let asset = sqlx::query_as::<_, AssetReference>(
        r#"SELECT asset.id,asset.name,asset.type AS type_,asset.image_id,
                  CASE WHEN image.state='已完成' AND image.file_path<>'' THEN image.file_path END AS reference_url
           FROM toonflow.assets asset
           LEFT JOIN toonflow.images image ON image.id=asset.image_id
           WHERE asset.id=$1 AND asset.project_id=$2 AND asset.type='scene'"#,
    )
    .bind(asset_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to validate scene asset"))?;
    asset
        .map(Some)
        .ok_or_else(|| AppError::bad_request("场景母版只能选择当前项目的场景资产"))
}

async fn validate_pinned_scene_image(
    pool: &PgPool,
    scene_asset_id: i64,
    image_id: i64,
) -> Result<(), AppError> {
    let ready: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM toonflow.images
             WHERE id=$1 AND assets_id=$2 AND state='已完成'
               AND coalesce(file_path,'')<>''
           )"#,
    )
    .bind(image_id)
    .bind(scene_asset_id)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::internal("failed to validate pinned scene image"))?;
    if ready {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "锁定的母版图片版本不可用，请重新选择场景资产图片",
        ))
    }
}

async fn load_reference_assets(
    pool: &PgPool,
    project_id: i64,
    asset_ids: &[i64],
) -> Result<Vec<AssetReference>, AppError> {
    if asset_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, AssetReference>(
        r#"SELECT asset.id,asset.name,asset.type AS type_,asset.image_id,
                  CASE WHEN image.state='已完成' AND image.file_path<>'' THEN image.file_path END AS reference_url
           FROM unnest($2::bigint[]) WITH ORDINALITY requested(id,position)
           JOIN toonflow.assets asset ON asset.id=requested.id AND asset.project_id=$1
           LEFT JOIN toonflow.images image ON image.id=asset.image_id
           ORDER BY requested.position"#,
    )
    .bind(project_id)
    .bind(asset_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to validate scene state references"))?;
    if rows.len() != asset_ids.len() {
        return Err(AppError::bad_request("状态参考图只能选择当前项目中的资产"));
    }
    if let Some(asset) = rows
        .iter()
        .find(|asset| asset.image_id.is_none() || asset.reference_url.is_none())
    {
        return Err(AppError::bad_request(format!(
            "状态参考资产“{}”还没有可用图片",
            asset.name
        )));
    }
    Ok(rows)
}

pub(crate) async fn ensure_base_state(
    connection: &mut PgConnection,
    scene_master_id: i64,
    timestamp: i64,
) -> Result<i64, AppError> {
    sqlx::query_scalar(
        r#"INSERT INTO toonflow.scene_states(
             scene_master_id,state_key,name,sequence,source,create_time,update_time
           ) VALUES($1,'base','初始状态',0,'system',$2,$2)
           ON CONFLICT(scene_master_id,state_key) DO UPDATE
             SET state_key=excluded.state_key
           RETURNING id"#,
    )
    .bind(scene_master_id)
    .bind(timestamp)
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to ensure base scene state"))
}

async fn validate_parent_state(
    connection: &mut PgConnection,
    scene_master_id: i64,
    state_id: Option<i64>,
    parent_state_id: Option<i64>,
    sequence: i32,
) -> Result<(), AppError> {
    let Some(parent_state_id) = parent_state_id else {
        return Ok(());
    };
    if state_id == Some(parent_state_id) {
        return Err(AppError::bad_request("场景状态不能以自己作为前置状态"));
    }
    let parent_sequence: Option<i32> = sqlx::query_scalar(
        "SELECT sequence FROM toonflow.scene_states WHERE id=$1 AND scene_master_id=$2",
    )
    .bind(parent_state_id)
    .bind(scene_master_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to validate parent scene state"))?;
    let Some(parent_sequence) = parent_sequence else {
        return Err(AppError::bad_request("前置状态不属于当前场景母版"));
    };
    if parent_sequence >= sequence {
        return Err(AppError::bad_request("前置状态的顺序必须早于当前状态"));
    }
    if let Some(state_id) = state_id {
        let child_out_of_order: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                 SELECT 1 FROM toonflow.scene_states
                 WHERE parent_state_id=$1 AND sequence<=$2
               )"#,
        )
        .bind(state_id)
        .bind(sequence)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| AppError::internal("failed to validate child scene states"))?;
        if child_out_of_order {
            return Err(AppError::bad_request("当前状态的顺序必须早于所有后续状态"));
        }
        let creates_cycle: bool = sqlx::query_scalar(
            r#"WITH RECURSIVE descendants(id) AS (
                 SELECT id FROM toonflow.scene_states WHERE parent_state_id=$1
                 UNION
                 SELECT child.id FROM toonflow.scene_states child
                 JOIN descendants parent ON child.parent_state_id=parent.id
               )
               SELECT EXISTS(SELECT 1 FROM descendants WHERE id=$2)"#,
        )
        .bind(state_id)
        .bind(parent_state_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| AppError::internal("failed to validate scene state chain"))?;
        if creates_cycle {
            return Err(AppError::bad_request("前置状态会形成循环状态链"));
        }
    }
    Ok(())
}

async fn touch_descendant_states(
    connection: &mut PgConnection,
    state_id: i64,
    timestamp: i64,
) -> Result<(), AppError> {
    sqlx::query(
        r#"WITH RECURSIVE descendants(id) AS (
             SELECT id FROM toonflow.scene_states WHERE parent_state_id=$1
             UNION
             SELECT child.id FROM toonflow.scene_states child
             JOIN descendants parent ON child.parent_state_id=parent.id
           )
           UPDATE toonflow.scene_states
           SET revision=revision+1,update_time=$2
           WHERE id IN (SELECT id FROM descendants)"#,
    )
    .bind(state_id)
    .bind(timestamp)
    .execute(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to invalidate descendant scene states"))?;
    Ok(())
}

fn ensure_json_object(value: &Value, field: &str) -> Result<(), AppError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(AppError::bad_request(format!("{field} 必须是 JSON 对象")))
    }
}

fn ensure_unique_ids(ids: &[i64]) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    if ids.iter().all(|id| seen.insert(*id)) {
        Ok(())
    } else {
        Err(AppError::bad_request("状态参考资产不能重复"))
    }
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() { fallback } else { value }
}

pub(crate) fn normalize_state_key(value: &str) -> Result<String, AppError> {
    let key = value.trim().to_ascii_lowercase();
    let valid = !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        && key.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        });
    if valid {
        Ok(key)
    } else {
        Err(AppError::bad_request(
            "场景状态键必须以小写字母开头，并且只包含小写字母、数字、_ 或 -",
        ))
    }
}

async fn load_candidate_scene_assets(
    connection: &mut PgConnection,
    project_id: i64,
    asset_ids: &[i64],
) -> Result<Vec<CandidateSceneAsset>, AppError> {
    if asset_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, CandidateSceneAsset>(
        r#"SELECT asset.id,asset.name,asset.image_id,
                  coalesce(
                    image.id IS NOT NULL AND image.assets_id=asset.id
                    AND image.state='已完成' AND coalesce(image.file_path,'')<>'',
                    false
                  ) AS ready
           FROM toonflow.assets asset
           LEFT JOIN toonflow.images image ON image.id=asset.image_id
           WHERE asset.project_id=$1 AND asset.id=ANY($2) AND asset.type='scene'
           ORDER BY array_position($2::bigint[],asset.id)"#,
    )
    .bind(project_id)
    .bind(asset_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to inspect storyboard scene assets"))
}

async fn load_scene_master_binding(
    connection: &mut PgConnection,
    project_id: i64,
    script_id: i64,
    scene_key: &str,
    expected_master_id: Option<i64>,
) -> Result<Option<SceneMasterBinding>, AppError> {
    sqlx::query_as::<_, SceneMasterBinding>(
        r#"SELECT master.id,master.scene_asset_id,master.pinned_image_id,master.status,
                  coalesce(
                    pinned.id IS NOT NULL AND pinned.assets_id=master.scene_asset_id
                    AND pinned.state='已完成' AND coalesce(pinned.file_path,'')<>'',
                    false
                  ) AS pinned_image_ready,
                  scene_asset.image_id AS scene_asset_image_id,
                  coalesce(
                    scene_image.id IS NOT NULL AND scene_image.assets_id=scene_asset.id
                    AND scene_image.state='已完成' AND coalesce(scene_image.file_path,'')<>'',
                    false
                  ) AS scene_asset_image_ready
           FROM toonflow.scene_masters master
           LEFT JOIN toonflow.assets scene_asset ON scene_asset.id=master.scene_asset_id
           LEFT JOIN toonflow.images pinned ON pinned.id=master.pinned_image_id
           LEFT JOIN toonflow.images scene_image ON scene_image.id=scene_asset.image_id
           WHERE master.project_id=$1 AND master.script_id=$2 AND master.scene_key=$3
             AND ($4::bigint IS NULL OR master.id=$4)"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(scene_key)
    .bind(expected_master_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard scene master"))
}

#[allow(clippy::too_many_arguments)]
async fn ensure_agent_scene_master(
    connection: &mut PgConnection,
    project_id: i64,
    script_id: i64,
    scene_key: &str,
    state_key: &str,
    expected_master_id: Option<i64>,
    candidates: &[CandidateSceneAsset],
    timestamp: i64,
) -> Result<SceneMasterBinding, AppError> {
    if state_key == BASE_STATE_KEY && candidates.len() > 1 {
        return Err(AppError::bad_request(
            "基础状态只能绑定一个场景资产；状态变化参考图请绑定到非基础状态",
        ));
    }
    let existing = load_scene_master_binding(
        connection,
        project_id,
        script_id,
        scene_key,
        expected_master_id,
    )
    .await?;
    if expected_master_id.is_some() && existing.is_none() {
        return Err(AppError::bad_request("分镜状态不属于当前场次"));
    }

    if let Some(existing) = existing {
        let base_candidate = candidates.first();
        if state_key == BASE_STATE_KEY
            && let (Some(locked_asset_id), Some(candidate)) =
                (existing.scene_asset_id, base_candidate)
            && locked_asset_id != candidate.id
        {
            return Err(AppError::bad_request(format!(
                "场次 {scene_key} 已锁定其他场景母版；不能在 base 状态混入“{}”",
                candidate.name
            )));
        }

        let desired_scene_asset_id = if state_key == BASE_STATE_KEY {
            existing
                .scene_asset_id
                .or_else(|| base_candidate.map(|candidate| candidate.id))
        } else {
            existing.scene_asset_id
        };
        let (asset_image_id, asset_image_ready) =
            if desired_scene_asset_id == existing.scene_asset_id {
                (
                    existing.scene_asset_image_id,
                    existing.scene_asset_image_ready,
                )
            } else if let Some(candidate) = base_candidate {
                (candidate.image_id, candidate.ready)
            } else {
                (None, false)
            };
        let desired_pinned_image_id = if desired_scene_asset_id.is_some() {
            existing
                .pinned_image_id
                .or_else(|| asset_image_id.filter(|_| asset_image_ready))
        } else {
            None
        };
        let desired_pinned_ready = if desired_pinned_image_id == existing.pinned_image_id {
            existing.pinned_image_ready
        } else {
            asset_image_ready
        };
        let desired_status = if desired_pinned_image_id.is_some() && desired_pinned_ready {
            "ready"
        } else if desired_scene_asset_id.is_none()
            && state_key != BASE_STATE_KEY
            && (!candidates.is_empty() || existing.status == "needs_review")
        {
            "needs_review"
        } else {
            "missing_reference"
        };
        let changed = existing.scene_asset_id != desired_scene_asset_id
            || existing.pinned_image_id != desired_pinned_image_id
            || existing.status != desired_status;
        if changed {
            sqlx::query(
                r#"UPDATE toonflow.scene_masters SET
                     scene_asset_id=$2,pinned_image_id=$3,status=$4,
                     revision=revision+1,update_time=$5
                   WHERE id=$1"#,
            )
            .bind(existing.id)
            .bind(desired_scene_asset_id)
            .bind(desired_pinned_image_id)
            .bind(desired_status)
            .bind(timestamp)
            .execute(&mut *connection)
            .await
            .map_err(|_| AppError::internal("failed to refresh storyboard scene master"))?;
        }
        return load_scene_master_binding(
            connection,
            project_id,
            script_id,
            scene_key,
            Some(existing.id),
        )
        .await?
        .ok_or_else(|| AppError::internal("storyboard scene master disappeared"));
    }

    let base_candidate = (state_key == BASE_STATE_KEY)
        .then(|| candidates.first())
        .flatten();
    let scene_asset_id = base_candidate.map(|candidate| candidate.id);
    let pinned_image_id = base_candidate
        .filter(|candidate| candidate.ready)
        .and_then(|candidate| candidate.image_id);
    let status = if pinned_image_id.is_some() {
        "ready"
    } else if state_key != BASE_STATE_KEY && !candidates.is_empty() {
        "needs_review"
    } else {
        "missing_reference"
    };
    let master_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO toonflow.scene_masters(
             project_id,script_id,scene_key,name,scene_asset_id,pinned_image_id,status,
             source,create_time,update_time
           ) VALUES($1,$2,$3,$3,$4,$5,$6,'agent',$7,$7)
           ON CONFLICT(project_id,script_id,scene_key) DO NOTHING
           RETURNING id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(scene_key)
    .bind(scene_asset_id)
    .bind(pinned_image_id)
    .bind(status)
    .bind(timestamp)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to create storyboard scene master"))?
    .unwrap_or(0);
    let expected_master_id = (master_id != 0).then_some(master_id);
    load_scene_master_binding(
        connection,
        project_id,
        script_id,
        scene_key,
        expected_master_id,
    )
    .await?
    .ok_or_else(|| AppError::internal("failed to resolve storyboard scene master"))
}

async fn load_agent_scene_state(
    connection: &mut PgConnection,
    state_id: i64,
) -> Result<AgentSceneState, AppError> {
    sqlx::query_as::<_, AgentSceneState>(
        r#"SELECT id,scene_master_id,state_key,parent_state_id,change_summary,state_prompt
           FROM toonflow.scene_states WHERE id=$1"#,
    )
    .bind(state_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard scene state"))
}

async fn validate_requested_parent_key(
    connection: &mut PgConnection,
    scene_master_id: i64,
    scene_state: &AgentSceneState,
    requested_parent_state_key: Option<&str>,
) -> Result<(), AppError> {
    let requested_parent_state_key = requested_parent_state_key
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if scene_state.state_key == BASE_STATE_KEY {
        return if requested_parent_state_key.is_none() {
            Ok(())
        } else {
            Err(AppError::bad_request(
                "基础状态不能设置 sceneStateParentKey",
            ))
        };
    }
    let Some(requested_parent_state_key) = requested_parent_state_key else {
        return Ok(());
    };
    let requested_parent_state_key = normalize_state_key(requested_parent_state_key)?;
    let requested_parent_state_id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM toonflow.scene_states WHERE scene_master_id=$1 AND state_key=$2",
    )
    .bind(scene_master_id)
    .bind(&requested_parent_state_key)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to validate requested parent scene state"))?;
    if requested_parent_state_id != scene_state.parent_state_id {
        Err(AppError::bad_request(
            "sceneStateParentKey 与既有状态链不一致",
        ))
    } else {
        Ok(())
    }
}

async fn ensure_stable_state_description(
    connection: &mut PgConnection,
    scene_state: &AgentSceneState,
    description: &str,
    timestamp: i64,
) -> Result<(), AppError> {
    if description.is_empty() {
        return Ok(());
    }
    let stored_description = if scene_state.state_prompt.trim().is_empty() {
        scene_state.change_summary.trim()
    } else {
        scene_state.state_prompt.trim()
    };
    if !stored_description.is_empty() {
        return if stored_description == description {
            Ok(())
        } else {
            Err(AppError::bad_request(format!(
                "sceneStateKey {} 已有不同的状态描述；请创建后续状态，不要覆盖既有状态",
                scene_state.state_key
            )))
        };
    }
    let result = sqlx::query(
        r#"UPDATE toonflow.scene_states
           SET change_summary=$2,state_prompt=$2,revision=revision+1,update_time=$3
           WHERE id=$1 AND change_summary='' AND state_prompt=''"#,
    )
    .bind(scene_state.id)
    .bind(description)
    .bind(timestamp)
    .execute(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to initialize scene state description"))?;
    if result.rows_affected() == 0 {
        let refreshed = load_agent_scene_state(connection, scene_state.id).await?;
        let stored_description = if refreshed.state_prompt.trim().is_empty() {
            refreshed.change_summary.trim()
        } else {
            refreshed.state_prompt.trim()
        };
        if stored_description != description {
            return Err(AppError::bad_request(format!(
                "sceneStateKey {} 已被写入不同的状态描述",
                scene_state.state_key
            )));
        }
        return Ok(());
    }
    touch_descendant_states(connection, scene_state.id, timestamp).await
}

async fn bind_inferred_state_references(
    connection: &mut PgConnection,
    state_id: i64,
    master_asset_id: Option<i64>,
    candidates: &[CandidateSceneAsset],
) -> Result<(), AppError> {
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.ready && Some(candidate.id) != master_asset_id)
    {
        let Some(image_id) = candidate.image_id else {
            continue;
        };
        let existing: Option<(i64, String)> = sqlx::query_as(
            r#"SELECT image_id,prompt_label
               FROM toonflow.scene_state_references
               WHERE scene_state_id=$1 AND asset_id=$2
               ORDER BY sort_order LIMIT 1"#,
        )
        .bind(state_id)
        .bind(candidate.id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|_| AppError::internal("failed to inspect inferred scene state reference"))?;
        if existing.as_ref().is_some_and(|(existing_image_id, label)| {
            *existing_image_id == image_id && label == &candidate.name
        }) {
            continue;
        }
        if existing.is_some() {
            sqlx::query(
                "DELETE FROM toonflow.scene_state_references WHERE scene_state_id=$1 AND asset_id=$2",
            )
            .bind(state_id)
            .bind(candidate.id)
            .execute(&mut *connection)
            .await
            .map_err(|_| AppError::internal("failed to replace inferred scene state reference"))?;
        }
        sqlx::query(
            r#"INSERT INTO toonflow.scene_state_references(
                 scene_state_id,sort_order,role,asset_id,image_id,prompt_label
               ) VALUES(
                 $1,
                 coalesce((SELECT max(sort_order)+1 FROM toonflow.scene_state_references WHERE scene_state_id=$1),0),
                 'state',$2,$3,$4
               )
               ON CONFLICT(scene_state_id,image_id) DO NOTHING"#,
        )
        .bind(state_id)
        .bind(candidate.id)
        .bind(image_id)
        .bind(&candidate.name)
        .execute(&mut *connection)
        .await
        .map_err(|_| AppError::internal("failed to bind inferred scene state reference"))?;
    }
    Ok(())
}

/// Resolves the durable state binding for a storyboard write. Existing scene masters are never
/// silently replaced: automatic inference only fills an empty master from a scene asset already
/// bound to the storyboard.
#[allow(clippy::too_many_arguments)] // 分镜落库时的全部场景定位输入
pub(crate) async fn resolve_storyboard_scene_state(
    connection: &mut PgConnection,
    project_id: i64,
    script_id: i64,
    scene_key: Option<&str>,
    requested_state_id: Option<i64>,
    requested_state_key: Option<&str>,
    requested_parent_state_key: Option<&str>,
    state_description: Option<&str>,
    asset_ids: &[i64],
) -> Result<Option<i64>, AppError> {
    let Some(scene_key) = scene_key else {
        if requested_state_id.is_some() || requested_state_key.is_some() {
            return Err(AppError::bad_request("绑定场景状态前必须先设置 sceneKey"));
        }
        return Ok(None);
    };
    let requested_state = if let Some(state_id) = requested_state_id {
        Some(
            sqlx::query_as::<_, AgentSceneState>(
                r#"SELECT scene_state.id,scene_state.scene_master_id,scene_state.state_key,
                          scene_state.parent_state_id,scene_state.change_summary,
                          scene_state.state_prompt
                   FROM toonflow.scene_states scene_state
                   JOIN toonflow.scene_masters master ON master.id=scene_state.scene_master_id
                   WHERE scene_state.id=$1 AND master.project_id=$2 AND master.script_id=$3
                     AND master.scene_key=$4"#,
            )
            .bind(state_id)
            .bind(project_id)
            .bind(script_id)
            .bind(scene_key)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|_| AppError::internal("failed to validate storyboard scene state"))?
            .ok_or_else(|| AppError::bad_request("分镜状态不属于当前场次"))?,
        )
    } else {
        None
    };
    let state_key = if let Some(scene_state) = requested_state.as_ref() {
        if let Some(requested_key) = requested_state_key
            && normalize_state_key(requested_key)? != scene_state.state_key
        {
            return Err(AppError::bad_request(
                "sceneStateId 与 sceneStateKey 指向的状态不一致",
            ));
        }
        scene_state.state_key.clone()
    } else {
        normalize_state_key(requested_state_key.unwrap_or(BASE_STATE_KEY))?
    };
    let candidate_assets = load_candidate_scene_assets(connection, project_id, asset_ids).await?;
    let timestamp = now_ms();
    let master = ensure_agent_scene_master(
        connection,
        project_id,
        script_id,
        scene_key,
        &state_key,
        requested_state.as_ref().map(|state| state.scene_master_id),
        &candidate_assets,
        timestamp,
    )
    .await?;
    let base_state_id = ensure_base_state(connection, master.id, timestamp).await?;
    let description = state_description.unwrap_or_default().trim();

    if let Some(scene_state) = requested_state.as_ref() {
        validate_requested_parent_key(
            connection,
            master.id,
            scene_state,
            requested_parent_state_key,
        )
        .await?;
        ensure_stable_state_description(connection, scene_state, description, timestamp).await?;
        if scene_state.state_key != BASE_STATE_KEY {
            bind_inferred_state_references(
                connection,
                scene_state.id,
                master.scene_asset_id,
                &candidate_assets,
            )
            .await?;
        }
        return Ok(Some(scene_state.id));
    }

    if state_key == BASE_STATE_KEY {
        if requested_parent_state_key.is_some_and(|value| !value.trim().is_empty()) {
            return Err(AppError::bad_request(
                "基础状态不能设置 sceneStateParentKey",
            ));
        }
        let base_state = load_agent_scene_state(connection, base_state_id).await?;
        ensure_stable_state_description(connection, &base_state, description, timestamp).await?;
        return Ok(Some(base_state_id));
    }

    let existing_state = sqlx::query_as::<_, AgentSceneState>(
        r#"SELECT id,scene_master_id,state_key,parent_state_id,change_summary,state_prompt
           FROM toonflow.scene_states
           WHERE scene_master_id=$1 AND state_key=$2"#,
    )
    .bind(master.id)
    .bind(&state_key)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|_| AppError::internal("failed to inspect storyboard scene state"))?;
    if existing_state.is_none() && description.is_empty() {
        return Err(AppError::bad_request(
            "新建非基础场景状态时必须填写 sceneStateDescription",
        ));
    }
    let requested_parent_state_id = if let Some(parent_state_key) = requested_parent_state_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let parent_state_key = normalize_state_key(parent_state_key)?;
        if parent_state_key == state_key {
            return Err(AppError::bad_request(
                "场景状态不能以自己作为 sceneStateParentKey",
            ));
        }
        Some(
            sqlx::query_scalar(
                "SELECT id FROM toonflow.scene_states WHERE scene_master_id=$1 AND state_key=$2",
            )
            .bind(master.id)
            .bind(&parent_state_key)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|_| AppError::internal("failed to resolve parent scene state"))?
            .ok_or_else(|| {
                AppError::bad_request(format!(
                    "sceneStateParentKey {parent_state_key} 不属于当前场次"
                ))
            })?,
        )
    } else {
        None
    };
    if let Some(existing_state) = existing_state.as_ref()
        && requested_parent_state_id.is_some()
        && requested_parent_state_id != existing_state.parent_state_id
    {
        return Err(AppError::bad_request(
            "同一 sceneStateKey 的 sceneStateParentKey 必须保持一致",
        ));
    }
    let parent_state_id = if let Some(existing_state) = existing_state.as_ref() {
        existing_state
            .parent_state_id
            .ok_or_else(|| AppError::bad_request("非基础状态缺少前置状态"))?
    } else if let Some(parent_state_id) = requested_parent_state_id {
        parent_state_id
    } else {
        return Err(AppError::bad_request(
            "新建非基础场景状态时必须填写 sceneStateParentKey",
        ));
    };
    let scene_state = if let Some(existing_state) = existing_state {
        ensure_stable_state_description(connection, &existing_state, description, timestamp)
            .await?;
        existing_state
    } else {
        let state_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO toonflow.scene_states(
                 scene_master_id,state_key,name,parent_state_id,sequence,change_summary,state_prompt,
                 source,create_time,update_time
               ) VALUES(
                 $1,$2,$2,$3,
                 (SELECT coalesce(max(sequence),0)+1 FROM toonflow.scene_states WHERE scene_master_id=$1),
                 $4,$4,'agent',$5,$5
               )
               RETURNING id"#,
        )
        .bind(master.id)
        .bind(&state_key)
        .bind(parent_state_id)
        .bind(description)
        .bind(timestamp)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| AppError::bad_request("场景状态键或顺序已存在"))?;
        load_agent_scene_state(connection, state_id).await?
    };
    bind_inferred_state_references(
        connection,
        scene_state.id,
        master.scene_asset_id,
        &candidate_assets,
    )
    .await?;
    Ok(Some(scene_state.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_key_contract_is_stable_and_url_safe() {
        assert_eq!(normalize_state_key(" Door_Broken ").unwrap(), "door_broken");
        assert!(normalize_state_key("门损坏").is_err());
        assert!(normalize_state_key("1broken").is_err());
        assert!(normalize_state_key("").is_err());
    }

    #[test]
    fn state_json_fields_must_be_objects() {
        assert!(ensure_json_object(&json!({"door":"broken"}), "objectStates").is_ok());
        assert!(ensure_json_object(&json!([]), "objectStates").is_err());
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn resolver_locks_master_and_state_contracts_without_noop_revision_churn() {
        use std::time::Duration;

        use rust_toon_framework_database::{DatabaseConfig, connect, migrate};

        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();
        let project_id = -9_300_001_i64;
        let script_id = -9_300_002_i64;
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.projects(id,name,create_time,update_time)
             VALUES($1,'scene resolver test',0,0)",
        )
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.scripts(id,name,project_id,create_time)
             VALUES($1,'episode',$2,0)",
        )
        .bind(script_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.assets(id,name,type,project_id)
             VALUES
               (-9300011,'客厅母版','scene',$1),
               (-9300012,'门已损坏','scene',$1)",
        )
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.images(id,file_path,type,assets_id,state)
             VALUES
               (-9300021,'/scene/living-room.png','scene',-9300011,'已完成'),
               (-9300022,'/scene/broken-door.png','scene',-9300012,'已完成')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE toonflow.assets
             SET image_id=CASE id WHEN -9300011 THEN -9300021 ELSE -9300022 END
             WHERE id IN (-9300011,-9300012)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let base_state_id = resolve_storyboard_scene_state(
            &mut tx,
            project_id,
            script_id,
            Some("sc1"),
            None,
            Some("base"),
            None,
            Some("北墙木门完好，沙发与茶几位置固定"),
            &[-9_300_011],
        )
        .await
        .unwrap()
        .unwrap();
        tx.commit().await.unwrap();
        let initial_revisions: (i32, i32) = sqlx::query_as(
            "SELECT master.revision,state.revision
             FROM toonflow.scene_states state
             JOIN toonflow.scene_masters master ON master.id=state.scene_master_id
             WHERE state.id=$1",
        )
        .bind(base_state_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let repeated_base_state_id = resolve_storyboard_scene_state(
            &mut tx,
            project_id,
            script_id,
            Some("sc1"),
            None,
            Some("base"),
            None,
            Some("北墙木门完好，沙发与茶几位置固定"),
            &[-9_300_011],
        )
        .await
        .unwrap()
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(repeated_base_state_id, base_state_id);
        let repeated_revisions: (i32, i32) = sqlx::query_as(
            "SELECT master.revision,state.revision
             FROM toonflow.scene_states state
             JOIN toonflow.scene_masters master ON master.id=state.scene_master_id
             WHERE state.id=$1",
        )
        .bind(base_state_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(repeated_revisions, initial_revisions);

        let mut tx = pool.begin().await.unwrap();
        assert!(
            resolve_storyboard_scene_state(
                &mut tx,
                project_id,
                script_id,
                Some("sc1"),
                None,
                Some("base"),
                None,
                Some("北墙木门完好，沙发与茶几位置固定"),
                &[-9_300_012],
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let damaged_state_id = resolve_storyboard_scene_state(
            &mut tx,
            project_id,
            script_id,
            Some("sc1"),
            None,
            Some("door_broken"),
            Some("base"),
            Some("北墙木门断裂倒向室内，沙发与茶几位置不变"),
            &[-9_300_011, -9_300_012],
        )
        .await
        .unwrap()
        .unwrap();
        tx.commit().await.unwrap();
        let damaged_revision: i32 =
            sqlx::query_scalar("SELECT revision FROM toonflow.scene_states WHERE id=$1")
                .bind(damaged_state_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let state_reference_assets: Vec<i64> = sqlx::query_scalar(
            "SELECT asset_id FROM toonflow.scene_state_references
             WHERE scene_state_id=$1 ORDER BY sort_order",
        )
        .bind(damaged_state_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(state_reference_assets, vec![-9_300_012]);

        let mut tx = pool.begin().await.unwrap();
        resolve_storyboard_scene_state(
            &mut tx,
            project_id,
            script_id,
            Some("sc1"),
            None,
            Some("door_broken"),
            Some("base"),
            Some("北墙木门断裂倒向室内，沙发与茶几位置不变"),
            &[-9_300_011, -9_300_012],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let repeated_damaged_revision: i32 =
            sqlx::query_scalar("SELECT revision FROM toonflow.scene_states WHERE id=$1")
                .bind(damaged_state_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(repeated_damaged_revision, damaged_revision);

        let mut tx = pool.begin().await.unwrap();
        assert!(
            resolve_storyboard_scene_state(
                &mut tx,
                project_id,
                script_id,
                Some("sc1"),
                None,
                Some("door_broken"),
                Some("base"),
                Some("门忽然恢复完好"),
                &[-9_300_011],
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        assert!(
            resolve_storyboard_scene_state(
                &mut tx,
                project_id,
                script_id,
                Some("sc1"),
                None,
                Some("lights_out"),
                None,
                Some("灯光熄灭，木门仍保持断裂"),
                &[-9_300_011],
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();

        sqlx::query(
            "INSERT INTO toonflow.storyboards(
               id,script_id,project_id,scene_key,scene_state_id,index,create_time
             ) VALUES
               (-9300031,$1,$2,'sc1',$3,0,0),
               (-9300032,$1,$2,'sc1',$4,1,0)",
        )
        .bind(script_id)
        .bind(project_id)
        .bind(base_state_id)
        .bind(damaged_state_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO toonflow.storyboards(
               id,script_id,project_id,scene_key,scene_state_id,index,create_time
             ) VALUES(-9300033,$1,$2,'sc1',$3,2,0)",
        )
        .bind(script_id)
        .bind(project_id)
        .bind(base_state_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        assert!(tx.commit().await.is_err());

        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
