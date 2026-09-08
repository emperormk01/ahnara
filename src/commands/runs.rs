//! Run database command handlers.

use anyhow::{bail, Result};
use std::path::PathBuf;

use crate::runs::RunDatabase;

fn open(db_path: Option<PathBuf>) -> Result<RunDatabase> {
    RunDatabase::open(db_path.unwrap_or_else(RunDatabase::default_path))
}

pub async fn handle_runs(action: crate::cli::RunsCommands, db_path: Option<PathBuf>) -> Result<()> {
    let db = open(db_path)?;
    match action {
        crate::cli::RunsCommands::List { limit } => handle_list_runs(&db, limit),
        crate::cli::RunsCommands::Show { id } => handle_show_run(&db, &id),
        crate::cli::RunsCommands::Export { id, output } => handle_export_run(&db, &id, output),
        crate::cli::RunsCommands::Replay { id } => handle_replay_run(&db, &id),
    }
}

fn handle_list_runs(db: &RunDatabase, limit: usize) -> Result<()> {
    for run in db.list_runs(limit)? {
        println!(
            "{}  {}  {}  {}  {}",
            run.id, run.status, run.kind, run.started_at, run.goal
        );
    }
    Ok(())
}

fn handle_show_run(db: &RunDatabase, id: &str) -> Result<()> {
    let run = db.get_run(id)?.ok_or_else(|| anyhow::anyhow!("run not found: {}", id))?;
    println!("Run: {}", run.id);
    println!("Kind: {}", run.kind);
    println!("Status: {}", run.status);
    println!("Started: {}", run.started_at);
    println!("Finished: {}", run.finished_at.unwrap_or_else(|| "-".into()));
    println!("Goal: {}", run.goal);
    println!("Metadata: {}", serde_json::to_string_pretty(&run.metadata)?);
    println!("\nSteps:");
    for (step_id, status, description, tool, error) in db.get_steps(id)? {
        println!(
            "- {} [{}] {} ({})",
            step_id, status, description, tool.unwrap_or_else(|| "manual".into())
        );
        if let Some(error) = error {
            println!("  error: {}", error);
        }
    }
    println!("\nEvents:");
    for (timestamp, event_type, message, _) in db.get_events(id)? {
        println!("- {} [{}] {}", timestamp, event_type, message);
    }
    Ok(())
}

fn handle_export_run(db: &RunDatabase, id: &str, output: Option<PathBuf>) -> Result<()> {
    let run = db.get_run(id)?.ok_or_else(|| anyhow::anyhow!("run not found: {}", id))?;
    let doc = serde_json::json!({
        "run": run,
        "steps": db.get_steps(id)?,
        "events": db.get_events(id)?,
    });
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, serde_json::to_string_pretty(&doc)? + "\n")?;
        println!("Exported run to {}", output.display());
    } else {
        println!("{}", serde_json::to_string_pretty(&doc)?);
    }
    Ok(())
}

fn handle_replay_run(db: &RunDatabase, id: &str) -> Result<()> {
    let run = db.get_run(id)?.ok_or_else(|| anyhow::anyhow!("run not found: {}", id))?;
    if run.kind != "plan" {
        bail!("only plan runs can be replayed currently");
    }
    println!("Replay metadata for run {}:", id);
    println!("{}", serde_json::to_string_pretty(&run.metadata)?);
    println!("Replay execution from stored plan snapshots is reserved for the next planner iteration.");
    Ok(())
}
