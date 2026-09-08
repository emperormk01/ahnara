//! Scheduler management tools - create, update, delete scheduled jobs at runtime

use crate::orchestrator::{Tool, ToolResult};
use crate::scheduler::{CronScheduler, ScheduleRunLog};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

pub type CronHandle = Arc<Mutex<Option<CronScheduler>>>;

/// Shared scheduler state that allows runtime job mutation
/// Wraps the mutable job list and persists changes to config.toml
#[derive(Clone)]
pub struct SchedulerManager {
    run_log: ScheduleRunLog,
    config_path: String,
    agent: Option<Arc<crate::agent::AgentCore>>,
    cron_handle: Option<CronHandle>,
}

impl SchedulerManager {
    pub fn new(run_log: ScheduleRunLog, config_path: String) -> Self {
        Self { run_log, config_path, agent: None, cron_handle: None }
    }

    pub fn set_live_scheduler(&mut self, agent: Arc<crate::agent::AgentCore>, cron_handle: CronHandle) {
        self.agent = Some(agent);
        self.cron_handle = Some(cron_handle);
    }

    fn load_config(&self) -> anyhow::Result<crate::config::AppConfig> {
        let expanded = shellexpand::tilde(&self.config_path);
        crate::config::AppConfig::load(expanded.as_ref())
    }

    fn save_config(&self, config: &crate::config::AppConfig) -> anyhow::Result<()> {
        let expanded = shellexpand::tilde(&self.config_path);
        let content = toml::to_string_pretty(config)?;
        std::fs::write(expanded.as_ref(), content)?;
        Ok(())
    }

    pub fn add_job(&self, name: &str, cron: &str, prompt: &str, timeout_secs: u64, session_id: Option<&str>) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        if config.scheduler.jobs.iter().any(|j| j.name == name) {
            anyhow::bail!("Job '{}' already exists", name);
        }

        let job = crate::config::ScheduleJobConfig {
            name: name.to_string(),
            cron: cron.to_string(),
            prompt: prompt.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            enabled: true,
            run_on_startup: false,
            timeout_secs,
        };

        config.scheduler.enabled = true;
        config.scheduler.jobs.push(job);
        self.save_config(&config)?;

        if let Ok(mut guard) = self.run_log.write() {
            guard.insert(name.to_string(), crate::scheduler::ScheduleRunEntry {
                name: name.to_string(),
                cron: cron.to_string(),
                prompt_summary: prompt.chars().take(100).collect(),
                last_run_at: 0,
                last_result_summary: String::new(),
                last_success: false,
                run_count: 0,
                enabled: true,
            });
        }

        if let (Some(cron_handle), Some(agent)) = (&self.cron_handle, &self.agent) {
            let handle = cron_handle.clone();
            let agent = agent.clone();
            let log = self.run_log.clone();
            let name = name.to_string();
            let cron = cron.to_string();
            let prompt = prompt.to_string();
            let session_id = session_id.map(|s| s.to_string());
            tokio::spawn(async move {
                let mut guard = handle.lock().await;
                if let Some(ref mut scheduler) = *guard {
                    if let Err(e) = scheduler.add_job(agent, name, cron, prompt, timeout_secs, session_id, log).await {
                        tracing::warn!("Failed to register job with live scheduler: {}", e);
                    }
                }
            });
        }

        Ok(())
    }

    pub fn update_job(&self, name: &str, cron: Option<&str>, prompt: Option<&str>, timeout_secs: Option<u64>, enabled: Option<bool>) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        let job = config.scheduler.jobs.iter_mut().find(|j| j.name == name)
            .ok_or_else(|| anyhow::anyhow!("Job '{}' not found", name))?;

        if let Some(c) = cron { job.cron = c.to_string(); }
        if let Some(p) = prompt { job.prompt = p.to_string(); }
        if let Some(t) = timeout_secs { job.timeout_secs = t; }
        if let Some(e) = enabled { job.enabled = e; }

        let job_clone = job.clone();
        self.save_config(&config)?;

        if let Ok(mut guard) = self.run_log.write() {
            if let Some(entry) = guard.get_mut(name) {
                entry.cron = job_clone.cron.clone();
                entry.prompt_summary = job_clone.prompt.chars().take(100).collect();
                entry.enabled = job_clone.enabled;
            }
        }

        if let (Some(cron_handle), Some(agent)) = (&self.cron_handle, &self.agent) {
            let handle = cron_handle.clone();
            let agent = agent.clone();
            let log = self.run_log.clone();
            let name_owned = name.to_string();
            let job = job_clone.clone();
            tokio::spawn(async move {
                let mut guard = handle.lock().await;
                if let Some(ref mut scheduler) = *guard {
                    if let Err(e) = scheduler.update_job(
                        agent, name_owned, Some(job.cron), Some(job.prompt),
                        Some(job.timeout_secs), Some(job.enabled),
                        job.session_id, log,
                    ).await {
                        tracing::warn!("Failed to update live job: {}", e);
                    }
                }
            });
        }

        Ok(())
    }

    pub fn delete_job(&self, name: &str) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        let before = config.scheduler.jobs.len();
        config.scheduler.jobs.retain(|j| j.name != name);
        if config.scheduler.jobs.len() == before {
            anyhow::bail!("Job '{}' not found", name);
        }

        self.save_config(&config)?;

        if let Ok(mut guard) = self.run_log.write() {
            guard.remove(name);
        }

        if let Some(cron_handle) = &self.cron_handle {
            let handle = cron_handle.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let mut guard = handle.lock().await;
                if let Some(ref mut scheduler) = *guard {
                    if let Err(e) = scheduler.remove_job(&name).await {
                        tracing::warn!("Failed to remove job '{}' from live scheduler: {}", name, e);
                    }
                }
            });
        }

        Ok(())
    }

    pub fn list_jobs(&self) -> Vec<serde_json::Value> {
        let guard = match self.run_log.read() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        guard.values().map(|e| {
            let last_run_human = if e.last_run_at == 0 {
                "never".to_string()
            } else {
                let ago = now.saturating_sub(e.last_run_at);
                if ago < 60 { format!("{}s ago", ago) }
                else if ago < 3600 { format!("{}m ago", ago / 60) }
                else if ago < 86400 { format!("{}h ago", ago / 3600) }
                else { format!("{}d ago", ago / 86400) }
            };
            json!({
                "name": e.name,
                "cron": e.cron,
                "prompt_summary": e.prompt_summary,
                "enabled": e.enabled,
                "run_count": e.run_count,
                "last_run": last_run_human,
                "last_success": e.last_success,
                "last_result": e.last_result_summary,
            })
        }).collect()
    }
}

// --- CreateScheduledJobTool ---

pub struct CreateScheduledJobTool {
    manager: SchedulerManager,
}

impl CreateScheduledJobTool {
    pub fn new(manager: SchedulerManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for CreateScheduledJobTool {
    fn name(&self) -> &str { "create_scheduled_job" }

    fn description(&self) -> &str {
        "Create a new cron-scheduled job that runs autonomously. The job will execute \
         a prompt at the specified cron interval. Use standard cron syntax (e.g. '0 */2 * * *' \
         for every 2 hours, '0 9 * * 1-5' for weekdays at 9am). The job persists across restarts."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Unique job name (lowercase, hyphens ok)"
                },
                "cron": {
                    "type": "string",
                    "description": "Cron expression (5 fields: min hour day month weekday). Examples: '*/5 * * * *' = every 5min, '0 */2 * * *' = every 2h, '0 9 * * 1-5' = weekdays 9am"
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt the agent will execute when the job fires"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Max execution time in seconds (default: 300)",
                    "minimum": 30
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional session ID for stateful jobs"
                }
            },
            "required": ["name", "cron", "prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        let cron = args["cron"].as_str().unwrap_or("");
        let prompt = args["prompt"].as_str().unwrap_or("");
        let timeout = args["timeout_secs"].as_u64().unwrap_or(300);
        let session_id = args["session_id"].as_str();

        if name.is_empty() || cron.is_empty() || prompt.is_empty() {
            return Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: json!({"error": "name, cron, and prompt are required"}),
                error: Some("Missing required parameters".into()),
                duration_ms: 0,
            });
        }

        match self.manager.add_job(name, cron, prompt, timeout, session_id) {
            Ok(()) => {
                tracing::info!("Created scheduled job '{}' with cron '{}'", name, cron);
                Ok(ToolResult {
                    tool_name: self.name().into(),
                    success: true,
                    output: json!({
                        "status": "created",
                        "name": name,
                        "cron": cron,
                        "note": "Job saved to config and activated immediately."
                    }),
                    error: None,
                    duration_ms: 0,
                })
            }
            Err(e) => Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            }),
        }
    }
}

// --- UpdateScheduledJobTool ---

pub struct UpdateScheduledJobTool {
    manager: SchedulerManager,
}

impl UpdateScheduledJobTool {
    pub fn new(manager: SchedulerManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for UpdateScheduledJobTool {
    fn name(&self) -> &str { "update_scheduled_job" }

    fn description(&self) -> &str {
        "Update an existing scheduled job's cron expression, prompt, timeout, or enabled state."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Job name to update" },
                "cron": { "type": "string", "description": "New cron expression (optional)" },
                "prompt": { "type": "string", "description": "New prompt (optional)" },
                "timeout_secs": { "type": "integer", "description": "New timeout (optional)" },
                "enabled": { "type": "boolean", "description": "Enable/disable the job (optional)" }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        let cron = args["cron"].as_str();
        let prompt = args["prompt"].as_str();
        let timeout = args["timeout_secs"].as_u64();
        let enabled = args["enabled"].as_bool();

        match self.manager.update_job(name, cron, prompt, timeout, enabled) {
            Ok(()) => Ok(ToolResult {
                tool_name: self.name().into(),
                success: true,
                output: json!({"status": "updated", "name": name}),
                error: None,
                duration_ms: 0,
            }),
            Err(e) => Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            }),
        }
    }
}

// --- DeleteScheduledJobTool ---

pub struct DeleteScheduledJobTool {
    manager: SchedulerManager,
}

impl DeleteScheduledJobTool {
    pub fn new(manager: SchedulerManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DeleteScheduledJobTool {
    fn name(&self) -> &str { "delete_scheduled_job" }

    fn description(&self) -> &str {
        "Delete a scheduled job permanently. The job will be removed from config and stop running."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Job name to delete" }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        match self.manager.delete_job(name) {
            Ok(()) => {
                tracing::info!("Deleted scheduled job '{}'", name);
                Ok(ToolResult {
                    tool_name: self.name().into(),
                    success: true,
                    output: json!({"status": "deleted", "name": name, "note": "Job removed from config and stopped immediately."}),
                    error: None,
                    duration_ms: 0,
                })
            }
            Err(e) => Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            }),
        }
    }
}

// --- ListScheduledJobsEnhancedTool (replaces the old read-only one with manager integration) ---

pub struct ListScheduledJobsEnhancedTool {
    manager: SchedulerManager,
}

impl ListScheduledJobsEnhancedTool {
    pub fn new(manager: SchedulerManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ListScheduledJobsEnhancedTool {
    fn name(&self) -> &str { "list_scheduled_jobs" }

    fn description(&self) -> &str {
        "List all scheduled jobs with their cron expressions, status, last run time, results, and run counts."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let jobs = self.manager.list_jobs();
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: json!({"jobs": jobs, "count": jobs.len()}),
            error: None,
            duration_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    fn make_test_manager() -> SchedulerManager {
        let log: ScheduleRunLog = Arc::new(RwLock::new(HashMap::new()));
        SchedulerManager::new(log, "/tmp/ahnara-test-scheduler.toml".to_string())
    }

    #[test]
    fn list_jobs_returns_empty_initially() {
        let manager = make_test_manager();
        let jobs = manager.list_jobs();
        assert!(jobs.is_empty());
    }
}
