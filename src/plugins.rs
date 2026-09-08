//! Plugin lifecycle hooks.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::config::{PluginConfig, PluginsConfig};
use crate::orchestrator::{ToolResult, ToolSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    Startup,
    BeforeMessage,
    AfterMessage,
    BeforeTool,
    AfterTool,
    Shutdown,
}

impl HookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::BeforeMessage => "before_message",
            Self::AfterMessage => "after_message",
            Self::BeforeTool => "before_tool",
            Self::AfterTool => "after_tool",
            Self::Shutdown => "shutdown",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "startup" => Some(Self::Startup),
            "before_message" => Some(Self::BeforeMessage),
            "after_message" => Some(Self::AfterMessage),
            "before_tool" => Some(Self::BeforeTool),
            "after_tool" => Some(Self::AfterTool),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    pub event: String,
    pub plugin: String,
    pub session_id: Option<String>,
    pub message: Option<String>,
    pub response: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub tool_result: Option<ToolResult>,
    pub available_tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginHookOutput {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub tool_args: Option<Value>,
    #[serde(default)]
    pub cancel: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PluginManager {
    config: PluginsConfig,
    tools: Vec<ToolSpec>,
}

impl PluginManager {
    pub fn new(config: PluginsConfig) -> Self {
        Self {
            config,
            tools: Vec::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn set_tools(&mut self, tools: Vec<ToolSpec>) {
        self.tools = tools;
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.clone()
    }

    pub fn enabled_plugins_for(&self, event: HookEvent) -> Vec<PluginConfig> {
        if !self.config.enabled {
            return Vec::new();
        }

        self.config
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled && plugin.has_hook(event))
            .cloned()
            .collect()
    }

    pub async fn run_hooks(
        &self,
        event: HookEvent,
        mut context: HookContext,
    ) -> Result<Vec<PluginHookOutput>> {
        let mut outputs = Vec::new();
        for plugin in self.enabled_plugins_for(event) {
            context.plugin = plugin.name.clone();
            let output = run_plugin_hook(&plugin, &context, self.config.timeout_secs).await?;
            outputs.push(output);
        }
        Ok(outputs)
    }

    pub async fn process_message_hooks(
        &self,
        event: HookEvent,
        session_id: Option<&str>,
        value: String,
    ) -> String {
        let mut current = value;
        let context = self.build_hook_context(event, session_id, &current);

        match self.run_hooks(event, context).await {
            Ok(outputs) => {
                current = self.apply_hook_outputs(event, current, outputs);
            }
            Err(err) => tracing::warn!("plugin hook failed for {}: {}", event.as_str(), err),
        }

        current
    }

    fn build_hook_context(
        &self,
        event: HookEvent,
        session_id: Option<&str>,
        current: &str,
    ) -> HookContext {
        HookContext {
            event: event.as_str().into(),
            plugin: String::new(),
            session_id: session_id.map(str::to_string),
            message: if event == HookEvent::BeforeMessage {
                Some(current.to_string())
            } else {
                None
            },
            response: if event == HookEvent::AfterMessage {
                Some(current.to_string())
            } else {
                None
            },
            tool_name: None,
            tool_args: None,
            tool_result: None,
            available_tools: self.tool_specs(),
        }
    }

    fn apply_hook_outputs(
        &self,
        event: HookEvent,
        mut current: String,
        outputs: Vec<HookOutput>,
    ) -> String {
        for output in outputs {
            if let Some(error) = output.error {
                tracing::warn!("plugin hook reported error: {}", error);
            }
            if event == HookEvent::BeforeMessage {
                if let Some(message) = output.message {
                    current = message;
                }
            } else if event == HookEvent::AfterMessage {
                if let Some(response) = output.response {
                    current = response;
                }
            }
        }
        current
    }

    pub async fn process_before_tool(&self, tool_name: &str, args: Value) -> Result<(bool, Value)> {
        let context = HookContext {
            event: HookEvent::BeforeTool.as_str().into(),
            plugin: String::new(),
            session_id: None,
            message: None,
            response: None,
            tool_name: Some(tool_name.into()),
            tool_args: Some(args.clone()),
            tool_result: None,
            available_tools: self.tool_specs(),
        };
        let mut current_args = args;
        for output in self.run_hooks(HookEvent::BeforeTool, context).await? {
            if output.cancel {
                return Ok((true, current_args));
            }
            if let Some(args) = output.tool_args {
                current_args = args;
            }
        }
        Ok((false, current_args))
    }

    pub async fn run_after_tool(&self, tool_name: &str, result: ToolResult) {
        let context = HookContext {
            event: HookEvent::AfterTool.as_str().into(),
            plugin: String::new(),
            session_id: None,
            message: None,
            response: None,
            tool_name: Some(tool_name.into()),
            tool_args: None,
            tool_result: Some(result),
            available_tools: self.tool_specs(),
        };
        if let Err(err) = self.run_hooks(HookEvent::AfterTool, context).await {
            tracing::warn!("after_tool plugin hook failed: {}", err);
        }
    }

    pub async fn run_lifecycle(&self, event: HookEvent) {
        let context = HookContext {
            event: event.as_str().into(),
            plugin: String::new(),
            session_id: None,
            message: None,
            response: None,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            available_tools: self.tool_specs(),
        };
        if let Err(err) = self.run_hooks(event, context).await {
            tracing::warn!("{} plugin hook failed: {}", event.as_str(), err);
        }
    }
}

async fn run_plugin_hook(
    plugin: &PluginConfig,
    context: &HookContext,
    default_timeout_secs: u64,
) -> Result<PluginHookOutput> {
    if plugin.command.trim().is_empty() {
        return Err(anyhow!("plugin {} has an empty command", plugin.name));
    }

    let timeout_secs = plugin.timeout_secs.unwrap_or(default_timeout_secs).max(1);
    let mut child = Command::new(&plugin.command)
        .args(&plugin.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn plugin {}", plugin.name))?;

    let mut stdin = child.stdin.take().context("plugin stdin unavailable")?;
    let payload = serde_json::to_vec(context)?;
    tokio::spawn(async move {
        let _ = stdin.write_all(&payload).await;
        let _ = stdin.write_all(b"\n").await;
    });

    let output = timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .with_context(|| format!("plugin {} timed out", plugin.name))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "plugin {} exited with {}: {}",
            plugin.name,
            output.status,
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(PluginHookOutput::default());
    }

    serde_json::from_str(&stdout)
        .with_context(|| format!("plugin {} returned invalid JSON", plugin.name))
}

pub fn plugin_result_error(tool_name: impl Into<String>, message: impl Into<String>) -> ToolResult {
    ToolResult {
        tool_name: tool_name.into(),
        success: false,
        output: json!({}),
        error: Some(message.into()),
        duration_ms: 0,
    }
}

pub type SharedPluginManager = Arc<PluginManager>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_events() {
        assert_eq!(HookEvent::from_str("startup"), Some(HookEvent::Startup));
        assert_eq!(
            HookEvent::from_str("before_tool"),
            Some(HookEvent::BeforeTool)
        );
        assert_eq!(HookEvent::from_str("bad"), None);
    }

    #[test]
    fn filters_enabled_plugins() {
        let manager = PluginManager::new(PluginsConfig {
            enabled: true,
            timeout_secs: 5,
            plugins: vec![PluginConfig {
                name: "audit".into(),
                enabled: true,
                command: "cat".into(),
                args: vec![],
                hooks: vec!["before_tool".into()],
                timeout_secs: None,
            }],
        });
        assert_eq!(manager.enabled_plugins_for(HookEvent::BeforeTool).len(), 1);
        assert_eq!(manager.enabled_plugins_for(HookEvent::AfterTool).len(), 0);
    }

    #[tokio::test]
    async fn hook_can_rewrite_message() {
        let config = PluginsConfig {
            enabled: true,
            timeout_secs: 5,
            plugins: vec![PluginConfig {
                name: "rewrite".into(),
                enabled: true,
                command: "python3".into(),
                args: vec!["-c".into(), "import json,sys; json.loads(sys.stdin.readline()); print(json.dumps({'message':'rewritten'}))".into()],
                hooks: vec!["before_message".into()],
                timeout_secs: Some(5),
            }],
        };
        let manager = PluginManager::new(config);
        let output = manager
            .process_message_hooks(HookEvent::BeforeMessage, Some("test"), "original".into())
            .await;
        assert_eq!(output, "rewritten");
    }
}
