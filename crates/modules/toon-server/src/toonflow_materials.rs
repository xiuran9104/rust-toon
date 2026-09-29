use crate::{ToonState, shared::require};
use axum::{Json, extract::State};
use base64::Engine;
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadClipRequest {
    project_id: i64,
    base64_data: String,
    #[serde(default = "default_clip_type")]
    type_: String,
    name: String,
}

fn default_clip_type() -> String {
    "clip".into()
}

fn decode_data_url(value: &str) -> Result<(&str, Vec<u8>), AppError> {
    let (header, encoded) = value
        .split_once(";base64,")
        .ok_or_else(|| AppError::bad_request("base64Data 必须是 data URL"))?;
    let extension = match header.trim_start_matches("data:") {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "bin",
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AppError::bad_request("base64Data 无法解码"))?;
    if bytes.is_empty() {
        return Err(AppError::bad_request("上传内容不能为空"));
    }
    Ok((extension, bytes))
}

async fn save_data_url(
    base64_data: &str,
    project_id: i64,
    category: &str,
    image_only: bool,
) -> Result<String, AppError> {
    let (extension, bytes) = decode_data_url(base64_data)?;
    if image_only && !["jpg", "png", "webp"].contains(&extension) {
        return Err(AppError::bad_request("仅支持 JPG、PNG、WebP 图片"));
    }
    crate::toonflow_storage::persist_asset_bytes(project_id, category, extension, bytes)
        .await
        .map_err(|_| AppError::internal("failed to save upload to object storage"))
}

pub(crate) async fn save_asset_cover_data_url(
    base64_data: &str,
    project_id: i64,
) -> Result<String, AppError> {
    save_data_url(base64_data, project_id, "asset-covers", true).await
}

pub async fn upload_clip(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<UploadClipRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    let project_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM toonflow.projects WHERE id=$1)")
            .bind(request.project_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to validate project"))?;
    if !project_exists {
        return Err(AppError::not_found("project not found"));
    }
    let id = chrono::Utc::now().timestamp_millis();
    let image_id = id + 1;
    let url = save_data_url(&request.base64_data, request.project_id, "materials", false).await?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin material transaction"))?;
    sqlx::query(
        "INSERT INTO toonflow.assets(id,name,type,project_id,start_time) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(&request.name)
    .bind(&request.type_)
    .bind(request.project_id)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to create material"))?;
    sqlx::query("INSERT INTO toonflow.images(id,file_path,type,assets_id,state) VALUES($1,$2,$3,$4,'已完成')")
        .bind(image_id)
        .bind(&url)
        .bind(&request.type_)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to create material file"))?;
    sqlx::query("UPDATE toonflow.assets SET image_id=$2 WHERE id=$1")
        .bind(id)
        .bind(image_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to select material file"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit material"))?;
    Ok(Json(ApiResponse::new(
        json!({"id":id,"imageId":image_id,"filePath":url,"type":request.type_,"name":request.name}),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowImageUploadRequest {
    project_id: i64,
    script_id: i64,
    base64_data: String,
}

pub async fn upload_flow_image(
    user: CurrentUser,
    Json(request): Json<FlowImageUploadRequest>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:project:update")?;
    let url = save_data_url(
        &request.base64_data,
        request.project_id,
        &format!("image-flow/{}", request.script_id),
        true,
    )
    .await?;
    Ok(Json(ApiResponse::new(url)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialRequest {
    project_id: i64,
    script_id: Option<i64>,
}

pub async fn list_materials(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<MaterialRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let materials = sqlx::query_as::<_, (i64, String, String, Option<String>)>(
        "SELECT a.id,a.name,a.type,i.file_path FROM toonflow.assets a LEFT JOIN toonflow.images i ON i.id=a.image_id WHERE a.project_id=$1 AND a.type='clip' ORDER BY a.id DESC",
    )
    .bind(request.project_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list materials"))?
    .into_iter()
    .map(|row| json!({"id":row.0,"name":row.1,"type":row.2,"filePath":row.3}))
    .collect::<Vec<_>>();
    let videos = if let Some(script_id) = request.script_id {
        let rows=sqlx::query_as::<_,(i64,Option<i64>,i64,Option<String>)>("SELECT t.id,t.video_id,v.id,v.file_path FROM toonflow.video_tracks t JOIN toonflow.videos v ON v.video_track_id=t.id AND v.state='生成成功' WHERE t.project_id=$1 AND t.script_id=$2 ORDER BY t.id,v.id DESC").bind(request.project_id).bind(script_id).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list material videos"))?;
        let mut groups = serde_json::Map::new();
        for (track_id, selected, video_id, path) in rows {
            let entry = groups
                .entry(track_id.to_string())
                .or_insert_with(|| json!({"id":track_id,"videoId":selected,"video":[]}));
            entry["video"]
                .as_array_mut()
                .expect("video array")
                .push(json!({"id":video_id,"filePath":path,"videoTrackId":track_id}));
        }
        groups.into_values().collect()
    } else {
        Vec::new()
    };
    Ok(Json(ApiResponse::new(
        json!({"data":materials,"video":videos}),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPageRequest {
    project_id: i64,
    #[serde(rename = "type")]
    type_: String,
    name: Option<String>,
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_page() -> i64 {
    1
}

fn default_limit() -> i64 {
    10
}

pub async fn asset_page(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<AssetPageRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let page = request.page.max(1);
    let limit = request.limit.clamp(1, 100);
    let search = request
        .name
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("%{}%", value.trim()));
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM toonflow.assets WHERE project_id=$1 AND type=$2 AND ($3::text IS NULL OR name ILIKE $3)",
    )
    .bind(request.project_id)
    .bind(&request.type_)
    .bind(&search)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to count assets"))?;
    let rows=sqlx::query_as::<_,(i64,String,String,Option<String>,String,String,Option<i64>,Option<i64>,Option<i64>)>("SELECT id,name,prompt,remark,type,description,image_id,parent_asset_id,flow_id FROM toonflow.assets WHERE project_id=$1 AND type=$2 AND ($3::text IS NULL OR name ILIKE $3) ORDER BY id DESC OFFSET $4 LIMIT $5").bind(request.project_id).bind(&request.type_).bind(search).bind((page-1)*limit).bind(limit).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to page assets"))?;
    Ok(Json(ApiResponse::new(json!({
        "data": rows.into_iter().map(|row|json!({"id":row.0,"name":row.1,"prompt":row.2,"remark":row.3,"type":row.4,"description":row.5,"imageId":row.6,"parentAssetId":row.7,"flowId":row.8,"projectId":request.project_id})).collect::<Vec<_>>(),
        "total": total
    }))))
}
