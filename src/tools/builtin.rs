//! Built-in tools

use crate::orchestrator::{Tool, ToolResult};
use async_trait::async_trait;
use anyhow::{anyhow, Result};

use crate::scheduler::ScheduleRunLog;

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
