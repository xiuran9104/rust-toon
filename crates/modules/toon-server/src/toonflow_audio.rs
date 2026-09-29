use crate::{ToonState, ai_client, shared::require};
use axum::{Json, extract::State};
use base64::Engine as _;
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn next_id(offset: i64) -> i64 {
    now_ms() * 1000 + offset
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRequest {
    project_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindRequest {
    assets_id: i64,
    audio_ids: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchBindRequest {
    project_id: i64,
    assets_ids: Vec<i64>,
}

#[derive(Deserialize)]
pub struct PollRequest {
    ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DubbingRequest {
    project_id: i64,
    assets_id: i64,
    text: String,
    #[serde(default = "default_voice")]
    voice: String,
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioItemRequest {
    id: Option<i64>,
    src: Option<String>,
    base64: Option<String>,
    prompt: String,
    #[serde(alias = "description")]
    describe: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddAudioAssetsRequest {
    name: String,
    #[serde(alias = "description")]
    describe: String,
    project_id: i64,
    assets_item: Vec<AudioItemRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAudioAssetsRequest {
    id: i64,
    name: String,
    #[serde(alias = "description")]
    describe: String,
    project_id: i64,
    assets_item: Vec<AudioItemRequest>,
}

fn decode_audio(data: &str) -> Result<(String, Vec<u8>), AppError> {
    let (metadata, encoded) = data
        .split_once(',')
        .ok_or_else(|| AppError::bad_request("音频 Base64 格式无效"))?;
    if !metadata.starts_with("data:audio/") || !metadata.ends_with(";base64") {
        return Err(AppError::bad_request("只支持 data:audio/*;base64 音频"));
    }
    let mime = metadata
        .trim_start_matches("data:audio/")
        .trim_end_matches(";base64");
    let extension = match mime {
        "mpeg" => "mp3",
        "x-wav" => "wav",
        "x-aiff" => "aiff",
        "x-m4a" => "m4a",
        "x-flac" => "flac",
        value => value,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AppError::bad_request("音频 Base64 无法解码"))?;
    Ok((extension.to_string(), bytes))
}

async fn materialize_audio(project_id: i64, item: &mut AudioItemRequest) -> Result<(), AppError> {
    if let Some(data) = item.base64.as_deref().filter(|value| !value.is_empty()) {
        let (extension, bytes) = decode_audio(data)?;
        item.src = Some(
            crate::toonflow_storage::persist_asset_bytes(project_id, "audio", &extension, bytes)
                .await
                .map_err(AppError::bad_request)?,
        );
    }
    if item.id.is_none() && item.src.as_deref().unwrap_or_default().is_empty() {
        return Err(AppError::bad_request("音频文件不能为空"));
    }
    Ok(())
}

async fn insert_audio_child(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    parent_id: i64,
    project_id: i64,
    item: &AudioItemRequest,
    offset: i64,
) -> Result<i64, AppError> {
    let asset_id = next_id(offset);
    let image_id = next_id(offset + 500);
    sqlx::query("INSERT INTO toonflow.assets(id,name,prompt,type,description,parent_asset_id,project_id,start_time) VALUES($1,$2,$3,'audio',$4,$5,$6,$7)")
        .bind(asset_id).bind(&item.name).bind(&item.prompt).bind(&item.describe).bind(parent_id).bind(project_id).bind(now_ms())
        .execute(&mut **tx).await.map_err(|_|AppError::internal("failed to add audio asset"))?;
    sqlx::query("INSERT INTO toonflow.images(id,file_path,type,assets_id,state) VALUES($1,$2,'audio',$3,'已完成')")
        .bind(image_id).bind(&item.src).bind(asset_id).execute(&mut **tx).await
        .map_err(|_|AppError::internal("failed to add audio file"))?;
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
        .bind(asset_id)
        .bind(image_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| AppError::internal("failed to link audio file"))?;
    Ok(asset_id)
}

pub async fn add_audio_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(mut request): Json<AddAudioAssetsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    for item in &mut request.assets_item {
        materialize_audio(request.project_id, item).await?;
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to add audio assets"))?;
    let parent_id = next_id(0);
    sqlx::query("INSERT INTO toonflow.assets(id,name,type,description,project_id,start_time) VALUES($1,$2,'audio',$3,$4,$5)")
        .bind(parent_id).bind(request.name).bind(request.describe).bind(request.project_id).bind(now_ms())
        .execute(&mut *tx).await.map_err(|_|AppError::internal("failed to add audio collection"))?;
    for (index, item) in request.assets_item.iter().enumerate() {
        insert_audio_child(
            &mut tx,
            parent_id,
            request.project_id,
            item,
            index as i64 + 1,
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit audio assets"))?;
    Ok(Json(ApiResponse::with_message(
        json!({"id":parent_id}),
        "新增资产成功",
    )))
}

pub async fn update_audio_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(mut request): Json<UpdateAudioAssetsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    for item in &mut request.assets_item {
        materialize_audio(request.project_id, item).await?;
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to update audio assets"))?;
    let updated = sqlx::query("UPDATE toonflow.assets SET name=$3,description=$4 WHERE id=$1 AND project_id=$2 AND type='audio'")
        .bind(request.id).bind(request.project_id).bind(request.name).bind(request.describe).execute(&mut *tx).await
        .map_err(|_|AppError::internal("failed to update audio collection"))?;
    if updated.rows_affected() == 0 {
        return Err(AppError::not_found("音频资产不存在"));
    }
    let incoming = request
        .assets_item
        .iter()
        .filter_map(|item| item.id)
        .collect::<Vec<_>>();
    sqlx::query("DELETE FROM toonflow.assets WHERE parent_asset_id=$1 AND NOT(id=ANY($2))")
        .bind(request.id)
        .bind(&incoming)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to remove audio items"))?;
    for (index, item) in request.assets_item.iter().enumerate() {
        if let Some(id) = item.id {
            let result=sqlx::query("UPDATE toonflow.assets SET name=$4,prompt=$5,description=$6 WHERE id=$1 AND parent_asset_id=$2 AND project_id=$3")
                .bind(id).bind(request.id).bind(request.project_id).bind(&item.name).bind(&item.prompt).bind(&item.describe)
                .execute(&mut *tx).await.map_err(|_|AppError::internal("failed to update audio item"))?;
            if result.rows_affected() == 0 {
                return Err(AppError::bad_request("音频子资产不属于当前资产"));
            }
            if item.src.is_some() {
                sqlx::query("UPDATE toonflow.images SET file_path=$2 WHERE assets_id=$1")
                    .bind(id)
                    .bind(&item.src)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| AppError::internal("failed to update audio file"))?;
            }
        } else {
            insert_audio_child(
                &mut tx,
                request.id,
                request.project_id,
                item,
                index as i64 + 1,
            )
            .await?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit audio assets"))?;
    Ok(Json(ApiResponse::with_message(
        json!({"id":request.id}),
        "更新资产成功",
    )))
}

fn default_voice() -> String {
    "alloy".to_string()
}

#[derive(Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AudioAssetRow {
    id: i64,
    name: String,
    description: String,
    #[serde(rename = "type")]
    type_: String,
    project_id: i64,
    audio_bind_state: Option<i32>,
    audio_url: Option<String>,
    releped_audio: Value,
}

pub async fn all_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<ApiResponse<Vec<AudioAssetRow>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, AudioAssetRow>(
        r#"SELECT a.id,a.name,a.description,a.type as type_,a.project_id,a.audio_bind_state,
                  i.file_path as audio_url,
                  COALESCE((SELECT jsonb_agg(jsonb_build_object('id',aa.id,'name',aa.name,'url',ai.file_path))
                    FROM toonflow.asset_audio_bindings b JOIN toonflow.assets aa ON aa.id=b.asset_audio_id
                    LEFT JOIN toonflow.images ai ON ai.id=aa.image_id WHERE b.asset_role_id=a.id),'[]'::jsonb) as releped_audio
           FROM toonflow.assets a LEFT JOIN toonflow.images i ON i.id=a.image_id
           WHERE a.project_id=$1 ORDER BY a.id DESC"#,
    ).bind(request.project_id).fetch_all(&state.pool).await
        .map_err(|_| AppError::internal("failed to list audio assets"))?;
    Ok(Json(ApiResponse::new(rows)))
}

pub async fn update_binding(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<BindRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to bind audio"))?;
    sqlx::query("DELETE FROM toonflow.asset_audio_bindings WHERE asset_role_id=$1")
        .bind(request.assets_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to bind audio"))?;
    if let Some(audio_id) = request.audio_ids.unwrap_or_default().into_iter().next() {
        sqlx::query("INSERT INTO toonflow.asset_audio_bindings(asset_role_id,asset_audio_id,create_time) VALUES($1,$2,$3)")
            .bind(request.assets_id).bind(audio_id).bind(now_ms()).execute(&mut *tx).await
            .map_err(|_| AppError::bad_request("音色资产不存在或不可绑定"))?;
    }
    sqlx::query("UPDATE toonflow.assets SET audio_bind_state=1 WHERE id=$1")
        .bind(request.assets_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to bind audio"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to bind audio"))?;
    Ok(Json(ApiResponse::new(json!(true))))
}

pub async fn batch_bind(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<BatchBindRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let audio_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM toonflow.assets WHERE project_id=$1 AND type='audio' ORDER BY id",
    )
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load audio assets"))?;
    let audio_id = audio_ids
        .first()
        .copied()
        .ok_or_else(|| AppError::bad_request("请先创建或生成音色资产"))?;
    for asset_id in request.assets_ids {
        sqlx::query("INSERT INTO toonflow.asset_audio_bindings(asset_role_id,asset_audio_id,create_time) VALUES($1,$2,$3) ON CONFLICT(asset_role_id,asset_audio_id) DO NOTHING")
            .bind(asset_id).bind(audio_id).bind(now_ms()).execute(&state.pool).await.map_err(|_| AppError::internal("failed to bind audio"))?;
        sqlx::query("UPDATE toonflow.assets SET audio_bind_state=1 WHERE id=$1 AND project_id=$2")
            .bind(asset_id)
            .bind(request.project_id)
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to update audio state"))?;
    }
    Ok(Json(ApiResponse::new(json!({"audioId":audio_id}))))
}

pub async fn poll(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<PollRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows: Vec<(i64, Option<i32>)> =
        sqlx::query_as("SELECT id,audio_bind_state FROM toonflow.assets WHERE id=ANY($1)")
            .bind(&request.ids)
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to poll audio"))?;
    Ok(Json(ApiResponse::new(json!(
        rows.into_iter()
            .map(|(id, state)| json!({"id":id,"audioBindState":state}))
            .collect::<Vec<_>>()
    ))))
}

pub async fn generate_dubbing(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<DubbingRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.text.trim().is_empty() {
        return Err(AppError::bad_request("配音文本不能为空"));
    }
    let configured = match request.model {
        Some(model) if !model.is_empty() => model,
        _ => {
            let deployment: Option<(Option<i64>,)> = sqlx::query_as(
                "SELECT model_config_id FROM toonflow.agent_deployments WHERE key='ttsDubbing' AND disabled=false",
            ).fetch_optional(&state.pool).await.map_err(|_| AppError::internal("failed to load tts model"))?;
            deployment
                .ok_or_else(|| AppError::bad_request("请先配置 ttsDubbing Agent"))?
                .0
                .ok_or_else(|| AppError::bad_request("ttsDubbing Agent 必须绑定统一语音模型"))?
                .to_string()
        }
    };
    sqlx::query("UPDATE toonflow.assets SET audio_bind_state=2 WHERE id=$1 AND project_id=$2")
        .bind(request.assets_id)
        .bind(request.project_id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to update audio state"))?;
    let generated_url =
        match ai_client::speech(&state.pool, &configured, &request.text, &request.voice).await {
            Ok(url) => url,
            Err(error) => {
                sqlx::query(
                "UPDATE toonflow.assets SET audio_bind_state=-1,prompt_error_reason=$2 WHERE id=$1",
            )
            .bind(request.assets_id)
            .bind(&error)
            .execute(&state.pool)
            .await
            .ok();
                return Err(AppError::bad_request(error));
            }
        };
    // TTS providers return a data URL. Store the bytes in object storage immediately so
    // the audio can be reused by video export and does not bloat PostgreSQL.
    let url = if generated_url.starts_with("data:audio/") {
        let (extension, bytes) = decode_audio(&generated_url)?;
        crate::toonflow_storage::persist_asset_bytes(request.project_id, "audio", &extension, bytes)
            .await
            .map_err(AppError::bad_request)?
    } else {
        generated_url
    };
    let audio_id = next_id(1);
    let image_id = next_id(2);
    let name: String = sqlx::query_scalar("SELECT name FROM toonflow.assets WHERE id=$1")
        .bind(request.assets_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to load asset"))?
        .unwrap_or_else(|| "角色".into());
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to save dubbing"))?;
    sqlx::query("INSERT INTO toonflow.assets(id,name,prompt,type,description,parent_asset_id,project_id) VALUES($1,$2,$3,'audio',$4,$5,$6)")
        .bind(audio_id).bind(format!("{name}-配音")).bind(&request.text).bind(format!("voice:{}",request.voice)).bind(request.assets_id).bind(request.project_id)
        .execute(&mut *tx).await.map_err(|_| AppError::internal("failed to save dubbing"))?;
    sqlx::query("INSERT INTO toonflow.images(id,file_path,type,assets_id,model,state) VALUES($1,$2,'audio',$3,$4,'生成完成')")
        .bind(image_id).bind(&url).bind(audio_id).bind(&configured).execute(&mut *tx).await.map_err(|_| AppError::internal("failed to save dubbing"))?;
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
        .bind(audio_id)
        .bind(image_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to select dubbing"))?;
    sqlx::query("INSERT INTO toonflow.asset_audio_bindings(asset_role_id,asset_audio_id,create_time) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(request.assets_id).bind(audio_id).bind(now_ms()).execute(&mut *tx).await.map_err(|_| AppError::internal("failed to bind dubbing"))?;
    sqlx::query(
        "UPDATE toonflow.assets SET audio_bind_state=1,prompt_error_reason=NULL WHERE id=$1",
    )
    .bind(request.assets_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to finish dubbing"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to save dubbing"))?;
    Ok(Json(ApiResponse::new(json!({"id":audio_id,"url":url}))))
}
