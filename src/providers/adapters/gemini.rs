//! Google Gemini adapter -- transforms to the Google AI Studio OpenAI-compatible
//! endpoint. Google speaks mostly OpenAI format over `/v1beta/openai/chat/completions`
//! but with key-as-query-param auth and stricter system-message ordering.

use crate::providers::adapters::ProviderAdapter;
use crate::providers::{
    CompletionRequest, CompletionResponse, Message, StreamChunk, ToolCall, Usage,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use tokio::sync::mpsc;

pub struct GeminiAdapter;

impl GeminiAdapter {
    pub fn new() -> Self {
        Self
    }

    fn deduplicate_system_messages(&self, messages: &[Message]) -> Vec<Message> {
        let mut system_content = String::new();
        let mut other_messages = Vec::new();
        for msg in messages {
            if msg.role == "system" {
                if let Some(ref c) = msg.content {
                    if !system_content.is_empty() {
                        system_content.push_str("\n\n");
                    }
                    system_content.push_str(c);
                }
            } else {
                other_messages.push(msg.clone());
            }
        }
        if system_content.is_empty() {
            system_content = "You are a helpful assistant.".to_string();
        }
        let mut result = vec![Message {
            role: "system".to_string(),
            content: Some(system_content),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            content_parts: None,
        }];
        result.extend(other_messages);
        result
    }
}

#[async_trait]
impl ProviderAdapter for GeminiAdapter {
    fn provider_type(&self) -> &str {
        "google"
    }

    fn build_url(&self, api_base: &str, api_key: &str, _model: &str) -> String {
        let base = api_base.trim_end_matches('/');
        if api_key.is_empty() {
            format!("{}/chat/completions", base)
        } else {
            format!("{}/chat/completions?key={}", base, api_key)
        }
    }

    fn build_headers(&self, _api_key: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        headers
    }

    fn transform_request(&self, request: &CompletionRequest) -> serde_json::Value {
        let model = request.model.split('/').last().unwrap_or(&request.model);
        let deduped = self.deduplicate_system_messages(&request.messages);
        let messages: Vec<serde_json::Value> = deduped.iter().map(|msg| {
            if let Some(ref parts) = msg.content_parts {
                serde_json::json!({"role": msg.role, "content": parts})
            } else {
                // Only include optional fields when set. Strict providers
                // reject explicit nulls (e.g. "name": null).
                let mut m = serde_json::json!({
                    "role": msg.role,
                    "content": msg.content,
                });
                if let Some(ref tc) = msg.tool_calls {
                    m["tool_calls"] = serde_json::to_value(tc).unwrap_or(serde_json::Value::Null);
                }
                if let Some(ref id) = msg.tool_call_id {
                    m["tool_call_id"] = serde_json::json!(id);
                }
                if let Some(ref name) = msg.name {
                    m["name"] = serde_json::json!(name);
                }
                m
            }
        }).collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": request.temperature.unwrap_or(1.0),
            "max_tokens": request.max_tokens.unwrap_or(8192),
            "stream": request.stream.unwrap_or(false),
        });

        if let Some(ref tools) = request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::to_value(tools).unwrap();
            }
        }
        body
    }

    fn parse_response(&self, body: &str) -> Result<CompletionResponse> {
        #[derive(serde::Deserialize)]
        struct OpenAICompletion {
            choices: Vec<OpenAIChoice>,
            #[serde(default)]
            usage: Option<OpenAIUsage>,
            #[serde(default)]
            error: Option<serde_json::Value>,
        }
        #[derive(serde::Deserialize)]
        struct OpenAIChoice {
            message: OpenAIMessage,
        }
        #[derive(serde::Deserialize)]
        struct OpenAIMessage {
            content: Option<String>,
            tool_calls: Option<Vec<ToolCall>>,
            reasoning: Option<String>,
            reasoning_content: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct OpenAIUsage {
            prompt_tokens: u32,
            completion_tokens: u32,
            total_tokens: u32,
        }

        let completion: OpenAICompletion = serde_json::from_str(body)
            .map_err(|e| anyhow!("Gemini adapter JSON parse error: {:#}", e))?;

        if let Some(err) = &completion.error {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow!("Gemini error: {}", msg));
        }

        let first = completion.choices.first();
        let content = first
            .map(|c| {
                c.message
                    .content
                    .clone()
                    .or(c.message.reasoning_content.clone())
                    .or(c.message.reasoning.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            tool_calls: first.and_then(|c| c.message.tool_calls.clone()),
            usage: completion.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
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
                    tracing::error!("Gemini stream request failed: {}", e);
                    return;
                }
            };
            if !response.status().is_success() {
                tracing::error!("Gemini stream HTTP {}", response.status());
                return;
            }
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                if let Ok(bytes) = chunk {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.starts_with("data: ") {
                            let data = &line[6..];
                            if data == "[DONE]" {
                                let _ = tx
                                    .send(StreamChunk {
                                        id: None,
                                        choices: vec![],
                                        done: true,
                                    })
                                    .await;
                                return;
                            }
                            if let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) {
                                let _ = tx.send(parsed).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}
