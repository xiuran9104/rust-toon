use crate::{ToonState, ai_client, shared::require, toonflow_image_edit_prompt};
use axum::{Json, extract::State};
use base64::Engine;
use image::{DynamicImage, ImageFormat, RgbaImage, imageops};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::task::JoinSet;

async fn normalize_image_references(references: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::with_capacity(references.len());
    for reference in references {
        if reference.starts_with("data:")
            || reference.starts_with("http://")
            || reference.starts_with("https://")
        {
            normalized.push(reference);
        } else if crate::toonflow_storage::is_asset_image_path(&reference) {
            normalized.push(crate::toonflow_storage::image_data_url(&reference).await?);
        } else {
            return Err(format!("不支持的参考图地址：{reference}"));
        }
    }
    Ok(normalized)
}

fn storyboard_image_size(quality: &str, ratio: &str) -> Result<String, String> {
    crate::toonflow_image_contract::ImageCanvas::for_quality(quality, ratio)
        .map(|canvas| canvas.size())
}

#[derive(Deserialize)]
pub struct FlowId {
    id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAssetUrlRequest {
    id: i64,
    url: String,
    flow_id: i64,
}

pub async fn update_asset_url(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UpdateAssetUrlRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.url.trim().is_empty() {
        return Err(AppError::bad_request("url 不能为空"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to update asset url"))?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM toonflow.assets WHERE id=$1)")
            .bind(request.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to load asset"))?;
    if !exists {
        return Err(AppError::not_found("资源未找到"));
    }
    let image_id = chrono::Utc::now().timestamp_micros();
    sqlx::query(
        "INSERT INTO toonflow.images(id,file_path,state,assets_id) VALUES($1,$2,'已完成',$3)",
    )
    .bind(image_id)
    .bind(request.url)
    .bind(request.id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to save asset image"))?;
    sqlx::query("UPDATE toonflow.assets SET flow_id=$2,image_id=$3 WHERE id=$1")
        .bind(request.id)
        .bind(request.flow_id)
        .bind(image_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to bind asset image"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit asset image"))?;
    Ok(Json(ApiResponse::with_message(
        json!({"imageId":image_id}),
        "更新资产图片成功",
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteDerivedAssetRequest {
    id: i64,
    project_id: i64,
}

pub async fn delete_derived_asset(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<DeleteDerivedAssetRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to delete derived asset"))?;
    let flow_id: Option<i64> = sqlx::query_scalar(
        "SELECT flow_id FROM toonflow.assets WHERE id=$1 AND project_id=$2 FOR UPDATE",
    )
    .bind(request.id)
    .bind(request.project_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load derived asset"))?
    .flatten();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM toonflow.assets WHERE id=$1 AND project_id=$2)",
    )
    .bind(request.id)
    .bind(request.project_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load derived asset"))?;
    if !exists {
        return Err(AppError::not_found("资源未找到"));
    }
    sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE asset_id=$1")
        .bind(request.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to unlink derived asset"))?;
    sqlx::query("DELETE FROM toonflow.assets WHERE id=$1 AND project_id=$2")
        .bind(request.id)
        .bind(request.project_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete derived asset"))?;
    if let Some(flow_id) = flow_id {
        sqlx::query("DELETE FROM toonflow.image_flows WHERE id=$1")
            .bind(flow_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to delete asset flow"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit derived asset deletion"))?;
    Ok(Json(ApiResponse::with_message((), "删除衍生资产成功")))
}
pub async fn get_flow(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<FlowId>,
) -> Result<Json<ApiResponse<Option<Value>>>, AppError> {
    require(&user, "toon:project:read")?;
    let row: Option<(i64, Value)> =
        sqlx::query_as("SELECT id,flow_data FROM toonflow.image_flows WHERE id=$1")
            .bind(req.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get image flow"))?;
    Ok(Json(ApiResponse::new(row.map(|r| {
        let mut value = r.1;
        value["id"] = json!(r.0);
        value
    }))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFlow {
    asset_id: Option<i64>,
    edges: Value,
    nodes: Value,
}
pub async fn save_flow(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<SaveFlow>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let id = chrono::Utc::now().timestamp_millis();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin image flow transaction"))?;
    sqlx::query("INSERT INTO toonflow.image_flows(id,flow_data)VALUES($1,$2)")
        .bind(id)
        .bind(json!({"edges":req.edges,"nodes":req.nodes}))
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to save image flow"))?;
    if let Some(asset_id) = req.asset_id {
        sqlx::query("UPDATE toonflow.assets SET flow_id=$2 WHERE id=$1")
            .bind(asset_id)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to bind image flow to asset"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit image flow"))?;
    Ok(Json(ApiResponse::new(json!({"id":id}))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFlow {
    flow_id: i64,
    edges: Value,
    nodes: Value,
}
pub async fn update_flow(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<UpdateFlow>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    sqlx::query("UPDATE toonflow.image_flows SET flow_data=$2 WHERE id=$1")
        .bind(req.flow_id)
        .bind(json!({"edges":req.edges,"nodes":req.nodes}))
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to update image flow"))?;
    Ok(Json(ApiResponse::new(())))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowImage {
    model: String,
    references: Option<Vec<String>>,
    quality: String,
    ratio: String,
    prompt: String,
    project_id: i64,
    storyboard_id: Option<i64>,
    #[serde(default)]
    target_type: String,
}
pub async fn generate_flow_image(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<FlowImage>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let ratio = match req.target_type.as_str() {
        "role" | "costume" | "tool" => {
            crate::toonflow_asset_prompt::asset_image_ratio(&req.target_type, &req.prompt)
        }
        _ => req.ratio.as_str(),
    };
    let size = storyboard_image_size(&req.quality, ratio).map_err(AppError::bad_request)?;
    let original_references = req.references.clone().unwrap_or_default();
    let scene_plan = if req.target_type == "storyboard" {
        let storyboard_id = req
            .storyboard_id
            .ok_or_else(|| AppError::bad_request("分镜图片编辑必须提供 storyboardId"))?;
        let script_id: i64 = sqlx::query_scalar(
            "SELECT script_id FROM toonflow.storyboards WHERE id=$1 AND project_id=$2",
        )
        .bind(storyboard_id)
        .bind(req.project_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to load storyboard image flow context"))?
        .ok_or_else(|| AppError::not_found("storyboard not found"))?;
        let asset_references = crate::toonflow_asset_context::load_storyboard_asset_references(
            &state.pool,
            req.project_id,
            script_id,
            storyboard_id,
        )
        .await?;
        Some(
            crate::toonflow_storyboard_references::build_storyboard_reference_plan(
                &state.pool,
                req.project_id,
                script_id,
                storyboard_id,
                asset_references,
                original_references.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut reference_paths = scene_plan
        .as_ref()
        .map(|plan| plan.paths.clone())
        .unwrap_or_default();
    if scene_plan.is_none() {
        reference_paths = original_references;
    }
    let references = normalize_image_references(reference_paths)
        .await
        .map_err(AppError::bad_request)?;
    let prompt = toonflow_image_edit_prompt::build(
        toonflow_image_edit_prompt::ImageEditTarget::parse(&req.target_type),
        &req.prompt,
        ratio,
        references.len(),
    );
    let prompt = scene_plan
        .as_ref()
        .map(|plan| plan.apply_to_prompt(prompt.clone()))
        .unwrap_or(prompt);
    let url = ai_client::image_with_references_for_project(
        &state.pool,
        Some(req.project_id),
        &req.model,
        &prompt,
        &size,
        references,
        req.target_type == "role",
    )
    .await
    .map_err(AppError::bad_request)?;
    let now = chrono::Utc::now().timestamp_millis();
    let task_input = json!({
        "projectId": req.project_id,
        "storyboardId": req.storyboard_id,
        "model": req.model,
        "quality": req.quality,
        "ratio": ratio,
        "size": size,
        "prompt": req.prompt,
        "references": req.references,
        "targetType": req.target_type,
    });
    let _ = sqlx::query("INSERT INTO toonflow.tasks(id,project_id,task_class,related_objects,model,description,state,start_time,input,progress_current,progress_total) VALUES($1,$2,'工作流图片生成',$3,$4,'工作流图片生成','success',$1,$5,1,1)")
        .bind(now)
        .bind(req.project_id)
        .bind(json!({"prompt":req.prompt,"url":&url}).to_string())
        .bind(req.model)
        .bind(task_input)
        .execute(&state.pool)
        .await;
    Ok(Json(ApiResponse::new(json!({
        "url":url,
        "sceneStateId":scene_plan.as_ref().and_then(|plan|plan.scene_state_id),
        "sceneGenerationContext":scene_plan.as_ref().map(|plan|plan.generation_context.clone()),
    }))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectId {
    project_id: i64,
}
pub async fn default_model(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ProjectId>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let row: Option<(Option<i64>, String)> =
        sqlx::query_as("SELECT image_model,image_quality FROM toonflow.projects WHERE id=$1")
            .bind(req.project_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get image model"))?;
    Ok(Json(ApiResponse::new(
        row.map(|r| json!({"imageModel":r.0,"imageQuality":r.1}))
            .unwrap_or(Value::Null),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardGenerate {
    storyboard_ids: Vec<i64>,
    project_id: i64,
    script_id: i64,
    concurrent_count: Option<usize>,
    compulsory: Option<bool>,
}

#[derive(Clone)]
pub(crate) struct StoryboardImageJob {
    pub(crate) id: i64,
    prompt: String,
}

#[derive(Debug, Default)]
pub(crate) struct StoryboardGenerationSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

pub(crate) async fn prepare_storyboard_generation(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    storyboard_ids: &[i64],
    compulsory: bool,
) -> Result<(Vec<Value>, Vec<StoryboardImageJob>), AppError> {
    if storyboard_ids.is_empty() {
        return Err(AppError::bad_request("storyboardIds不能为空"));
    }
    crate::toonflow_storyboard_asset_validation::reject_base_roles_for_storyboards(
        pool,
        storyboard_ids,
    )
    .await?;
    let rows=sqlx::query_as::<_,(i64,String,i32)>("SELECT id,prompt,should_generate_image FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND id=ANY($3)").bind(project_id).bind(script_id).bind(storyboard_ids).fetch_all(pool).await.map_err(|_|AppError::internal("failed to get storyboards"))?;
    if rows.is_empty() {
        return Err(AppError::not_found("未查到分镜数据"));
    }
    let jobs = rows
        .iter()
        .filter(|row| compulsory || row.2 != 0)
        .map(|row| StoryboardImageJob {
            id: row.0,
            prompt: row.1.clone(),
        })
        .collect::<Vec<_>>();
    let ids = jobs.iter().map(|job| job.id).collect::<Vec<_>>();
    sqlx::query("UPDATE toonflow.storyboards SET state='生成中',reason=NULL WHERE id=ANY($1)")
        .bind(&ids)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to start storyboards"))?;
    let response=rows.iter().map(|row|json!({"id":row.0,"prompt":row.1,"state":if ids.contains(&row.0){"生成中"}else{"未生成"},"shouldGenerateImage":row.2})).collect();
    Ok((response, jobs))
}

/// 对齐方案 P2 穿越物件连续性：合并多条分镜声明的随身物件。同名物件
/// 以最后一次声明为准（年代/描述可更新），顺序保持首次出现次序。
pub(crate) fn merge_carried_objects(declared_per_board: &[Value]) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for board_objects in declared_per_board {
        let Some(items) = board_objects.as_array() else {
            continue;
        };
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            match merged
                .iter()
                .position(|existing| existing.get("name").and_then(Value::as_str) == Some(name))
            {
                Some(position) => merged[position] = item.clone(),
                None => merged.push(item.clone()),
            }
        }
    }
    merged
}

fn carried_objects_summary(objects: &[Value]) -> String {
    objects
        .iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?;
            match item.get("era").and_then(Value::as_str).filter(|era| !era.is_empty()) {
                Some(era) => Some(format!("{name}（{era}）")),
                None => Some(name.to_string()),
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

async fn generate_storyboard_job(
    pool: sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    model: String,
    quality: String,
    ratio: String,
    job: StoryboardImageJob,
) -> bool {
    let asset_references = match crate::toonflow_asset_context::load_storyboard_asset_references(
        &pool, project_id, script_id, job.id,
    )
    .await
    {
        Ok(references) => references,
        Err(error) => {
            let reason = format!("分镜参考资产查询失败：{error}");
            let _ = sqlx::query(
                "UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1",
            )
            .bind(job.id)
            .bind(reason)
            .execute(&pool)
            .await;
            return false;
        }
    };
    let prompt_assets = asset_references
        .iter()
        .map(|reference| reference.prompt_asset.clone())
        .collect::<Vec<_>>();
    if let Err(reason) = crate::toonflow_storyboard_prompt_validation::validate_storyboard_prompt(
        &job.prompt,
        &prompt_assets,
    ) {
        let _ =
            sqlx::query("UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1")
                .bind(job.id)
                .bind(reason)
                .execute(&pool)
                .await;
        return false;
    }
    let reference_plan =
        match crate::toonflow_storyboard_references::build_storyboard_reference_plan(
            &pool,
            project_id,
            script_id,
            job.id,
            asset_references,
            Vec::new(),
        )
        .await
        {
            Ok(plan) => plan,
            Err(error) => {
                let reason = error.to_string();
                let _ = sqlx::query(
                    "UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1",
                )
                .bind(job.id)
                .bind(reason)
                .execute(&pool)
                .await;
                return false;
            }
        };
    let references = match normalize_image_references(reference_plan.paths.clone()).await {
        Ok(references) => references,
        Err(reason) => {
            let reason = format!("分镜参考资产读取失败：{reason}");
            let _ = sqlx::query(
                "UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1",
            )
            .bind(job.id)
            .bind(reason)
            .execute(&pool)
            .await;
            return false;
        }
    };
    let mut generation_prompt = reference_plan.apply_to_prompt(
        crate::toonflow_asset_prompt::storyboard_generation_prompt(&job.prompt),
    );
    // P1 视觉质检：与资产图片同构的闭环（初始 + 最多 2 次定向重试，
    // 仍失败保留最后一版）。质检不可用时不阻断生成。
    let framing: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT coalesce(video_desc,''),coalesce(shot_size,''),coalesce(camera_move,''),coalesce(time_of_day,'') FROM toonflow.storyboards WHERE id=$1",
    )
    // （随身物件另行查询）
    .bind(job.id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();
    let (framing_desc, shot_size, camera_move, time_of_day) = framing
        .unwrap_or_else(|| (String::new(), String::new(), String::new(), String::new()));
    // 对齐方案 P1 场景日夜状态：昼夜影响光线与氛围，注入生成提示词。
    let mut generation_prompt = if !time_of_day.trim().is_empty() {
        format!("{generation_prompt}\n时间氛围：{time_of_day}。光线、色温与阴影必须与该时间一致，不得出现矛盾光源。")
    } else {
        generation_prompt
    };
    let description = if framing_desc.trim().is_empty() {
        job.prompt.clone()
    } else {
        framing_desc
    };
    // 穿越物件连续性：本镜声明 + 同轨道更早分镜的声明合并继承。
    let track_carried: Vec<Value> = sqlx::query_scalar(
        r#"SELECT s2.carried_objects FROM toonflow.storyboards s2
           JOIN toonflow.storyboards cur ON cur.id=$4
           WHERE s2.project_id=$1 AND s2.script_id=$2
             AND s2.track_id IS NOT NULL AND s2.track_id=cur.track_id AND s2.id<>$4
             AND s2.carried_objects<>'[]'::jsonb
           ORDER BY s2.index NULLS LAST,s2.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(job.id)
    .bind(job.id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    let own_carried = sqlx::query_scalar::<_, Value>(
        "SELECT carried_objects FROM toonflow.storyboards WHERE id=$1",
    )
    .bind(job.id)
    .fetch_one(&pool)
    .await
    .unwrap_or_else(|_| json!([]));
    let merged_carried = merge_carried_objects(&track_carried);
    let carried_summary = carried_objects_summary(&merged_carried);
    if !carried_summary.is_empty() {
        generation_prompt = format!(
            "{generation_prompt}\n随身物件（连续性要求，必须出现在画面中且年代特征正确）：{carried_summary}。不得凭空消失、不得更换年代样式。"
        );
    }
    let asset_summary = prompt_assets
        .iter()
        .map(|asset| (asset.name.as_str(), asset.kind.as_str()))
        .collect::<Vec<_>>();
    let expectations = crate::toonflow_visual_qc::storyboard_expectations(
        &description,
        &shot_size,
        &camera_move,
        &time_of_day,
        &carried_summary,
        &asset_summary,
    );
    let mut qc_attempts: Vec<Value> = Vec::new();
    let mut submitted_prompt = generation_prompt.clone();
    let mut final_path: Option<String> = None;
    for attempt in 0..=crate::toonflow_visual_qc::MAX_VISUAL_QC_RETRIES {
        let generated = match async {
            let size = storyboard_image_size(&quality, &ratio)?;
            ai_client::image_with_references_for_project(
                &pool,
                Some(project_id),
                &model,
                &submitted_prompt,
                &size,
                references.clone(),
                false,
            )
            .await
        }
        .await
        {
            Ok(url) => url,
            Err(reason) => {
                let _ = sqlx::query(
                    "UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1",
                )
                .bind(job.id)
                .bind(reason)
                .execute(&pool)
                .await;
                return false;
            }
        };
        let object_name = format!("storyboard-{}-{}", job.id, uuid::Uuid::new_v4());
        let file_path =
            match crate::toonflow_storage::persist_remote_project_image(
                &generated,
                project_id,
                "storyboards",
                &object_name,
            )
            .await
            {
                Ok(file_path) => file_path,
                Err(reason) => {
                    let reason = format!("分镜图片持久化失败：{reason}");
                    let _ = sqlx::query(
                        "UPDATE toonflow.storyboards SET state='生成失败',reason=$2 WHERE id=$1",
                    )
                    .bind(job.id)
                    .bind(reason)
                    .execute(&pool)
                    .await;
                    return false;
                }
            };
        match crate::toonflow_visual_qc::evaluate_image(&pool, project_id, &file_path, &expectations)
            .await
        {
            Ok(report) => {
                let attempt_entry = if report.passed {
                    json!({"attempt": attempt + 1, "passed": true, "summary": report.summary})
                } else {
                    json!({
                        "attempt": attempt + 1,
                        "passed": false,
                        "summary": report.summary,
                        "failures": report.failures,
                        "imagePath": file_path,
                    })
                };
                qc_attempts.push(attempt_entry);
                if report.passed || attempt >= crate::toonflow_visual_qc::MAX_VISUAL_QC_RETRIES {
                    final_path = Some(file_path);
                    break;
                }
                if let Some(repair) = crate::toonflow_visual_qc::repair_instructions(&report) {
                    submitted_prompt = format!("{generation_prompt}\n{repair}");
                }
            }
            Err(_) => {
                final_path = Some(file_path);
                break;
            }
        }
    }
    let Some(file_path) = final_path else {
        return false;
    };
    let mut generation_context = reference_plan.generation_context;
    if !qc_attempts.is_empty() {
        generation_context["visualQcHistory"] = json!(qc_attempts);
    }
    sqlx::query(
        "UPDATE toonflow.storyboards SET file_path=$2,state='已完成',reason=NULL,generated_scene_state_id=$3,scene_generation_context=$4 WHERE id=$1 AND state='生成中'",
    )
    .bind(job.id)
    .bind(file_path)
    .bind(reference_plan.scene_state_id)
    .bind(generation_context)
    .execute(&pool)
    .await
    .is_ok_and(|result| result.rows_affected() == 1)
}

pub(crate) async fn run_storyboard_generation(
    pool: sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    jobs: Vec<StoryboardImageJob>,
    concurrent_count: usize,
    workflow_node_run_id: Option<i64>,
) -> StoryboardGenerationSummary {
    let mut summary = StoryboardGenerationSummary {
        total: jobs.len(),
        ..Default::default()
    };
    let setting: Option<(Option<i64>, String, String)> = sqlx::query_as(
        "SELECT image_model,image_quality,video_ratio FROM toonflow.projects WHERE id=$1",
    )
    .bind(project_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();
    let Some((Some(model), quality, ratio)) = setting else {
        let ids = jobs.iter().map(|job| job.id).collect::<Vec<_>>();
        let _ = sqlx::query(
            "UPDATE toonflow.storyboards SET state='生成失败',reason='项目未配置图片模型'
             WHERE id=ANY($1)",
        )
        .bind(&ids)
        .execute(&pool)
        .await;
        summary.failed = summary.total;
        if let Some(node_run_id) = workflow_node_run_id {
            let _ = sqlx::query(
                "UPDATE toonflow.workflow_node_runs SET progress_current=$2 WHERE id=$1",
            )
            .bind(node_run_id)
            .bind(summary.total as i32)
            .execute(&pool)
            .await;
        }
        return summary;
    };
    let concurrency = concurrent_count.clamp(1, 10);
    let mut jobs = jobs.into_iter();
    let mut running = JoinSet::new();
    for _ in 0..concurrency {
        let Some(job) = jobs.next() else { break };
        running.spawn(generate_storyboard_job(
            pool.clone(),
            project_id,
            script_id,
            model.to_string(),
            quality.clone(),
            ratio.clone(),
            job,
        ));
    }
    while let Some(result) = running.join_next().await {
        match result {
            Ok(true) => summary.succeeded += 1,
            Ok(false) | Err(_) => summary.failed += 1,
        }
        if let Some(node_run_id) = workflow_node_run_id {
            let _ = sqlx::query(
                "UPDATE toonflow.workflow_node_runs
                 SET progress_current=LEAST(progress_current+1,progress_total) WHERE id=$1",
            )
            .bind(node_run_id)
            .execute(&pool)
            .await;
        }
        if let Some(job) = jobs.next() {
            running.spawn(generate_storyboard_job(
                pool.clone(),
                project_id,
                script_id,
                model.to_string(),
                quality.clone(),
                ratio.clone(),
                job,
            ));
        }
    }
    summary
}

pub(crate) async fn load_storyboard_generation_results(
    pool: &sqlx::PgPool,
    storyboard_ids: &[i64],
) -> Vec<Value> {
    sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, String, i32)>(
        "SELECT id,coalesce(state,''),reason,file_path,prompt,should_generate_image
         FROM toonflow.storyboards WHERE id=ANY($1) ORDER BY index,id",
    )
    .bind(storyboard_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|row| {
        json!({
            "id": row.0,
            "state": row.1,
            "reason": row.2,
            "filePath": row.3,
            "src": row.3,
            "prompt": row.4,
            "shouldGenerateImage": row.5,
        })
    })
    .collect()
}

pub async fn schedule_storyboard_generation(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    storyboard_ids: &[i64],
    concurrent_count: usize,
    compulsory: bool,
) -> Result<Vec<Value>, AppError> {
    let (response, jobs) =
        prepare_storyboard_generation(pool, project_id, script_id, storyboard_ids, compulsory)
            .await?;
    let pool = pool.clone();
    tokio::spawn(async move {
        let _ =
            run_storyboard_generation(pool, project_id, script_id, jobs, concurrent_count, None)
                .await;
    });
    Ok(response)
}

#[cfg(test)]
mod prompt_tests {
    use super::storyboard_image_size;

    #[test]
    fn converts_project_ratio_to_provider_dimensions() {
        assert_eq!(storyboard_image_size("2K", "16:9").unwrap(), "2560x1440");
        assert_eq!(storyboard_image_size("2K", "9:16").unwrap(), "1440x2560");
        assert_eq!(storyboard_image_size("2K", "1:1").unwrap(), "2560x2560");
    }
}

pub async fn generate_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<StoryboardGenerate>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:update")?;
    let response = schedule_storyboard_generation(
        &state.pool,
        req.project_id,
        req.script_id,
        &req.storyboard_ids,
        req.concurrent_count.unwrap_or(2),
        req.compulsory.unwrap_or(false),
    )
    .await?;
    Ok(Json(ApiResponse::new(response)))
}

#[derive(Deserialize)]
pub struct Ids {
    ids: Vec<i64>,
}
pub async fn poll_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Ids>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:scene:read")?;
    let rows=sqlx::query_as::<_,(i64,String,Option<String>,Option<String>,String)>("SELECT id,coalesce(state,''),reason,file_path,prompt FROM toonflow.storyboards WHERE id=ANY($1) AND state<>'生成中'").bind(req.ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to poll storyboards"))?;
    Ok(Json(ApiResponse::new(rows.into_iter().map(|r|json!({"id":r.0,"state":r.1,"reason":r.2,"filePath":r.3,"src":r.3,"prompt":r.4})).collect())))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardUrl {
    id: i64,
    url: String,
    flow_id: i64,
    generated_scene_state_id: Option<i64>,
    scene_generation_context: Option<Value>,
}
pub async fn update_storyboard_url(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<StoryboardUrl>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let scene_generation_context = req.scene_generation_context.unwrap_or_else(|| json!({}));
    if !scene_generation_context.is_object() {
        return Err(AppError::bad_request(
            "sceneGenerationContext 必须是 JSON 对象",
        ));
    }
    sqlx::query("UPDATE toonflow.storyboards SET file_path=$2,flow_id=$3,state='已完成',should_generate_image=$4,generated_scene_state_id=$5,scene_generation_context=$6 WHERE id=$1")
        .bind(req.id)
        .bind(&req.url)
        .bind(req.flow_id)
        .bind(if req.url.is_empty(){0}else{1})
        .bind(req.generated_scene_state_id)
        .bind(scene_generation_context)
        .execute(&state.pool)
        .await
        .map_err(|_|AppError::internal("failed to update storyboard image"))?;
    Ok(Json(ApiResponse::new(json!({"message":"更新分镜成功"}))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteStoryboards {
    pub(crate) ids: Vec<i64>,
    pub(crate) project_id: i64,
}
pub async fn delete_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<DeleteStoryboards>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:delete")?;
    if req.ids.is_empty() {
        return Err(AppError::bad_request("请先选择分镜"));
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin transaction"))?;
    let rows: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id,track_id,flow_id FROM toonflow.storyboards WHERE id=ANY($1) AND project_id=$2",
    )
    .bind(&req.ids)
    .bind(req.project_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to load storyboards"))?;
    if rows.is_empty() {
        return Err(AppError::not_found("当前选择分镜不存在"));
    }
    let storyboard_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
    let track_ids = rows
        .iter()
        .filter_map(|row| row.1)
        .collect::<std::collections::BTreeSet<_>>();
    let flow_ids = rows.iter().filter_map(|row| row.2).collect::<Vec<_>>();
    sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=ANY($1)")
        .bind(&storyboard_ids)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete storyboard assets"))?;
    sqlx::query("DELETE FROM toonflow.storyboards WHERE id=ANY($1) AND project_id=$2")
        .bind(&storyboard_ids)
        .bind(req.project_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete storyboards"))?;
    if !flow_ids.is_empty() {
        sqlx::query("DELETE FROM toonflow.image_flows WHERE id=ANY($1)")
            .bind(&flow_ids)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to delete storyboard image flows"))?;
    }
    for track_id in track_ids {
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
            sqlx::query("UPDATE toonflow.video_tracks SET duration=(SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE track_id=$1) WHERE id=$1")
                .bind(track_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to update storyboard track"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit transaction"))?;
    Ok(Json(ApiResponse::new(json!({"message":"视频删除成功"}))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    #[serde(alias = "ids")]
    storyboard_ids: Vec<i64>,
}

async fn preview_png(pool: &sqlx::PgPool, ids: &[i64]) -> Result<Option<Vec<u8>>, AppError> {
    let rows = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT id,file_path FROM toonflow.storyboards WHERE id=ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to get storyboard images"))?;
    let paths = rows
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    let mut images = Vec::new();
    let mut original_images = Vec::new();
    for id in ids {
        let Some(path) = paths.get(id).and_then(Clone::clone) else {
            continue;
        };
        let bytes = if let Some(encoded) = path.split_once(";base64,").map(|v| v.1) {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| AppError::bad_request("invalid image data"))?
        } else {
            reqwest::get(path)
                .await
                .map_err(|_| AppError::bad_request("failed to fetch storyboard image"))?
                .bytes()
                .await
                .map_err(|_| AppError::bad_request("failed to read storyboard image"))?
                .to_vec()
        };
        if looks_like_image(&bytes) {
            original_images.push(bytes.clone());
        }
        if let Ok(image) = image::load_from_memory(&bytes) {
            images.push(image);
        }
    }
    if images.is_empty() {
        if ids.len() == 1 {
            return Ok(original_images.into_iter().next());
        }
        return Ok(None);
    }
    let thumb = 256u32;
    let cols = images.len().min(5) as u32;
    let row_count = images.len().div_ceil(cols as usize) as u32;
    let mut canvas = RgbaImage::from_pixel(
        cols * thumb,
        row_count * thumb,
        image::Rgba([255, 255, 255, 255]),
    );
    for (index, image) in images.into_iter().enumerate() {
        let resized = image.resize(thumb, thumb, imageops::FilterType::Lanczos3);
        let x = index as u32 % cols * thumb + (thumb - resized.width()) / 2;
        let y = index as u32 / cols * thumb + (thumb - resized.height()) / 2;
        imageops::overlay(&mut canvas, &resized, x.into(), y.into());
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(canvas)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|_| AppError::internal("failed to encode preview"))?;
    Ok(Some(cursor.into_inner()))
}

fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
}

pub async fn preview_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<PreviewRequest>,
) -> Result<Json<ApiResponse<Option<String>>>, AppError> {
    require(&user, "toon:scene:read")?;
    let png = preview_png(&state.pool, &req.storyboard_ids).await?;
    Ok(Json(ApiResponse::new(png.map(|data| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(data)
        )
    }))))
}

pub async fn download_storyboards(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<PreviewRequest>,
) -> Result<axum::response::Response, AppError> {
    require(&user, "toon:scene:read")?;
    let png = preview_png(&state.pool, &req.storyboard_ids)
        .await?
        .ok_or_else(|| AppError::not_found("没有可下载的分镜图片"))?;
    axum::response::Response::builder()
        .header("content-type", "image/png")
        .header(
            "content-disposition",
            "attachment; filename=storyboard-preview.png",
        )
        .body(axum::body::Body::from(png))
        .map_err(|_| AppError::internal("failed to build download"))
}

#[cfg(test)]
mod tests {
    use super::{carried_objects_summary, merge_carried_objects, normalize_image_references};

    #[tokio::test]
    async fn keeps_provider_usable_reference_urls() {
        let references = vec![
            "https://cdn.example.com/reference.jpg".to_string(),
            "data:image/png;base64,iVBORw0KGgo=".to_string(),
        ];
        assert_eq!(
            normalize_image_references(references.clone())
                .await
                .unwrap(),
            references
        );
    }

    #[tokio::test]
    async fn rejects_unknown_relative_reference_paths() {
        let error = normalize_image_references(vec!["/unknown/image.jpg".to_string()])
            .await
            .unwrap_err();
        assert!(error.contains("不支持的参考图地址"));
    }

    #[test]
    fn merges_by_name_with_latest_declaration_winning() {
        use serde_json::json;
        let merged = merge_carried_objects(&[
            json!([{"name":"手机","era":"现代"}]),
            json!([{"name":"背包"}]),
            json!([{"name":"手机","era":"现代","description":"碎屏贴膜"}]),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["name"], json!("手机"));
        assert_eq!(merged[0]["description"], json!("碎屏贴膜"));
        assert_eq!(merged[1]["name"], json!("背包"));
        let summary = carried_objects_summary(&merged);
        assert!(summary.contains("手机"));
        assert!(summary.contains("背包"));
    }

    #[test]
    fn tolerates_empty_and_malformed_carried_declarations() {
        use serde_json::json;
        assert!(merge_carried_objects(&[]).is_empty());
        assert!(merge_carried_objects(&[json!([])]).is_empty());
        assert!(merge_carried_objects(&[json!(null), json!([{"era":"现代"}])]).is_empty());
        assert_eq!(carried_objects_summary(&[]), "");
    }

}
