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
    let skill_path = skills_dir.join(name).join("SKILL.md");
    if !skill_path.exists() {
        println!("Skill '{}' not found.", name);
        return Ok(());
    }

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
                        println!("✗ Failed to install: {}", e);
                    }
                }
            } else if let Some(skill_name) = name {
                println!("📥 Installing skill: {}", skill_name);
                match installer.install(&skill_name).await {
                    Ok(msg) => {
                        println!("✓ {}", msg);
                    }
                    Err(e) => {
                        println!("✗ Failed: {}", e);
                    }
                }
            } else {
                println!("✗ Please specify a skill name, --url, or --git\n");
            }
            println!();
        }

        crate::cli::SkillCommands::Uninstall { name } => {
            println!();
            if installer.is_installed(&name) {
                installer.uninstall(&name)?;
                println!("✓ Uninstalled skill: {}\n", name);
            } else {
                println!("✗ Skill '{}' is not installed\n", name);
            }
        }

        crate::cli::SkillCommands::Create { name, description } => {
            println!();
            let desc = description.as_deref().unwrap_or("A custom skill");
            let skill_dir = installer.create_from_template(&name, desc)?;
            println!("✓ Created skill '{}' at:\n  {:?}\n", name, skill_dir);
            println!("Edit the SKILL.md file to add your instructions.\n");
        }

        crate::cli::SkillCommands::Update { name } => {
            println!();
            if installer.is_installed(&name) {
                // Re-install from registry
                match installer.install(&name).await {
                    Ok(msg) => println!("✓ {}\n", msg),
                    Err(e) => println!("✗ Failed to update: {}\n", e),
                }
            } else {
                println!("✗ Skill '{}' is not installed\n", name);
            }
        }

        crate::cli::SkillCommands::Browse => {
            println!("\n🌐 Available Skills in Registry\n");

            let skills = installer.list_available().await?;

            // Group by category
            let mut by_category: std::collections::HashMap<String, Vec<_>> =
                std::collections::HashMap::new();
            for skill in skills {
                by_category
                    .entry(skill.category.clone())
                    .or_default()
                    .push(skill);
            }

            for (category, skills) in by_category {
                println!("\n📁 {}", category);
                for skill in skills {
                    let installed = if installer.is_installed(&skill.name) {
                        "✓"
                    } else {
                        " "
                    };
                    println!(
                        "  [{}] {} - {}",
                        installed,
                        skill.name,
                        if skill.description.len() > 50 {
                            format!("{}...", &skill.description[..skill.description.floor_char_boundary(47)])
                        } else {
                            skill.description.clone()
                        }
                    );
                }
            }

            println!("\n💡 Install with: ahnara skill install <name>\n");
        }

        crate::cli::SkillCommands::Info { name } => {
            println!();

            let skill_path = find_skill(&skills_dir, &name)?;
            let content = fs::read_to_string(&skill_path)?;
            let skill = crate::skills::Skill::parse(&content)?;

            println!("📦 {}\n", skill.name());
            println!("{}\n", skill.description());

            if let Some(cat) = &skill.meta.category {
                println!("Category: {}", cat);
            }
            if let Some(license) = &skill.meta.license {
                println!("License: {}", license);
            }
            if let Some(compat) = &skill.meta.compatibility {
                println!("Compatibility: {}", compat);
            }

            println!("\n{}\n", "─".repeat(50));
            println!("{}\n", skill.body);
        }

        crate::cli::SkillCommands::Tap { action } => {
            let registry = SkillRegistry::new();
            match action {
                crate::cli::SkillTapCommands::List => {
                    let config = registry.load_taps()?;
                    println!("\n🔌 Skill Registry Taps\n");
                    for tap in config.taps {
                        let status = if tap.enabled { "enabled" } else { "disabled" };
                        let checksum = tap.sha256.as_deref().unwrap_or("none");
                        println!(
                            "  {} [{}] priority={} sha256={}\n    {}",
                            tap.name, status, tap.priority, checksum, tap.url
                        );
                    }
                    println!("\nConfig: {}\n", registry.tap_path().display());
                }
                crate::cli::SkillTapCommands::Add {
                    name,
                    url,
                    sha256,
                    priority,
                } => {
                    registry.add_tap(&name, &url, sha256, priority)?;
                    println!("✓ Added skill tap '{}'", name);
                }
                crate::cli::SkillTapCommands::Remove { name } => {
                    registry.remove_tap(&name)?;
                    println!("✓ Removed skill tap '{}'", name);
                }
            }
        }
    }

    Ok(())
}

fn find_skill(skills_dir: &PathBuf, name: &str) -> Result<PathBuf> {
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
