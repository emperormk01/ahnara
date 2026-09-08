//! Runtime capability registry and self-awareness manifest.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::config::AppConfig;
use crate::orchestrator::ToolOrchestrator;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CapabilitySource {
    BuiltIn,
    Tool,
    Mcp,
    Skill,
    SkillTap,
    Plugin,
    Scheduler,
    Runtime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub configured: bool,
    pub source: CapabilitySource,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityManifest {
    pub version: String,
    pub capabilities: Vec<Capability>,
}

impl CapabilityManifest {
    pub fn new(config: &AppConfig, orchestrator: Option<&ToolOrchestrator>) -> Self {
        let mut capabilities = builtin_capabilities(config);

        if let Some(orchestrator) = orchestrator {
            for tool in orchestrator.list_tools() {
                capabilities.push(Capability {
                    name: format!("tool:{}", tool.name),
                    description: tool.description,
                    enabled: true,
                    configured: true,
                    source: CapabilitySource::Tool,
                    commands: vec![],
                });
            }
        }

        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        capabilities.dedup_by(|a, b| a.name == b.name);

        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities,
        }
    }

    pub fn prompt_summary(&self) -> String {
        let mut lines = vec![
            "Runtime capability awareness:".to_string(),
            "You are AHNARA and can use the following enabled capabilities when relevant:"
                .to_string(),
        ];

        for capability in self.capabilities.iter().filter(|c| c.enabled) {
            let configured = if capability.configured {
                "configured"
            } else {
                "available, needs config"
            };
            let commands = if capability.commands.is_empty() {
                String::new()
            } else {
                format!(" Commands: {}.", capability.commands.join(", "))
            };
            lines.push(format!(
                "- {} [{:?}, {}]: {}{}",
                capability.name, capability.source, configured, capability.description, commands
            ));
        }

        lines.join("\n")
    }

    pub fn human_summary(&self) -> String {
        let mut grouped: BTreeMap<String, Vec<&Capability>> = BTreeMap::new();
        for capability in &self.capabilities {
            grouped
                .entry(format!("{:?}", capability.source))
                .or_default()
                .push(capability);
        }

        let mut output = String::new();
        output.push_str(&format!("AHNARA capabilities v{}\n", self.version));
        for (source, items) in grouped {
            output.push_str(&format!("\n{}\n", source));
            for item in items {
                let state = match (item.enabled, item.configured) {
                    (true, true) => "enabled/configured",
                    (true, false) => "enabled/needs-config",
                    (false, true) => "disabled/configured",
                    (false, false) => "disabled/unconfigured",
                };
                output.push_str(&format!(
                    "  - {} ({}) - {}\n",
                    item.name, state, item.description
                ));
                if !item.commands.is_empty() {
                    output.push_str(&format!("    commands: {}\n", item.commands.join(", ")));
                }
            }
        }
        output
    }

    pub fn as_json(&self) -> Value {
        json!(self)
    }
}

pub fn builtin_capabilities(config: &AppConfig) -> Vec<Capability> {
    vec![
        Capability {
            name: "tool-orchestration".into(),
            description:
                "Execute registered tools with approval policy enforcement and plugin hooks.".into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::BuiltIn,
            commands: vec![],
        },
        Capability {
            name: "mcp-client".into(),
            description:
                "Connect stdio MCP servers and expose remote tools through the orchestrator.".into(),
            enabled: config.mcp.enabled,
            configured: config.mcp.enabled && !config.mcp.servers.is_empty(),
            source: CapabilitySource::Mcp,
            commands: vec![],
        },
        Capability {
            name: "skills".into(),
            description: "Discover, install, create, and run markdown Skills.".into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::Skill,
            commands: vec!["skill list".into(), "skill search".into(), "run".into()],
        },
        Capability {
            name: "skills-hub-taps".into(),
            description: "Merge skills from multiple registry taps with checksum-aware manifests."
                .into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::SkillTap,
            commands: vec![
                "skill tap list".into(),
                "skill tap add".into(),
                "skill tap remove".into(),
            ],
        },
        Capability {
            name: "planner-dag".into(),
            description: "Create structured task plans and execute dependency-ordered DAG steps."
                .into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::BuiltIn,
            commands: vec!["plan".into(), "run-plan".into()],
        },
        Capability {
            name: "persistent-run-database".into(),
            description:
                "Persist run lifecycle, step records, and events to SQLite for audit and replay."
                    .into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::Runtime,
            commands: vec![
                "runs list".into(),
                "runs show".into(),
                "runs export".into(),
                "runs replay".into(),
            ],
        },
        Capability {
            name: "checkpoints-rollback".into(),
            description: "Snapshot and restore conversation sessions from checkpoint files.".into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::Runtime,
            commands: vec![],
        },
        Capability {
            name: "scheduler".into(),
            description: "Run recurring autonomous agent jobs from cron expressions.".into(),
            enabled: config.scheduler.enabled,
            configured: config.scheduler.enabled && !config.scheduler.jobs.is_empty(),
            source: CapabilitySource::Scheduler,
            commands: vec![],
        },
        Capability {
            name: "plugins".into(),
            description: "Run lifecycle and message/tool hooks through external plugin commands."
                .into(),
            enabled: config.plugins.enabled,
            configured: config.plugins.enabled && !config.plugins.plugins.is_empty(),
            source: CapabilitySource::Plugin,
            commands: vec![],
        },
        Capability {
            name: "memory-reflection-compaction".into(),
            description: "Persist sessions, compact long histories, and generate reflections."
                .into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::BuiltIn,
            commands: vec![],
        },
        Capability {
            name: "gateway-api".into(),
            description: "Serve chat, status, skills, reflection, and capability HTTP endpoints."
                .into(),
            enabled: true,
            configured: true,
            source: CapabilitySource::BuiltIn,
            commands: vec!["gateway".into()],
        },
        Capability {
            name: "discord-channel".into(),
            description: "Run the Discord gateway channel when configured.".into(),
            enabled: config.channels.discord.enabled,
            configured: config.channels.discord.enabled
                && !config.channels.discord.token.is_empty(),
            source: CapabilitySource::BuiltIn,
            commands: vec!["gateway".into()],
        },
        Capability {
            name: "telegram-channel".into(),
            description: "Run the Telegram gateway channel when configured.".into(),
            enabled: config.channels.telegram.enabled,
            configured: config.channels.telegram.enabled
                && !config.channels.telegram.token.is_empty(),
            source: CapabilitySource::BuiltIn,
            commands: vec!["gateway".into()],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_mentions_dag_and_runs() {
        let config = AppConfig::default();
        let manifest = CapabilityManifest::new(&config, None);
        let prompt = manifest.prompt_summary();
        assert!(prompt.contains("planner-dag"));
        assert!(prompt.contains("persistent-run-database"));
    }
}
