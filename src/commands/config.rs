//! Config command handler

use anyhow::{bail, Result};
use std::fs;

pub fn handle_config(action: crate::cli::ConfigCommands) -> Result<()> {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
    
    match action {
        crate::cli::ConfigCommands::Show { format } => {
            if !config_path.exists() {
                bail!("Config file not found. Run `ahnara setup` first.");
            }
            
            let content = fs::read_to_string(&config_path)?;
            
            match format.as_str() {
                "toml" => println!("{}", content),
                "json" => {
                    let value: toml::Value = toml::from_str(&content)?;
                    println!("{}", serde_json::to_string_pretty(&value)?);
                }
                _ => bail!("Unknown format: {}", format),
            }
        }
        
        crate::cli::ConfigCommands::Get { key } => {
            if !config_path.exists() {
                bail!("Config file not found. Run `ahnara setup` first.");
            }
            
            let content = fs::read_to_string(&config_path)?;
            let config: toml::Value = toml::from_str(&content)?;
            
            let parts: Vec<&str> = key.split('.').collect();
            let mut current = &config;
            
            for part in &parts[..parts.len()-1] {
                current = current.get(part)
                    .ok_or_else(|| anyhow::anyhow!("Key not found: {}", key))?;
            }
            
            let last = parts.last().unwrap();
            if let Some(value) = current.get(last) {
                match value {
                    toml::Value::String(s) => println!("{}", s),
                    toml::Value::Integer(i) => println!("{}", i),
                    toml::Value::Float(f) => println!("{}", f),
                    toml::Value::Boolean(b) => println!("{}", b),
                    _ => println!("{}", value),
                }
            } else {
                bail!("Key not found: {}", key);
            }
        }
        
        crate::cli::ConfigCommands::Set { key, value } => {
            if !config_path.exists() {
                bail!("Config file not found. Run `ahnara setup` first.");
            }
            
            let content = fs::read_to_string(&config_path)?;
            
            // Parse and modify
            let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            
            // Handle nested keys like sub_agents.enabled
            let parts: Vec<&str> = key.split('.').collect();
            
            // Determine value format
            let value_str = if value == "true" || value == "false" {
                value.clone()
            } else if value.parse::<u32>().is_ok() {
                value.clone()
            } else if value.parse::<f32>().is_ok() {
                value.clone()
            } else {
                format!("\"{}\"", value)
            };
            
            // Simple key update for known patterns
            let mut found = false;
            let mut in_section = None;
            
            for i in 0..lines.len() {
                let line = &lines[i];
                
                // Track current section
                if line.starts_with('[') && line.ends_with(']') {
                    let section = &line[1..line.len()-1];
                    in_section = Some(section.to_string());
                    continue;
                }
                
                // Check for key match
                if parts.len() == 1 {
                    // Top-level key (unlikely)
                    if line.starts_with(&format!("{} = ", parts[0])) {
                        lines[i] = format!("{} = {}", parts[0], value_str);
                        found = true;
                        break;
                    }
                } else if parts.len() == 2 {
                    // Section.key format
                    if let Some(ref section) = in_section {
                        if section == parts[0] {
                            if line.starts_with(&format!("{} = ", parts[1])) || 
                               line.trim().starts_with(&format!("{} = ", parts[1])) {
                                // Preserve indentation
                                let indent = if line.starts_with("  ") { "  " } else { "" };
                                lines[i] = format!("{}{} = {}", indent, parts[1], value_str);
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }
            
            if !found {
                // Try to add the key if section exists
                if parts.len() == 2 {
                    let mut added = false;
                    for i in 0..lines.len() {
                        if lines[i] == format!("[{}]", parts[0]) {
                            // Add after section header
                            lines.insert(i + 1, format!("{} = {}", parts[1], value_str));
                            added = true;
                            break;
                        }
                    }
                    if !added {
                        // Add section and key at end
                        lines.push(format!("[{}]", parts[0]));
                        lines.push(format!("{} = {}", parts[1], value_str));
                    }
                    found = true;
                }
            }
            
            if found {
                let new_content = lines.join("\n") + "\n";
                fs::write(&config_path, new_content)?;
                println!("✅ Set {} = {}", key, value);
            } else {
                bail!("Could not find or create key: {}", key);
            }
        }
        
        crate::cli::ConfigCommands::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            let status = std::process::Command::new(&editor)
                .arg(&config_path)
                .status()?;
            
            if status.success() {
                println!("Config updated.");
            } else {
                bail!("Editor exited with error");
            }
        }
        
        crate::cli::ConfigCommands::Reset { yes } => {
            if !yes {
                println!("This will reset all configuration to defaults.");
                let confirm = dialoguer::Confirm::new()
                    .with_prompt("Continue?")
                    .default(false)
                    .interact()?;
                
                if !confirm {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            
            if config_path.exists() {
                fs::remove_file(&config_path)?;
            }
            println!("Configuration reset. Run `ahnara setup` to configure.");
        }
        
        crate::cli::ConfigCommands::Validate => {
            if !config_path.exists() {
                bail!("Config file not found. Run `ahnara setup` first.");
            }
            
            let content = fs::read_to_string(&config_path)?;
            match toml::from_str::<crate::config::AppConfig>(&content) {
                Ok(_) => println!("✅ Configuration is valid."),
                Err(e) => bail!("Configuration error: {}", e),
            }
        }
    }
    
    Ok(())
}