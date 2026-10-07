mod chat;
mod chat_role;
mod factory;
mod knowledge;
mod knowledge_download;
mod media;
mod midjourney;
mod model_prompt_map;
mod provider;
mod tools;
mod vector;
mod write;

pub use factory::AiModelFactory;

use axum::{
    Json, Router,
    extract::{Query, State},
    middleware::from_fn_with_state,
    routing::{delete, get, post, put},
};
use rust_toon_ai_api::{
    AiModelType, AiPlatform, ChatMessage, ChatRequest, EmbeddingRequest, ImageRequest, ModelConfig,
    SpeechRequest,
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::{CurrentUser, Permission, TokenService, authenticate};
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

#[derive(Clone)]
pub struct AiState {
    pool: PgPool,
    tokens: TokenService,
    factory: AiModelFactory,
}
impl AiState {
    pub fn new(pool: PgPool, tokens: TokenService) -> Self {
        let factory = AiModelFactory::new(pool.clone());
        media::spawn_music_sync(pool.clone(), factory.clone());
        midjourney::spawn_sync(pool.clone(), factory.clone());
        Self {
            factory,
            pool,
            tokens,
        }
    }
    pub fn factory(&self) -> AiModelFactory {
        self.factory.clone()
    }
}
fn require(user: &CurrentUser, code: &str) -> Result<(), AppError> {
    if user.role_codes.iter().any(|role| role == "super_admin") {
        return Ok(());
    }
    let permission = Permission::new(code).map_err(|_| AppError::internal("invalid policy"))?;
    if user.can(&permission) {
        Ok(())
    } else {
        Err(AppError::forbidden("permission denied"))
    }
}

pub fn routes(state: AiState) -> Router {
    let protected = Router::new()
        .route("/ai/model/page", get(page))
        .route("/ai/model/simple-list", get(simple_list))
        .route("/ai/model/get", get(get_one))
        .route("/ai/model/create", post(create))
        .route("/ai/model/update", put(update))
        .route("/ai/model/delete", delete(remove))
        .route("/ai/model/test", post(test))
        .route("/ai/model/discover", post(discover_models))
        .route("/ai/model/platforms", get(platforms))
        .route("/ai/model-prompt-map/list", post(model_prompt_map::list))
        .route("/ai/model-prompt-map/save", post(model_prompt_map::save))
        .route(
            "/ai/model-prompt-map/delete",
            post(model_prompt_map::remove),
        )
        .merge(chat::routes())
        .merge(chat_role::routes())
        .merge(midjourney::routes())
        .merge(tools::routes())
        .merge(media::routes())
        .merge(knowledge::routes())
        .merge(write::routes())
        .route_layer(from_fn_with_state(state.tokens.clone(), authenticate));
    Router::new().merge(protected).with_state(state)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageQuery {
    page_no: Option<i64>,
    page_size: Option<i64>,
    name: Option<String>,
    model: Option<String>,
    platform: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
    status: Option<i32>,
}
fn normalize_type(value: &str) -> Option<&'static str> {
    match value {
        "1" | "chat" => Some("chat"),
        "2" | "image" => Some("image"),
        "3" | "speech" => Some("speech"),
        "4" | "video" => Some("video"),
        "5" | "embedding" => Some("embedding"),
        "6" | "rerank" => Some("rerank"),
        "transcription" => Some("transcription"),
        "music" => Some("music"),
        _ => None,
    }
}
async fn page(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:model:query")?;
    let page = query.page_no.unwrap_or(1).max(1);
    let size = query.page_size.unwrap_or(10).clamp(1, 200);
    let type_filter = query.type_.as_deref().and_then(normalize_type);
    let rows=sqlx::query("SELECT id,name,key,platform,type,model,api_key,url,status,config FROM ai.model_configs WHERE ($1::text IS NULL OR name ILIKE '%'||$1||'%') AND ($2::text IS NULL OR model ILIKE '%'||$2||'%') AND ($3::text IS NULL OR platform=$3) AND ($4::text IS NULL OR type=$4) AND ($5::integer IS NULL OR status=$5) ORDER BY id DESC LIMIT $6 OFFSET $7").bind(&query.name).bind(&query.model).bind(&query.platform).bind(type_filter).bind(query.status).bind(size).bind((page-1)*size).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list AI models"))?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM ai.model_configs WHERE ($1::text IS NULL OR name ILIKE '%'||$1||'%') AND ($2::text IS NULL OR model ILIKE '%'||$2||'%') AND ($3::text IS NULL OR platform=$3) AND ($4::text IS NULL OR type=$4) AND ($5::integer IS NULL OR status=$5)").bind(&query.name).bind(&query.model).bind(&query.platform).bind(type_filter).bind(query.status).fetch_one(&state.pool).await.map_err(|_|AppError::internal("failed to count AI models"))?;
    let list = rows
        .into_iter()
        .map(row_model)
        .map(|model| {
            let capabilities = capability_tags(&model);
            let mut value = serde_json::to_value(model).unwrap_or_else(|_| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("capabilities".into(), json!(capabilities));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({"list":list,"total":total}))))
}
#[derive(Deserialize)]
struct SimpleListQuery {
    #[serde(rename = "type")]
    type_: Option<String>,
}
async fn simple_list(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(query): Query<SimpleListQuery>,
) -> Result<Json<ApiResponse<Vec<ModelConfig>>>, AppError> {
    require(&user, "ai:model:query")?;
    let type_filter = query.type_.as_deref().and_then(normalize_type);
    let rows = sqlx::query("SELECT id,name,key,platform,type,model,api_key,url,status,config FROM ai.model_configs WHERE status=0 AND ($1::text IS NULL OR type=$1) ORDER BY name,id")
        .bind(type_filter)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to list AI models"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter().map(row_model).collect(),
    )))
}
fn row_model(row: sqlx::postgres::PgRow) -> ModelConfig {
    ModelConfig {
        id: row.get("id"),
        name: row.get("name"),
        key: row.get("key"),
        platform: row.get("platform"),
        type_: row.get("type"),
        model: row.get("model"),
        api_key: row.get("api_key"),
        url: row.get("url"),
        status: row.get("status"),
        config: row.get("config"),
    }
}

fn capability_tags(model: &ModelConfig) -> Vec<String> {
    let mut tags = match model.type_.as_str() {
        "chat" => vec!["文本对话".into(), "Agent".into()],
        "image" => vec!["图片生成".into(), "批量生成".into()],
        "video" => vec!["视频生成".into(), "首尾帧".into()],
        "speech" => vec!["语音合成".into()],
        "transcription" => vec!["语音识别".into()],
        "embedding" => vec!["向量检索".into()],
        "rerank" => vec!["结果重排".into()],
        "music" => vec!["音乐生成".into()],
        _ => Vec::new(),
    };
    if let Some(extra) = model.config.get("capabilities").and_then(Value::as_array) {
        for value in extra.iter().filter_map(Value::as_str) {
            if !tags.iter().any(|tag| tag == value) {
                tags.push(value.to_string());
            }
        }
    }
    tags
}
#[derive(Deserialize)]
struct IdQuery {
    id: i64,
}
async fn get_one(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(query): Query<IdQuery>,
) -> Result<Json<ApiResponse<ModelConfig>>, AppError> {
    require(&user, "ai:model:query")?;
    Ok(Json(ApiResponse::new(
        state.factory.config(query.id).await?,
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveModel {
    id: Option<i64>,
    name: String,
    key: String,
    platform: String,
    #[serde(rename = "type")]
    type_: String,
    model: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    url: String,
    #[serde(default = "enabled")]
    status: i32,
    #[serde(default)]
    config: Value,
}
fn enabled() -> i32 {
    0
}
fn validate_model(request: &SaveModel) -> Result<(), AppError> {
    let platform = AiPlatform::parse(&request.platform)
        .ok_or_else(|| AppError::bad_request("unsupported platform"))?;
    let model_type = AiModelType::parse(&request.type_)
        .ok_or_else(|| AppError::bad_request("unsupported model type"))?;
    if !platform.supports(model_type) {
        return Err(AppError::bad_request(format!(
            "平台 {} 不支持 {} 类型",
            platform.code(),
            model_type.code()
        )));
    }
    if request.name.trim().is_empty()
        || request.key.trim().is_empty()
        || request.model.trim().is_empty()
    {
        return Err(AppError::bad_request("name、key、model 不能为空"));
    }
    Ok(())
}

async fn resolve_api_key(
    state: &AiState,
    request: &SaveModel,
    current_id: Option<i64>,
) -> Result<String, AppError> {
    if !request.api_key.trim().is_empty() {
        return Ok(request.api_key.trim().to_string());
    }
    if matches!(
        AiPlatform::parse(&request.platform),
        Some(AiPlatform::Ollama)
    ) {
        return Ok(String::new());
    }
    let api_key: Option<String> = sqlx::query_scalar(
        "SELECT api_key FROM ai.model_configs
         WHERE platform=$1 AND api_key<>''
           AND ($2='' OR url=$2)
         ORDER BY CASE WHEN id=$3 THEN 0 ELSE 1 END, update_time DESC
         LIMIT 1",
    )
    .bind(&request.platform)
    .bind(request.url.trim())
    .bind(current_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to resolve AI model credential"))?;
    api_key.ok_or_else(|| AppError::bad_request("API 密钥不能为空；当前平台没有可复用的已保存密钥"))
}
async fn create(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(request): Json<SaveModel>,
) -> Result<Json<ApiResponse<i64>>, AppError> {
    require(&user, "ai:model:create")?;
    validate_model(&request)?;
    let api_key = resolve_api_key(&state, &request, None).await?;
    let id = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO ai.model_configs(id,name,key,platform,type,model,api_key,url,status,config,create_time,update_time)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11)").bind(id).bind(request.name).bind(request.key).bind(request.platform).bind(request.type_).bind(request.model).bind(api_key).bind(request.url).bind(request.status).bind(request.config).bind(id).execute(&state.pool).await.map_err(|error|if error.to_string().contains("unique"){AppError::bad_request("model key already exists")}else{AppError::internal("failed to create AI model")})?;
    Ok(Json(ApiResponse::new(id)))
}
async fn update(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(request): Json<SaveModel>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&user, "ai:model:update")?;
    validate_model(&request)?;
    let id = request
        .id
        .ok_or_else(|| AppError::bad_request("id is required"))?;
    let api_key = resolve_api_key(&state, &request, Some(id)).await?;
    let result=sqlx::query("UPDATE ai.model_configs SET name=$2,key=$3,platform=$4,type=$5,model=$6,api_key=$7,url=$8,status=$9,config=$10,update_time=$11 WHERE id=$1").bind(id).bind(request.name).bind(request.key).bind(request.platform).bind(request.type_).bind(request.model).bind(api_key).bind(request.url).bind(request.status).bind(request.config).bind(chrono::Utc::now().timestamp_millis()).execute(&state.pool).await.map_err(|_|AppError::internal("failed to update AI model"))?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("AI model not found"));
    }
    Ok(Json(ApiResponse::new(true)))
}
async fn remove(
    user: CurrentUser,
    State(state): State<AiState>,
    Query(query): Query<IdQuery>,
) -> Result<Json<ApiResponse<bool>>, AppError> {
    require(&user, "ai:model:delete")?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin AI model deletion"))?;
    sqlx::query(
        "UPDATE toonflow.projects
         SET chat_model = CASE WHEN chat_model=$1 THEN NULL ELSE chat_model END,
             image_model = CASE WHEN image_model=$1 THEN NULL ELSE image_model END,
             video_model = CASE WHEN video_model=$1 THEN NULL ELSE video_model END,
             update_time = CASE
               WHEN chat_model=$1 OR image_model=$1 OR video_model=$1
               THEN $2 ELSE update_time END
         WHERE chat_model=$1 OR image_model=$1 OR video_model=$1",
    )
    .bind(query.id)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to clear ToonFlow model references"))?;
    let result = sqlx::query("DELETE FROM ai.model_configs WHERE id=$1")
        .bind(query.id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .and_then(|database_error| database_error.code())
                .as_deref()
                == Some("23503")
            {
                AppError::bad_request("该模型已有生成记录或知识库引用，不能直接删除")
            } else {
                AppError::internal("failed to delete AI model")
            }
        })?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("AI model not found"));
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit AI model deletion"))?;
    Ok(Json(ApiResponse::new(true)))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestModel {
    id: i64,
    prompt: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverModels {
    platform: String,
    url: String,
    #[serde(default)]
    api_key: String,
}

fn discovered_model_type(model: &str) -> &'static str {
    let id = model.to_ascii_lowercase();
    if id.contains("embedding") || id.contains("embed") {
        "embedding"
    } else if id.contains("rerank") {
        "rerank"
    } else if id.contains("transcrib") || id.contains("whisper") {
        "transcription"
    } else if id.contains("tts") || id.contains("audio") {
        "speech"
    } else if id.contains("sora") || id.contains("video") || id.contains("seedance") {
        "video"
    } else if id.contains("image")
        || id.contains("seedream")
        || id.contains("dall-e")
        || id.contains("flux")
        || id.contains("cogview")
    {
        "image"
    } else {
        "chat"
    }
}

async fn discover_models(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(request): Json<DiscoverModels>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:model:query")?;
    let platform = AiPlatform::parse(&request.platform)
        .ok_or_else(|| AppError::bad_request("unsupported platform"))?;
    if request.url.trim().is_empty() {
        return Err(AppError::bad_request("API 地址不能为空"));
    }
    let client = reqwest::Client::new();
    let builder = if platform == AiPlatform::Gemini {
        client
            .get(format!("{}/models", request.url.trim_end_matches('/')))
            .query(&[("key", request.api_key.as_str())])
    } else {
        let mut builder = client.get(format!("{}/models", request.url.trim_end_matches('/')));
        if !request.api_key.is_empty() {
            builder = builder.bearer_auth(request.api_key.trim_start_matches("Bearer "));
        }
        builder
    };
    let response = provider::send_with_retry(builder)
        .await
        .map_err(|error| AppError::bad_request(format!("获取模型列表失败：{error}")))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|error| AppError::bad_request(format!("模型列表响应无效：{error}")))?;
    if !status.is_success() {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("供应商拒绝获取模型列表");
        return Err(AppError::bad_request(message));
    }
    let items = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = items
        .into_iter()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(|id| id.trim_start_matches("models/").to_string())
        })
        .filter_map(|id| {
            let model_type = AiModelType::parse(discovered_model_type(&id))?;
            platform
                .supports(model_type)
                .then(|| json!({"id":id,"type":model_type.code()}))
        })
        .collect::<Vec<_>>();
    let source = format!("{}/models", request.url.trim_end_matches('/'));
    let synced_at = chrono::Utc::now().timestamp_millis();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to start model catalog sync"))?;
    sqlx::query(
        "UPDATE ai.model_catalog
         SET missing_count=missing_count+1
         WHERE platform=$1 AND source='discover'",
    )
    .bind(platform.code())
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to clear old model catalog"))?;
    for model in &models {
        sqlx::query(
            "INSERT INTO ai.model_catalog(
                platform,model,type,source,source_url,active,synced_at,missing_count
             ) VALUES($1,$2,$3,'discover',$4,TRUE,$5,0)
             ON CONFLICT(platform,model) DO UPDATE
             SET type=EXCLUDED.type,source='discover',source_url=EXCLUDED.source_url,
                 active=TRUE,synced_at=EXCLUDED.synced_at,missing_count=0",
        )
        .bind(platform.code())
        .bind(model["id"].as_str().unwrap_or_default())
        .bind(model["type"].as_str().unwrap_or("chat"))
        .bind(&source)
        .bind(synced_at)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to persist model catalog"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit model catalog sync"))?;
    Ok(Json(ApiResponse::new(json!({
        "models": models,
        "platform": platform.code(),
        "source": source,
        "syncedAt": synced_at,
        "persisted": true,
    }))))
}
async fn test(
    user: CurrentUser,
    State(state): State<AiState>,
    Json(request): Json<TestModel>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:model:update")?;
    let started = std::time::Instant::now();
    let config = state.factory.config(request.id).await?;
    let model_type = AiModelType::parse(&config.type_)
        .ok_or_else(|| AppError::bad_request("unsupported AI model type"))?;
    let prompt = request.prompt.unwrap_or_else(|| "模型连接测试".into());
    let result = match model_type {
        AiModelType::Chat => {
            let response = state
                .factory
                .chat(
                    request.id,
                    ChatRequest {
                        model: String::new(),
                        messages: vec![ChatMessage {
                            role: "user".into(),
                            content: prompt,
                        }],
                        temperature: Some(0.2),
                        max_tokens: Some(256),
                        ..Default::default()
                    },
                )
                .await?;
            json!({"content":response.content,"reasoning":response.reasoning,"usage":response.usage})
        }
        AiModelType::Image => serde_json::to_value(
            state
                .factory
                .image(
                    request.id,
                    ImageRequest {
                        prompt,
                        size: "1024x1024".into(),
                        references: Vec::new(),
                    },
                )
                .await?,
        )
        .map_err(|_| AppError::internal("failed to serialize image test"))?,
        AiModelType::Video => serde_json::to_value(
            state
                .factory
                .video(request.id, json!({"prompt":prompt}))
                .await?,
        )
        .map_err(|_| AppError::internal("failed to serialize video test"))?,
        AiModelType::Speech => serde_json::to_value(
            state
                .factory
                .speech(
                    request.id,
                    SpeechRequest {
                        input: prompt,
                        voice: "alloy".into(),
                        format: "mp3".into(),
                    },
                )
                .await?,
        )
        .map_err(|_| AppError::internal("failed to serialize speech test"))?,
        AiModelType::Embedding => serde_json::to_value(
            state
                .factory
                .embedding(
                    request.id,
                    EmbeddingRequest {
                        inputs: vec![prompt],
                    },
                )
                .await?,
        )
        .map_err(|_| AppError::internal("failed to serialize embedding test"))?,
        _ => {
            return Err(AppError::bad_request(format!(
                "{} model test is not implemented",
                model_type.code()
            )));
        }
    };
    Ok(Json(ApiResponse::new(
        json!({"success":true,"latencyMs":started.elapsed().as_millis(),"result":result}),
    )))
}
async fn platforms(
    user: CurrentUser,
    State(state): State<AiState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "ai:model:query")?;
    let platform_rows = sqlx::query(
        "SELECT platform,label,default_url,supported_types
         FROM ai.model_platforms WHERE enabled=TRUE ORDER BY platform",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load model platforms"))?;
    let model_rows = sqlx::query(
        "SELECT platform,model,type,source,synced_at
         FROM ai.model_catalog WHERE active=TRUE ORDER BY platform,type,model",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load model catalog"))?;
    let models = model_rows
        .into_iter()
        .map(|row| {
            json!({
                "platform": row.get::<String, _>("platform"),
                "model": row.get::<String, _>("model"),
                "type": row.get::<String, _>("type"),
                "source": row.get::<String, _>("source"),
                "syncedAt": row.get::<i64, _>("synced_at"),
            })
        })
        .collect::<Vec<_>>();
    let platforms = platform_rows
        .into_iter()
        .map(|row| {
            let platform = row.get::<String, _>("platform");
            let types = row.get::<Vec<String>, _>("supported_types");
            let mut presets = serde_json::Map::new();
            for model_type in &types {
                let entries = models
                    .iter()
                    .filter(|model| {
                        model["platform"] == platform
                            && model["type"] == *model_type
                            && model["source"] == "preset"
                    })
                    .map(|model| {
                        let id = model["model"].as_str().unwrap_or_default();
                        json!({"label":id,"model":id})
                    })
                    .collect::<Vec<_>>();
                presets.insert(model_type.clone(), json!(entries));
            }
            json!({
                "platform": platform,
                "label": row.get::<String, _>("label"),
                "url": row.get::<String, _>("default_url"),
                "types": types,
                "presets": presets,
            })
        })
        .collect::<Vec<_>>();
    let cached_models = models
        .into_iter()
        .filter(|model| model["source"] == "discover")
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({
        "platforms": platforms,
        "cachedModels": cached_models,
        "types": ["chat","image","video","speech","transcription","music","embedding","rerank"]
    }))))
}

#[cfg(test)]
mod tests {
    use super::discovered_model_type;

    #[test]
    fn discovered_models_are_classified_before_merging_with_presets() {
        assert_eq!(discovered_model_type("qwen3-rerank"), "rerank");
        assert_eq!(discovered_model_type("qwen3-vl-rerank"), "rerank");
        assert_eq!(discovered_model_type("text-embedding-v4"), "embedding");
        assert_eq!(
            discovered_model_type("doubao-seedance-1-5-pro-251215"),
            "video"
        );
        assert_eq!(discovered_model_type("doubao-seedream-4-5-251128"), "image");
        assert_eq!(discovered_model_type("qwen-max"), "chat");
    }
}
