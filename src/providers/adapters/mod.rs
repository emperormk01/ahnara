//! Provider adapters -- translate AHNARA's internal CompletionRequest/Response
//! into provider-native formats and back.
//!
//! Each adapter implements `ProviderAdapter` and handles:
//!   - URL construction (e.g., Google uses query-param key, Anthropic uses /v1/messages)
//!   - Request body transformation (message format, system prompt placement, tool schema)
//!   - Response parsing (provider-specific JSON shapes)
//!   - Streaming format translation (Anthropic sends SSE differently)
//!   - Header construction (Anthropic wants x-api-key, Google wants ?key= in URL)
//!
//! The `AdapterRegistry` maps provider_type strings to adapter instances so
//! `OpenAICompatibleProvider` can delegate format concerns to the right adapter.

pub mod anthropic;
pub mod gemini;
pub mod openai;
#[cfg(test)]
pub mod adapter_tests;

use crate::providers::{CompletionRequest, CompletionResponse, StreamChunk};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use tokio::sync::mpsc;

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn provider_type(&self) -> &str;

    fn build_url(&self, api_base: &str, api_key: &str, model: &str) -> String;

    fn build_headers(&self, api_key: &str) -> reqwest::header::HeaderMap;

    fn transform_request(&self, request: &CompletionRequest) -> serde_json::Value;

    fn parse_response(&self, body: &str) -> Result<CompletionResponse>;

    async fn stream_response(
        &self,
        client: &Client,
        url: &str,
        headers: reqwest::header::HeaderMap,
        body: serde_json::Value,
    ) -> Result<mpsc::Receiver<StreamChunk>>;
}

pub struct AdapterRegistry {
    adapters: HashMap<String, Box<dyn ProviderAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        let mut adapters: HashMap<String, Box<dyn ProviderAdapter>> = HashMap::new();
        adapters.insert("openai".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("openrouter".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("groq".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("deepseek".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("nvidia".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("openai-compatible".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("custom/openai-compatible".into(), Box::new(openai::OpenAIAdapter::new()));
        adapters.insert("google".into(), Box::new(gemini::GeminiAdapter::new()));
        adapters.insert("anthropic".into(), Box::new(anthropic::AnthropicAdapter::new()));
        adapters.insert("custom/anthropic".into(), Box::new(anthropic::AnthropicAdapter::new()));
        Self { adapters }
    }

    pub fn get(&self, provider_type: &str) -> Option<&dyn ProviderAdapter> {
        self.adapters.get(provider_type).map(|a| a.as_ref())
    }

    pub fn get_or_default(&self, provider_type: &str) -> &dyn ProviderAdapter {
        self.get(provider_type).unwrap_or_else(|| {
            self.adapters.get("openai-compatible")
                .or_else(|| self.adapters.values().next())
                .expect("no provider adapters registered")
                .as_ref()
        })
    }
}
