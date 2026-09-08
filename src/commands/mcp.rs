//! /mcp command - Manage MCP server integrations from chat
//!
//! Usage:
//!   /mcp                          - List all configured MCP servers
//!   /mcp list                     - Same as above
//!   /mcp add <name> <cmd> [args]  - Add a new MCP server
//!   /mcp remove <name>            - Remove an MCP server
//!   /mcp enable <name>            - Enable a server (in config)
//!   /mcp disable <name>           - Disable a server (comment out)
//!   /mcp tools <name>             - List tools exposed by a server
//!   /mcp help                     - Show usage

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::AgentCore;
use crate::config::{AppConfig, McpServerConfig};

/// Path to the config file
fn config_path() -> PathBuf {
    let base = std::env::var("AHNARA_CONFIG")
        .unwrap_or_else(|_| "~/.ahnara/config.toml".into());
    if base.starts_with('~') {
        dirs::home_dir()
            .unwrap_or_else(|| "/root".into())
            .join(&base[2..])
    } else {
        PathBuf::from(&base)
    }
}

fn load_config(path: &PathBuf) -> Result<AppConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {:?}", path))?;
    toml::from_str(&content).context("Failed to parse config")
}

fn save_config(path: &PathBuf, config: &AppConfig) -> Result<()> {
    let content = toml::to_string_pretty(config)
        .context("Failed to serialize config")?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &content)
        .with_context(|| format!("Failed to write config tmp: {:?}", tmp))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename: {:?} -> {:?}", tmp, path))?;
    Ok(())
}

fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Handle the /mcp command. Returns a response string.
/// `agent` is used for live reload after add/remove/enable/disable
/// and for querying real tool names in `tools`.
pub async fn handle_mcp(args: &str, agent: Option<&Arc<AgentCore>>) -> Result<String> {
    let parts: Vec<&str> = args.trim().split_whitespace().collect();

    if parts.is_empty() || parts[0] == "list" {
        return list_servers();
    }

    match parts[0] {
        "help" | "h" => Ok(help_text()),
        "add" => {
            if parts.len() < 3 {
                return Err(anyhow!("Usage: /mcp add <name> <command> [args...]"));
            }
            let name = parts[1].to_string();
            let command = parts[2].to_string();
            let args_list: Vec<String> = parts[3..].iter().map(|s| s.to_string()).collect();
            let resp = add_server(&name, command, args_list)?;
            // Hot-reload if agent is available
            if let Some(agent) = agent {
                let reload = agent.reload_mcp().await;
                return Ok(format!("{}\n\n{}", resp, reload));
            }
            Ok(format!("{}\n(Restart required for changes to take effect)", resp))
        }
        "remove" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /mcp remove <name>"));
            }
            let name = parts[1];
            let resp = remove_server(name)?;
            if let Some(agent) = agent {
                let reload = agent.reload_mcp().await;
                return Ok(format!("{}\n\n{}", resp, reload));
            }
            Ok(format!("{}\n(Restart required)", resp))
        }
        "enable" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /mcp enable <name>"));
            }
            let name = parts[1];
            let resp = toggle_server(name, true)?;
            if let Some(agent) = agent {
                let reload = agent.reload_mcp().await;
                return Ok(format!("{}\n\n{}", resp, reload));
            }
            Ok(format!("{}\n(Restart required)", resp))
        }
        "disable" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /mcp disable <name>"));
            }
            let name = parts[1];
            let resp = toggle_server(name, false)?;
            if let Some(agent) = agent {
                let reload = agent.reload_mcp().await;
                return Ok(format!("{}\n\n{}", resp, reload));
            }
            Ok(format!("{}\n(Restart required)", resp))
        }
        "tools" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /mcp tools <name>"));
            }
            let name = parts[1];
            // Try live tools first if agent available
            if let Some(agent) = agent {
                let live_tools = agent.mcp_tools_for(name);
                if !live_tools.is_empty() {
                    let mut lines = vec![
                        format!("Tools from MCP server '{}' ({}):", name, live_tools.len()),
                        String::new(),
                    ];
                    for t in &live_tools {
                        lines.push(format!("  - {}", t));
                    }
                    lines.push(String::new());
                    lines.push("These tools are available in the agent's tool registry.".into());
                    return Ok(lines.join("\n"));
                }
            }
            // Fallback: show config details
            list_server_tools(name)
        }
        _ => {
            // Treat as server name for detail view
            list_server_detail(parts[0])
        }
    }
}

fn help_text() -> String {
    "\
MCP Server Management
=====================

Commands:
  /mcp                    List all MCP servers
  /mcp add <name> <cmd> [args]  Add a server
  /mcp remove <name>      Remove a server
  /mcp enable <name>      Enable a server
  /mcp disable <name>     Disable a server
  /mcp tools <name>       List tools from a server
  /mcp <name>             Show server details
  /mcp help               This message

Examples:
  /mcp add github npx -y @modelcontextprotocol/server-github
  /mcp add linear npx -y @modelcontextprotocol/server-linear
  /mcp tools github
  /mcp remove github

Changes take effect immediately (no restart needed)."
        .into()
}

fn list_servers() -> Result<String> {
    let path = config_path();
    let config = load_config(&path)?;

    if !config.mcp.enabled {
        return Ok("MCP is disabled. Enable it in config.toml under [mcp] enabled = true".into());
    }

    if config.mcp.servers.is_empty() {
        return Ok("No MCP servers configured.\n\n\
            Add one: /mcp add <name> <command> [args...]\n\
            Example: /mcp add github npx -y @modelcontextprotocol/server-github"
            .into());
    }

    let mut lines = vec![format!("MCP Servers ({})", config.mcp.servers.len())];
    lines.push(String::new());

    for server in &config.mcp.servers {
        let status = if server.command.is_empty() {
            "INVALID (no command)"
        } else {
            "configured"
        };
        let tool_filter = if !server.include_tools.is_empty() {
            format!(" (include: {})", server.include_tools.join(", "))
        } else if !server.exclude_tools.is_empty() {
            format!(" (exclude: {})", server.exclude_tools.join(", "))
        } else {
            String::new()
        };
        let env_hint = if server.env.is_empty() {
            String::new()
        } else {
            format!(" env={}", server.env.len())
        };
        lines.push(format!(
            "  {} - {} [{}]{}{}",
            server.name, server.command, status, tool_filter, env_hint
        ));
    }

    lines.push(String::new());
    lines.push("Commands: /mcp add|remove|enable|disable|tools <name>".into());
    lines.push("Details: /mcp <name>  |  Help: /mcp help".into());

    Ok(lines.join("\n"))
}

fn list_server_detail(name: &str) -> Result<String> {
    let path = config_path();
    let config = load_config(&path)?;

    let server = config
        .mcp
        .servers
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow!("No MCP server named '{}'", name))?;

    let mut lines = vec![format!("MCP Server: {}", server.name)];
    lines.push(format!("  Command: {}", server.command));
    if !server.args.is_empty() {
        lines.push(format!("  Args: {}", server.args.join(" ")));
    }
    if !server.env.is_empty() {
        lines.push(format!("  Env vars: {}", server.env.keys().cloned().collect::<Vec<_>>().join(", ")));
    }
    if let Some(ref prefix) = server.tool_prefix {
        lines.push(format!("  Tool prefix: {}", prefix));
    }
    if !server.include_tools.is_empty() {
        lines.push(format!("  Include tools: {}", server.include_tools.join(", ")));
    }
    if !server.exclude_tools.is_empty() {
        lines.push(format!("  Exclude tools: {}", server.exclude_tools.join(", ")));
    }
    lines.push(format!("  Timeout: {}s", server.timeout_secs));

    Ok(lines.join("\n"))
}

fn list_server_tools(name: &str) -> Result<String> {
    let path = config_path();
    let config = load_config(&path)?;

    let server = config
        .mcp
        .servers
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow!("No MCP server named '{}'", name))?;

    if server.command.is_empty() {
        return Err(anyhow!("Server '{}' has no command configured", name));
    }

    let tool_prefix = server
        .tool_prefix
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(&server.name);
    let sanitized = sanitize(tool_prefix);

    let mut lines = vec![format!("MCP server '{}' tools:", name)];
    if !server.include_tools.is_empty() {
        lines.push(format!("  Included tools ({}):", server.include_tools.len()));
        for t in &server.include_tools {
            lines.push(format!("    - mcp_{}_{}", sanitized, sanitize(t)));
        }
    } else if !server.exclude_tools.is_empty() {
        lines.push(format!("  Excluded tools: {}", server.exclude_tools.join(", ")));
        lines.push("  All other tools from this server are included.".into());
    } else {
        lines.push("  All tools included (use /mcp tools <name> with live agent to see real tools).".into());
    }

    Ok(lines.join("\n"))
}

fn add_server(name: &str, command: String, args: Vec<String>) -> Result<String> {
    let path = config_path();
    let mut config = load_config(&path)?;

    if config.mcp.servers.iter().any(|s| s.name == name) {
        return Err(anyhow!(
            "Server '{}' already exists. Use /mcp remove {} first.",
            name, name
        ));
    }

    let server = McpServerConfig {
        name: name.to_string(),
        command,
        args,
        ..Default::default()
    };
    config.mcp.servers.push(server);
    config.mcp.enabled = true;
    save_config(&path, &config)?;

    Ok(format!(
        "MCP server '{}' added and enabled.\n  Command: {} {}",
        name,
        config.mcp.servers.last().unwrap().command,
        config.mcp.servers.last().unwrap().args.join(" ")
    ))
}

fn remove_server(name: &str) -> Result<String> {
    let path = config_path();
    let mut config = load_config(&path)?;

    let before = config.mcp.servers.len();
    config.mcp.servers.retain(|s| s.name != name);
    if config.mcp.servers.len() == before {
        return Err(anyhow!("No MCP server named '{}'", name));
    }
    save_config(&path, &config)?;

    Ok(format!("MCP server '{}' removed.", name))
}

fn toggle_server(name: &str, enabled: bool) -> Result<String> {
    let path = config_path();
    let mut config = load_config(&path)?;

    let server = config
        .mcp
        .servers
        .iter_mut()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow!("No MCP server named '{}'", name))?;

    // We don't have an enabled field on McpServerConfig, so we use include_tools as a proxy.
    // An empty command means disabled.
    // For now, toggle by setting/clearing args marker.
    // Actually, let's just use the global enabled flag and rename approach.
    // The simplest: if disabling, move command to a commented-out approach isn't possible in TOML.
    // So we store the original command in tool_prefix when disabled.
    if !enabled {
        if server.command.is_empty() {
            return Ok(format!("Server '{}' is already disabled.", name));
        }
        server.tool_prefix = Some(format!("__disabled__:{}", server.command));
        server.command = String::new();
    } else {
        if let Some(ref stored) = server.tool_prefix {
            if stored.starts_with("__disabled__:") {
                server.command = stored.strip_prefix("__disabled__:")
                    .unwrap_or("")
                    .to_string();
                server.tool_prefix = None;
            }
        }
        if server.command.is_empty() {
            return Err(anyhow!("Server '{}' has no command to re-enable.", name));
        }
    }

    save_config(&path, &config)?;
    let action = if enabled { "enabled" } else { "disabled" };
    Ok(format!("MCP server '{}' {}.", name, action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help() {
        let resp = help_text();
        assert!(resp.contains("MCP Server Management"));
        assert!(resp.contains("/mcp add"));
    }

    #[test]
    fn test_sanitize() {
        assert_eq!(sanitize("GitHub Tools"), "github_tools");
        assert_eq!(sanitize("@modelcontextprotocol/server-github"), "modelcontextprotocol_server_github");
    }
}
