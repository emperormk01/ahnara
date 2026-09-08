//! Built-in tools

// Tools are defined in orchestrator/mod.rs for simplicity
// This module would contain additional specialized tools

use crate::orchestrator::{Tool, ToolResult};
use async_trait::async_trait;
use anyhow::{anyhow, Result};

use crate::scheduler::ScheduleRunLog;

/// HTTP fetch tool
pub struct HttpFetchTool {
 client: reqwest::Client,
}

impl HttpFetchTool {
 pub fn new() -> Self {
 Self {
 client: reqwest::Client::new(),
 }
 }
}

#[async_trait]
impl Tool for HttpFetchTool {
 fn name(&self) -> &str { "http_fetch" }
 fn description(&self) -> &str { "Fetch URL content" }
 fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object", "properties": {"url": {"type": "string"}}}) }
 async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
 let url_str = args["url"].as_str()
 .ok_or_else(|| anyhow!("Missing url parameter"))?;

 if let Some(error) = validate_url(url_str) {
     return Ok(ToolResult { tool_name: self.name().into(), success: false, output: serde_json::Value::Null, error: Some(error), duration_ms: 0 });
 }

 let response = self.client.get(url_str).send().await
 .map_err(|e| anyhow!("Request failed: {}", e))?;

 let status = response.status().as_u16();
 let body = response.text().await
 .map_err(|e| anyhow!("Failed to read response: {}", e))?;

 Ok(ToolResult {
 tool_name: self.name().to_string(),
 success: status >= 200 && status < 300,
 output: serde_json::json!({
 "status": status,
 "body": body.chars().take(10000).collect::<String>()
 }),
 error: None,
 duration_ms: 0,
 })
 }
}

fn validate_url(url_str: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url_str).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Some(format!("Blocked scheme: {}", scheme));
    }
    let host = parsed.host_str()?;
    if host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "::1" {
        return Some("Blocked localhost access".into());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(ipv4) if ipv4.is_private() || ipv4.is_loopback() || ipv4.is_link_local() || ipv4.is_unspecified() => {
                return Some("Blocked private IP access".into());
            }
            std::net::IpAddr::V6(ipv6) if ipv6.is_loopback() || ipv6.is_unspecified() => {
                return Some("Blocked IPv6 loopback".into());
            }
            _ => {}
        }
    }
    None
}

// Script execution tool - DISABLED FOR SECURITY
pub struct ScriptTool;

#[async_trait]
impl Tool for ScriptTool {
 fn name(&self) -> &str { "execute_script" }
 fn description(&self) -> &str { "Execute Python/TypeScript/Bash scripts" }
 fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
 async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
     Ok(ToolResult {
         tool_name: self.name().into(),
         success: false,
         output: serde_json::Value::Null,
         error: Some("DISABLED: Arbitrary command execution is not permitted for security reasons.".into()),
         duration_ms: 0,
     })
 }
}

// Parallel execution tool - DISABLED FOR SECURITY
pub struct ParallelTool;

#[async_trait]
impl Tool for ParallelTool {
 fn name(&self) -> &str { "execute_parallel" }
 fn description(&self) -> &str { "Execute commands in parallel" }
 fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
 async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
     Ok(ToolResult {
         tool_name: self.name().into(),
         success: false,
         output: serde_json::Value::Null,
         error: Some("DISABLED: Arbitrary command execution is not permitted for security reasons.".into()),
         duration_ms: 0,
     })
 }
}

pub struct ListScheduledJobsTool {
 log: ScheduleRunLog,
}

impl ListScheduledJobsTool {
 pub fn new(log: ScheduleRunLog) -> Self {
 Self { log }
 }
}

#[async_trait]
impl Tool for ListScheduledJobsTool {
 fn name(&self) -> &str { "list_scheduled_jobs" }
 fn description(&self) -> &str { "List all scheduled jobs with their status, last run time, and results" }
 fn parameters(&self) -> serde_json::Value {
 serde_json::json!({"type": "object", "properties": {}})
 }
 async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
 let entries = {
 let guard = self.log.read().map_err(|e| anyhow!("lock poisoned: {}", e))?;
 guard.values().cloned().collect::<Vec<_>>()
 };
 let now = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH)
 .unwrap_or_default()
 .as_secs();
 let jobs: Vec<_> = entries.iter().map(|e| {
 let last_run_human = if e.last_run_at == 0 {
 "never".to_string()
 } else {
 let ago_secs = now.saturating_sub(e.last_run_at);
 if ago_secs < 60 { format!("{}s ago", ago_secs) }
 else if ago_secs < 3600 { format!("{}m ago", ago_secs / 60) }
 else if ago_secs < 86400 { format!("{}h ago", ago_secs / 3600) }
 else { format!("{}d ago", ago_secs / 86400) }
 };
 serde_json::json!({
 "name": e.name,
 "cron": e.cron,
 "prompt_summary": e.prompt_summary,
 "enabled": e.enabled,
 "run_count": e.run_count,
 "last_run": last_run_human,
 "last_success": e.last_success,
 "last_result": e.last_result_summary,
 })
 }).collect();
 Ok(ToolResult {
 tool_name: self.name().into(),
 success: true,
 output: serde_json::json!({"jobs": jobs}),
 error: None,
 duration_ms: 0,
 })
 }
}
