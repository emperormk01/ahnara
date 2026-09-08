//! Schedule command - manage scheduled jobs from any channel

use crate::tools::scheduler_tools::SchedulerManager;

/// Handle /schedule command across all channels
///
/// Usage:
///   /schedule list                    - List all jobs
///   /schedule add <name> <cron> <prompt>  - Create a job
///   /schedule remove <name>           - Delete a job
///   /schedule enable <name>           - Enable a job
///   /schedule disable <name>          - Disable a job
///   /schedule info <name>             - Show job details
pub async fn handle_schedule(args: &str, manager: &SchedulerManager) -> String {
    let parts: Vec<&str> = args.trim().splitn(4, ' ').collect();
    let subcmd = parts.first().copied().unwrap_or("list");

    match subcmd {
        "list" | "ls" | "" => handle_list_jobs(manager),
        "add" | "create" => handle_add_job(&parts, manager),
        "remove" | "delete" | "rm" => handle_remove_job(&parts, manager),
        "enable" => handle_enable_job(&parts, manager),
        "disable" => handle_disable_job(&parts, manager),
        "info" => handle_job_info(&parts, manager),
        _ => format!("Unknown command: {}\n\n{}", subcmd, help_text()),
    }
}

fn handle_list_jobs(manager: &SchedulerManager) -> String {
    let jobs = manager.list_jobs();
    if jobs.is_empty() {
        return "No scheduled jobs configured.\n\nUsage: /schedule add <name> <cron> <prompt>".to_string();
    }

    let mut out = String::from("Scheduled Jobs\n\n");
    for job in &jobs {
        let name = job["name"].as_str().unwrap_or("?");
        let cron = job["cron"].as_str().unwrap_or("?");
        let enabled = job["enabled"].as_bool().unwrap_or(true);
        let runs = job["run_count"].as_u64().unwrap_or(0);
        let last = job["last_run"].as_str().unwrap_or("never");
        let success = job["last_success"].as_bool().unwrap_or(false);
        let status_icon = if !enabled { "OFF" } else if success || runs == 0 { "OK" } else { "ERR" };
        let summary = job["prompt_summary"].as_str().unwrap_or("");

        out.push_str(&format!(
            "[{}] {} ({})\n  Cron: {}\n  Runs: {} | Last: {}\n  Prompt: {}\n\n",
            status_icon, name, if enabled { "enabled" } else { "disabled" },
            cron, runs, last,
            if summary.len() > 80 { format!("{}...", &summary[..80]) } else { summary.to_string() }
        ));
    }
    out
}

fn handle_add_job(parts: &[&str], manager: &SchedulerManager) -> String {
    if parts.len() < 4 {
        return "Usage: /schedule add <name> <cron> <prompt>\n\n\
            Examples:\n\
            /schedule add heartbeat */5 * * * * Say heartbeat\n\
            /schedule add daily-report 0 9 * * * Generate a daily summary report\n\
            /schedule add cleanup 0 0 * * 0 Clean up old sessions".to_string();
    }
    let name = parts[1];
    let cron = parts[2];
    let prompt = parts[3];

    match manager.add_job(name, cron, prompt, 300, None) {
        Ok(()) => format!("Job '{}' created.\nCron: {}\nPrompt: {}\n\nNote: Active on next gateway restart.", name, cron, prompt),
        Err(e) => format!("Error: {}", e),
    }
}

fn handle_remove_job(parts: &[&str], manager: &SchedulerManager) -> String {
    if parts.len() < 2 {
        return "Usage: /schedule remove <name>".to_string();
    }
    let name = parts[1];
    match manager.delete_job(name) {
        Ok(()) => format!("Job '{}' deleted.", name),
        Err(e) => format!("Error: {}", e),
    }
}

fn handle_enable_job(parts: &[&str], manager: &SchedulerManager) -> String {
    if parts.len() < 2 {
        return "Usage: /schedule enable <name>".to_string();
    }
    let name = parts[1];
    match manager.update_job(name, None, None, None, Some(true)) {
        Ok(()) => format!("Job '{}' enabled.", name),
        Err(e) => format!("Error: {}", e),
    }
}

fn handle_disable_job(parts: &[&str], manager: &SchedulerManager) -> String {
    if parts.len() < 2 {
        return "Usage: /schedule disable <name>".to_string();
    }
    let name = parts[1];
    match manager.update_job(name, None, None, None, Some(false)) {
        Ok(()) => format!("Job '{}' disabled.", name),
        Err(e) => format!("Error: {}", e),
    }
}

fn handle_job_info(parts: &[&str], manager: &SchedulerManager) -> String {
    if parts.len() < 2 {
        return "Usage: /schedule info <name>".to_string();
    }
    let name = parts[1];
    let jobs = manager.list_jobs();
    match jobs.iter().find(|j| j["name"].as_str() == Some(name)) {
        Some(job) => format!(
            "Job: {}\nCron: {}\nEnabled: {}\nRuns: {}\nLast run: {}\nLast success: {}\nLast result: {}",
            job["name"].as_str().unwrap_or("?"),
            job["cron"].as_str().unwrap_or("?"),
            job["enabled"].as_bool().unwrap_or(false),
            job["run_count"].as_u64().unwrap_or(0),
            job["last_run"].as_str().unwrap_or("never"),
            job["last_success"].as_bool().unwrap_or(false),
            job["last_result"].as_str().unwrap_or("(none)"),
        ),
        None => format!("Job '{}' not found.", name),
    }
}

fn help_text() -> &'static str {
    "Usage: /schedule <command>\n\n\
        Commands:\n\
        /schedule list                     - List all jobs\n\
        /schedule add <name> <cron> <prompt> - Add a job\n\
        /schedule remove <name>           - Remove a job\n\
        /schedule enable <name>           - Enable a job\n\
        /schedule disable <name>          - Disable a job\n\
        /schedule info <name>             - Show job details"
}
                None => format!("Job '{}' not found.", name),
            }
        }
    }
}

fn help_text() -> &'static str {
    "Usage: /schedule <command>\n\n\
        Commands:\n\
        /schedule list                     - List all jobs\n\
        /schedule add <name> <cron> <prompt> - Add a job\n\
        /schedule remove <name>           - Remove a job\n\
        /schedule enable <name>           - Enable a job\n\
        /schedule disable <name>          - Disable a job\n\
        /schedule info <name>             - Show job details"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::ScheduleRunLog;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    fn make_manager() -> SchedulerManager {
        let log: ScheduleRunLog = Arc::new(RwLock::new(HashMap::new()));
        SchedulerManager::new(log, "/tmp/ahnara-test-sched-cmd.toml".to_string())
    }

    #[tokio::test]
    async fn list_empty_returns_message() {
        let mgr = make_manager();
        let result = handle_schedule("list", &mgr).await;
        assert!(result.contains("No scheduled jobs"));
    }
}
