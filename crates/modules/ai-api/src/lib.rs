use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiPlatform {
    TongYi,
    YiYan,
    DeepSeek,
    ZhiPu,
    XingHuo,
    VolcEngine,
    HunYuan,
    SiliconFlow,
    MiniMax,
    Moonshot,
    BaiChuan,
    StepFun,
    OpenAI,
    AzureOpenAI,
    Anthropic,
    Gemini,
    Ollama,
    StableDiffusion,
    Midjourney,
    Suno,
    Grok,
    OpenAICompatible,
    /// External agent-engine sidecar (piren). The platform string stored in
    /// `ai.model_configs.platform` is the verbatim variant name `AgentEngine`;
    /// `agent-engine` is accepted as a deserialization/matching alias.
    #[serde(alias = "agent-engine")]
    AgentEngine,
}

impl AiPlatform {
    pub const ALL: &'static [Self] = &[
        Self::TongYi,
        Self::YiYan,
        Self::DeepSeek,
        Self::ZhiPu,
        Self::XingHuo,
        Self::VolcEngine,
        Self::HunYuan,
        Self::SiliconFlow,
        Self::MiniMax,
        Self::Moonshot,
        Self::BaiChuan,
        Self::StepFun,
        Self::OpenAI,
        Self::AzureOpenAI,
        Self::Anthropic,
        Self::Gemini,
        Self::Ollama,
        Self::StableDiffusion,
        Self::Midjourney,
        Self::Suno,
        Self::Grok,
        Self::OpenAICompatible,
        Self::AgentEngine,
    ];
    pub fn code(self) -> &'static str {
        match self {
            Self::TongYi => "TongYi",
            Self::YiYan => "YiYan",
            Self::DeepSeek => "DeepSeek",
            Self::ZhiPu => "ZhiPu",
            Self::XingHuo => "XingHuo",
            Self::VolcEngine => "VolcEngine",
            Self::HunYuan => "HunYuan",
            Self::SiliconFlow => "SiliconFlow",
            Self::MiniMax => "MiniMax",
            Self::Moonshot => "Moonshot",
            Self::BaiChuan => "BaiChuan",
            Self::StepFun => "StepFun",
            Self::OpenAI => "OpenAI",
            Self::AzureOpenAI => "AzureOpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::Ollama => "Ollama",
            Self::StableDiffusion => "StableDiffusion",
            Self::Midjourney => "Midjourney",
            Self::Suno => "Suno",
            Self::Grok => "Grok",
            Self::OpenAICompatible => "OpenAICompatible",
            Self::AgentEngine => "AgentEngine",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        if value == "agent-engine" {
            // Kebab-case alias accepted for hand-written rows and configs.
            return Some(Self::AgentEngine);
        }
        Self::ALL.iter().copied().find(|item| item.code() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TongYi => "通义千问",
            Self::YiYan => "文心一言",
            Self::DeepSeek => "DeepSeek",
            Self::ZhiPu => "智谱清言",
            Self::XingHuo => "讯飞星火",
            Self::VolcEngine => "火山引擎",
            Self::HunYuan => "腾讯混元",
            Self::SiliconFlow => "硅基流动",
            Self::MiniMax => "MiniMax",
            Self::Moonshot => "Kimi",
            Self::BaiChuan => "百川智能",
            Self::StepFun => "阶跃星辰",
            Self::OpenAI => "OpenAI 官方",
            Self::AzureOpenAI => "微软 Azure（OpenAI）",
            Self::Anthropic => "Claude",
            Self::Gemini => "Gemini",
            Self::Ollama => "Ollama 本地模型",
            Self::StableDiffusion => "Stable Diffusion",
            Self::Midjourney => "Midjourney",
            Self::Suno => "Suno 音乐",
            Self::Grok => "Grok",
            Self::OpenAICompatible => "OpenAI 兼容平台",
            Self::AgentEngine => "AgentEngine 智能体引擎",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::TongYi => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Self::YiYan => "https://qianfan.baidubce.com/v2",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::ZhiPu => "https://open.bigmodel.cn/api/paas/v4",
            Self::XingHuo => "https://spark-api-open.xf-yun.com/v1",
            Self::VolcEngine => "https://ark.cn-beijing.volces.com/api/v3",
            Self::HunYuan => "https://api.hunyuan.cloud.tencent.com/v1",
            Self::SiliconFlow => "https://api.siliconflow.cn/v1",
            Self::MiniMax => "https://api.minimax.chat/v1",
            Self::Moonshot => "https://api.moonshot.cn/v1",
            Self::BaiChuan => "https://api.baichuan-ai.com/v1",
            Self::StepFun => "https://api.stepfun.com/v1",
            Self::OpenAI => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::Ollama => "http://127.0.0.1:11434/v1",
            Self::StableDiffusion => "http://127.0.0.1:7860",
            Self::Grok => "https://api.x.ai/v1",
            Self::AgentEngine => "http://127.0.0.1:7750",
            Self::AzureOpenAI | Self::Midjourney | Self::Suno | Self::OpenAICompatible => "",
        }
    }

    pub fn supported_types(self) -> &'static [AiModelType] {
        use AiModelType::{Chat, Embedding, Image, Music, Rerank, Speech, Transcription, Video};

        match self {
            Self::OpenAI => &[Chat, Image, Video, Speech, Transcription, Embedding],
            Self::TongYi | Self::SiliconFlow => &[Chat, Image, Video, Speech, Embedding, Rerank],
            Self::VolcEngine => &[Chat, Image, Video, Speech, Transcription, Embedding],
            Self::MiniMax => &[Chat, Image, Video, Speech, Music],
            Self::Gemini => &[Chat, Image, Video, Speech, Music, Embedding],
            Self::Grok => &[Chat, Image, Video, Speech, Transcription],
            Self::ZhiPu => &[Chat, Image, Video, Speech, Transcription, Embedding],
            Self::StepFun => &[Chat, Image, Speech, Transcription],
            Self::Ollama => &[Chat, Embedding],
            Self::StableDiffusion | Self::Midjourney => &[Image],
            Self::Suno => &[Music],
            Self::OpenAICompatible => &[
                Chat,
                Image,
                Video,
                Speech,
                Transcription,
                Music,
                Embedding,
                Rerank,
            ],
            Self::YiYan
            | Self::DeepSeek
            | Self::XingHuo
            | Self::HunYuan
            | Self::Moonshot
            | Self::BaiChuan
            | Self::AzureOpenAI
            | Self::Anthropic => &[Chat],
            Self::AgentEngine => &[Chat],
        }
    }

    pub fn supports(self, model_type: AiModelType) -> bool {
        self.supported_types().contains(&model_type)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiModelType {
    Chat,
    Image,
    Video,
    Speech,
    Transcription,
    Music,
    Embedding,
    Rerank,
}

impl AiModelType {
    pub fn code(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Image => "image",
            Self::Video => "video",
            Self::Speech => "speech",
            Self::Transcription => "transcription",
            Self::Music => "music",
            Self::Embedding => "embedding",
            Self::Rerank => "rerank",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" | "text" => Some(Self::Chat),
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "speech" | "tts" | "audio" => Some(Self::Speech),
            "transcription" => Some(Self::Transcription),
            "music" => Some(Self::Music),
            "embedding" => Some(Self::Embedding),
            "rerank" => Some(Self::Rerank),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub id: i64,
    pub name: String,
    pub key: String,
    pub platform: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub model: String,
    pub api_key: String,
    pub url: String,
    pub status: i32,
    pub config: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default, alias = "VideoMode")]
    pub video_modes: Vec<String>,
    #[serde(default)]
    pub duration_resolution_map: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    pub think_levels: Vec<String>,
    #[serde(default)]
    pub multi_reference: bool,
    #[serde(default)]
    pub video_resolutions: Vec<String>,
    #[serde(default)]
    pub video_min_duration: Option<i64>,
    #[serde(default)]
    pub video_max_duration: Option<i64>,
    #[serde(default)]
    pub video_max_references: Option<usize>,
    #[serde(default)]
    pub video_audio: Option<bool>,
}

impl ModelConfig {
    pub fn capabilities(&self) -> ModelCapabilities {
        serde_json::from_value(
            self.config
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| self.config.clone()),
        )
        .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// Authenticated chat context threaded from the chat handlers for the
    /// AgentEngine sidecar platform; ignored by every other platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub usage: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRequest {
    pub prompt: String,
    pub size: String,
    #[serde(default)]
    pub references: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaResponse {
    pub url: String,
    pub task_id: Option<String>,
    pub raw: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechRequest {
    pub input: String,
    pub voice: String,
    pub format: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub inputs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub embeddings: Vec<Vec<f32>>,
    pub usage: Value,
}

#[cfg(test)]
mod tests {
    use super::{AiModelType, AiPlatform};

    #[test]
    fn yudao_platform_codes_round_trip() {
        for platform in AiPlatform::ALL {
            assert_eq!(AiPlatform::parse(platform.code()), Some(*platform));
        }
    }

    #[test]
    fn agent_engine_accepts_variant_name_and_kebab_alias() {
        assert_eq!(
            AiPlatform::parse("AgentEngine"),
            Some(AiPlatform::AgentEngine)
        );
        assert_eq!(
            AiPlatform::parse("agent-engine"),
            Some(AiPlatform::AgentEngine)
        );
        assert_eq!(AiPlatform::AgentEngine.code(), "AgentEngine");
    }

    #[test]
    fn model_type_aliases_are_supported() {
        assert_eq!(AiModelType::parse("text"), Some(AiModelType::Chat));
        assert_eq!(AiModelType::parse("tts"), Some(AiModelType::Speech));
    }

    #[test]
    fn platform_capabilities_reject_invalid_combinations() {
        assert!(AiPlatform::DeepSeek.supports(AiModelType::Chat));
        assert!(!AiPlatform::DeepSeek.supports(AiModelType::Image));
        assert!(AiPlatform::Midjourney.supports(AiModelType::Image));
        assert!(!AiPlatform::Midjourney.supports(AiModelType::Chat));
        assert_eq!(
            AiPlatform::DeepSeek.default_url(),
            "https://api.deepseek.com"
        );
    }
}
