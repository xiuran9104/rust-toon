use crate::provider::{
    AgentEngineProvider, AnthropicProvider, AzureOpenAiProvider, ChatProvider, GeminiProvider,
    OpenAiCompatibleProvider, VolcEngineMediaProvider, provider_app_error,
};
use rust_toon_ai_api::{
    AiModelType, AiPlatform, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    ImageRequest, MediaResponse, ModelConfig, SpeechRequest,
};
use rust_toon_framework_web::AppError;
use serde_json::Value;
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct AiModelFactory {
    pool: PgPool,
}

fn model_is_enabled(status: i32) -> bool {
    status == 0
}

impl AiModelFactory {
    async fn typed(&self, id: i64, expected: AiModelType) -> Result<ModelConfig, AppError> {
        let config = self.config(id).await?;
        if !model_is_enabled(config.status) {
            return Err(AppError::bad_request("AI model is disabled"));
        }
        if AiModelType::parse(&config.type_) != Some(expected) {
            return Err(AppError::bad_request(format!(
                "AI model type must be {}",
                expected.code()
            )));
        }
        Ok(config)
    }
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn config(&self, id: i64) -> Result<ModelConfig, AppError> {
        let row=sqlx::query("SELECT id,name,key,platform,type,model,api_key,url,status,config FROM ai.model_configs WHERE id=$1").bind(id).fetch_optional(&self.pool).await.map_err(|_|AppError::internal("failed to load AI model"))?.ok_or_else(||AppError::not_found("AI model not found"))?;
        Ok(ModelConfig {
            id: row.get("id"),
            name: row.get("name"),
            key: row.get("key"),
            platform: row.get("platform"),
            type_: row.get("type"),
            model: row.get("model"),
            api_key: row.get("api_key"),
            url: row.get("url"),
            status: row.get("status"),
            config: row.get::<Value, _>("config"),
        })
    }
    pub async fn chat(&self, id: i64, mut request: ChatRequest) -> Result<ChatResponse, AppError> {
        let config = self.config(id).await?;
        if !model_is_enabled(config.status) {
            return Err(AppError::bad_request("AI model is disabled"));
        }
        if AiModelType::parse(&config.type_) != Some(AiModelType::Chat) {
            return Err(AppError::bad_request("AI model type is not chat"));
        }
        request.model = config.model.clone();
        let platform = AiPlatform::parse(&config.platform)
            .ok_or_else(|| AppError::bad_request("unsupported AI platform"))?;
        let provider: Box<dyn ChatProvider> = match platform {
            AiPlatform::OpenAI
            | AiPlatform::TongYi
            | AiPlatform::XingHuo
            | AiPlatform::DeepSeek
            | AiPlatform::VolcEngine
            | AiPlatform::HunYuan
            | AiPlatform::SiliconFlow
            | AiPlatform::MiniMax
            | AiPlatform::Moonshot
            | AiPlatform::BaiChuan
            | AiPlatform::StepFun
            | AiPlatform::YiYan
            | AiPlatform::ZhiPu
            | AiPlatform::Grok
            | AiPlatform::Ollama
            | AiPlatform::OpenAICompatible => Box::new(OpenAiCompatibleProvider),
            AiPlatform::Anthropic => Box::new(AnthropicProvider),
            AiPlatform::Gemini => Box::new(GeminiProvider),
            AiPlatform::AzureOpenAI => Box::new(AzureOpenAiProvider),
            AiPlatform::AgentEngine => Box::new(AgentEngineProvider),
            _ => {
                return Err(AppError::bad_request(format!(
                    "platform {} provider is not implemented yet",
                    config.platform
                )));
            }
        };
        provider
            .chat(&config, &request)
            .await
            .map_err(provider_app_error)
    }
    pub async fn chat_tools(
        &self,
        id: i64,
        messages: Vec<Value>,
        tools: Vec<Value>,
        temperature: Option<f64>,
        max_tokens: Option<u32>,
    ) -> Result<Value, AppError> {
        let config = self.typed(id, AiModelType::Chat).await?;
        let platform = AiPlatform::parse(&config.platform)
            .ok_or_else(|| AppError::bad_request("unsupported AI platform"))?;
        let mut body = serde_json::json!({"model":config.model,"messages":messages,"temperature":temperature.unwrap_or(0.7)});
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }
        if let Some(limit) = max_tokens {
            body["max_tokens"] = serde_json::json!(limit)
        }
        match platform {
            AiPlatform::AzureOpenAI => {
                let version = config
                    .config
                    .get("apiVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-10-21");
                let mut azure = config.clone();
                azure.url = format!(
                    "{}/openai/deployments/{}",
                    config.url.trim_end_matches('/'),
                    config.model
                );
                azure.config = serde_json::json!({"textPath":format!("/chat/completions?api-version={version}")});
                OpenAiCompatibleProvider
                    .raw_chat_with_header(&azure, body, "api-key")
                    .await
                    .map_err(provider_app_error)
            }
            AiPlatform::OpenAI
            | AiPlatform::TongYi
            | AiPlatform::XingHuo
            | AiPlatform::DeepSeek
            | AiPlatform::VolcEngine
            | AiPlatform::HunYuan
            | AiPlatform::SiliconFlow
            | AiPlatform::MiniMax
            | AiPlatform::Moonshot
            | AiPlatform::BaiChuan
            | AiPlatform::StepFun
            | AiPlatform::YiYan
            | AiPlatform::ZhiPu
            | AiPlatform::Grok
            | AiPlatform::Ollama
            | AiPlatform::OpenAICompatible => OpenAiCompatibleProvider
                .raw_chat_with_header(&config, body, "authorization")
                .await
                .map_err(provider_app_error),
            // The engine runs its own built-in tools; external tool
            // definitions are ignored for this platform.
            AiPlatform::AgentEngine => AgentEngineProvider
                .chat_tools(&config, messages)
                .await
                .map_err(provider_app_error),
            _ => Err(AppError::bad_request(format!(
                "平台 {} 暂不支持工具调用",
                config.platform
            ))),
        }
    }

    pub async fn chat_tools_stream<F, Fut>(
        &self,
        id: i64,
        messages: Vec<Value>,
        tools: Vec<Value>,
        temperature: Option<f64>,
        max_tokens: Option<u32>,
        on_delta: F,
    ) -> Result<Value, AppError>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let config = self.typed(id, AiModelType::Chat).await?;
        let platform = AiPlatform::parse(&config.platform)
            .ok_or_else(|| AppError::bad_request("unsupported AI platform"))?;
        let (config, auth_header) = if platform == AiPlatform::AzureOpenAI {
            let version = config
                .config
                .get("apiVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-10-21");
            let mut azure = config.clone();
            azure.url = format!(
                "{}/openai/deployments/{}",
                azure.url.trim_end_matches('/'),
                azure.model
            );
            azure.config =
                serde_json::json!({"textPath":format!("/chat/completions?api-version={version}")});
            (azure, "api-key")
        } else {
            (config, "authorization")
        };
        match platform {
            AiPlatform::OpenAI
            | AiPlatform::TongYi
            | AiPlatform::XingHuo
            | AiPlatform::DeepSeek
            | AiPlatform::VolcEngine
            | AiPlatform::HunYuan
            | AiPlatform::SiliconFlow
            | AiPlatform::MiniMax
            | AiPlatform::Moonshot
            | AiPlatform::BaiChuan
            | AiPlatform::StepFun
            | AiPlatform::YiYan
            | AiPlatform::ZhiPu
            | AiPlatform::Grok
            | AiPlatform::Ollama
            | AiPlatform::OpenAICompatible
            | AiPlatform::AzureOpenAI => OpenAiCompatibleProvider
                .chat_tools_stream(
                    &config,
                    messages,
                    tools,
                    temperature.unwrap_or(0.7),
                    max_tokens,
                    on_delta,
                    auth_header,
                )
                .await
                .map_err(provider_app_error),
            AiPlatform::Anthropic => AnthropicProvider
                .chat_tools_stream(
                    &config,
                    messages,
                    tools,
                    temperature.unwrap_or(0.7),
                    max_tokens,
                    on_delta,
                )
                .await
                .map_err(provider_app_error),
            AiPlatform::Gemini => GeminiProvider
                .chat_tools_stream(&config, messages, tools, on_delta)
                .await
                .map_err(provider_app_error),
            // External tool definitions are ignored: the engine drives its
            // own built-in tools, so this reuses the plain engine turn.
            AiPlatform::AgentEngine => AgentEngineProvider
                .chat_tools_stream(&config, messages, on_delta)
                .await
                .map_err(provider_app_error),
            _ => Err(AppError::bad_request(format!(
                "平台 {} 暂不支持流式工具调用",
                config.platform
            ))),
        }
    }
    pub async fn chat_stream<F, Fut>(
        &self,
        id: i64,
        mut request: ChatRequest,
        on_delta: F,
    ) -> Result<ChatResponse, AppError>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let config = self.config(id).await?;
        if !model_is_enabled(config.status) {
            return Err(AppError::bad_request("AI model is disabled"));
        }
        if AiModelType::parse(&config.type_) != Some(AiModelType::Chat) {
            return Err(AppError::bad_request("AI model type is not chat"));
        }
        request.model = config.model.clone();
        let platform = AiPlatform::parse(&config.platform)
            .ok_or_else(|| AppError::bad_request("unsupported AI platform"))?;
        match platform {
            AiPlatform::OpenAI
            | AiPlatform::TongYi
            | AiPlatform::XingHuo
            | AiPlatform::DeepSeek
            | AiPlatform::VolcEngine
            | AiPlatform::HunYuan
            | AiPlatform::SiliconFlow
            | AiPlatform::MiniMax
            | AiPlatform::Moonshot
            | AiPlatform::BaiChuan
            | AiPlatform::StepFun
            | AiPlatform::YiYan
            | AiPlatform::ZhiPu
            | AiPlatform::Grok
            | AiPlatform::Ollama
            | AiPlatform::OpenAICompatible => OpenAiCompatibleProvider
                .chat_stream(&config, &request, on_delta)
                .await
                .map_err(provider_app_error),
            AiPlatform::Anthropic => AnthropicProvider
                .chat_stream(&config, &request, on_delta)
                .await
                .map_err(provider_app_error),
            AiPlatform::Gemini => GeminiProvider
                .chat_stream(&config, &request, on_delta)
                .await
                .map_err(provider_app_error),
            AiPlatform::AzureOpenAI => AzureOpenAiProvider
                .chat_stream(&config, &request, on_delta)
                .await
                .map_err(provider_app_error),
            AiPlatform::AgentEngine => AgentEngineProvider
                .chat_stream(&config, &request, on_delta)
                .await
                .map_err(provider_app_error),
            _ => Err(AppError::bad_request(format!(
                "platform {} streaming provider is not implemented",
                config.platform
            ))),
        }
    }
    pub async fn image(&self, id: i64, request: ImageRequest) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Image).await?;
        OpenAiCompatibleProvider
            .image(&config, &request)
            .await
            .map_err(provider_app_error)
    }
    pub async fn midjourney_imagine(
        &self,
        id: i64,
        payload: Value,
    ) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Image).await?;
        if config.platform != AiPlatform::Midjourney.code() {
            return Err(AppError::bad_request("所选模型不是 Midjourney 图片模型"));
        }
        OpenAiCompatibleProvider
            .midjourney_imagine(&config, payload)
            .await
            .map_err(provider_app_error)
    }
    pub async fn midjourney_action(
        &self,
        id: i64,
        payload: Value,
    ) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Image).await?;
        if config.platform != AiPlatform::Midjourney.code() {
            return Err(AppError::bad_request("所选模型不是 Midjourney 图片模型"));
        }
        OpenAiCompatibleProvider
            .midjourney_action(&config, payload)
            .await
            .map_err(provider_app_error)
    }
    pub async fn poll_midjourney(&self, id: i64, task_id: &str) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Image).await?;
        if config.platform != AiPlatform::Midjourney.code() {
            return Err(AppError::bad_request("所选模型不是 Midjourney 图片模型"));
        }
        OpenAiCompatibleProvider
            .poll_midjourney(&config, task_id)
            .await
            .map_err(provider_app_error)
    }
    pub async fn video(&self, id: i64, payload: Value) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Video).await?;
        if config.platform == AiPlatform::VolcEngine.code() {
            return VolcEngineMediaProvider
                .video(&config, payload)
                .await
                .map_err(provider_app_error);
        }
        OpenAiCompatibleProvider
            .video(&config, payload)
            .await
            .map_err(provider_app_error)
    }
    pub async fn poll_video(&self, id: i64, task_id: &str) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Video).await?;
        if config.platform != AiPlatform::VolcEngine.code() {
            return Err(AppError::bad_request("该视频平台不支持任务轮询"));
        }
        VolcEngineMediaProvider
            .poll_video(&config, task_id)
            .await
            .map_err(provider_app_error)
    }
    pub async fn music(&self, id: i64, payload: Value) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Music).await?;
        OpenAiCompatibleProvider
            .music(&config, payload)
            .await
            .map_err(provider_app_error)
    }
    pub async fn poll_music(&self, id: i64, task_id: &str) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Music).await?;
        OpenAiCompatibleProvider
            .poll_music(&config, task_id)
            .await
            .map_err(provider_app_error)
    }
    pub async fn speech(&self, id: i64, request: SpeechRequest) -> Result<MediaResponse, AppError> {
        let config = self.typed(id, AiModelType::Speech).await?;
        OpenAiCompatibleProvider
            .speech(&config, &request)
            .await
            .map_err(provider_app_error)
    }
    pub async fn embedding(
        &self,
        id: i64,
        request: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, AppError> {
        let config = self.typed(id, AiModelType::Embedding).await?;
        OpenAiCompatibleProvider
            .embedding(&config, &request)
            .await
            .map_err(provider_app_error)
    }
}

#[cfg(test)]
mod status_tests {
    use super::model_is_enabled;

    #[test]
    fn follows_yudao_common_status_semantics() {
        assert!(model_is_enabled(0));
        assert!(!model_is_enabled(1));
    }
}
