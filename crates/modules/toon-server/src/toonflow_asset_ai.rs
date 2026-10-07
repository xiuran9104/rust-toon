use crate::{
    ToonState, ai_client,
    shared::require,
    toonflow_asset_prompt, toonflow_prompt_store,
    toonflow_storage::{
        delete_asset_file, image_data_url, persist_remote_image, record_cleanup_failure,
    },
};
use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishItem {
    assets_id: i64,
    #[serde(rename = "type")]
    type_: String,
    name: String,
    describe: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishRequest {
    assets_id: i64,
    project_id: i64,
    #[serde(rename = "type")]
    type_: String,
    name: String,
    describe: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    items: Vec<PolishItem>,
    project_id: i64,
    concurrent_count: Option<usize>,
    other_text_prompt: String,
}
fn manual_key(kind: &str, derivative: bool) -> Option<(&'static str, &'static str)> {
    match kind {
        "role" => Some((
            "角色",
            if derivative {
                "art_character_derivative"
            } else {
                "art_character"
            },
        )),
        "scene" => Some((
            "场景",
            if derivative {
                "art_scene_derivative"
            } else {
                "art_scene"
            },
        )),
        "tool" => Some((
            "道具",
            if derivative {
                "art_prop_derivative"
            } else {
                "art_prop"
            },
        )),
        "costume" => Some((
            "服装",
            if derivative {
                "art_character_derivative"
            } else {
                "art_character"
            },
        )),
        _ => None,
    }
}

async fn run(
    pool: &sqlx::PgPool,
    project_id: i64,
    item: PolishItem,
    extra: &str,
) -> Result<String, String> {
    sqlx::query(
        "UPDATE toonflow.assets SET prompt_state='生成中',prompt_error_reason=NULL WHERE id=$1",
    )
    .bind(item.assets_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    let context:Option<(String,String,String,String,Option<i64>)>=sqlx::query_as("SELECT p.art_style,p.project_type,p.type,p.intro,a.parent_asset_id FROM toonflow.assets a JOIN toonflow.projects p ON p.id=a.project_id WHERE a.id=$1 AND a.project_id=$2").bind(item.assets_id).bind(project_id).fetch_optional(pool).await.map_err(|e|e.to_string())?;
    let (manual_path, project_template, project_type, project_intro, parent) =
        context.ok_or_else(|| "资产不存在".to_string())?;
    let project_context = toonflow_asset_prompt::project_world_context(
        &project_template,
        &project_type,
        &project_intro,
    );
    let (label, key) =
        manual_key(&item.type_, parent.is_some()).ok_or_else(|| "不支持的类型".to_string())?;

    // Costume assets describe a reusable garment, not the person wearing it.
    // Sending costume descriptions through the character manual can introduce
    // age, body and portrait terms that both conflict with the no-model layout
    // and unnecessarily trigger image-provider text moderation.
    if item.type_ == "costume" {
        let prompt = standalone_costume_prompt(&item.describe);
        sqlx::query("UPDATE toonflow.assets SET prompt=$2,prompt_state='已完成',prompt_error_reason=NULL WHERE id=$1")
            .bind(item.assets_id)
            .bind(&prompt)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(prompt);
    }
    let data: Option<(Value,)> = sqlx::query_as(
        "SELECT data FROM toonflow.creative_manuals WHERE kind='visual' AND path=$1",
    )
    .bind(manual_path)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let system = data
        .and_then(|r| {
            r.0.as_array().and_then(|items| {
                items
                    .iter()
                    .find(|v| v.get("value").and_then(Value::as_str) == Some(key))
                    .and_then(|v| v.get("data"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "视觉手册未定义".to_string())?;
    let prompt = ai_client::project_text(
        pool,
        "universalAi",
        project_id,
        &toonflow_asset_prompt::polish_system_prompt(
            &system,
            extra,
            &project_context,
            &item.type_,
            parent.is_some(),
        ),
        &toonflow_asset_prompt::polish_user_prompt(label, &item.name, &item.describe),
    )
    .await?;
    let prompt = toonflow_asset_prompt::asset_visual_description(&item.type_, &prompt);
    if prompt.is_empty() {
        return Err("AI 润色未生成可用的资产提示词".to_string());
    }
    sqlx::query("UPDATE toonflow.assets SET prompt=$2,prompt_state='已完成',prompt_error_reason=NULL WHERE id=$1").bind(item.assets_id).bind(&prompt).execute(pool).await.map_err(|e|e.to_string())?;
    Ok(prompt)
}

fn standalone_costume_prompt(description: &str) -> String {
    let neutral_description = description
        .replace("按摩服务", "理疗服务")
        .replace("按摩技师", "理疗技师")
        .replace("按摩师", "理疗师")
        .replace("合体收腰", "修身利落");
    format!(
        "独立服装产品设定，深色中性背景，服装平铺或悬挂展示，无人物、无人台、无人体部位。服装描述：{}",
        neutral_description.trim()
    )
}
async fn mark_failed(pool: &sqlx::PgPool, id: i64, reason: &str) {
    let _ = sqlx::query(
        "UPDATE toonflow.assets SET prompt_state='失败',prompt_error_reason=$2 WHERE id=$1",
    )
    .bind(id)
    .bind(reason)
    .execute(pool)
    .await;
}

pub(crate) async fn polish_extracted_assets(
    pool: &sqlx::PgPool,
    project_id: i64,
    asset_ids: &[i64],
    concurrent_count: usize,
) -> Result<(), String> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, (i64, String, String, String)>(
        r#"SELECT id,type,name,coalesce(description,'')
           FROM toonflow.assets
           WHERE project_id=$1 AND id=ANY($2) AND type=ANY($3)
             AND coalesce(prompt,'')=''"#,
    )
    .bind(project_id)
    .bind(asset_ids)
    .bind(vec!["role", "scene", "tool", "costume"])
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrent_count.clamp(1, 10)));
    let mut jobs = tokio::task::JoinSet::new();
    for (assets_id, type_, name, describe) in rows {
        let pool = pool.clone();
        let permit = semaphore.clone().acquire_owned().await;
        jobs.spawn(async move {
            let Ok(_permit) = permit else { return };
            let item = PolishItem {
                assets_id,
                type_,
                name,
                describe,
            };
            if let Err(reason) = run(&pool, project_id, item, "").await {
                mark_failed(&pool, assets_id, &reason).await;
            }
        });
    }
    while jobs.join_next().await.is_some() {}
    Ok(())
}

pub async fn polish(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<PolishRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let item = PolishItem {
        assets_id: req.assets_id,
        type_: req.type_,
        name: req.name,
        describe: req.describe,
    };
    match run(&state.pool, req.project_id, item.clone(), "").await {
        Ok(prompt) => Ok(Json(ApiResponse::new(
            json!({"prompt":prompt,"assetsId":item.assets_id}),
        ))),
        Err(reason) => {
            mark_failed(&state.pool, item.assets_id, &reason).await;
            Err(AppError::bad_request(reason))
        }
    }
}
pub async fn batch_polish(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<BatchRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let total = req.items.len();
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(
            req.concurrent_count.unwrap_or(1).clamp(1, 20),
        ));
        let mut jobs = Vec::new();
        for item in req.items {
            let pool = pool.clone();
            let extra = req.other_text_prompt.clone();
            let permit = sem.clone().acquire_owned().await;
            let project_id = req.project_id;
            jobs.push(tokio::spawn(async move {
                if permit.is_ok()
                    && let Err(reason) = run(&pool, project_id, item.clone(), &extra).await
                {
                    mark_failed(&pool, item.assets_id, &reason).await;
                }
            }));
        }
        for job in jobs {
            let _ = job.await;
        }
    });
    Ok(Json(ApiResponse::new(json!({"total":total}))))
}
#[derive(Deserialize)]
pub struct Ids {
    ids: Vec<i64>,
}
pub async fn poll_prompts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Ids>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows=sqlx::query_as::<_,(i64,String,String,Option<String>)>("SELECT id,prompt,coalesce(prompt_state,''),prompt_error_reason FROM toonflow.assets WHERE id=ANY($1) AND coalesce(prompt_state,'')<>'生成中'").bind(req.ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to poll asset prompts"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"prompt":r.1,"promptState":r.2,"promptErrorReason":r.3}))
            .collect(),
    )))
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageItem {
    pub(crate) id: i64,
    #[serde(rename = "type")]
    pub(crate) type_: String,
    #[serde(rename = "name")]
    pub(crate) _name: String,
    pub(crate) prompt: String,
    pub(crate) base64: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum ModelId {
    Number(i64),
    Text(String),
}

impl ModelId {
    fn as_configured(&self) -> String {
        match self {
            Self::Number(id) => id.to_string(),
            Self::Text(id) => id.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateImageRequest {
    project_id: i64,
    model: ModelId,
    resolution: String,
    id: i64,
    #[serde(rename = "type")]
    type_: String,
    name: String,
    prompt: String,
    base64: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImageRequest {
    project_id: i64,
    model: ModelId,
    resolution: String,
    concurrent_count: Option<usize>,
    items: Vec<ImageItem>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryImageRequest {
    project_id: i64,
    ids: Vec<i64>,
    concurrent_count: Option<usize>,
}
pub(crate) async fn new_image(
    pool: &sqlx::PgPool,
    project_id: i64,
    item: &ImageItem,
    model: &str,
    resolution: &str,
    offset: i64,
) -> Result<ScheduledImage, AppError> {
    let input_hash = generation_input_hash(pool, item, project_id, model, resolution).await?;
    if let Some(existing) = reusable_image(pool, item.id, &item.prompt, &input_hash).await? {
        return Ok(existing);
    }
    // Image editing/upload already uses microseconds. Mixing millisecond IDs
    // made ORDER BY id DESC permanently prefer an older edited image.
    static SEQUENCE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let now = chrono::Utc::now().timestamp_micros() + offset;
    let previous = SEQUENCE
        .fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |previous| Some(now.max(previous + 1)),
        )
        .expect("image ID update always succeeds");
    let id = now.max(previous + 1);
    let size = crate::toonflow_image_contract::ImageCanvas::for_quality(
        resolution,
        toonflow_asset_prompt::asset_image_ratio(&item.type_, &item.prompt),
    )
    .map_err(AppError::bad_request)?
    .size();
    let inserted = sqlx::query("INSERT INTO toonflow.images(id,type,assets_id,model,resolution,state,input_hash) VALUES($1,$2,$3,$4,$5,'生成中',$6) ON CONFLICT DO NOTHING")
        .bind(id).bind(&item.type_).bind(item.id).bind(model).bind(size).bind(&input_hash)
        .execute(pool).await.map_err(|_|AppError::internal("failed to create image"))?;
    if inserted.rows_affected() == 0 {
        return reusable_image(pool, item.id, &item.prompt, &input_hash)
            .await?
            .ok_or_else(|| AppError::internal("failed to resolve concurrent image generation"));
    }
    Ok(ScheduledImage {
        id,
        state: "生成中".into(),
        file_path: None,
        reused: false,
        created: true,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledImage {
    pub(crate) id: i64,
    pub(crate) state: String,
    pub(crate) file_path: Option<String>,
    pub(crate) reused: bool,
    pub(crate) created: bool,
}

async fn generation_input_hash(
    pool: &sqlx::PgPool,
    item: &ImageItem,
    project_id: i64,
    model: &str,
    resolution: &str,
) -> Result<String, AppError> {
    let context: Value = sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
          'projectTemplate',p.project_type,'projectType',p.type,
          'projectIntro',p.intro,'artStyle',p.art_style,
          'projectImageModel',p.image_model,'imageQuality',p.image_quality,
          'assetDescription',a.description,'assetPrompt',a.prompt,
          'parentAssetId',a.parent_asset_id,'parentPrompt',parent.prompt,
          'parentImageId',parent.image_id,'mappedPrompt',coalesce(mapped.data,''))
        FROM toonflow.assets a
        JOIN toonflow.projects p ON p.id=a.project_id
        LEFT JOIN toonflow.assets parent ON parent.id=a.parent_asset_id
        LEFT JOIN LATERAL (
          SELECT pr.data FROM ai.model_prompt_maps mp
          JOIN toonflow.prompts pr ON pr.source_key=mp.prompt_key
          WHERE mp.model_config_id=CASE WHEN $3 ~ '^[0-9]+$' THEN $3::bigint ELSE NULL END
            AND mp.enabled=true
          ORDER BY mp.update_time DESC,mp.id DESC,pr.id DESC LIMIT 1
        ) mapped ON true
        WHERE a.id=$1 AND a.project_id=$2"#,
    )
    .bind(item.id)
    .bind(project_id)
    .bind(model)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to build image generation input"))?
    .ok_or_else(|| AppError::not_found("asset not found"))?;
    let input = json!({
        "version": 1,
        "assetId": item.id,
        "assetType": item.type_,
        "requestedPrompt": item.prompt,
        "model": model,
        "resolution": resolution,
        "referenceSha256": item.base64.as_deref().map(crate::toonflow_prompt_trace::sha256),
        "context": context,
    });
    Ok(crate::toonflow_prompt_trace::sha256(&input.to_string()))
}

async fn reusable_image(
    pool: &sqlx::PgPool,
    asset_id: i64,
    requested_prompt: &str,
    input_hash: &str,
) -> Result<Option<ScheduledImage>, AppError> {
    let row: Option<(i64, String, Option<String>)> = sqlx::query_as(
        r#"SELECT i.id,i.state,i.file_path
           FROM toonflow.images i
           JOIN toonflow.assets a ON a.id=i.assets_id
           WHERE i.assets_id=$1
             AND i.input_hash=$3
             AND (i.state='生成中' OR
                  (a.image_id=i.id AND a.prompt=$2 AND i.state='已完成'
                   AND nullif(i.file_path,'') IS NOT NULL))
           ORDER BY CASE WHEN i.state='已完成' THEN 0 ELSE 1 END,i.id DESC LIMIT 1"#,
    )
    .bind(asset_id)
    .bind(requested_prompt)
    .bind(input_hash)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to find reusable image generation"))?;
    Ok(row.map(|(id, state, file_path)| ScheduledImage {
        id,
        state,
        file_path,
        reused: true,
        created: false,
    }))
}

async fn wait_for_image(pool: &sqlx::PgPool, image_id: i64) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    loop {
        let row: Option<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT state,file_path,error_reason FROM toonflow.images WHERE id=$1")
                .bind(image_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| error.to_string())?;
        let Some((state, file_path, error_reason)) = row else {
            return Err("图片任务不存在".into());
        };
        match state.as_str() {
            "已完成" => {
                return file_path
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| "图片任务完成但缺少文件".into());
            }
            "生成失败" | "已取消" => return Err(error_reason.unwrap_or_else(|| state)),
            _ if tokio::time::Instant::now() >= deadline => {
                return Err("等待图片生成超时，请检查图片服务".into());
            }
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }
}
async fn make_image(
    pool: &sqlx::PgPool,
    project_id: i64,
    model: &str,
    resolution: &str,
    item: ImageItem,
    image_id: i64,
) -> Result<String, String> {
    let result = make_image_inner(pool, project_id, model, resolution, item, image_id).await;
    if let Err(reason) = &result {
        // Includes prompt/config lookup, download, validation and persistence
        // errors; never replace a cancellation or an already completed state.
        let updated = sqlx::query(
            "UPDATE toonflow.images SET state='生成失败',error_reason=$2 WHERE id=$1 AND state='生成中'",
        ).bind(image_id).bind(reason).execute(pool).await;
        if let Err(error) = updated {
            tracing::error!(image_id, %error, "failed to record asset image failure");
        }
    }
    result
}

async fn make_image_inner(
    pool: &sqlx::PgPool,
    project_id: i64,
    model: &str,
    resolution: &str,
    item: ImageItem,
    image_id: i64,
) -> Result<String, String> {
    let running: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM toonflow.images WHERE id=$1 AND state='生成中')",
    )
    .bind(image_id)
    .fetch_one(pool)
    .await
    .map_err(|error| error.to_string())?;
    if !running {
        return Err("生成任务已取消".into());
    }
    let project: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT art_style,project_type,type,intro FROM toonflow.projects WHERE id=$1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (style, project_template, project_type, project_intro) =
        project.ok_or_else(|| "项目为空".to_string())?;
    let project_context = toonflow_asset_prompt::project_world_context(
        &project_template,
        &project_type,
        &project_intro,
    );
    let asset: (Option<i64>, String, String) = sqlx::query_as(
        "SELECT parent_asset_id,coalesce(description,''),coalesce(prompt,'') FROM toonflow.assets WHERE id=$1 AND project_id=$2 AND type=$3",
    )
    .bind(item.id)
    .bind(project_id)
    .bind(&item.type_)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "资产不存在、类型不匹配或不属于当前项目".to_string())?;
    let (parent_id, source_description, saved_prompt) = asset;
    let derivative = parent_id.is_some();
    let mut references = Vec::new();
    let (visual_description, face_instruction) = if item.type_ == "role" {
        if let Some(parent_id) = parent_id {
            let parent: Option<(String, String)> = sqlx::query_as(
                "SELECT a.prompt,i.file_path FROM toonflow.assets a JOIN toonflow.images i ON i.id=a.image_id WHERE a.id=$1 AND a.project_id=$2 AND a.type='role' AND i.state='已完成' AND i.file_path IS NOT NULL",
            ).bind(parent_id).bind(project_id).fetch_optional(pool).await.map_err(|error| error.to_string())?;
            let (parent_prompt, path) = parent
                .ok_or_else(|| "请先生成该衍生造型对应的基础角色图片，避免人物换脸".to_string())?;
            references.push(image_data_url(&path).await?);
            (
                item.prompt.clone(),
                crate::toonflow_face_identity::identity_instruction(&parent_prompt),
            )
        } else {
            let prepared = crate::toonflow_face_identity::prepare_base_identity(
                pool,
                project_id,
                item.id,
                &item.prompt,
            )
            .await?;
            let identity = crate::toonflow_face_identity::identity_instruction(&prepared);
            (prepared, identity)
        }
    } else {
        (item.prompt.clone(), String::new())
    };
    if let Some(reference) = &item.base64
        && !references.contains(reference)
    {
        references.push(reference.clone());
    }
    let default_prompt_key = match (item.type_.as_str(), derivative) {
        ("role", false) => "asset_image_role_base",
        ("role", true) => "asset_image_role_derivative",
        ("scene", _) => "asset_image_scene",
        ("tool", _) => "asset_image_tool",
        ("costume", _) => "asset_image_costume",
        _ => "",
    };
    // A model-level mapping is an explicit override configured in the AI
    // console. Keep the type-derived prompt as the safe fallback when no
    // mapping exists (or when the model value is not a numeric config id).
    let mapped_prompt_key: Option<String> = if let Ok(model_id) = model.parse::<i64>() {
        sqlx::query_scalar::<_, String>(
            "SELECT prompt_key FROM ai.model_prompt_maps
             WHERE model_config_id=$1 AND enabled=true
             ORDER BY update_time DESC, id DESC LIMIT 1",
        )
        .bind(model_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
    } else {
        None
    };
    let prompt_key = mapped_prompt_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or(default_prompt_key);
    let managed_instruction = if prompt_key.is_empty() {
        None
    } else {
        Some(toonflow_prompt_store::load(pool, prompt_key, "").await)
    };
    let ratio = toonflow_asset_prompt::asset_image_ratio(&item.type_, &visual_description);
    let canvas = crate::toonflow_image_contract::ImageCanvas::for_quality(resolution, ratio)?;
    let active =
        sqlx::query("UPDATE toonflow.images SET resolution=$2 WHERE id=$1 AND state='生成中'")
            .bind(image_id)
            .bind(canvas.size())
            .execute(pool)
            .await
            .map_err(|error| error.to_string())?;
    if active.rows_affected() == 0 {
        return Err("生成任务已取消".into());
    }
    // The current submitted/saved prompt is the generation input. An older
    // extraction description must not silently remove an explicit later edit.
    let effective_source =
        toonflow_asset_prompt::generation_source(&source_description, &item.prompt);
    let prompt = toonflow_asset_prompt::image_prompt_with_source_instruction(
        &style,
        &item.type_,
        Some(effective_source),
        Some(&project_context),
        crate::toonflow_face_identity::appearance_description(&visual_description),
        derivative,
        !references.is_empty(),
        managed_instruction.as_deref(),
    );
    let appearance = if item.type_ == "role" {
        format!(
            "服装与发型锁定：{}。画风仅决定绘画或摄影技法，不得改变角色描述指定的服装时代、款式和发型；现代卫衣、长裤、运动鞋不得因古风画风改成古装、盘扣褂或武侠服装。",
            toonflow_asset_prompt::role_appearance_anchors(&visual_description)
        )
    } else {
        String::new()
    };
    let prompt = format!(
        "{prompt}\n{face_instruction}\n{appearance}\n画布宽高比固定为 {ratio}，输出尺寸 {}；按画布缩放完整主体，不得通过裁切主体适配画布。",
        canvas.size()
    );
    let provenance = json!({
        "version": 1,
        "projectId": project_id,
        "assetId": item.id,
        "imageId": image_id,
        "parentAssetId": parent_id,
        "assetType": item.type_,
        "projectContext": crate::toonflow_prompt_trace::source("projects.type + projects.intro", &project_context),
        "artStyle": crate::toonflow_prompt_trace::source("projects.art_style", &style),
        "assetDescription": crate::toonflow_prompt_trace::source("assets.description", &source_description),
        "savedPrompt": crate::toonflow_prompt_trace::source("assets.prompt", &saved_prompt),
        "requestedPrompt": crate::toonflow_prompt_trace::source(
            if saved_prompt == item.prompt { "assets.prompt" } else { "request.prompt" }, &item.prompt,
        ),
        "effectiveDescription": crate::toonflow_prompt_trace::source("generation_source", effective_source),
        "identityDescription": crate::toonflow_prompt_trace::source("face_identity", &visual_description),
        "managedPromptKey": prompt_key,
        "managedInstruction": crate::toonflow_prompt_trace::source("prompts.data", managed_instruction.as_deref().unwrap_or_default()),
        "references": references.iter().enumerate().map(|(index, reference)| json!({
            "order": index + 1, "sha256": crate::toonflow_prompt_trace::sha256(reference)
        })).collect::<Vec<_>>(),
        "layoutControlReferenceAppended": item.type_ == "role",
    });
    // P1 视觉质检闭环：生成 → 质检 → 不合格时带定向修复指令重试，最多
    // MAX_VISUAL_QC_RETRIES 次；两次重试仍失败保留最后一版交人工。质检
    // 不可用（未配置视觉模型或调用失败）不阻断生成，按原行为完成。
    let mut qc_attempts: Vec<Value> = Vec::new();
    let mut submitted_prompt = prompt.clone();
    let mut final_path: Option<String> = None;
    let expectations = crate::toonflow_visual_qc::asset_image_expectations(
        &item.type_,
        effective_source,
        &appearance,
        &style,
    );
    for attempt in 0..=crate::toonflow_visual_qc::MAX_VISUAL_QC_RETRIES {
        let mut attempt_provenance = provenance.clone();
        if !qc_attempts.is_empty() {
            attempt_provenance["visualQcHistory"] = json!(qc_attempts);
        }
        let generated = match ai_client::image_with_provenance_for_project(
            pool,
            Some(project_id),
            model,
            &submitted_prompt,
            &canvas.size(),
            references.clone(),
            item.type_ == "role",
            Some(attempt_provenance),
        )
        .await
        {
            Ok(path) => path,
            Err(reason) => return Err(reason),
        };
        let path = persist_remote_image(&generated, item.id).await?;
        match crate::toonflow_visual_qc::evaluate_image(pool, project_id, &path, &expectations)
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
                        "imagePath": path,
                    })
                };
                qc_attempts.push(attempt_entry);
                if report.passed || attempt >= crate::toonflow_visual_qc::MAX_VISUAL_QC_RETRIES {
                    final_path = Some(path);
                    break;
                }
                if let Some(repair) = crate::toonflow_visual_qc::repair_instructions(&report) {
                    submitted_prompt = format!("{prompt}\n{repair}");
                }
            }
            Err(_) => {
                final_path = Some(path);
                break;
            }
        }
    }
    let path = final_path.ok_or_else(|| "视觉质检后未取得图片".to_string())?;
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    let updated = sqlx::query("UPDATE toonflow.images SET file_path=$2,state='已完成',error_reason=NULL WHERE id=$1 AND state='生成中'")
        .bind(image_id)
        .bind(&path)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    if updated.rows_affected() == 0 {
        return Err("生成任务已取消".into());
    }
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1 AND project_id=$3")
        .bind(item.id)
        .bind(image_id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(path)
}

pub(crate) async fn schedule_asset_generation(
    pool: &sqlx::PgPool,
    project_id: i64,
    asset_ids: &[i64],
    concurrent_count: usize,
) -> Result<Vec<Value>, AppError> {
    if asset_ids.is_empty() {
        return Err(AppError::bad_request("ids不能为空"));
    }
    polish_extracted_assets(pool, project_id, asset_ids, concurrent_count)
        .await
        .map_err(AppError::bad_request)?;
    let setting: Option<(Option<i64>, String)> =
        sqlx::query_as("SELECT image_model,image_quality FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| AppError::internal("failed to load project image model"))?;
    let (model, resolution) = setting.ok_or_else(|| AppError::not_found("project not found"))?;
    let model = model
        .ok_or_else(|| AppError::bad_request("请先配置项目图片模型"))?
        .to_string();
    let rows: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        r#"SELECT a.id,a.type,a.name,a.prompt,
                  (SELECT i.file_path FROM toonflow.assets p JOIN toonflow.images i ON i.id=p.image_id WHERE p.id=a.parent_asset_id AND i.state='已完成')
           FROM toonflow.assets a
           WHERE a.project_id=$1 AND a.id=ANY($2) AND coalesce(a.prompt,'')<>''"#,
    )
    .bind(project_id)
    .bind(asset_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load assets"))?;
    let mut queue = Vec::new();
    let mut response = Vec::new();
    for (offset, (id, type_, name, prompt, reference_path)) in rows.into_iter().enumerate() {
        let base64 = match reference_path {
            Some(path) => Some(image_data_url(&path).await.map_err(AppError::bad_request)?),
            None => None,
        };
        let item = ImageItem {
            id,
            type_,
            _name: name,
            prompt,
            base64,
        };
        let scheduled =
            new_image(pool, project_id, &item, &model, &resolution, offset as i64).await?;
        response.push(json!({
            "id":item.id,"imageId":scheduled.id,"state":scheduled.state,
            "filePath":scheduled.file_path,"reused":scheduled.reused
        }));
        if scheduled.created {
            queue.push((scheduled.id, item));
        }
    }
    let pool = pool.clone();
    tokio::spawn(async move {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrent_count.clamp(1, 10)));
        for (image_id, item) in queue {
            let permit = sem.clone().acquire_owned().await;
            let pool = pool.clone();
            let model = model.clone();
            let resolution = resolution.clone();
            tokio::spawn(async move {
                if permit.is_ok() {
                    let _ =
                        make_image(&pool, project_id, &model, &resolution, item, image_id).await;
                }
            });
        }
    });
    Ok(response)
}

pub(crate) async fn schedule_and_wait_asset_generation(
    pool: &sqlx::PgPool,
    project_id: i64,
    asset_ids: &[i64],
    concurrent_count: usize,
) -> Result<Vec<Value>, AppError> {
    let scheduled =
        schedule_asset_generation(pool, project_id, asset_ids, concurrent_count).await?;
    let image_ids = scheduled
        .iter()
        .filter_map(|item| item.get("imageId").and_then(Value::as_i64))
        .collect::<Vec<_>>();
    if image_ids.len() != asset_ids.len() {
        return Err(AppError::bad_request("部分衍生资产未能创建图片任务"));
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    loop {
        let rows: Vec<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id,state,file_path,error_reason FROM toonflow.images WHERE id=ANY($1) ORDER BY id",
        )
        .bind(&image_ids)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to wait for derived asset images"))?;
        if rows.len() != image_ids.len() {
            return Err(AppError::bad_request("衍生图片任务记录不完整"));
        }
        let failures = rows
            .iter()
            .filter(|(_, state, _, _)| state == "生成失败" || state == "已取消")
            .map(|(id, _, _, reason)| format!("{id}: {}", reason.as_deref().unwrap_or("生成失败")))
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            return Err(AppError::bad_request(format!(
                "衍生图片未全部生成成功：{}",
                failures.join("；")
            )));
        }
        if rows.iter().all(|(_, state, path, _)| {
            state == "已完成" && path.as_deref().is_some_and(|path| !path.is_empty())
        }) {
            let completed = rows
                .into_iter()
                .map(|(image_id, _, file_path, _)| (image_id, file_path))
                .collect::<std::collections::HashMap<_, _>>();
            return Ok(scheduled
                .into_iter()
                .map(|mut item| {
                    let image_id = item
                        .get("imageId")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    item["state"] = json!("已完成");
                    item["filePath"] = completed
                        .get(&image_id)
                        .cloned()
                        .flatten()
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                    item
                })
                .collect());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::bad_request(
                "等待衍生图片生成超时，请检查图片服务",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
pub async fn generate_image(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<GenerateImageRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let model = req.model.as_configured();
    let item = ImageItem {
        id: req.id,
        type_: req.type_,
        _name: req.name,
        prompt: req.prompt,
        base64: req.base64,
    };
    let scheduled = new_image(
        &state.pool,
        req.project_id,
        &item,
        &model,
        &req.resolution,
        0,
    )
    .await?;
    let result = if let Some(path) = scheduled.file_path.clone() {
        Ok(path)
    } else if scheduled.created {
        make_image(
            &state.pool,
            req.project_id,
            &model,
            &req.resolution,
            item.clone(),
            scheduled.id,
        )
        .await
    } else {
        wait_for_image(&state.pool, scheduled.id).await
    };
    match result {
        Ok(path) => Ok(Json(ApiResponse::new(
            json!({"path":path,"assetsId":item.id,"imageId":scheduled.id,"reused":scheduled.reused}),
        ))),
        Err(reason) => Err(AppError::bad_request(reason)),
    }
}
pub async fn batch_generate_images(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<BatchImageRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let total = req.items.len();
    let model = req.model.as_configured();
    let mut queue = Vec::new();
    let mut scheduled_items = Vec::new();
    for (index, item) in req.items.into_iter().enumerate() {
        let scheduled = new_image(
            &state.pool,
            req.project_id,
            &item,
            &model,
            &req.resolution,
            index as i64,
        )
        .await?;
        scheduled_items.push(json!({"id":item.id,"imageId":scheduled.id,"state":scheduled.state,"filePath":scheduled.file_path,"reused":scheduled.reused}));
        if scheduled.created {
            queue.push((scheduled.id, item));
        }
    }
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(
            req.concurrent_count.unwrap_or(1).clamp(1, 10),
        ));
        for (image_id, item) in queue {
            let permit = sem.clone().acquire_owned().await;
            let pool = pool.clone();
            let model = model.clone();
            let resolution = req.resolution.clone();
            tokio::spawn(async move {
                if permit.is_ok() {
                    let _ = make_image(&pool, req.project_id, &model, &resolution, item, image_id)
                        .await;
                }
            });
        }
    });
    Ok(Json(ApiResponse::new(
        json!({"total":total,"items":scheduled_items}),
    )))
}
pub async fn retry_images(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<RetryImageRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    if req.ids.is_empty() {
        return Err(AppError::bad_request("ids不能为空"));
    }
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT a.id, i.id, i.state FROM toonflow.assets a JOIN LATERAL (SELECT id,state FROM toonflow.images WHERE assets_id=a.id ORDER BY id DESC LIMIT 1) i ON true WHERE a.project_id=$1 AND a.id=ANY($2)",
    )
    .bind(req.project_id)
    .bind(&req.ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load retryable image assets"))?;
    let retry_ids = rows
        .iter()
        .filter(|(_, _, state)| state == "生成失败" || state == "已取消")
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    if retry_ids.is_empty() {
        return Err(AppError::bad_request("没有可重试的失败图片任务"));
    }
    let scheduled = schedule_asset_generation(
        &state.pool,
        req.project_id,
        &retry_ids,
        req.concurrent_count.unwrap_or(5),
    )
    .await?;
    let previous: std::collections::HashMap<i64, i64> = rows
        .into_iter()
        .filter(|(id, _, _)| retry_ids.contains(id))
        .map(|(id, image_id, _)| (id, image_id))
        .collect();
    for item in &scheduled {
        let asset_id = item.get("id").and_then(Value::as_i64);
        let image_id = item.get("imageId").and_then(Value::as_i64);
        if let (Some(asset_id), Some(image_id)) = (asset_id, image_id)
            && let Some(previous_id) = previous.get(&asset_id)
        {
            sqlx::query("UPDATE toonflow.images SET retry_of_id=$2 WHERE id=$1")
                .bind(image_id)
                .bind(previous_id)
                .execute(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to link image retry"))?;
        }
    }
    Ok(Json(ApiResponse::new(json!({
        "total": scheduled.len(),
        "ids": retry_ids,
        "items": scheduled,
    }))))
}
pub async fn poll_images(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<Ids>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows=sqlx::query_as::<_,(i64,String,Option<String>,Option<String>,i64)>("SELECT a.id,i.state,i.file_path,i.error_reason,i.id FROM toonflow.assets a JOIN LATERAL (SELECT id,state,file_path,error_reason FROM toonflow.images WHERE assets_id=a.id ORDER BY id DESC LIMIT 1) i ON true WHERE a.id=ANY($1)").bind(req.ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to poll images"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"state":r.1,"filePath":r.2,"errorReason":r.3,"imageId":r.4}))
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::standalone_costume_prompt;

    #[tokio::test]
    #[ignore = "run with script/test-image-contract.sh (isolated PostgreSQL, no paid model)"]
    async fn image_contract_rejects_bad_results_without_replacing_assets() {
        use super::*;
        use axum::{Router, routing::post};
        use base64::Engine;
        use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
        use std::time::Duration;

        let pool = connect(
            &DatabaseConfig::new(
                std::env::var("TEST_DATABASE_URL").expect("isolated TEST_DATABASE_URL"),
                1,
                5,
                Duration::from_secs(10),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        migrate(&pool).await.unwrap();
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2048, 512)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(output.into_inner());
        let mut cropped = image::RgbImage::from_pixel(2560, 1696, image::Rgb([225, 225, 225]));
        for x in 300..750 {
            for y in 100..1696 {
                cropped.put_pixel(x, y, image::Rgb([35, 35, 35]));
            }
        }
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::from(cropped)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let cropped_encoded = base64::engine::general_purpose::STANDARD.encode(output.into_inner());
        let image_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None::<Value>));
        let capture = captured.clone();
        let planned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let planned_calls = planned.clone();
        let app = Router::new().route(
            "/images/generations",
            post(move |Json(body): Json<Value>| {
                let capture = capture.clone();
                let encoded = if image_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0
                {
                    encoded.clone()
                } else {
                    cropped_encoded.clone()
                };
                async move {
                    *capture.lock().await = Some(body);
                    Json(json!({"data":[{"b64_json":encoded}]}))
                }
            }),
        );
        let app = app.route(
            "/chat/completions",
            post(move || {
                planned_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async {
                    Json(json!({"choices":[{"message":{"content":json!({
                    "face_shape":"宽方脸，颌骨宽厚", "brows":"浓密平直眉",
                    "eyes":"较窄眼裂，眼距适中", "nose":"鼻头饱满，鼻翼较宽",
                    "mouth":"上薄下厚唇，宽下巴", "skin":"自然青年肤质",
                    "distinctive_features":"宽颌方下巴与平眉"
                }).to_string()}}]}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let id = chrono::Utc::now().timestamp_micros();
        sqlx::query("INSERT INTO toonflow.projects(id,name,art_style,create_time,update_time) VALUES($1,'image contract test','realistic',$1,$1)")
            .bind(id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.assets(id,project_id,name,type,prompt) VALUES($1,$1,'character','role','young man')")
            .bind(id).execute(&pool).await.unwrap();
        let old_image_id = id - 1;
        sqlx::query("INSERT INTO toonflow.images(id,assets_id,state,file_path) VALUES($1,$2,'已完成','/existing.png')")
            .bind(old_image_id).bind(id).execute(&pool).await.unwrap();
        sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
            .bind(id)
            .bind(old_image_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO ai.model_configs(id,name,key,platform,type,model,url,status,create_time,update_time) VALUES($1,'image contract mock',$2,'VolcEngine','image','mock-image',$3,0,$1,$1)")
            .bind(id).bind(format!("image-contract-{id}")).bind(&url).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO ai.model_configs(id,name,key,platform,type,model,url,status,create_time,update_time) VALUES($1,'face mock',$2,'OpenAICompatible','chat','mock-chat',$3,0,$1,$1)")
            .bind(id+1).bind(format!("face-contract-{id}")).bind(&url).execute(&pool).await.unwrap();
        sqlx::query("UPDATE toonflow.projects SET chat_model=$2 WHERE id=$1")
            .bind(id)
            .bind(id + 1)
            .execute(&pool)
            .await
            .unwrap();
        let item = ImageItem {
            id,
            type_: "role".into(),
            _name: "character".into(),
            prompt: "青年男性，深灰卫衣，长裤，运动鞋，head to collarbone complete，半身人像特写"
                .into(),
            base64: None,
        };
        let image_id = new_image(&pool, id, &item, &id.to_string(), "2K", 0)
            .await
            .unwrap()
            .id;
        let duplicate = new_image(&pool, id, &item, &id.to_string(), "2K", 1)
            .await
            .unwrap();
        assert_eq!(duplicate.id, image_id);
        assert!(duplicate.reused);
        assert!(!duplicate.created);
        let active_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM toonflow.images WHERE assets_id=$1 AND state='生成中'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_count, 1, "equivalent clicks share one paid request");
        assert!(
            image_id > old_image_id,
            "a new generation must sort after an older edited image"
        );
        let error = make_image(&pool, id, &id.to_string(), "2K", item.clone(), image_id)
            .await
            .unwrap_err();
        assert!(error.contains("比例不合格"), "{error}");
        let body = captured.lock().await.clone().unwrap();
        assert_eq!(body["size"], "2560x1696");
        assert!(
            body["image"][0]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(
            body["prompt"]
                .as_str()
                .unwrap()
                .contains("最后一张参考图仅为全身构图控制图")
        );
        assert!(
            !body["prompt"]
                .as_str()
                .unwrap()
                .contains("head to collarbone")
        );
        let state: (String, Option<String>) =
            sqlx::query_as("SELECT state,error_reason FROM toonflow.images WHERE id=$1")
                .bind(image_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state.0, "生成失败");
        assert!(state.1.unwrap().contains("比例不合格"));
        let selected: i64 = sqlx::query_scalar("SELECT image_id FROM toonflow.assets WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(selected, old_image_id);
        let task: (String, Value) = sqlx::query_as("SELECT state,input FROM toonflow.tasks WHERE project_id=$1 AND task_class='image' ORDER BY id DESC LIMIT 1").bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(task.0, "failed");
        assert_eq!(task.1["size"], "2560x1696");
        assert_eq!(task.1["prompt"], body["prompt"]);
        assert_eq!(task.1["promptProvenance"]["imageId"], image_id);
        assert_eq!(
            task.1["promptProvenance"]["requestedPrompt"]["content"],
            item.prompt
        );
        let saved: String = sqlx::query_scalar("SELECT prompt FROM toonflow.assets WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(saved.contains("【角色面部身份 v1】"));
        assert!(
            body["prompt"]
                .as_str()
                .unwrap()
                .contains("宽方脸，颌骨宽厚")
        );
        let reused =
            crate::toonflow_face_identity::prepare_base_identity(&pool, id, id, &item.prompt)
                .await
                .unwrap();
        assert_eq!(reused, saved);
        assert_eq!(planned.load(std::sync::atomic::Ordering::Relaxed), 1);

        // A matching pixel ratio must not allow a cropped body through either.
        let image_id = new_image(&pool, id, &item, &id.to_string(), "2K", 0)
            .await
            .unwrap()
            .id;
        let error = make_image(&pool, id, &id.to_string(), "2K", item.clone(), image_id)
            .await
            .unwrap_err();
        assert!(error.contains("疑似被裁切"), "{error}");
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.images WHERE id=$1")
            .bind(image_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "生成失败");
        let selected: i64 = sqlx::query_scalar("SELECT image_id FROM toonflow.assets WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(selected, old_image_id);
        assert_eq!(planned.load(std::sync::atomic::Ordering::Relaxed), 1);

        // Early failures must also terminate, and retries cannot overwrite cancellation.
        sqlx::query("UPDATE toonflow.images SET state='生成中',error_reason=NULL WHERE id=$1")
            .bind(image_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            make_image(&pool, -1, &id.to_string(), "2K", item.clone(), image_id)
                .await
                .is_err()
        );
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.images WHERE id=$1")
            .bind(image_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "生成失败");
        sqlx::query(
            "UPDATE toonflow.images SET state='已取消',error_reason='user cancelled' WHERE id=$1",
        )
        .bind(image_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            make_image(&pool, id, &id.to_string(), "2K", item, image_id)
                .await
                .is_err()
        );
        let state: (String, String) =
            sqlx::query_as("SELECT state,error_reason FROM toonflow.images WHERE id=$1")
                .bind(image_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, ("已取消".into(), "user cancelled".into()));
        server.abort();
    }

    #[test]
    fn costume_prompt_is_garment_only_and_uses_neutral_service_wording() {
        let prompt =
            standalone_costume_prompt("深紫色按摩技师装，合体收腰，适合武馆按摩服务场景。");

        assert!(prompt.contains("深紫色理疗技师装"));
        assert!(prompt.contains("修身利落"));
        assert!(prompt.contains("理疗服务场景"));
        assert!(prompt.contains("无人物"));
        assert!(!prompt.contains("按摩"));
        assert!(!prompt.contains("合体收腰"));
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetImageRequest {
    assets_id: i64,
}
pub async fn get_images(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<AssetImageRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let asset: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT id,image_id FROM toonflow.assets WHERE id=$1")
            .bind(req.assets_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get asset"))?;
    let asset = asset.ok_or_else(|| AppError::not_found("asset not found"))?;
    let rows=sqlx::query_as::<_,(i64,Option<String>,String)>("SELECT id,file_path,coalesce(state,'') FROM toonflow.images WHERE assets_id=$1 ORDER BY id DESC").bind(req.assets_id).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to get images"))?;
    Ok(Json(ApiResponse::new(
        json!({"id":asset.0,"imageId":asset.1,"tempAssets":rows.into_iter().map(|r|json!({"id":r.0,"filePath":r.1,"state":r.2,"selected":asset.1==Some(r.0)})).collect::<Vec<_>>()}),
    )))
}
#[derive(Deserialize)]
pub struct ImageId {
    id: i64,
}
pub async fn cancel_image(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ImageId>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let result = sqlx::query("UPDATE toonflow.images SET state='生成失败',error_reason='用户取消生成' WHERE id=$1 AND state='生成中'")
        .bind(req.id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to cancel image generation"))?;
    Ok(Json(ApiResponse::new(
        json!({"message":"取消成功","canceled":result.rows_affected()>0}),
    )))
}
pub async fn delete_image(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ImageId>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let path: Option<String> =
        sqlx::query_scalar("SELECT file_path FROM toonflow.images WHERE id=$1")
            .bind(req.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load image file"))?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin transaction"))?;
    sqlx::query("UPDATE toonflow.assets SET image_id=NULL WHERE image_id=$1")
        .bind(req.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to unbind image"))?;
    sqlx::query("DELETE FROM toonflow.images WHERE id=$1")
        .bind(req.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to delete image"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit transaction"))?;
    if let Some(path) = path
        && let Err(error) = delete_asset_file(&path).await
    {
        record_cleanup_failure(&state.pool, &path, "image", Some(req.id), &error).await;
    }
    Ok(Json(ApiResponse::new(
        json!({"message":"资产图片删除成功"}),
    )))
}
