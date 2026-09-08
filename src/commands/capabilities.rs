//! Capability command handler.

use anyhow::Result;

use crate::capabilities::CapabilityManifest;
use crate::config::AppConfig;
use crate::orchestrator::ToolOrchestrator;

pub async fn handle_capabilities(json: bool) -> Result<()> {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara/config.toml"));
    let config = AppConfig::load(config_path.to_str().unwrap_or("~/.ahnara/config.toml"))
        .unwrap_or_default();
    let orchestrator = ToolOrchestrator::new();
    let manifest = CapabilityManifest::new(&config, Some(&orchestrator));

    if json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
    } else {
        print!("{}", manifest.human_summary());
    }

    Ok(())
}
