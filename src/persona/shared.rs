//! Shared persona management across all channels.

use anyhow::{anyhow, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::persona::{PersonaConfig, ResponseLength, Tone};

fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not find home directory"))?;
    Ok(home.join(".ahnara/config.toml"))
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn save_config(config: &crate::config::AppConfig) -> Result<()> {
    let path = config_path()?;
    ensure_parent(&path)?;
    let toml = toml::to_string_pretty(config)?;
    fs::write(path, toml)?;
    Ok(())
}

fn load_config() -> Result<crate::config::AppConfig> {
    let path = config_path()?;
    crate::config::AppConfig::load(
        path.to_str()
            .ok_or_else(|| anyhow!("Invalid config path"))?,
    )
}

pub fn load_current_persona() -> Result<PersonaConfig> {
    let config = load_config()?;
    let path = config_path()?;
    let config_dir = path.parent().ok_or_else(|| anyhow!("Missing config dir"))?;
    config.persona.load(config_dir)
}

pub fn set_name(name: &str) -> Result<()> {
    let mut config = load_config()?;
    config.persona.name = name.to_string();
    save_config(&config)
}

pub fn set_behavior(text: &str) -> Result<()> {
    let mut config = load_config()?;
    config.persona.behavior = text.to_string();
    save_config(&config)
}

pub fn set_tone(tone: &str) -> Result<()> {
    let mut config = load_config()?;
    config.persona.style.tone = match tone.to_ascii_lowercase().as_str() {
        "professional" => Tone::Professional,
        "casual" => Tone::Casual,
        "technical" => Tone::Technical,
        "friendly" => Tone::Friendly,
        _ => {
            return Err(anyhow!(
                "Invalid tone. Use professional, casual, technical, or friendly"
            ))
        }
    };
    save_config(&config)
}

pub fn set_length(length: &str) -> Result<()> {
    let mut config = load_config()?;
    config.persona.style.length = match length.to_ascii_lowercase().as_str() {
        "concise" => ResponseLength::Concise,
        "balanced" => ResponseLength::Balanced,
        "detailed" => ResponseLength::Detailed,
        _ => {
            return Err(anyhow!(
                "Invalid length. Use concise, balanced, or detailed"
            ))
        }
    };
    save_config(&config)
}

pub fn set_no_em_dashes(enabled: bool) -> Result<()> {
    let mut config = load_config()?;
    config.persona.style.formatting.no_em_dashes = enabled;
    save_config(&config)
}

pub fn set_no_emojis(enabled: bool) -> Result<()> {
    let mut config = load_config()?;
    config.persona.style.formatting.no_emojis = enabled;
    save_config(&config)
}

pub fn toggle_no_emojis() -> Result<bool> {
    let mut config = load_config()?;
    let next = !config.persona.style.formatting.no_emojis;
    config.persona.style.formatting.no_emojis = next;
    save_config(&config)?;
    Ok(next)
}

pub fn toggle_no_em_dashes() -> Result<bool> {
    let mut config = load_config()?;
    let next = !config.persona.style.formatting.no_em_dashes;
    config.persona.style.formatting.no_em_dashes = next;
    save_config(&config)?;
    Ok(next)
}
pub fn set_persona_file(file: &str) -> Result<()> {
    let mut config = load_config()?;
    config.persona.persona_file = Some(file.to_string());
    save_config(&config)
}

pub fn save_persona_to_file(output: Option<&str>) -> Result<PathBuf> {
    let persona = load_current_persona()?;
    let path = match output {
        Some(path) => PathBuf::from(path),
        None => {
            let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not find home directory"))?;
            home.join(".ahnara/PERSONA.md")
        }
    };
    ensure_parent(&path)?;
    let content = format!(
        "---\nname: {}\nlength: {}\ntone: {}\nno_em_dashes: {}\nno_emojis: {}\n---\n\n{}\n",
        persona.name,
        persona.style.length,
        persona.style.tone,
        persona.style.formatting.no_em_dashes,
        persona.style.formatting.no_emojis,
        persona.behavior
    );
    fs::write(&path, content)?;
    Ok(path)
}

pub fn reset_persona() -> Result<()> {
    let mut config = load_config()?;
    config.persona = PersonaConfig::default();
    save_config(&config)
}
