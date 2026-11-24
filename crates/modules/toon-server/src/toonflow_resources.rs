use axum::{
    Json,
    extract::{Path, State},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

#[derive(Deserialize)]
pub struct ModelListRequest {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDetailRequest {
    model_id: String,
}

pub async fn model_list(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ModelListRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let kind = if request.kind == "text" {
        "chat"
    } else {
        request.kind.as_str()
    };
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id,name,model,type,platform FROM ai.model_configs WHERE status=0 AND ($1='all' OR type=$1) AND ($1<>'all' OR type<>'video') ORDER BY name,id",
    )
    .bind(kind)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list models"))?;
    let data = rows
        .into_iter()
        .map(|(id, label, model, kind, platform)| {
            let public_kind = if kind == "chat" { "text" } else { &kind };
            json!({"id":platform,"label":label,"value":model,"type":public_kind,"name":platform,"modelConfigId":id})
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!(data))))
}

pub async fn model_detail(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ModelDetailRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:read")?;
    let numeric_id = request.model_id.parse::<i64>().ok();
    let (platform, model) = request
        .model_id
        .split_once(':')
        .map(|(platform, model)| (Some(platform), model))
        .unwrap_or((None, request.model_id.as_str()));
    let row: Option<(i64, String, String, String, String, Value)> = sqlx::query_as(
        "SELECT id,name,model,type,platform,config FROM ai.model_configs WHERE status=0 AND (($1::bigint IS NOT NULL AND id=$1) OR ($1 IS NULL AND model=$2 AND ($3::text IS NULL OR platform=$3))) ORDER BY id LIMIT 1",
    ).bind(numeric_id).bind(model).bind(platform).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to load model"))?;
    let data = row
        .map(|(id, name, model, kind, platform, config)| {
            let public_kind = if kind == "chat" { "text" } else { &kind };
            json!({"id":id,"name":name,"modelName":model,"type":public_kind,"vendorId":platform,"config":config})
        })
        .unwrap_or(Value::Null);
    Ok(Json(ApiResponse::new(data)))
}

pub async fn version() -> Json<ApiResponse<&'static str>> {
    Json(ApiResponse::new(env!("CARGO_PKG_VERSION")))
}

#[derive(Deserialize)]
pub struct ExtractStyleRequest {
    images: Vec<String>,
}

pub async fn extract_style_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ExtractStyleRequest>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.images.is_empty() {
        return Err(AppError::bad_request("请至少提供一张图片"));
    }
    let content = request
        .images
        .into_iter()
        .map(|image| json!({"type":"image_url","image_url":{"url":image}}))
        .collect::<Vec<_>>();
    let raw = crate::ai_client::text_tools(
        &state.pool,
        "universalAi",
        vec![
            json!({"role":"system","content":"综合分析图片共同画风，只输出一个括号包裹的中英文画风提示词，格式：(画风：中文描述, idiomatic English style prompt)。无法识别时输出无法描述。"}),
            json!({"role":"user","content":content}),
        ],
        Vec::new(),
    )
    .await
    .map_err(AppError::bad_request)?;
    let result = raw
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if result.is_empty() {
        return Err(AppError::bad_request("模型未返回画风提示词"));
    }
    Ok(Json(ApiResponse::new(result)))
}

use crate::{
    ToonState,
    shared::{affected, require},
};

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ArtStyle {
    id: i64,
    name: String,
    file_url: String,
    label: String,
    prompt: String,
    create_time: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveArtStyle {
    id: Option<i64>,
    name: String,
    #[serde(default)]
    file_url: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    prompt: String,
}

pub async fn list_art_styles(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<ArtStyle>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, ArtStyle>("SELECT id, name, file_url, label, prompt, create_time FROM toonflow.art_styles ORDER BY create_time DESC")
        .fetch_all(&state.pool).await.map_err(|_| AppError::internal("failed to list art styles"))?;
    Ok(Json(ApiResponse::new(rows)))
}

pub async fn save_art_style(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveArtStyle>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    sqlx::query("INSERT INTO toonflow.art_styles (id,name,file_url,label,prompt) VALUES (coalesce($1::bigint, nextval('toonflow.art_styles_id_seq')),$2,$3,$4,$5) ON CONFLICT (id) DO UPDATE SET name=excluded.name,file_url=excluded.file_url,label=excluded.label,prompt=excluded.prompt")
        .bind(request.id).bind(request.name).bind(request.file_url).bind(request.label).bind(request.prompt)
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to save art style"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn delete_art_style(
    user: CurrentUser,
    State(state): State<ToonState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let result = sqlx::query("DELETE FROM toonflow.art_styles WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to delete art style"))?;
    affected(result.rows_affected(), "art style")?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(serialize_with = "serialize_task_id")]
    id: i64,
    project_id: Option<i64>,
    project_name: Option<String>,
    task_class: String,
    related_objects: String,
    model: String,
    description: String,
    state: String,
    start_time: Option<i64>,
    reason: Option<String>,
    input: serde_json::Value,
    #[serde(serialize_with = "serialize_optional_task_id")]
    retry_of_id: Option<i64>,
    progress_current: Option<i32>,
    progress_total: Option<i32>,
}

pub async fn list_tasks(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<Task>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, Task>("SELECT t.id,t.project_id,p.name project_name,t.task_class,coalesce(n.chapter,t.related_objects) related_objects,coalesce(mc.name,t.model) model,t.description,t.state,t.start_time,t.reason,t.input,t.retry_of_id,t.progress_current,t.progress_total FROM toonflow.tasks t LEFT JOIN toonflow.projects p ON p.id=t.project_id LEFT JOIN toonflow.agent_deployments d ON d.key=t.model LEFT JOIN ai.model_configs mc ON mc.id=coalesce(d.model_config_id,CASE WHEN t.model ~ '^[0-9]+$' THEN t.model::bigint END) LEFT JOIN toonflow.novels n ON t.task_class='novelEvent' AND n.id::text=t.related_objects WHERE t.task_class<>'text' ORDER BY t.start_time DESC NULLS LAST,t.id DESC")
        .fetch_all(&state.pool).await.map_err(|_| AppError::internal("failed to list tasks"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskQuery {
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_limit")]
    limit: i64,
    task_class: Option<String>,
    state: Option<String>,
    project_id: Option<i64>,
}

fn default_page() -> i64 {
    1
}
fn default_limit() -> i64 {
    10
}

#[derive(Debug, Serialize)]
pub struct TaskPage {
    data: Vec<Task>,
    total: i64,
    stats: TaskStats,
}

#[derive(Debug, Serialize, FromRow)]
pub struct TaskStats {
    total: i64,
    running: i64,
    success: i64,
    failed: i64,
}

pub async fn query_tasks(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<TaskQuery>,
) -> Result<Json<ApiResponse<TaskPage>>, AppError> {
    require(&user, "toon:project:read")?;
    load_task_page(&state.pool, request)
        .await
        .map(|page| Json(ApiResponse::new(page)))
}

async fn load_task_page(pool: &sqlx::PgPool, request: TaskQuery) -> Result<TaskPage, AppError> {
    let page = request.page.max(1);
    let limit = request.limit.clamp(1, 100);
    let task_class = request.task_class.filter(|value| !value.is_empty());
    let task_state = request
        .state
        .filter(|value| !value.is_empty())
        .map(|value| match value.to_lowercase().as_str() {
            "success" => "completed".to_string(),
            "error" => "failed".to_string(),
            _ => value.to_lowercase(),
        });
    let rows = sqlx::query_as::<_, Task>(
        r#"SELECT t.id,t.project_id,p.name project_name,t.task_class,
                  coalesce(n.chapter,t.related_objects) related_objects,
                  coalesce(mc.name,t.model) model,t.description,t.state,t.start_time,t.reason,t.input,t.retry_of_id,t.progress_current,t.progress_total
           FROM toonflow.tasks t
           LEFT JOIN toonflow.projects p ON p.id=t.project_id
           LEFT JOIN toonflow.agent_deployments d ON d.key=t.model
           LEFT JOIN ai.model_configs mc ON mc.id=coalesce(d.model_config_id,CASE WHEN t.model ~ '^[0-9]+$' THEN t.model::bigint END)
           LEFT JOIN toonflow.novels n ON t.task_class='novelEvent' AND n.id::text=t.related_objects
           WHERE t.task_class<>'text'
             AND ($1::text IS NULL OR t.task_class=$1)
             AND ($2::text IS NULL OR CASE lower(t.state)
                  WHEN 'success' THEN 'completed' WHEN 'error' THEN 'failed'
                  ELSE lower(t.state) END=$2)
             AND ($3::bigint IS NULL OR t.project_id=$3)
           ORDER BY t.start_time DESC NULLS LAST,t.id DESC OFFSET $4 LIMIT $5"#,
    )
    .bind(&task_class)
    .bind(&task_state)
    .bind(request.project_id)
    .bind((page - 1).saturating_mul(limit))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to query tasks"))?;
    let total: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM toonflow.tasks WHERE task_class<>'text'
         AND ($1::text IS NULL OR task_class=$1)
         AND ($2::text IS NULL OR CASE lower(state) WHEN 'success' THEN 'completed'
              WHEN 'error' THEN 'failed' ELSE lower(state) END=$2)
         AND ($3::bigint IS NULL OR project_id=$3)",
    )
    .bind(&task_class)
    .bind(task_state)
    .bind(request.project_id)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::internal("failed to count tasks"))?;
    // Summary cards cover all states for the selected project/category, not just
    // the current page or selected state.
    let stats = sqlx::query_as::<_, TaskStats>(
        "SELECT count(*) AS total,
           count(*) FILTER (WHERE lower(state)='running') AS running,
           count(*) FILTER (WHERE lower(state) IN ('success','completed')) AS success,
           count(*) FILTER (WHERE lower(state) IN ('failed','error')) AS failed
         FROM toonflow.tasks WHERE task_class<>'text'
           AND ($1::text IS NULL OR task_class=$1)
           AND ($2::bigint IS NULL OR project_id=$2)",
    )
    .bind(task_class)
    .bind(request.project_id)
    .fetch_one(pool)
    .await
    .map_err(|_| AppError::internal("failed to summarize tasks"))?;
    Ok(TaskPage {
        data: rows,
        total: total.0,
        stats,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskId {
    #[serde(deserialize_with = "deserialize_task_id")]
    task_id: i64,
}

// Snowflake task IDs exceed JavaScript's safe integer range. Keep their
// decimal representation intact across JSON, while accepting older callers.
fn serialize_task_id<S: serde::Serializer>(id: &i64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(id)
}

fn serialize_optional_task_id<S: serde::Serializer>(
    id: &Option<i64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    id.map(|value| value.to_string()).serialize(serializer)
}

fn deserialize_task_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Text(String),
        Number(i64),
    }
    match Id::deserialize(deserializer)? {
        Id::Text(value) => value.parse().map_err(serde::de::Error::custom),
        Id::Number(value) => Ok(value),
    }
}

#[cfg(test)]
mod task_pagination_tests {
    use super::*;

    #[test]
    fn task_ids_survive_browser_json_round_trips() {
        let id = 1_789_003_357_277_804_801_i64;
        let task = Task {
            id,
            project_id: None,
            project_name: None,
            task_class: "image".into(),
            related_objects: String::new(),
            model: String::new(),
            description: String::new(),
            state: "completed".into(),
            start_time: None,
            reason: None,
            input: serde_json::json!({}),
            retry_of_id: Some(id - 1),
            progress_current: None,
            progress_total: None,
        };
        let json = serde_json::to_value(task).unwrap();
        assert_eq!(json["id"], id.to_string());
        assert_eq!(json["retryOfId"], (id - 1).to_string());
        let request: TaskId =
            serde_json::from_value(serde_json::json!({"taskId": json["id"]})).unwrap();
        assert_eq!(request.task_id, id);
        assert_eq!(
            serde_json::from_value::<TaskId>(serde_json::json!({"taskId": 42}))
                .unwrap()
                .task_id,
            42
        );
        assert!(
            serde_json::from_value::<TaskId>(serde_json::json!({"taskId": "invalid"})).is_err()
        );
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn filters_pages_and_summarizes_legacy_task_states() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        // A single connection keeps every fixture inside one rolled-back transaction.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query("BEGIN").execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO toonflow.projects(id,name,create_time,update_time)
            VALUES(-91001,'pagination A',0,0),(-91002,'pagination B',0,0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.tasks(id,project_id,task_class,state,start_time) VALUES
            (-91101,-91001,'pagination-test','success',100),
            (-91102,-91001,'pagination-test','COMPLETED',100),
            (-91103,-91001,'pagination-test','error',100),
            (-91104,-91001,'pagination-test','failed',100),
            (-91105,-91001,'pagination-test','running',100),
            (-91106,-91002,'pagination-test','success',100),
            (-91107,-91001,'other-test','success',100),
            (-91108,-91001,'text','success',100)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (filter, expected) in [
            ("completed", vec![-91101, -91102]),
            ("ERROR", vec![-91103, -91104]),
        ] {
            for (index, id) in expected.into_iter().enumerate() {
                let result = load_task_page(
                    &pool,
                    TaskQuery {
                        page: index as i64 + 1,
                        limit: 1,
                        task_class: Some("pagination-test".into()),
                        state: Some(filter.into()),
                        project_id: Some(-91001),
                    },
                )
                .await
                .unwrap();
                assert_eq!(result.total, 2);
                assert_eq!(result.data.len(), 1);
                assert_eq!(result.data[0].id, id);
                assert_eq!(
                    (
                        result.stats.total,
                        result.stats.running,
                        result.stats.success,
                        result.stats.failed
                    ),
                    (5, 1, 2, 2)
                );
            }
        }
        sqlx::query("ROLLBACK").execute(&pool).await.unwrap();
        pool.close().await;
    }
}

pub async fn task_details(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<TaskId>,
) -> Result<Json<ApiResponse<Option<Task>>>, AppError> {
    require(&user, "toon:project:read")?;
    let row = sqlx::query_as::<_, Task>("SELECT t.id,t.project_id,p.name project_name,t.task_class,coalesce(n.chapter,t.related_objects) related_objects,coalesce(mc.name,t.model) model,t.description,t.state,t.start_time,t.reason,t.input,t.retry_of_id,t.progress_current,t.progress_total FROM toonflow.tasks t LEFT JOIN toonflow.projects p ON p.id=t.project_id LEFT JOIN toonflow.agent_deployments d ON d.key=t.model LEFT JOIN ai.model_configs mc ON mc.id=coalesce(d.model_config_id,CASE WHEN t.model ~ '^[0-9]+$' THEN t.model::bigint END) LEFT JOIN toonflow.novels n ON t.task_class='novelEvent' AND n.id::text=t.related_objects WHERE t.id=$1 AND t.task_class<>'text'")
        .bind(request.task_id).fetch_optional(&state.pool).await.map_err(|_| AppError::internal("failed to get task"))?;
    Ok(Json(ApiResponse::new(row)))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TaskCategory {
    task_class: String,
}

pub async fn task_categories(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<TaskCategory>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, TaskCategory>(
        "SELECT DISTINCT task_class FROM toonflow.tasks WHERE task_class<>'' AND task_class<>'text' ORDER BY task_class",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list task categories"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Serialize, FromRow)]
pub struct TaskProject {
    id: i64,
    name: String,
}

pub async fn task_projects(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<TaskProject>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, TaskProject>(
        "SELECT id,name FROM toonflow.projects WHERE name<>'' ORDER BY name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list task projects"))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    id: i64,
    name: String,
    #[serde(rename = "type")]
    type_: String,
    data: String,
    use_data: Option<String>,
    source_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SavePrompt {
    id: Option<i64>,
    name: String,
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    data: String,
    #[serde(rename = "useData")]
    use_data: Option<String>,
    #[serde(rename = "sourceKey")]
    source_key: Option<String>,
}

pub async fn list_prompts(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<Prompt>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, Prompt>(
        "SELECT id,name,type as type_,data,use_data,source_key
         FROM toonflow.prompts
         ORDER BY CASE source_key
           WHEN 'eventExtraction' THEN 0
           WHEN 'scriptAssetExtraction' THEN 1
           WHEN 'videoPromptGeneration' THEN 2
           WHEN 'audioBindPrompt' THEN 3
           ELSE 10
         END, id DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list prompts"))?;
    Ok(Json(ApiResponse::new(rows)))
}

pub async fn save_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SavePrompt>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let source_key = request.source_key.filter(|value| !value.trim().is_empty());
    sqlx::query("INSERT INTO toonflow.prompts (id,name,type,data,use_data,source_key) VALUES (coalesce($1,nextval('toonflow.prompts_id_seq')),$2,$3,$4,$5,$6) ON CONFLICT (id) DO UPDATE SET name=excluded.name,type=excluded.type,data=excluded.data,use_data=excluded.use_data,source_key=excluded.source_key")
        .bind(request.id).bind(request.name).bind(request.type_).bind(request.data).bind(request.use_data).bind(source_key).execute(&state.pool).await.map_err(|_| AppError::internal("failed to save prompt"))?;
    Ok(Json(ApiResponse::new(())))
}

pub async fn delete_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    let result = sqlx::query("DELETE FROM toonflow.prompts WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to delete prompt"))?;
    affected(result.rows_affected(), "prompt")?;
    Ok(Json(ApiResponse::new(())))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    id: String,
    name: String,
    description: String,
    #[serde(rename = "type")]
    type_: String,
    path: String,
    state: i32,
    create_time: i64,
    update_time: i64,
}

pub async fn list_skills(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<Skill>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, Skill>("SELECT id,name,description,type as type_,path,state,create_time,update_time FROM toonflow.skill_list ORDER BY update_time DESC").fetch_all(&state.pool).await.map_err(|_| AppError::internal("failed to list skills"))?;
    Ok(Json(ApiResponse::new(rows)))
}

pub async fn skill_paths(
    user: CurrentUser,
    State(state): State<ToonState>,
) -> Result<Json<ApiResponse<Vec<String>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows: Vec<(String,)> = sqlx::query_as("SELECT path FROM toonflow.skill_list ORDER BY path")
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to list skill paths"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter().map(|row| row.0).collect(),
    )))
}

#[derive(Debug, Deserialize)]
pub struct SkillPathRequest {
    path: String,
}
pub async fn skill_content(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SkillPathRequest>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:project:read")?;
    let content: Option<(String,)> =
        sqlx::query_as("SELECT content FROM toonflow.skill_list WHERE path=$1")
            .bind(request.path)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get skill content"))?;
    Ok(Json(ApiResponse::new(
        content.map(|row| row.0).unwrap_or_default(),
    )))
}

#[derive(Debug, Deserialize)]
pub struct SaveSkillContentRequest {
    path: String,
    content: String,
}
pub async fn save_skill_content(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<SaveSkillContentRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.path.contains("..")
        || request.path.starts_with('/')
        || request.path.starts_with('\\')
    {
        return Err(AppError::bad_request("无效的路径"));
    }
    let result =
        sqlx::query("UPDATE toonflow.skill_list SET content=$2,update_time=$3 WHERE path=$1")
            .bind(request.path)
            .bind(request.content)
            .bind(chrono::Utc::now().timestamp_millis())
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to save skill content"))?;
    affected(result.rows_affected(), "skill")?;
    Ok(Json(ApiResponse::new(())))
}
