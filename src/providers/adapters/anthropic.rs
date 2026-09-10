//! Anthropic Claude adapter -- transforms AHNARA messages into Anthropic's
//! Messages API format (/v1/messages). Anthropic has a fundamentally different
//! structure: system prompt is a top-level field, not a message, tool results use
//! content blocks with tool_use_id, tools go in a top-level tools array, and the
//! response uses content blocks instead of choices.

use crate::providers::adapters::ProviderAdapter;
use crate::providers::{
    CompletionRequest, CompletionResponse, StreamChunk, ToolCall, Usage,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;

pub struct AnthropicAdapter;

impl AnthropicAdapter {
    pub fn new() -> Self {
        Self
    }

    fn append_system_content(&self, system_content: &mut String, msg: &crate::providers::Message) {
        if let Some(ref c) = msg.content {
            if !system_content.is_empty() {
                system_content.push_str("\n\n");
            }
            system_content.push_str(c);
        }
    }

    fn transform_user_message(&self, msg: &crate::providers::Message) -> Value {
        if let Some(ref parts) = msg.content_parts {
            let blocks: Vec<Value> = parts.iter().map(|part| {
                match part {
                    crate::providers::ContentPart::Text { text } => {
                        serde_json::json!({"type": "text", "text": text})
                    }
                    crate::providers::ContentPart::ImageUrl { image_url } => {
                        self.transform_image_part(&image_url.url)
                    }
                }
            }).collect();
            serde_json::json!({"role": "user", "content": blocks})
        } else {
            let content = msg.content.as_deref().unwrap_or("");
            serde_json::json!({"role": "user", "content": content})
        }
    }

    fn transform_image_part(&self, url: &str) -> Value {
        if let Some(rest) = url.strip_prefix("data:") {
            let (media_type, data) = rest.split_once(";base64,").unwrap_or(("application/octet-stream", url));
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                }
            })
        } else {
            serde_json::json!({"type": "text", "text": format!("[Image URL: {}]", url)})
        }
    }

    fn transform_assistant_message(&self, msg: &crate::providers::Message) -> Value {
        if let Some(ref tool_calls) = msg.tool_calls {
            let mut content_blocks: Vec<Value> = Vec::new();
            if let Some(ref text) = msg.content {
                if !text.is_empty() {
                    content_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text,
                    }));
                }
            }
            for tc in tool_calls {
                let args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": args,
                }));
            }
            serde_json::json!({
                "role": "assistant",
                "content": content_blocks,
            })
        } else {
            let content = msg.content.as_deref().unwrap_or("");
            serde_json::json!({
                "role": "assistant",
                "content": content,
            })
        }
    }

    fn transform_tool_message(&self, msg: &crate::providers::Message) -> Value {
        let content = msg.content.as_deref().unwrap_or("");
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                "content": content,
            }],
        })
    }

    async fn process_stream_event(event: &Value, tx: &mpsc::Sender<StreamChunk>) {
        if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
            match event_type {
                "content_block_delta" => {
                    if let Some(text_val) = event.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                        let _ = tx.send(StreamChunk {
                            id: None,
                            choices: vec![crate::providers::StreamChoice {
                                index: 0,
                                delta: crate::providers::StreamDelta {
                                    content: text_val.to_string(),
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                            done: false,
                        }).await;
                    }
                }
                "message_stop" => {
                    let _ = tx.send(StreamChunk { id: None, choices: vec![], done: true }).await;
                }
                _ => {}
            }
        }
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn provider_type(&self) -> &str {
        "anthropic"
    }

    fn build_url(&self, api_base: &str, _api_key: &str, _model: &str) -> String {
        let base = api_base.trim_end_matches('/');
        format!("{}/messages", base)
    }

    fn build_headers(&self, api_key: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        headers.insert("x-api-key", api_key.parse().unwrap());
        headers.insert(
            "anthropic-version",
            "2023-06-01".parse().unwrap(),
        );
        headers
    }

    fn transform_request(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut system_content = String::new();
        let mut messages: Vec<Value> = Vec::new();

        for msg in &request.messages {
            match msg.role.as_str() {
                "system" => {
                    self.append_system_content(&mut system_content, msg);
                }
                "user" => {
                    messages.push(self.transform_user_message(msg));
                }
                "assistant" => {
                    messages.push(self.transform_assistant_message(msg));
                }
                "tool" => {
                    messages.push(self.transform_tool_message(msg));
                }
                _ => {}
            }
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "messages": messages,
        });

        if !system_content.is_empty() {
            // Block form with a cache breakpoint: the system prompt is the
            // largest static prefix, so pinning it lets repeat turns reuse it.
            body["system"] = serde_json::json!([{
                "type": "text",
                "text": system_content,
                "cache_control": {"type": "ephemeral"},
            }]);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                let mut anthropic_tools: Vec<Value> = tools.iter().map(|t| {
                    serde_json::json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "input_schema": t.function.parameters,
                    })
                }).collect();
                // Cache breakpoint on the last tool: the tool list is static
                // across turns, so it stays cached while messages grow.
                if let Some(last) = anthropic_tools.last_mut() {
                    last["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
                body["tools"] = serde_json::Value::Array(anthropic_tools);
            }
        }

        body
    }

    fn parse_response(&self, body: &str) -> Result<CompletionResponse> {
        #[derive(serde::Deserialize)]
        struct AnthropicResponse {
            #[serde(default)]
            content: Vec<AnthropicContentBlock>,
            #[serde(default)]
            usage: Option<AnthropicUsage>,
            #[serde(default)]
            error: Option<AnthropicError>,
        }
        #[derive(serde::Deserialize)]
        #[serde(tag = "type")]
        enum AnthropicContentBlock {
            #[serde(rename = "text")]
            Text { text: String },
            #[serde(rename = "tool_use")]
            ToolUse {
                id: String,
                name: String,
                input: Value,
            },
        }
        #[derive(serde::Deserialize)]
        struct AnthropicUsage {
            input_tokens: u32,
            output_tokens: u32,
        }
        #[derive(serde::Deserialize)]
        struct AnthropicError {
            message: String,
        }

        let response: AnthropicResponse = serde_json::from_str(body)
            .map_err(|e| anyhow!("Anthropic adapter JSON parse error: {:#}", e))?;

        if let Some(err) = response.error {
            return Err(anyhow!("Anthropic error: {}", err.message));
        }

        let mut content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in &response.content {
            match block {
                AnthropicContentBlock::Text { text } => {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(text);
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        call_type: "function".to_string(),
                        function: crate::providers::FunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(input).unwrap_or_default(),
                        },
                        thought_signature: None,
                    });
                }
            }
        }

        Ok(CompletionResponse {
            content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            usage: response.usage.map(|u| Usage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: u.input_tokens + u.output_tokens,
            }),
        })
    }

    async fn stream_response(
        &self,
        client: &Client,
        url: &str,
        headers: reqwest::header::HeaderMap,
        body: serde_json::Value,
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(100);
        let url = url.to_string();
        let client = client.clone();

        tokio::spawn(async move {
            let response = match client.post(&url).headers(headers).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Anthropic stream request failed: {}", e);
                    return;
                }
            };
            if !response.status().is_success() {
                tracing::error!("Anthropic stream HTTP {}", response.status());
                return;
            }
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                if let Ok(bytes) = chunk {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                let _ = tx.send(StreamChunk { id: None, choices: vec![], done: true }).await;
                                return;
                            }
                            if let Ok(event) = serde_json::from_str::<Value>(data) {
                                Self::process_stream_event(&event, &tx).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}
