//! Run command handler - execute a skill

use anyhow::Result;
use std::fs;

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
    
    // If skill has scripts, run them
    let scripts_dir = skill_path.parent().unwrap().join("scripts");
    if scripts_dir.exists() && !args.is_empty() {
        println!("Running with args: {:?}", args);
        // TODO: Execute scripts
    }
    
    Ok(())
}