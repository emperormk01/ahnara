//! Cron scheduler for autonomous recurring agent tasks.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tokio_cron_scheduler::{Job, JobScheduler};

use crate::agent::AgentCore;
use crate::config::{ScheduleJobConfig, SchedulerConfig};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};
use std::fs;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRunEntry {
    pub name: String,
    pub cron: String,
    pub prompt_summary: String,
    pub last_run_at: u64,
    pub last_result_summary: String,
    pub last_success: bool,
    pub run_count: u64,
    pub enabled: bool,
}

pub type ScheduleRunLog = Arc<RwLock<HashMap<String, ScheduleRunEntry>>>;

const STATE_FILE: &str = "~/.ahnara/schedule_state.json";

fn load_state_file() -> HashMap<String, ScheduleRunEntry> {
    let path = shellexpand::tilde(STATE_FILE);
    match fs::read_to_string(path.as_ref()) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn persist_state_file(log: &ScheduleRunLog) {
    let path = shellexpand::tilde(STATE_FILE);
    if let Ok(guard) = log.read() {
        if let Ok(json) = serde_json::to_string_pretty(&*guard) {
            let _ = fs::write(path.as_ref(), json);
        }
    }
}

/// Pre-populate a run log from config so entries exist before the first run fires.
pub fn create_run_log(jobs: &[ScheduleJobConfig]) -> ScheduleRunLog {
    let mut persisted = load_state_file();
    let _now = now_epoch();
    let mut map = HashMap::new();

    for job in jobs {
        let entry = persisted.remove(&job.name).unwrap_or_else(|| ScheduleRunEntry {
            name: job.name.clone(),
            cron: job.cron.clone(),
            prompt_summary: job.prompt.chars().take(100).collect(),
            last_run_at: 0,
            last_result_summary: String::new(),
            last_success: false,
            run_count: 0,
            enabled: job.enabled,
        });
        map.insert(job.name.clone(), entry);
    }

    Arc::new(RwLock::new(map))
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct CronScheduler {
    scheduler: JobScheduler,
    job_id_map: HashMap<String, Uuid>,
}

impl CronScheduler {
    pub async fn start(agent: Arc<AgentCore>, config: SchedulerConfig, log: ScheduleRunLog) -> Result<Option<Self>> {
        if !config.enabled || config.jobs.is_empty() {
            return Ok(None);
        }

        let scheduler = JobScheduler::new()
            .await
            .context("failed to create cron scheduler")?;

        let mut job_id_map = HashMap::new();
        let mut registered = 0usize;
        for job_config in config.jobs.into_iter().filter(|job| job.enabled) {
            validate_job(&job_config)?;
            let cron = job_config.cron.clone();
            let name = job_config.name.clone();
            let prompt = job_config.prompt.clone();
            let session_id = job_config.session_id.clone();
            let timeout_secs = job_config.timeout_secs;
            let run_on_startup = job_config.run_on_startup;
            let job_agent = agent.clone();
            let job_log = log.clone();

            let job = Job::new_async(cron.as_str(), move |_uuid, _lock| {
                let agent = job_agent.clone();
                let name = name.clone();
                let prompt = prompt.clone();
                let session_id = session_id.clone();
                let log = job_log.clone();
                Box::pin(async move {
                    run_scheduled_job(agent, name, prompt, session_id, timeout_secs, log).await;
                })
            })
            .with_context(|| format!("invalid cron expression for job {}", job_config.name))?;

            let job_name = job_config.name.clone();
            let uuid = scheduler
                .add(job)
                .await
                .with_context(|| format!("failed to add cron job {}", job_config.name))?;
            job_id_map.insert(job_name, uuid);

            if run_on_startup {
                let startup_agent = agent.clone();
                let startup_name = job_config.name.clone();
                let startup_prompt = job_config.prompt.clone();
                let startup_session_id = job_config.session_id.clone();
                let startup_log = log.clone();
                tokio::spawn(async move {
                    run_scheduled_job(
                        startup_agent,
                        startup_name,
                        startup_prompt,
                        startup_session_id,
                        timeout_secs,
                        startup_log,
                    )
                    .await;
                });
            }

            registered += 1;
        }

        if registered == 0 {
            return Ok(None);
        }

        scheduler
            .start()
            .await
            .context("failed to start cron scheduler")?;
        tracing::info!("⏰ Started cron scheduler with {} jobs", registered);
        Ok(Some(Self { scheduler, job_id_map }))
    }

    pub async fn add_job(
        &mut self,
        agent: Arc<AgentCore>,
        name: String,
        cron: String,
        prompt: String,
        timeout_secs: u64,
        session_id: Option<String>,
        log: ScheduleRunLog,
    ) -> Result<()> {
        let job_agent = agent.clone();
        let job_name = name.clone();
        let job_prompt = prompt.clone();
        let job_session_id = session_id.clone();
        let job_log = log.clone();

        let job = Job::new_async(cron.as_str(), move |_uuid, _lock| {
            let agent = job_agent.clone();
            let name = name.clone();
            let prompt = job_prompt.clone();
            let session_id = job_session_id.clone();
            let log = job_log.clone();
            Box::pin(async move {
                run_scheduled_job(agent, name, prompt, session_id, timeout_secs, log).await;
            })
        })
        .with_context(|| format!("invalid cron expression for job '{}'", job_name))?;

        let uuid = self.scheduler.add(job).await
            .map_err(|e| anyhow::anyhow!("failed to add job '{}': {:?}", job_name, e))?;

        self.job_id_map.insert(job_name.clone(), uuid);
        tracing::info!("Added live cron job '{}' ({})", job_name, uuid);
        Ok(())
    }

    pub async fn remove_job(&mut self, name: &str) -> Result<()> {
        let uuid = self.job_id_map.remove(name)
            .ok_or_else(|| anyhow::anyhow!("job '{}' not found in live scheduler", name))?;

        self.scheduler.remove(&uuid).await
            .map_err(|e| anyhow::anyhow!("failed to remove job '{}': {:?}", name, e))?;

        tracing::info!("Removed live cron job '{}'", name);
        Ok(())
    }

    pub async fn update_job(
        &mut self,
        agent: Arc<AgentCore>,
        name: String,
        cron: Option<String>,
        prompt: Option<String>,
        timeout_secs: Option<u64>,
        enabled: Option<bool>,
        session_id: Option<String>,
        log: ScheduleRunLog,
    ) -> Result<()> {
        self.remove_job(&name).await?;

        let new_enabled = enabled.unwrap_or(true);
        if !new_enabled {
            tracing::info!("Disabled live cron job '{}'", name);
            return Ok(());
        }

        let new_cron = cron.unwrap_or_else(|| {
            log.read().ok()
                .and_then(|g| g.get(&name).map(|e| e.cron.clone()))
                .unwrap_or_default()
        });

        self.add_job(agent, name, new_cron, prompt.unwrap_or_default(), timeout_secs.unwrap_or(300), session_id, log).await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.scheduler
            .shutdown()
            .await
            .context("failed to shut down cron scheduler")
    }
}

async fn run_scheduled_job(
    agent: Arc<AgentCore>,
    name: String,
    prompt: String,
    session_id: Option<String>,
    timeout_secs: u64,
    log: ScheduleRunLog,
) {
    tracing::info!("Running scheduled job: {}", name);
    let task = agent.process(&prompt, session_id.as_deref());
    let result = timeout(Duration::from_secs(timeout_secs), task).await;

    let (success, summary) = match &result {
        Ok(response) => {
            let s = summarize(response);
            tracing::info!("Scheduled job complete: {} ({})", name, s);
            (true, s)
        }
        Err(_) => {
            tracing::error!("Scheduled job timed out: {} after {}s", name, timeout_secs);
            (false, format!("timed out after {}s", timeout_secs))
        }
    };

    // Update the shared run log
    if let Ok(mut guard) = log.write() {
        if let Some(entry) = guard.get_mut(&name) {
            entry.last_run_at = now_epoch();
            entry.last_result_summary = summary;
            entry.last_success = success;
            entry.run_count += 1;
        }
    }
    persist_state_file(&log);
}

fn validate_job(job: &ScheduleJobConfig) -> Result<()> {
    if job.name.trim().is_empty() {
        anyhow::bail!("scheduled job name cannot be empty");
    }
    if job.cron.trim().is_empty() {
        anyhow::bail!("scheduled job {} cron cannot be empty", job.name);
    }
    if job.prompt.trim().is_empty() {
        anyhow::bail!("scheduled job {} prompt cannot be empty", job.name);
    }
    if job.timeout_secs == 0 {
        anyhow::bail!(
            "scheduled job {} timeout_secs must be greater than 0",
            job.name
        );
    }
    Ok(())
}

fn summarize(response: &str) -> String {
    const MAX: usize = 160;
    let compact = response.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= MAX {
        compact
    } else {
        format!("{}...", &compact[..MAX])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_job_config() {
        let job = ScheduleJobConfig {
            name: "heartbeat".into(),
            cron: "0 0/5 * * * *".into(),
            prompt: "Say heartbeat".into(),
            ..Default::default()
        };
        assert!(validate_job(&job).is_ok());
    }

    #[test]
    fn rejects_empty_prompt() {
        let job = ScheduleJobConfig {
            name: "bad".into(),
            cron: "0 0/5 * * * *".into(),
            prompt: String::new(),
            ..Default::default()
        };
        assert!(validate_job(&job).is_err());
    }

    #[test]
    fn summarizes_long_output() {
        let out = summarize(&"x ".repeat(200));
        assert!(out.ends_with("..."));
        assert!(out.len() <= 163);
    }
}
