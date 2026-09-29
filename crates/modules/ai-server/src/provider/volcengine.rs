use rust_toon_ai_api::{MediaResponse, ModelConfig};
use serde_json::{Map, Value, json};

pub struct VolcEngineMediaProvider;

const SEEDREAM_MIN_PIXELS: u64 = 3_686_400;

pub(super) fn normalize_seedream_size(model: &str, size: &str) -> String {
    if !model.to_ascii_lowercase().contains("seedream") {
        return size.to_string();
    }
    let Some((width, height)) = size.split_once('x').and_then(|(width, height)| {
        Some((width.parse::<u64>().ok()?, height.parse::<u64>().ok()?))
    }) else {
        return size.to_string();
    };
    if width == 0 || height == 0 || width.saturating_mul(height) >= SEEDREAM_MIN_PIXELS {
        return size.to_string();
    }
    let scale = (SEEDREAM_MIN_PIXELS as f64 / (width * height) as f64).sqrt();
    let align = |value: u64| (((value as f64 * scale).ceil() as u64).div_ceil(32)) * 32;
    format!("{}x{}", align(width), align(height))
}

/// Keep Ark image generation deterministic and aligned with Toonflow-app's
/// Seedream request contract.
pub(super) fn apply_image_generation_options(model: &str, body: &mut Value) {
    body["response_format"] = json!("url");
    body["watermark"] = json!(false);

    let model = model.to_ascii_lowercase();
    if model.contains("seedream") && !model.contains("seedream-3-0") {
        body["sequential_image_generation"] = json!("disabled");
    }
}

fn validate_seedance_duration(model: &str, duration: i64) -> Result<i64, String> {
    if model.to_ascii_lowercase().contains("seedance-1-5") && !(4..=12).contains(&duration) {
        Err(format!(
            "Seedance 1.5 不支持 {duration} 秒视频，允许范围为 4 至 12 秒"
        ))
    } else {
        Ok(duration)
    }
}

impl VolcEngineMediaProvider {
    fn client(&self, config: &ModelConfig) -> reqwest::Client {
        let _ = config;
        super::http_client()
    }

    fn url(config: &ModelConfig, path: &str) -> String {
        format!("{}{}", config.url.trim_end_matches('/'), path)
    }

    fn video_body(config: &ModelConfig, payload: Value) -> Result<Value, String> {
        if payload.get("content").is_some() {
            let mut body = payload;
            body["model"] = json!(config.model);
            return Ok(body);
        }
        let prompt = payload
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Seedance 视频提示词不能为空".to_string())?;
        let mut content = vec![json!({"type":"text","text":prompt})];
        let mode = payload
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("singleImage");
        let reference_count = payload
            .get("references")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if let Some(references) = payload.get("references").and_then(Value::as_array) {
            for (index, reference) in references.iter().filter_map(Value::as_str).enumerate() {
                let role = match mode {
                    "text" => "reference_image",
                    "startFrameOptional" if reference_count == 1 => "last_frame",
                    "startEndRequired" | "endFrameOptional" | "startFrameOptional"
                        if index + 1 == reference_count && reference_count > 1 =>
                    {
                        "last_frame"
                    }
                    _ if index == 0 => "first_frame",
                    _ => "reference_image",
                };
                content.push(json!({
                    "type":"image_url",
                    "image_url":{"url":reference},
                    "role":role
                }));
            }
        }
        let mut body = Map::new();
        body.insert("model".into(), json!(config.model));
        body.insert("content".into(), Value::Array(content));
        for key in [
            "ratio",
            "duration",
            "resolution",
            "seed",
            "generate_audio",
            "watermark",
        ] {
            if let Some(value) = payload.get(key).cloned() {
                let value = if key == "duration" {
                    match value.as_i64() {
                        Some(duration) => {
                            json!(validate_seedance_duration(&config.model, duration)?)
                        }
                        None => value,
                    }
                } else {
                    value
                };
                body.insert(key.into(), value);
            }
        }
        if !body.contains_key("ratio")
            && let Some(value) = payload.get("aspect_ratio").cloned()
        {
            body.insert("ratio".into(), value);
        }
        if !body.contains_key("generate_audio")
            && let Some(value) = payload.get("audio").cloned()
        {
            body.insert("generate_audio".into(), value);
        }
        Ok(Value::Object(body))
    }

    fn response(value: Value, task_id: Option<&str>) -> Result<MediaResponse, String> {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(status, "failed" | "cancelled") {
            let reason = value
                .pointer("/error/message")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("Seedance 视频生成失败");
            return Err(reason.to_string());
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .or(task_id)
            .map(str::to_string);
        let url = value
            .pointer("/content/video_url")
            .or_else(|| value.pointer("/content/videoUrl"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_none() && url.is_empty() {
            return Err("Seedance 响应缺少任务 ID 或视频 URL".into());
        }
        Ok(MediaResponse {
            url,
            task_id: id,
            raw: value,
        })
    }

    pub async fn video(
        &self,
        config: &ModelConfig,
        payload: Value,
    ) -> Result<MediaResponse, String> {
        let path = config
            .config
            .get("videoGeneratePath")
            .and_then(Value::as_str)
            .unwrap_or("/contents/generations/tasks");
        let response = super::send_with_retry(
            self.client(config)
                .post(Self::url(config, path))
                .bearer_auth(config.api_key.trim_start_matches("Bearer "))
                .json(&Self::video_body(config, payload)?),
        )
        .await?;
        let (status, value) = super::response_json(response, "Seedance 视频任务创建失败").await?;
        if !status.is_success() {
            return Err(super::upstream_error(
                status,
                &value,
                "Seedance 视频任务创建失败",
            ));
        }
        Self::response(value, None)
    }

    pub async fn poll_video(
        &self,
        config: &ModelConfig,
        task_id: &str,
    ) -> Result<MediaResponse, String> {
        let template = config
            .config
            .get("videoQueryPath")
            .and_then(Value::as_str)
            .unwrap_or("/contents/generations/tasks/{taskId}");
        let path = template.replace("{taskId}", task_id);
        let response = super::send_with_retry(
            self.client(config)
                .get(Self::url(config, &path))
                .bearer_auth(config.api_key.trim_start_matches("Bearer ")),
        )
        .await?;
        let (status, value) = super::response_json(response, "Seedance 视频任务查询失败").await?;
        if !status.is_success() {
            return Err(super::upstream_error(
                status,
                &value,
                "Seedance 视频任务查询失败",
            ));
        }
        Self::response(value, Some(task_id))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        VolcEngineMediaProvider, apply_image_generation_options, validate_seedance_duration,
        normalize_seedream_size,
    };
    use rust_toon_ai_api::ModelConfig;
    use serde_json::json;

    fn config() -> ModelConfig {
        ModelConfig {
            id: 1,
            name: "Seedance".into(),
            key: "seedance".into(),
            platform: "VolcEngine".into(),
            type_: "video".into(),
            model: "doubao-seedance-2-0-260128".into(),
            api_key: "key".into(),
            url: "https://ark.cn-beijing.volces.com/api/v3".into(),
            status: 1,
            config: json!({}),
        }
    }

    #[test]
    fn raises_small_seedream_sizes_to_the_provider_minimum() {
        assert_eq!(
            normalize_seedream_size("doubao-seedream-4-5-251128", "512x512"),
            "1920x1920"
        );
        assert_eq!(
            normalize_seedream_size("doubao-seedream-4-5-251128", "512x288"),
            "2560x1440"
        );
        assert_eq!(normalize_seedream_size("dall-e-3", "512x512"), "512x512");
    }

    #[test]
    fn applies_toonflow_seedream_image_options() {
        let mut body = json!({"model":"doubao-seedream-4-5-251128"});
        apply_image_generation_options("doubao-seedream-4-5-251128", &mut body);

        assert_eq!(body["response_format"], "url");
        assert_eq!(body["watermark"], false);
        assert_eq!(body["sequential_image_generation"], "disabled");
    }

    #[test]
    fn seedream_3_does_not_receive_unsupported_sequential_option() {
        let mut body = json!({"model":"doubao-seedream-3-0-t2i"});
        apply_image_generation_options("doubao-seedream-3-0-t2i", &mut body);

        assert_eq!(body["response_format"], "url");
        assert_eq!(body["watermark"], false);
        assert!(body.get("sequential_image_generation").is_none());
    }

    #[test]
    fn rejects_unsupported_seedance_1_5_duration_without_clamping() {
        assert!(validate_seedance_duration("doubao-seedance-1-5-pro", 3).is_err());
        assert!(validate_seedance_duration("doubao-seedance-1-5-pro", 15).is_err());
        assert_eq!(
            validate_seedance_duration("doubao-seedance-1-5-pro", 4).unwrap(),
            4
        );
        assert_eq!(
            validate_seedance_duration("doubao-seedance-2-0", 3).unwrap(),
            3
        );
    }

    #[test]
    fn converts_toonflow_payload_to_seedance_content() {
        let body = VolcEngineMediaProvider::video_body(&config(), json!({"prompt":"镜头推进","references":["https://example.com/first.png"],"aspect_ratio":"16:9","audio":true})).unwrap();
        assert_eq!(body["model"], "doubao-seedance-2-0-260128");
        assert_eq!(body["content"][1]["role"], "first_frame");
        assert_eq!(body["ratio"], "16:9");
        assert_eq!(body["generate_audio"], true);
    }

    #[test]
    fn assigns_first_and_last_frame_roles_without_reference_media() {
        let body = VolcEngineMediaProvider::video_body(
            &config(),
            json!({
                "prompt":"镜头推进",
                "mode":"startEndRequired",
                "references":["https://example.com/first.png","https://example.com/last.png"]
            }),
        )
        .unwrap();
        assert_eq!(body["content"][1]["role"], "first_frame");
        assert_eq!(body["content"][2]["role"], "last_frame");
    }

    #[test]
    fn assigns_a_single_required_end_frame_to_the_last_frame_role() {
        let body = VolcEngineMediaProvider::video_body(
            &config(),
            json!({
                "prompt":"镜头推进",
                "mode":"startFrameOptional",
                "references":["https://example.com/last.png"]
            }),
        )
        .unwrap();
        assert_eq!(body["content"][1]["role"], "last_frame");
    }

    #[test]
    fn assigns_a_single_required_start_frame_to_the_first_frame_role() {
        let body = VolcEngineMediaProvider::video_body(
            &config(),
            json!({
                "prompt":"镜头推进",
                "mode":"endFrameOptional",
                "references":["https://example.com/first.png"]
            }),
        )
        .unwrap();
        assert_eq!(body["content"][1]["role"], "first_frame");
    }

    #[test]
    fn assigns_optional_start_and_required_end_when_both_are_present() {
        let body = VolcEngineMediaProvider::video_body(
            &config(),
            json!({
                "prompt":"镜头推进",
                "mode":"startFrameOptional",
                "references":["https://example.com/first.png","https://example.com/last.png"]
            }),
        )
        .unwrap();
        assert_eq!(body["content"][1]["role"], "first_frame");
        assert_eq!(body["content"][2]["role"], "last_frame");
    }

    #[test]
    fn parses_completed_seedance_task() {
        let result = VolcEngineMediaProvider::response(json!({"id":"cgt-1","status":"succeeded","content":{"video_url":"https://example.com/video.mp4"}}), None).unwrap();
        assert_eq!(result.task_id.as_deref(), Some("cgt-1"));
        assert_eq!(result.url, "https://example.com/video.mp4");
    }
}
