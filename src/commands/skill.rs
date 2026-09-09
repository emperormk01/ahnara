//! Skill command handler

use anyhow::{bail, Result};
use std::fs;
use std::path::PathBuf;

use crate::skills::{registry::SkillRegistry, SkillInstaller};

pub async fn handle_skill(action: crate::cli::SkillCommands) -> Result<()> {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ahnara");

    let skills_dir = config_dir.join("skills");
    fs::create_dir_all(&skills_dir)?;

    let mut installer = SkillInstaller::new(skills_dir.clone());

    match action {
        crate::cli::SkillCommands::List { detailed } => handle_list_skills(&skills_dir, detailed),
        crate::cli::SkillCommands::Search { query } => handle_search_skills(&installer, &query).await,
        crate::cli::SkillCommands::Install { name, url, git } => handle_install_skill(&mut installer, &name, url, git).await,
        crate::cli::SkillCommands::Uninstall { name } => handle_uninstall_skill(&mut installer, &name),
        crate::cli::SkillCommands::Info { name } => handle_skill_info(&skills_dir, &name),
        crate::cli::SkillCommands::Update { name } => handle_update_skill(&mut installer, &name).await,
        crate::cli::SkillCommands::Browse => handle_browse_skills(&installer).await,
    }
}

fn handle_list_skills(skills_dir: &std::path::Path, detailed: bool) -> Result<()> {
    println!("\n📚 Installed Skills\n");

    let mut count = 0;
    for entry in walkdir::WalkDir::new(skills_dir)
        .min_depth(2)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "SKILL.md" {
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(skill) = crate::skills::Skill::parse(&content) {
                    if detailed {
                        println!("{}", "─".repeat(50));
                        println!("📦 {}", skill.name());
                        println!("   {}", skill.description());
                        if let Some(cat) = &skill.meta.category {
                            println!("   Category: {}", cat);
                        }
                        println!();
                    } else {
                        println!(
                            "  {} - {}",
                            skill.name(),
                            if skill.description().len() > 60 {
                                format!("{}...", &skill.description()[..skill.description().floor_char_boundary(57)])
                            } else {
                                skill.description().to_string()
                            }
                        );
                    }
                    count += 1;
                }
            }
        }
    }

    println!("\n{} skills found.\n", count);
    Ok(())
}

async fn handle_search_skills(installer: &SkillInstaller, query: &str) -> Result<()> {
    println!("\n🔍 Searching registry for: '{}'\n", query);

    let results: Vec<crate::skills::registry::RegistrySkill> = installer.search(query).await?;

    if results.is_empty() {
        println!("No skills found matching '{}'\n", query);
        println!("💡 Try different keywords or browse all: ahnara skill browse");
    } else {
        for skill in &results {
            let installed = if installer.is_installed(&skill.name) { "✓" } else { " " };
            println!("  [{}] {} - {}", installed, skill.name, skill.description);
        }
        println!(
            "\n{} results. Install with: ahnara skill install <name>\n",
            results.len()
        );
    }
    Ok(())
}

async fn handle_install_skill(
    installer: &mut SkillInstaller,
    name: &str,
    url: Option<String>,
    git: Option<String>,
) -> Result<()> {
    println!();

    if let Some(url) = url {
        println!("📥 Installing from URL: {}", url);
        match installer.install_from_url(&url).await {
            Ok(skill_name) => println!("✓ Installed skill: {}", skill_name),
            Err(e) => println!("✗ Failed to install: {}", e),
        }
    } else if let Some(git_url) = git {
        println!("📥 Installing from git: {}", git_url);
        match installer.install_from_git(&git_url) {
            Ok(skills) => println!("✓ Installed: {}", skills),
            Err(e) => println!("✗ Failed: {}", e),
        }
    } else {
        println!("📥 Installing: {}", name);
        match installer.install(name).await {
            Ok(skill_name) => println!("✓ Installed skill: {}", skill_name),
            Err(e) => println!("✗ Failed to install: {}", e),
        }
    }

    println!();
    Ok(())
}

fn handle_uninstall_skill(installer: &mut SkillInstaller, name: &str) -> Result<()> {
    println!("🗑️  Uninstalling: {}", name);
    match installer.uninstall(name) {
        Ok(()) => println!("✓ Uninstalled skill: {}", name),
        Err(e) => println!("✗ Failed to uninstall: {}", e),
    }
    Ok(())
}

fn handle_skill_info(skills_dir: &std::path::Path, name: &str) -> Result<()> {
    let skill_path = match find_skill(skills_dir, name) {
        Ok(p) => p,
        Err(_) => {
            println!("Skill '{}' not found.", name);
            return Ok(());
        }
    };

    let content = fs::read_to_string(&skill_path)?;
    let skill = crate::skills::Skill::parse(&content)?;

    println!("\n📦 {}", skill.name());
    println!("   {}", skill.description());
    if let Some(cat) = &skill.meta.category {
        println!("   Category: {}", cat);
    }
    if let Some(version) = &skill.meta.version {
        println!("   Version: {}", version);
    }
    println!();
    Ok(())
}

async fn handle_update_skill(installer: &mut SkillInstaller, name: &str) -> Result<()> {
    println!("🔄 Updating: {}", name);
    match installer.update(name).await {
        Ok(()) => println!("✓ Updated skill: {}", name),
        Err(e) => println!("✗ Failed to update: {}", e),
    }
    Ok(())
}

async fn handle_browse_skills(installer: &SkillInstaller) -> Result<()> {
    println!("\n🔍 Browsing all available skills...\n");
    let results = installer.browse().await?;
    for skill in &results {
        let installed = if installer.is_installed(&skill.name) { "✓" } else { " " };
        println!("  [{}] {} - {}", installed, skill.name, skill.description);
    }
    println!("\n{} skills available. Install with: ahnara skill install <name>\n", results.len());
    Ok(())
}

fn find_skill(skills_dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(skills_dir)
        .min_depth(2)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "SKILL.md" {
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(skill) = crate::skills::Skill::parse(&content) {
                    if skill.name() == name {
                        return Ok(entry.path().to_path_buf());
                    }
                }
            }
        }
    }

    bail!("Skill '{}' not found", name)
}
