//! Run command handler - execute a skill

use anyhow::Result;
use std::fs;
use tokio::process::Command as AsyncCommand;
use tokio::time::{timeout, Duration};

pub async fn handle_run(skill: String, args: Vec<String>) -> Result<()> {
    let skills_dir = dirs::home_dir()
        .map(|h| h.join(".ahnara/skills"))
        .ok_or_else(|| anyhow::anyhow!("Could not find skills directory"))?;
    
    // Find skill
    let mut skill_path = None;
    for entry in walkdir::WalkDir::new(&skills_dir)
        .min_depth(2)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "SKILL.md")
    {
        let content = fs::read_to_string(entry.path())?;
        if let Ok(s) = crate::skills::Skill::parse(&content) {
            if s.name() == skill {
                skill_path = Some(entry.path().to_path_buf());
                break;
            }
        }
    }
    
    let skill_path = skill_path.ok_or_else(|| anyhow::anyhow!("Skill '{}' not found", skill))?;
    
    // Load skill
    let content = fs::read_to_string(&skill_path)?;
    let parsed_skill = crate::skills::Skill::parse(&content)?;
    
    println!("\n🦞 Running skill: {}\n", parsed_skill.name());
    println!("{}\n", parsed_skill.body);
    
    // If skill has scripts, list or run them
    let scripts_dir = skill_path.parent().unwrap().join("scripts");
    if scripts_dir.is_dir() {
        if args.is_empty() {
            let mut names: Vec<String> = fs::read_dir(&scripts_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();
            names.sort();
            if names.is_empty() {
                println!("No scripts in this skill.");
            } else {
                println!("Available scripts (run with: ahnara run {} <script> [args...]):", skill);
                for name in &names {
                    println!("  {}", name);
                }
            }
        } else {
            run_skill_script(&scripts_dir, &args).await?;
        }
    }

    Ok(())
}

/// Execute a script from a skill's `scripts/` directory.
///
/// `args[0]` selects the script by file name, the rest are passed through.
/// The resolved path must stay inside `scripts_dir` (no traversal), and only
/// `.sh`, `.py`, and `.js` files are runnable.
async fn run_skill_script(scripts_dir: &std::path::Path, args: &[String]) -> Result<()> {
    let requested = &args[0];
    if requested.contains('/') || requested.contains('\\') || requested == "." || requested == ".." {
        return Err(anyhow::anyhow!("Invalid script name: {}", requested));
    }
    let script_path = scripts_dir.join(requested);
    let canonical = fs::canonicalize(&script_path)
        .map_err(|_| anyhow::anyhow!("Script '{}' not found", requested))?;
    let canonical_dir = fs::canonicalize(scripts_dir)?;
    if !canonical.starts_with(&canonical_dir) || !canonical.is_file() {
        return Err(anyhow::anyhow!("Script '{}' not found", requested));
    }

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut cmd = match ext.as_str() {
        "sh" => {
            let mut c = AsyncCommand::new("bash");
            c.arg(&canonical);
            c
        }
        "py" => {
            let mut c = AsyncCommand::new("python3");
            c.arg(&canonical);
            c
        }
        "js" => {
            let mut c = AsyncCommand::new("node");
            c.arg(&canonical);
            c
        }
        _ => {
            return Err(anyhow::anyhow!(
                "Unsupported script type '{}'. Only .sh, .py, and .js are runnable.",
                requested
            ));
        }
    };
    if args.len() > 1 {
        cmd.args(&args[1..]);
    }
    cmd.current_dir(scripts_dir);
    cmd.env("AHNARA_SKILL_SCRIPT", requested);

    println!("\n▶ Running {} {}\n", requested, args[1..].join(" "));
    let output = timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("Script timed out after 60s"))??;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        println!("{}", stdout);
    }
    if !stderr.trim().is_empty() {
        eprintln!("{}", stderr);
    }
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Script exited with status {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}