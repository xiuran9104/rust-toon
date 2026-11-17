-- 0028: 本地模型（Ollama）支持完整的 OpenAI 兼容模型类型集合。
-- 本地部署通常以 OpenAI 兼容协议对外提供模型，可能同时代理对话、图片、视频、
-- 语音、转写、音乐、向量与重排模型，因此把 Ollama 的类型白名单放宽到与
-- OpenAI 兼容平台一致，使网关校验、模型同步与前端类型下拉保持同步。
INSERT INTO ai.model_platforms (
    platform, label, default_url, supported_types, enabled, update_time
)
VALUES (
    'Ollama',
    'Ollama 本地模型',
    'http://127.0.0.1:11434/v1',
    '{chat,image,video,speech,transcription,music,embedding,rerank}',
    TRUE,
    0
)
ON CONFLICT (platform) DO UPDATE
SET supported_types = EXCLUDED.supported_types;
