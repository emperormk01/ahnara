//! Provider command handler

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

fn load_config() -> Result<crate::config::AppConfig> {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
    crate::config::AppConfig::load(&config_path.to_string_lossy())
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.ahnara/config.toml"))
}

pub async fn handle_provider(action: crate::cli::ProviderCommands) -> Result<()> {
    match action {
        crate::cli::ProviderCommands::List => handle_list_providers(),
        crate::cli::ProviderCommands::Active => handle_active_provider(),
        crate::cli::ProviderCommands::Use { name } => handle_use_provider(&name),
        crate::cli::ProviderCommands::Test { name } => handle_test_provider(name).await,
        crate::cli::ProviderCommands::Add { name, base, key } => handle_add_provider(&name, &base, key),
        crate::cli::ProviderCommands::Remove { name } => handle_remove_provider(&name),
    }
}

fn handle_list_providers() -> Result<()> {
    match load_config() {
        Ok(config) => {
            if config.providers.providers.is_empty() {
                println!("No providers configured.");
                println!("Run `ahnara setup` to add one.");
            } else {
                println!("\nConfigured Providers\n");
                for p in &config.providers.providers {
                    let active = if p.name == config.providers.active { " (active)" } else { "" };
                    println!("  {}{} - {}", p.name, active, p.api_base);
                }
                println!("\nActive: {}", if config.providers.active.is_empty() { "(none)" } else { &config.providers.active });
            }
        }
        Err(e) => {
            println!("Could not load config: {}", e);
            println!("Run `ahnara setup` to create one.");
        }
    }
    println!();
    Ok(())
}

fn handle_active_provider() -> Result<()> {
    match load_config() {
        Ok(config) => {
            if config.providers.active.is_empty() || config.providers.providers.is_empty() {
                println!("No active provider configured.");
                println!("Run `ahnara setup` to set one up.");
            } else {
                println!("Active provider: {}", config.providers.active);
                if let Some(p) = config.providers.providers.iter().find(|p| p.name == config.providers.active) {
                    println!("  Base URL: {}", p.api_base);
                    println!("  Model: {}", config.agent.default_model);
                }
            }
        }
        Err(e) => println!("Could not load config: {}", e),
    }
    Ok(())
}

fn handle_use_provider(name: &str) -> Result<()> {
    let path = config_path();
    if !path.exists() {
        println!("No config found. Run `ahnara setup` first.");
        return Ok(());
    }
    let raw = fs::read_to_string(&path)?;
    let mut doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("Failed to parse config: {}", e))?;

    let has_provider = doc.get("providers")
        .and_then(|p| p.get("providers"))
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().any(|e| e.get("name").and_then(|n| n.as_str()) == Some(name)))
        .unwrap_or(false);

    if !has_provider {
        println!("Provider '{}' not found in config.", name);
        println!("Available:");
        if let Some(arr) = doc.get("providers").and_then(|p| p.get("providers")).and_then(|p| p.as_array()) {
            for p in arr {
                if let Some(n) = p.get("name").and_then(|n| n.as_str()) {
                    println!("  {}", n);
                }
            }
        }
        return Ok(());
    }

    if let Some(providers) = doc.get_mut("providers").and_then(|p| p.as_table_mut()) {
        providers.insert("active".to_string(), toml::Value::String(name.into()));
    }
    fs::write(&path, toml::to_string_pretty(&doc)?)?;
    println!("Switched to provider: {}", name);
    Ok(())
}

async fn handle_test_provider(name: Option<String>) -> Result<()> {
    let config = load_config()?;
    let providers_to_test: Vec<_> = match &name {
        Some(n) => config.providers.providers.iter().filter(|p| p.name == *n).collect(),
        None => config.providers.providers.iter().collect(),
    };

    if providers_to_test.is_empty() {
        println!("No providers to test. Run `ahnara setup` first.");
        return Ok(());
    }

    println!("Testing providers...\n");
    for p in &providers_to_test {
        let url = format!("{}/models", p.api_base.trim_end_matches('/'));
        println!("Testing {} ({})...", p.name, p.api_base);

        match reqwest::Client::new()
            .get(&url)
            .header("Authorization", format!("Bearer {}", p.api_key))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || status.as_u16() == 401 || status.as_u16() == 403 {
                    println!("  {}: reachable (HTTP {})", p.name, status);
                } else {
                    println!("  {}: HTTP {} - unexpected", p.name, status);
                }
            }
            Err(e) => {
                println!("  {}: FAILED - {}", p.name, e);
            }
        }
    }
    println!();
    Ok(())
}

fn handle_add_provider(name: &str, base: &str, key: Option<String>) -> Result<()> {
    let path = config_path();
    if !path.exists() {
        println!("No config found. Run `ahnara setup` first.");
        return Ok(());
    }
    let raw = fs::read_to_string(&path)?;
    let mut doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("Failed to parse config: {}", e))?;

    let api_key = match key {
        Some(k) => k,
        None => dialoguer::Input::new()
            .with_prompt("API Key")
            .interact_text()?,
    };

    let entry = toml::Value::Table({
        let mut t = toml::map::Map::new();
        t.insert("name".to_string(), toml::Value::String(name.into()));
        t.insert("api_key".to_string(), toml::Value::String(api_key));
        t.insert("api_base".to_string(), toml::Value::String(base.into()));
        t
    });

    let providers_table = doc.as_table_mut()
        .unwrap()
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[providers] is not a table"))?;

    let arr = providers_table
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Array(vec![]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("providers.providers is not an array"))?;

    if let Some(idx) = arr.iter().position(|e| e.get("name").and_then(|n| n.as_str()) == Some(name)) {
        arr[idx] = entry;
    } else {
        arr.push(entry);
    }

    if arr.len() == 1 {
        providers_table.insert("active".to_string(), toml::Value::String(name.into()));
    }

    fs::write(&path, toml::to_string_pretty(&doc)?)?;
    println!("Added provider: {} ({})", name, base);
    Ok(())
}

fn handle_remove_provider(name: &str) -> Result<()> {
    let path = config_path();
    if !path.exists() {
        println!("No config found.");
        return Ok(());
    }
    let raw = fs::read_to_string(&path)?;
    let mut doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("Failed to parse config: {}", e))?;

    let providers_table = doc.get_mut("providers").and_then(|p| p.as_table_mut());

    if let Some(table) = providers_table {
        if let Some(arr) = table.get_mut("providers").and_then(|p| p.as_array_mut()) {
            let before = arr.len();
            arr.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some(name));
            if arr.len() == before {
                println!("Provider '{}' not found.", name);
                return Ok(());
            }
        }
        if table.get("active").and_then(|a| a.as_str()) == Some(name) {
            table.remove("active");
        }
    }

    fs::write(&path, toml::to_string_pretty(&doc)?)?;
    println!("Removed provider: {}", name);
    Ok(())
}
