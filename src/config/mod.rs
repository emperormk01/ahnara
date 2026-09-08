//! Configuration management for AHNARA

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::persona::PersonaConfig;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub sub_agents: SubAgentsConfig,
    pub persona: PersonaConfig,
    pub providers: ProvidersConfig,
    pub memory: MemoryConfig,
    pub channels: ChannelsConfig,
    pub tools: ToolsConfig,
    pub mcp: McpConfig,
    pub scheduler: SchedulerConfig,
    pub plugins: PluginsConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub name: String,
    pub default_model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub max_tool_iterations: u32,
    #[serde(default = "default_nudge_after_tool_calls")]
    pub nudge_after_tool_calls: u32,
    pub context_window_tokens: u32,
    #[serde(default = "default_recent_history_turns")]
    pub recent_history_turns: usize,
    #[serde(default = "default_tool_output_max_chars")]
    pub tool_output_max_chars: usize,
    pub timezone: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "AHNARA".into(),
            default_model: "".into(),
            max_tokens: 8192,
            temperature: 1.0,
            max_tool_iterations: 100,
            nudge_after_tool_calls: 10,
            context_window_tokens: 20000,
            recent_history_turns: 60,
            tool_output_max_chars: 10_000,
            timezone: "UTC".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProvidersConfig {
    #[serde(default = "default_provider_name")]
    pub active: String,
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    #[serde(default = "default_pool_size")]
    pub connection_pool_size: usize,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
}

fn default_provider_name() -> String {
    "".into()
}
fn default_pool_size() -> usize {
    32
}
fn default_timeout() -> u64 {
    60
}
fn default_recent_history_turns() -> usize {
    60
}
fn default_tool_output_max_chars() -> usize {
    10_000
}
fn default_nudge_after_tool_calls() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderEntry {
    pub name: String,
    pub api_key: String,
    pub api_base: String,
    #[serde(default)]
    pub extra_headers: Option<HashMap<String, String>>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            active: "".into(),
            providers: vec![],
            connection_pool_size: 32,
            request_timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubAgentsConfig {
    /// Enable sub-agent delegation
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum complexity score to trigger delegation (0-100)
    #[serde(default = "default_min_complexity")]
    pub min_complexity: u32,
    /// Maximum token budget per session for sub-agents
    #[serde(default = "default_max_budget")]
    pub max_budget: u32,
    /// Maximum concurrent sub-agents
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Timeout for sub-agent tasks (seconds)
    #[serde(default = "default_subagent_timeout")]
    pub timeout_secs: u64,
    /// Fallback to main agent on failure
    #[serde(default = "default_true")]
    pub fallback_on_error: bool,
    /// Cost tracking enabled
    #[serde(default = "default_true")]
    pub track_cost: bool,
}

fn default_min_complexity() -> u32 {
    50
}
fn default_max_budget() -> u32 {
    30000
}
fn default_max_concurrent() -> u32 {
    5
}
fn default_subagent_timeout() -> u64 {
    60
}

impl Default for SubAgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_complexity: 50,
            max_budget: 30000,
            max_concurrent: 5,
            timeout_secs: 60,
            fallback_on_error: true,
            track_cost: true,
        }
    }
}

fn default_cache_size() -> usize {
    1000
}
fn default_db_path() -> String {
    "~/.ahnara/memory.db".into()
}
fn default_consolidation() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryConfig {
    #[serde(default = "default_cache_size")]
    pub hot_cache_size: usize,
    #[serde(default = "default_db_path")]
    pub database_path: String,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default = "default_consolidation")]
    pub consolidation_interval_secs: u64,
    // Compaction settings
    #[serde(default = "default_true")]
    pub compaction_enabled: bool,
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: usize,
    #[serde(default = "default_compaction_keep_recent")]
    pub compaction_keep_recent: usize,
    #[serde(default = "default_compaction_cooldown")]
    pub compaction_cooldown_secs: u64,
    // Reflection settings
    #[serde(default = "default_true")]
    pub reflection_enabled: bool,
    #[serde(default = "default_reflection_min_messages")]
    pub reflection_min_messages: usize,
    #[serde(default = "default_reflection_cooldown")]
    pub reflection_cooldown_secs: u64,
    #[serde(default = "default_reflection_interval")]
    pub reflection_interval_secs: u64,
}

fn default_compaction_threshold() -> usize {
    40
}
fn default_compaction_keep_recent() -> usize {
    10
}
fn default_compaction_cooldown() -> u64 {
    300
}
fn default_reflection_min_messages() -> usize {
    5
}
fn default_reflection_cooldown() -> u64 {
    300
}
fn default_reflection_interval() -> u64 {
    300
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            hot_cache_size: 1000,
            database_path: "~/.ahnara/memory.db".into(),
            embedding_model: None,
            consolidation_interval_secs: 300,
            compaction_enabled: true,
            compaction_threshold: 40,
            compaction_keep_recent: 10,
            compaction_cooldown_secs: 300,
            reflection_enabled: true,
            reflection_min_messages: 5,
            reflection_cooldown_secs: 300,
            reflection_interval_secs: 300, // 5 minutes of inactivity
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ChannelsConfig {
    pub telegram: TelegramConfig,
    pub discord: DiscordConfig,
    pub slack: SlackConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token: String,
    pub webhook_url: Option<String>,
    pub allowed_users: Vec<String>,
    pub group_policy: GroupPolicy,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub token: String,
    pub allowed_guilds: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct SlackConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub app_token: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum GroupPolicy {
    #[serde(rename = "mention")]
    Mention,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "closed")]
    Closed,
}

impl Default for GroupPolicy {
    fn default() -> Self {
        Self::Mention
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub tool_prefix: Option<String>,
    pub include_tools: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            tool_prefix: None,
            include_tools: Vec::new(),
            exclude_tools: Vec::new(),
            timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct SchedulerConfig {
    pub enabled: bool,
    pub jobs: Vec<ScheduleJobConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ScheduleJobConfig {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub session_id: Option<String>,
    pub enabled: bool,
    pub run_on_startup: bool,
    pub timeout_secs: u64,
}

impl Default for ScheduleJobConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            cron: String::new(),
            prompt: String::new(),
            session_id: None,
            enabled: true,
            run_on_startup: false,
            timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub exec_enabled: bool,
    #[serde(default = "default_exec_timeout")]
    pub exec_timeout_secs: u64,
    #[serde(default = "default_true")]
    pub restrict_to_workspace: bool,
    #[serde(default)]
    pub web_search_enabled: bool,
    #[serde(default = "default_brave")]
    pub web_search_provider: String,
    #[serde(default)]
    pub web_search_api_key: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_exec_timeout() -> u64 {
    60
}
fn default_brave() -> String {
    "brave".into()
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            exec_enabled: true,
            exec_timeout_secs: 60,
            restrict_to_workspace: true,
            web_search_enabled: true,
            web_search_provider: "brave".into(),
            web_search_api_key: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub plugins: Vec<PluginConfig>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_secs: 10,
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginConfig {
    pub name: String,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub hooks: Vec<String>,
    pub timeout_secs: Option<u64>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            hooks: Vec::new(),
            timeout_secs: None,
        }
    }
}

impl PluginConfig {
    pub fn has_hook(&self, event: crate::plugins::HookEvent) -> bool {
        self.hooks
            .iter()
            .filter_map(|hook| crate::plugins::HookEvent::from_str(hook))
            .any(|hook| hook == event)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub cors_enabled: bool,
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    18789
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 18789,
            cors_enabled: true,
        }
    }
}

impl AppConfig {
    pub fn load(path: &str) -> Result<Self> {
        // Try to load from file, otherwise use defaults with env overrides
        if Path::new(path).exists() {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {}", path))?;
            let config: AppConfig =
                toml::from_str(&content).with_context(|| "Failed to parse config file")?;
            return Ok(config.with_env_overrides());
        }

        // Create default config with env overrides
        Ok(Self::default().with_env_overrides())
    }

    fn with_env_overrides(mut self) -> Self {
        // First: any provider whose key is empty OR still the setup placeholder
        // can be filled from a single global `AHNARA_API_KEY` env var.
        // This lets users export a key once instead of repeating it in
        // config.toml for every provider.
        let global_key = std::env::var("AHNARA_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());
        const PLACEHOLDER: &str = "<set via ahnara token or AHNARA_API_KEY env>";
        for provider in &mut self.providers.providers {
            if provider.api_key.is_empty() || provider.api_key == PLACEHOLDER {
                if let Some(ref k) = global_key {
                    provider.api_key = k.clone();
                }
            }
        }
        // Check if any provider has an empty API key and try to fill from env
        for provider in &mut self.providers.providers {
            if provider.api_key.is_empty() {
                // Try to fill from environment based on provider name
                match provider.name.to_lowercase().as_str() {
                    "nvidia" => {
                        if let Ok(key) = std::env::var("NVIDIA_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://integrate.api.nvidia.com/v1".into();
                        }
                    }
                    "openai" => {
                        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://api.openai.com/v1".into();
                        }
                    }
                    "google" | "google_ai_studio" | "gemini" => {
                        if let Ok(key) = std::env::var("GOOGLE_AI_STUDIO_KEY") {
                            provider.api_key = key;
                            provider.api_base =
                                "https://generativelanguage.googleapis.com/v1beta/openai".into();
                        }
                    }
                    "anthropic" => {
                        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://api.anthropic.com/v1".into();
                        }
                    }
                    "openrouter" => {
                        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://openrouter.ai/api/v1".into();
                        }
                    }
                    "groq" => {
                        if let Ok(key) = std::env::var("GROQ_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://api.groq.com/openai/v1".into();
                        }
                    }
                    "deepseek" => {
                        if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
                            provider.api_key = key;
                            provider.api_base = "https://api.deepseek.com/v1".into();
                        }
                    }
                    _ => {}
                }
            }
        }

        // Telegram token from env (only if not set in config)
        if self.channels.telegram.token.is_empty() {
            if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
                self.channels.telegram.token = token.clone();
                self.channels.telegram.enabled = !token.is_empty();
            }
        }

        // Discord token from env (only if not set in config)
        if self.channels.discord.token.is_empty() {
            if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
                self.channels.discord.token = token.clone();
                self.channels.discord.enabled = !token.is_empty();
            }
        }

        // Memory database path
        if let Ok(path) = std::env::var("AHNARA_DB_PATH") {
            self.memory.database_path = path;
        }

        self
    }
}
