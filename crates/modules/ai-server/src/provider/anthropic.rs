use async_trait::async_trait;
use futures_util::StreamExt;
use rust_toon_ai_api::{ChatRequest, ChatResponse, ModelConfig};
use serde_json::{Value, json};

use super::ChatProvider;

pub struct AnthropicProvider;

fn body(request: &ChatRequest, stream: bool) -> Value {
    let system = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let messages = request
        .messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| json!({"role":m.role,"content":m.content}))
        .collect::<Vec<_>>();
    json!({"model":request.model,"system":system,"messages":messages,"temperature":request.temperature.unwrap_or(0.7),"max_tokens":request.max_tokens.unwrap_or(4096),"stream":stream})
}

fn builder(config: &ModelConfig, request: &ChatRequest, stream: bool) -> reqwest::RequestBuilder {
    let path = config
        .config
        .get("textPath")
        .and_then(Value::as_str)
        .unwrap_or("/v1/messages");
    super::http_client()
        .post(format!("{}{}", config.url.trim_end_matches('/'), path))
        .header("x-api-key", &config.api_key)
        .header(
            "anthropic-version",
            config
                .config
                .get("anthropicVersion")
                .and_then(Value::as_str)
                .unwrap_or("2023-06-01"),
        )
        .json(&body(request, stream))
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    async fn chat(
        &self,
        config: &ModelConfig,
        request: &ChatRequest,
    ) -> Result<ChatResponse, String> {
        let response = super::send_with_retry(builder(config, request, false)).await?;
        let (status, value) = super::response_json(response, "Anthropic 请求失败").await?;
        if !status.is_success() {
            return Err(super::upstream_error(status, &value, "Anthropic 请求失败"));
        }
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.get("text").and_then(Value::as_str))
            .collect::<String>();
        if content.is_empty() {
            return Err("Anthropic 未返回文本".into());
        }
        Ok(ChatResponse {
            content,
            reasoning: None,
            usage: value.get("usage").cloned().unwrap_or_else(|| json!({})),
        })
    }
}

impl AnthropicProvider {
    pub async fn chat_tools_stream<F, Fut>(
        &self,
        config: &ModelConfig,
        messages: Vec<Value>,
        tools: Vec<Value>,
        temperature: f64,
        max_tokens: Option<u32>,
        mut on_delta: F,
    ) -> Result<Value, String>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let system = messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        let user_messages = messages
            .into_iter()
            .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
            .collect::<Vec<_>>();
        let mut body = json!({"model":config.model,"system":system,"messages":user_messages,"temperature":temperature,"max_tokens":max_tokens.unwrap_or(4096),"stream":true});
        if !tools.is_empty() {
            body["tools"] = json!(tools.into_iter().filter_map(|tool| {
                let function = tool.get("function")?;
                Some(json!({"name":function.get("name")?,"description":function.get("description").cloned().unwrap_or(json!("")),"input_schema":function.get("parameters").cloned().unwrap_or(json!({"type":"object"}))}))
            }).collect::<Vec<_>>());
        }
        let path = config
            .config
            .get("textPath")
            .and_then(Value::as_str)
            .unwrap_or("/v1/messages");
        let response = super::send_with_retry(
            super::http_client()
                .post(format!("{}{}", config.url.trim_end_matches('/'), path))
                .header("x-api-key", &config.api_key)
                .header(
                    "anthropic-version",
                    config
                        .config
                        .get("anthropicVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("2023-06-01"),
                )
                .json(&body),
        )
        .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            return Err(super::upstream_error(
                status,
                &value,
                "Anthropic 流式工具请求失败",
            ));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut pending_utf8 = Vec::new();
        let mut content = String::new();
        let mut usage = json!({});
        let mut calls: Vec<Value> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| super::transport_error(&error))?;
            super::append_utf8_chunk(&mut buffer, &mut pending_utf8, &chunk)?;
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                let event: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
                if let Some(summary) = event.get("usage") {
                    usage = summary.clone();
                }
                match event.get("type").and_then(Value::as_str) {
                    Some("content_block_start") => {
                        if let Some(block) = event
                            .get("content_block")
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                        {
                            calls.push(json!({"id":block.get("id").cloned().unwrap_or(json!("")),"type":"function","function":{"name":block.get("name").cloned().unwrap_or(json!("")),"arguments":""}}));
                        }
                    }
                    Some("content_block_delta") => {
                        let delta = event.get("delta").cloned().unwrap_or(json!({}));
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            content.push_str(text);
                            on_delta(json!({"content":text})).await?;
                        }
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            if let Some(call) = calls.last_mut()
                                && let Some(args) = call.pointer_mut("/function/arguments")
                            {
                                let previous = args.as_str().unwrap_or("").to_string();
                                *args = json!(format!("{previous}{partial}"));
                            }
                            on_delta(json!({"tool_calls":[{"index":calls.len().saturating_sub(1),"function":{"arguments":partial}}]})).await?;
                        }
                    }
                    _ => {}
                }
            }
        }
        super::finish_utf8_stream(&pending_utf8)?;
        let mut message = json!({"role":"assistant","content":content});
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        Ok(
            json!({"choices":[{"message":message,"finish_reason":if calls.is_empty(){"stop"}else{"tool_calls"}}],"usage":usage}),
        )
    }

    pub async fn chat_stream<F, Fut>(
        &self,
        config: &ModelConfig,
        request: &ChatRequest,
        mut on_delta: F,
    ) -> Result<ChatResponse, String>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let response = super::send_with_retry(builder(config, request, true)).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            return Err(super::upstream_error(status, &value, "Anthropic 请求失败"));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut pending_utf8 = Vec::new();
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| super::transport_error(&error))?;
            super::append_utf8_chunk(&mut buffer, &mut pending_utf8, &chunk)?;
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                let value: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
                if let Some(delta) = value.pointer("/delta/text").and_then(Value::as_str) {
                    content.push_str(delta);
                    on_delta(delta.into()).await?;
                }
            }
        }
        super::finish_utf8_stream(&pending_utf8)?;
        if content.is_empty() {
            return Err("Anthropic 未返回流式文本".into());
        }
        Ok(ChatResponse {
            content,
            reasoning: None,
            usage: json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::body;
    use rust_toon_ai_api::{ChatMessage, ChatRequest};
    #[test]
    fn separates_system_message() {
        let v = body(
            &ChatRequest {
                model: "claude".into(),
                messages: vec![
                    ChatMessage {
                        role: "system".into(),
                        content: "rules".into(),
                    },
                    ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                    },
                ],
                temperature: None,
                max_tokens: None,
                ..Default::default()
            },
            false,
        );
        assert_eq!(v["system"], "rules");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    }
}
