use super::ChatProvider;
use async_trait::async_trait;
use futures_util::StreamExt;
use rust_toon_ai_api::{ChatRequest, ChatResponse, ModelConfig};
use serde_json::{Value, json};

pub struct GeminiProvider;

fn body(request: &ChatRequest) -> Value {
    let contents=request.messages.iter().filter(|m|m.role!="system").map(|m|json!({"role":if m.role=="assistant"{"model"}else{"user"},"parts":[{"text":m.content}]})).collect::<Vec<_>>();
    let system = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut value = json!({"contents":contents,"generationConfig":{"temperature":request.temperature.unwrap_or(0.7),"maxOutputTokens":request.max_tokens.unwrap_or(4096)}});
    if !system.is_empty() {
        value["systemInstruction"] = json!({"parts":[{"text":system}]});
    }
    value
}
fn url(config: &ModelConfig, stream: bool) -> String {
    let action = if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    format!(
        "{}/v1beta/models/{}:{}&key={}",
        config.url.trim_end_matches('/'),
        config.model,
        action,
        config.api_key
    )
    .replace("generateContent&key", "generateContent?key")
}
fn extract(value: &Value) -> String {
    value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect()
}

#[async_trait]
impl ChatProvider for GeminiProvider {
    async fn chat(
        &self,
        config: &ModelConfig,
        request: &ChatRequest,
    ) -> Result<ChatResponse, String> {
        let response = super::send_with_retry(
            super::http_client()
                .post(url(config, false))
                .json(&body(request)),
        )
        .await?;
        let (status, value) = super::response_json(response, "Gemini 请求失败").await?;
        if !status.is_success() {
            return Err(super::upstream_error(status, &value, "Gemini 请求失败"));
        }
        let content = extract(&value);
        if content.is_empty() {
            return Err("Gemini 未返回文本".into());
        }
        Ok(ChatResponse {
            content,
            reasoning: None,
            usage: value
                .get("usageMetadata")
                .cloned()
                .unwrap_or_else(|| json!({})),
        })
    }
}
impl GeminiProvider {
    pub async fn chat_tools_stream<F, Fut>(
        &self,
        config: &ModelConfig,
        messages: Vec<Value>,
        tools: Vec<Value>,
        mut on_delta: F,
    ) -> Result<Value, String>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let mut request = messages.iter().filter(|m| m.get("role").and_then(Value::as_str) != Some("system")).map(|m| json!({"role":if m.get("role").and_then(Value::as_str)==Some("assistant"){"model"}else{"user"},"parts":[{"text":m.get("content").and_then(Value::as_str).unwrap_or("")}]})).collect::<Vec<_>>();
        if request.is_empty() {
            request.push(json!({"role":"user","parts":[{"text":""}]}));
        }
        let mut body = json!({"contents":request,"generationConfig":{"temperature":0.7}});
        if !tools.is_empty() {
            body["tools"] = json!([{"functionDeclarations":tools.into_iter().filter_map(|t|{let f=t.get("function")?;Some(json!({"name":f.get("name")?,"description":f.get("description").cloned().unwrap_or(json!("")),"parameters":f.get("parameters").cloned().unwrap_or(json!({"type":"object"}))}))}).collect::<Vec<_>>() }]);
        }
        let response =
            super::send_with_retry(super::http_client().post(url(config, true)).json(&body))
                .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            return Err(super::upstream_error(
                status,
                &value,
                "Gemini 流式工具请求失败",
            ));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut usage = json!({});
        let mut calls = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(
                &chunk.map_err(|e| super::transport_error(&e))?,
            ));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                let value: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
                if let Some(summary) = value.get("usageMetadata") {
                    usage = summary.clone();
                }
                for part in value
                    .pointer("/candidates/0/content/parts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        content.push_str(text);
                        on_delta(json!({"content":text})).await?;
                    }
                    if let Some(call) = part.get("functionCall") {
                        let index = calls.len();
                        let name = call.get("name").cloned().unwrap_or(json!(""));
                        let args = call.get("args").cloned().unwrap_or(json!({}));
                        calls.push(json!({"id":format!("gemini-tool-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}));
                        on_delta(json!({"tool_calls":[{"index":index,"id":format!("gemini-tool-{index}"),"function":{"name":call.get("name").cloned().unwrap_or(json!("")),"arguments":args.to_string()}}]})).await?;
                    }
                }
            }
        }
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
        let response = super::send_with_retry(
            super::http_client()
                .post(url(config, true))
                .json(&body(request)),
        )
        .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            return Err(super::upstream_error(status, &value, "Gemini 请求失败"));
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(
                &chunk.map_err(|error| super::transport_error(&error))?,
            ));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                let value: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
                let delta = extract(&value);
                if !delta.is_empty() {
                    content.push_str(&delta);
                    on_delta(delta).await?;
                }
            }
        }
        if content.is_empty() {
            return Err("Gemini 未返回流式文本".into());
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
    fn maps_assistant_to_model() {
        let v = body(&ChatRequest {
            model: "x".into(),
            messages: vec![ChatMessage {
                role: "assistant".into(),
                content: "hi".into(),
            }],
            temperature: None,
            max_tokens: None,
            ..Default::default()
        });
        assert_eq!(v["contents"][0]["role"], "model");
    }
}
