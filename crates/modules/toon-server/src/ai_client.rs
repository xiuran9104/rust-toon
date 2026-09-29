use serde_json::Value;
use sqlx::PgPool;
use std::sync::atomic::{AtomicI64, Ordering};

static TASK_SEQUENCE: AtomicI64 = AtomicI64::new(0);

fn normalized_app_error(error: rust_toon_framework_web::AppError) -> String {
    if let Some(data) = error.data()
        && data.get("code").is_some()
        && data.get("category").is_some()
    {
        return data.to_string();
    }
    serde_json::json!({
        "code": format!("AI_REQUEST_{}", error.code()),
        "category": if error.status().is_server_error() { "internal" } else { "request" },
        "message": error.message(),
        "status": error.status().as_u16(),
        "retryable": error.status().is_server_error(),
    })
    .to_string()
}

fn task_id() -> i64 {
    chrono::Utc::now().timestamp_millis() * 1000
        + TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed) % 1000
}

fn should_record_task(task_class: &str) -> bool {
    task_class != "text"
}

async fn recorded<T, F>(
    pool: &PgPool,
    task_class: &str,
    model: &str,
    description: &str,
    future: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    recorded_with_context(pool, None, None, task_class, model, description, future).await
}

async fn recorded_with_context<T, F>(
    pool: &PgPool,
    project_id: Option<i64>,
    progress_total: Option<i32>,
    task_class: &str,
    model: &str,
    description: &str,
    future: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    // Text calls are implementation details of domain workflows and Agent runs.
    // Persisting every nested model request creates duplicate, user-visible
    // tasks such as `universalAi` in addition to the actual workflow task.
    if !should_record_task(task_class) {
        return future.await;
    }
    let id = task_id();
    sqlx::query("INSERT INTO toonflow.tasks(id,project_id,task_class,model,description,state,start_time,progress_current,progress_total) VALUES($1,$2,$3,$4,$5,'running',$6,0,$7)")
        .bind(id).bind(project_id).bind(task_class).bind(model).bind(description).bind(chrono::Utc::now().timestamp_millis()).bind(progress_total)
        .execute(pool).await.map_err(|error| format!("创建 AI 任务记录失败：{error}"))?;
    match future.await {
        Ok(value) => {
            sqlx::query("UPDATE toonflow.tasks SET state='success',progress_current=coalesce(progress_total,progress_current),reason=NULL WHERE id=$1")
                .bind(id)
                .execute(pool)
                .await
                .map_err(|error| format!("更新 AI 任务记录失败：{error}"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = sqlx::query("UPDATE toonflow.tasks SET state='failed',reason=$2 WHERE id=$1")
                .bind(id)
                .bind(&error)
                .execute(pool)
                .await;
            Err(error)
        }
    }
}

/// Records a generated image together with its result so the task center can
/// render the image instead of treating it as a text-only task.
async fn recorded_image_with_context<F>(
    pool: &PgPool,
    project_id: Option<i64>,
    model: &str,
    input: Value,
    future: F,
) -> Result<String, String>
where
    F: std::future::Future<Output = Result<String, String>>,
{
    let id = task_id();
    sqlx::query("INSERT INTO toonflow.tasks(id,project_id,task_class,model,description,state,start_time,progress_current,progress_total,input) VALUES($1,$2,'image',$3,'图片生成','running',$4,0,1,$5)")
        .bind(id)
        .bind(project_id)
        .bind(model)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(input)
        .execute(pool)
        .await
        .map_err(|error| format!("创建 AI 任务记录失败：{error}"))?;
    match future.await {
        Ok(value) => {
            let result = serde_json::json!({"url": &value}).to_string();
            sqlx::query("UPDATE toonflow.tasks SET state='success',related_objects=CASE WHEN length($2)<=255 THEN $2 ELSE related_objects END,progress_current=1,reason=NULL WHERE id=$1")
                .bind(id)
                .bind(result)
                .execute(pool)
                .await
                .map_err(|error| format!("更新 AI 任务记录失败：{error}"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = sqlx::query("UPDATE toonflow.tasks SET state='failed',reason=$2 WHERE id=$1")
                .bind(id)
                .bind(&error)
                .execute(pool)
                .await;
            Err(error)
        }
    }
}

async fn project_agent_model(
    pool: &PgPool,
    key: &str,
    project_id: i64,
) -> Result<(i64, i32, i32), String> {
    let row: Option<(Option<i64>, i32, i32, bool)> = sqlx::query_as(
        "SELECT coalesce(p.chat_model,d.model_config_id),d.temperature,d.max_output_tokens,d.disabled
         FROM toonflow.agent_deployments d
         LEFT JOIN toonflow.projects p ON p.id=$2
         WHERE d.key=$1",
    )
    .bind(key)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let (model, temperature, tokens, disabled) =
        row.ok_or_else(|| format!("Agent {key} 未配置"))?;
    if disabled {
        return Err(format!("Agent {key} 已停用"));
    }
    Ok((
        model.ok_or_else(|| format!("Agent {key} 尚未绑定对话模型"))?,
        temperature,
        tokens,
    ))
}

pub async fn project_model_id(pool: &PgPool, key: &str, project_id: i64) -> Result<i64, String> {
    project_agent_model(pool, key, project_id)
        .await
        .map(|(model, _, _)| model)
}

async fn agent_model(pool: &PgPool, key: &str) -> Result<(i64, i32, i32), String> {
    let row: Option<(Option<i64>, i32, i32, bool)> = sqlx::query_as(
        "SELECT model_config_id,temperature,max_output_tokens,disabled FROM toonflow.agent_deployments WHERE key=$1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let (model, temperature, tokens, disabled) =
        row.ok_or_else(|| format!("Agent {key} 未配置"))?;
    if disabled {
        return Err(format!("Agent {key} 已停用"));
    }
    Ok((
        model.ok_or_else(|| format!("Agent {key} 尚未绑定统一 AI 模型"))?,
        temperature,
        tokens,
    ))
}

pub async fn text_tools(
    pool: &PgPool,
    key: &str,
    messages: Vec<Value>,
    tools: Vec<Value>,
) -> Result<Value, String> {
    let (model, temperature, tokens) = agent_model(pool, key).await?;
    recorded(pool, "text", &model.to_string(), key, async move {
        rust_toon_ai_server::AiModelFactory::new(pool.clone())
            .chat_tools(
                model,
                messages,
                tools,
                Some(temperature as f64),
                (tokens > 0).then_some(tokens as u32),
            )
            .await
            .map_err(normalized_app_error)
    })
    .await
}

pub async fn project_text_tools(
    pool: &PgPool,
    key: &str,
    project_id: i64,
    messages: Vec<Value>,
    tools: Vec<Value>,
) -> Result<Value, String> {
    let (model, temperature, tokens) = project_agent_model(pool, key, project_id).await?;
    recorded_with_context(
        pool,
        Some(project_id),
        None,
        "text",
        &model.to_string(),
        key,
        async move {
            rust_toon_ai_server::AiModelFactory::new(pool.clone())
                .chat_tools(
                    model,
                    messages,
                    tools,
                    Some(temperature as f64),
                    (tokens > 0).then_some(tokens as u32),
                )
                .await
                .map_err(normalized_app_error)
        },
    )
    .await
}

pub async fn project_text_tools_stream<F, Fut>(
    pool: &PgPool,
    key: &str,
    project_id: i64,
    messages: Vec<Value>,
    tools: Vec<Value>,
    on_delta: F,
) -> Result<Value, String>
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let (model, temperature, tokens) = project_agent_model(pool, key, project_id).await?;
    recorded_with_context(
        pool,
        Some(project_id),
        None,
        "text",
        &model.to_string(),
        key,
        async move {
            rust_toon_ai_server::AiModelFactory::new(pool.clone())
                .chat_tools_stream(
                    model,
                    messages,
                    tools,
                    Some(temperature as f64),
                    (tokens > 0).then_some(tokens as u32),
                    on_delta,
                )
                .await
                .map_err(normalized_app_error)
        },
    )
    .await
}
fn chat_request(
    system: &str,
    user: &str,
    temperature: i32,
    tokens: i32,
) -> rust_toon_ai_api::ChatRequest {
    rust_toon_ai_api::ChatRequest {
        model: String::new(),
        messages: vec![
            rust_toon_ai_api::ChatMessage {
                role: "system".into(),
                content: system.into(),
            },
            rust_toon_ai_api::ChatMessage {
                role: "user".into(),
                content: user.into(),
            },
        ],
        temperature: Some(temperature as f64),
        max_tokens: (tokens > 0).then_some(tokens as u32),
        ..Default::default()
    }
}
pub async fn project_text(
    pool: &PgPool,
    key: &str,
    project_id: i64,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let (model, temperature, tokens) = project_agent_model(pool, key, project_id).await?;
    recorded_with_context(
        pool,
        Some(project_id),
        None,
        "text",
        &model.to_string(),
        key,
        async move {
            rust_toon_ai_server::AiModelFactory::new(pool.clone())
                .chat(model, chat_request(system, user, temperature, tokens))
                .await
                .map(|response| response.content)
                .map_err(normalized_app_error)
        },
    )
    .await
}

/// Executes a project-scoped text request without creating a second generic
/// `text` task. Domain workflows should create their own descriptive task
/// (for example, chapter event extraction) and use this helper for the nested
/// model call.
pub async fn project_text_untracked(
    pool: &PgPool,
    key: &str,
    project_id: i64,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let (model, temperature, tokens) = project_agent_model(pool, key, project_id).await?;
    rust_toon_ai_server::AiModelFactory::new(pool.clone())
        .chat(model, chat_request(system, user, temperature, tokens))
        .await
        .map(|response| response.content)
        .map_err(normalized_app_error)
}

pub async fn text(pool: &PgPool, key: &str, system: &str, user: &str) -> Result<String, String> {
    let (model, temperature, tokens) = agent_model(pool, key).await?;
    recorded(pool, "text", &model.to_string(), key, async move {
        rust_toon_ai_server::AiModelFactory::new(pool.clone())
            .chat(model, chat_request(system, user, temperature, tokens))
            .await
            .map(|x| x.content)
            .map_err(normalized_app_error)
    })
    .await
}
pub async fn project_text_stream<F, Fut>(
    pool: &PgPool,
    key: &str,
    project_id: i64,
    system: &str,
    user: &str,
    on_delta: F,
) -> Result<String, String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let (model, temperature, tokens) = project_agent_model(pool, key, project_id).await?;
    recorded_with_context(
        pool,
        Some(project_id),
        None,
        "text",
        &model.to_string(),
        key,
        async move {
            rust_toon_ai_server::AiModelFactory::new(pool.clone())
                .chat_stream(
                    model,
                    chat_request(system, user, temperature, tokens),
                    on_delta,
                )
                .await
                .map(|x| x.content)
                .map_err(normalized_app_error)
        },
    )
    .await
}
fn model_id(value: &str, kind: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("{kind}模型必须是统一 AI 模型 ID"))
}
/// Generates an image with ordered visual references when the configured provider supports edits.
#[cfg(test)]
pub async fn image_with_references(
    pool: &PgPool,
    configured: &str,
    prompt: &str,
    size: &str,
    references: Vec<String>,
) -> Result<String, String> {
    image_with_references_for_project(pool, None, configured, prompt, size, references, false).await
}

pub async fn image_with_references_for_project(
    pool: &PgPool,
    project_id: Option<i64>,
    configured: &str,
    prompt: &str,
    size: &str,
    references: Vec<String>,
    role_sheet: bool,
) -> Result<String, String> {
    image_with_provenance_for_project(
        pool, project_id, configured, prompt, size, references, role_sheet, None,
    )
    .await
}

pub(crate) async fn image_with_provenance_for_project(
    pool: &PgPool,
    project_id: Option<i64>,
    configured: &str,
    prompt: &str,
    size: &str,
    references: Vec<String>,
    role_sheet: bool,
    provenance: Option<Value>,
) -> Result<String, String> {
    let model = model_id(configured, "图片")?;
    let canvas = crate::toonflow_image_contract::ImageCanvas::parse(size)?;
    let mut references = references;
    let prompt = if role_sheet {
        references.push(crate::toonflow_image_contract::role_layout_reference(
            canvas.width as f64 / canvas.height as f64 > 1.75,
        )?);
        format!(
            "{prompt}\n最后一张参考图仅为全身构图控制图：严格采用每个人物从头顶到鞋底的完整占位、人物相对画布的大小以及上下留白，人物高度不得超出该占位。灰色轮廓不含角色身份、服装或画风，也不规定物种和体型，非人类角色仍按原物种完整呈现。不得复制灰色人形、线条或人台外观；面貌与服装完全按当前角色文字设定和其他人物参考图绘制。每一列都必须看见完整双腿和鞋底，不能放大为半身。"
        )
    } else {
        prompt.to_string()
    };
    let input = crate::toonflow_prompt_trace::image_input(
        &prompt,
        size,
        references.len(),
        role_sheet,
        provenance,
    );
    recorded_image_with_context(pool, project_id, &model.to_string(), input, async move {
        let mut last_error = String::new();
        for attempt in 1..=3 {
            match rust_toon_ai_server::AiModelFactory::new(pool.clone())
                .image(
                    model,
                    rust_toon_ai_api::ImageRequest {
                        prompt: prompt.clone(),
                        size: size.into(),
                        references: references.clone(),
                    },
                )
                .await
            {
                Ok(response) => {
                    return crate::toonflow_storage::validate_and_persist_generated_image(
                        &response.url,
                        project_id,
                        canvas,
                        role_sheet,
                    )
                    .await;
                }
                Err(error) => {
                    last_error = normalized_app_error(error);
                    if attempt == 3 || !is_transient_model_error(&last_error) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(attempt as u64 * 2)).await;
                }
            }
        }
        Err(last_error)
    })
    .await
}

fn is_transient_model_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "rate limit",
        "429",
        "too many requests",
        "timeout",
        "timed out",
        "connection",
        "temporarily unavailable",
        "502",
        "503",
        "504",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}
/// A provider-side video task handle. `url` is present when the model
/// responded synchronously; otherwise `task_id` must be polled.
pub struct VideoSubmission {
    pub url: Option<String>,
    pub task_id: Option<String>,
}

/// Submit a video generation request and return the provider handle without
/// polling. Callers persist `task_id` before polling so a Gateway restart can
/// resume the paid task instead of failing or resubmitting it.
pub async fn video_submit(
    pool: &PgPool,
    configured: &str,
    payload: Value,
) -> Result<VideoSubmission, String> {
    let model_id = validate_video_request(pool, configured, &payload).await?;
    let factory = rust_toon_ai_server::AiModelFactory::new(pool.clone());
    let response = factory
        .video(model_id, payload)
        .await
        .map_err(normalized_app_error)?;
    Ok(VideoSubmission {
        url: (!response.url.is_empty()).then_some(response.url),
        task_id: response.task_id,
    })
}

/// Poll a previously submitted provider video task until it yields a URL,
/// fails, or times out. Used both right after submission and when resuming
/// interrupted generations after a Gateway restart.
pub async fn video_poll_task(
    pool: &PgPool,
    configured: &str,
    task_id: &str,
) -> Result<String, String> {
    let model_id = model_id(configured, "视频")?;
    let factory = rust_toon_ai_server::AiModelFactory::new(pool.clone());
    let interval = std::env::var("AI_VIDEO_POLL_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1);
    let timeout = std::env::var("AI_VIDEO_POLL_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600)
        .max(interval);
    for _ in 0..timeout.div_ceil(interval) {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let result = factory
            .poll_video(model_id, task_id)
            .await
            .map_err(normalized_app_error)?;
        if !result.url.is_empty() {
            return Ok(result.url);
        }
        let state = result
            .raw
            .pointer("/status")
            .or_else(|| result.raw.pointer("/state"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            state.as_str(),
            "failed" | "error" | "cancelled" | "canceled"
        ) {
            let reason = [
                "/error/message",
                "/error",
                "/message",
                "/failReason",
                "/data/error",
            ]
            .iter()
            .find_map(|path| result.raw.pointer(path).and_then(Value::as_str))
            .unwrap_or("上游视频任务失败");
            return Err(format!("视频任务 {task_id} 失败：{reason}"));
        }
    }
    Err(format!("视频任务 {task_id} 等待超时"))
}

async fn validate_video_request(
    pool: &PgPool,
    configured: &str,
    payload: &Value,
) -> Result<i64, String> {
    let model_id = model_id(configured, "视频")?;
    let config = rust_toon_ai_server::AiModelFactory::new(pool.clone())
        .config(model_id)
        .await
        .map_err(normalized_app_error)?;
    let capabilities = config.capabilities();
    validate_video_payload(&config.name, &config.model, &capabilities, payload)?;
    Ok(model_id)
}

fn validate_video_payload(
    model_name: &str,
    model: &str,
    capabilities: &rust_toon_ai_api::ModelCapabilities,
    payload: &Value,
) -> Result<(), String> {
    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !capabilities.video_modes.is_empty()
        && !mode.is_empty()
        && !capabilities.video_modes.iter().any(|value| value == mode)
    {
        return Err(format!("模型 {model_name} 不支持视频模式 {mode}"));
    }
    let reference_count = payload
        .get("references")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let expected_references = match mode {
        "text" => None,
        "singleImage" => Some((1, 1)),
        "startEndRequired" => Some((2, 2)),
        "endFrameOptional" | "startFrameOptional" => Some((1, 2)),
        "" => None,
        _ => return Err(format!("未知的视频生成模式：{mode}")),
    };
    if let Some((minimum, maximum)) = expected_references
        && !(minimum..=maximum).contains(&reference_count)
    {
        return Err(format!(
            "视频模式 {mode} 需要 {minimum} 至 {maximum} 张参考图，实际 {reference_count} 张"
        ));
    }
    let maximum_references = capabilities.video_max_references.unwrap_or(4);
    if reference_count > maximum_references {
        return Err(format!(
            "模型 {model_name} 最多支持 {maximum_references} 张参考图，实际 {reference_count} 张"
        ));
    }
    let duration = payload.get("duration").and_then(Value::as_i64);
    let resolution = payload.get("resolution").and_then(Value::as_str);
    if let Some(duration) = duration {
        let inferred_seedance_1_5 = model.to_ascii_lowercase().contains("seedance-1-5");
        let minimum = capabilities
            .video_min_duration
            .or(inferred_seedance_1_5.then_some(4));
        let maximum = capabilities
            .video_max_duration
            .or(inferred_seedance_1_5.then_some(12));
        if minimum.is_some_and(|minimum| duration < minimum)
            || maximum.is_some_and(|maximum| duration > maximum)
        {
            return Err(format!(
                "模型 {model_name} 不支持 {duration} 秒视频，允许范围为 {} 至 {} 秒",
                minimum.map_or_else(|| "未限制".into(), |value| value.to_string()),
                maximum.map_or_else(|| "未限制".into(), |value| value.to_string()),
            ));
        }
    }
    if let Some(resolution) = resolution
        && !capabilities.video_resolutions.is_empty()
        && !capabilities
            .video_resolutions
            .iter()
            .any(|value| value == resolution)
    {
        return Err(format!("模型 {model_name} 不支持分辨率 {resolution}"));
    }
    if let (Some(duration), Some(resolution), Some(allowed)) = (
        duration,
        resolution,
        duration.and_then(|value| capabilities.duration_resolution_map.get(&value.to_string())),
    ) && !allowed.is_empty()
        && !allowed.iter().any(|value| value == resolution)
    {
        return Err(format!(
            "模型 {model_name} 在 {duration} 秒时不支持分辨率 {resolution}"
        ));
    }
    if payload.get("audio").and_then(Value::as_bool) == Some(true)
        && capabilities.video_audio == Some(false)
    {
        return Err(format!("模型 {model_name} 不支持同步生成音频"));
    }
    Ok(())
}

pub async fn speech(
    pool: &PgPool,
    configured: &str,
    input: &str,
    voice: &str,
) -> Result<String, String> {
    let model = model_id(configured, "TTS")?;
    recorded(pool, "speech", &model.to_string(), "语音生成", async move {
        rust_toon_ai_server::AiModelFactory::new(pool.clone())
            .speech(
                model,
                rust_toon_ai_api::SpeechRequest {
                    input: input.into(),
                    voice: voice.into(),
                    format: "mp3".into(),
                },
            )
            .await
            .map(|x| x.url)
            .map_err(normalized_app_error)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{is_transient_model_error, should_record_task, validate_video_payload};
    use rust_toon_ai_api::ModelCapabilities;
    use serde_json::json;

    #[test]
    fn nested_text_requests_are_not_user_visible_tasks() {
        assert!(!should_record_task("text"));
        assert!(should_record_task("image"));
        assert!(should_record_task("speech"));
    }

    #[test]
    fn retries_rate_limits_and_network_failures() {
        assert!(is_transient_model_error("rate limit exceeded"));
        assert!(is_transient_model_error("HTTP 429 Too Many Requests"));
        assert!(is_transient_model_error("connection timed out"));
        assert!(is_transient_model_error("upstream returned 503"));
    }

    #[test]
    fn does_not_retry_prompt_rejections() {
        assert!(!is_transient_model_error(
            "input text may contain sensitive information"
        ));
    }

    #[test]
    fn validates_video_modes_reference_counts_and_audio_capability() {
        let capabilities = ModelCapabilities {
            video_modes: vec!["singleImage".into(), "startEndRequired".into()],
            video_resolutions: vec!["720p".into(), "1080p".into()],
            video_min_duration: Some(4),
            video_max_duration: Some(10),
            video_max_references: Some(2),
            video_audio: Some(false),
            ..Default::default()
        };
        assert!(validate_video_payload(
            "测试视频模型",
            "video-v1",
            &capabilities,
            &json!({
                "mode":"startEndRequired",
                "references":["first","last"],
                "duration":6,
                "resolution":"1080p",
                "audio":false
            }),
        )
        .is_ok());
        assert!(validate_video_payload(
            "测试视频模型",
            "video-v1",
            &capabilities,
            &json!({"mode":"startEndRequired","references":["first"],"duration":6}),
        )
        .unwrap_err()
        .contains("需要 2 至 2 张参考图"));
        assert!(validate_video_payload(
            "测试视频模型",
            "video-v1",
            &capabilities,
            &json!({"mode":"singleImage","references":["first"],"duration":6,"audio":true}),
        )
        .unwrap_err()
        .contains("不支持同步生成音频"));
    }

    #[test]
    fn rejects_configured_resolution_duration_and_seedance_legacy_clamping() {
        let capabilities = ModelCapabilities {
            video_resolutions: vec!["720p".into()],
            video_min_duration: Some(5),
            video_max_duration: Some(8),
            ..Default::default()
        };
        assert!(validate_video_payload(
            "限制模型",
            "video-v1",
            &capabilities,
            &json!({"mode":"text","references":[],"duration":4,"resolution":"720p"}),
        )
        .is_err());
        assert!(validate_video_payload(
            "限制模型",
            "video-v1",
            &capabilities,
            &json!({"mode":"text","references":[],"duration":6,"resolution":"1080p"}),
        )
        .is_err());
        assert!(validate_video_payload(
            "Seedance 1.5",
            "doubao-seedance-1-5-pro",
            &ModelCapabilities::default(),
            &json!({"mode":"text","references":[],"duration":3}),
        )
        .unwrap_err()
        .contains("允许范围为 4 至 12 秒"));
    }
}
