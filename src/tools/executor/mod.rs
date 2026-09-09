//! Code Execution Tools - Sandboxed multi-language execution
//!
//! Features:
//! - execute_code: Python/TypeScript/Shell with sandboxing
//! - execute_parallel: Parallel command execution
//! - Resource limits (memory, CPU, timeouts)
//! - Output truncation
//! - Workspace restrictions
//! - Environment abstraction (local, Docker, SSH)
//! - Session snapshots and CWD tracking

pub mod environment;
pub mod local_env;
pub mod docker_env;
pub mod ssh_env;
pub mod runtime;
pub mod languages;
pub mod sandbox;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;

use crate::orchestrator::{Tool, ToolResult};

pub use environment::{
    ExecutionConfig, Environment,
    EnvironmentType, TerminalConfig,
};
pub use local_env::LocalEnvironment;
pub use docker_env::DockerEnvironment;
pub use ssh_env::SshEnvironment;
pub use sandbox::{Sandbox, SandboxConfig, Limits};

/// Result from code execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub truncated: bool,
}

/// Create the appropriate environment from config.
pub async fn create_environment(
    config: &TerminalConfig,
    workspace: &PathBuf,
) -> Result<Box<dyn Environment>> {
    match config.environment {
        EnvironmentType::Local => {
            Ok(Box::new(LocalEnvironment::new(
                workspace.clone(),
                std::collections::HashMap::new(),
            )))
        }
        EnvironmentType::Docker => {
            let mut docker = DockerEnvironment::new(
                config.docker.clone(),
                workspace.clone(),
            ).await?;
            docker.start(workspace).await?;
            Ok(Box::new(docker))
        }
        EnvironmentType::Ssh => {
            Ok(Box::new(SshEnvironment::new(
                config.ssh.clone(),
                workspace.clone(),
            )))
        }
    }
}

/// Multi-language code execution tool
pub struct ExecuteCodeTool {
    config: ExecutionConfig,
}

impl ExecuteCodeTool {
    pub fn new() -> Self {
        Self {
            config: ExecutionConfig::default(),
        }
    }

    pub fn with_config(config: ExecutionConfig) -> Self {
        Self { config }
    }

    fn sandbox(&self) -> SandboxConfig {
        // Single source of truth for execution guardrails lives in
        // `sandbox::SandboxConfig`; merge the tool's workspace limits into it
        // plus the extra fork-bomb spelling and JS modules this tool blocks.
        let mut sb = SandboxConfig {
            max_output_chars: self.config.max_output_chars,
            workspace_root: Some(self.config.workspace_root.clone()),
            ..SandboxConfig::default()
        };
        for extra in [":(){ :|:& };:"] {
            if !sb.blocked_patterns.iter().any(|p| p == extra) {
                sb.blocked_patterns.push(extra.into());
            }
        }
        for extra in ["http", "tls", "crypto"] {
            if !sb.blocked_modules.iter().any(|m| m == extra) {
                sb.blocked_modules.push(extra.into());
            }
        }
        sb
    }

    fn validate(&self, code: &str, lang: &str) -> Result<()> {
        let sb = self.sandbox();
        sb.validate_command(code).map_err(|e| anyhow!(e))?;

        // Language-specific import validation
        match lang {
            "python" => sb.validate_python(code).map_err(|e| anyhow!(e))?,
            "typescript" | "javascript" => sb.validate_js(code).map_err(|e| anyhow!(e))?,
            _ => {}
        }

        Ok(())
    }

    fn truncate_output(&self, output: &str) -> (String, bool) {
        self.sandbox().truncate_output(output)
    }

    async fn execute_internal(
        &self,
        lang: &str,
        code: &str,
    ) -> Result<ExecutionResult> {
        tracing::debug!("execute_code: lang={}, code_len={}", lang, code.len());
        if let Err(e) = self.validate(code, lang) {
            tracing::warn!("Validation failed: {}", e);
            return Err(e);
        }

        // Write to temp file
        let ext = match lang {
            "python" => "py",
            "typescript" => "ts",
            "javascript" => "js",
            _ => "sh",
        };
        let temp_file = std::env::temp_dir().join(format!("aux_code_{}.{}", std::process::id(), ext));
        tokio::fs::write(&temp_file, code).await?;

        // Build runner command
        let runner = match lang {
            "python" => vec!["python3".to_string(), temp_file.to_string_lossy().to_string()],
            "typescript" | "javascript" => vec!["bun".to_string(), temp_file.to_string_lossy().to_string()],
            _ => vec!["sh".to_string(), temp_file.to_string_lossy().to_string()],
        };

        let start = Instant::now();
        let duration = Duration::from_secs(self.config.timeout.as_secs());

        // Execute with timeout
        let output = timeout(duration, async {
            Command::new(&runner[0])
                .args(&runner[1..])
                .output()
                .await
        }).await;

        // Cleanup
        let _ = tokio::fs::remove_file(&temp_file).await;

        match output {
            Ok(Ok(o)) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                let (stdout, truncated_out) = self.truncate_output(&stdout);
                let (stderr, truncated_err) = self.truncate_output(&stderr);

                Ok(ExecutionResult {
                    success: o.status.success(),
                    stdout,
                    stderr,
                    exit_code: o.status.code().unwrap_or(-1),
                    duration_ms: start.elapsed().as_millis() as u64,
                    truncated: truncated_out || truncated_err,
                })
            }
            Ok(Err(e)) => Err(anyhow!("Execution failed: {}", e)),
            Err(_) => Err(anyhow!("Execution timed out after {}s", self.config.timeout.as_secs())),
        }
    }
}

impl Default for ExecuteCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ExecuteCodeTool {
    fn name(&self) -> &str { "execute_code" }

    fn description(&self) -> &str {
        "Execute Python, TypeScript, or Shell code with sandboxing, timeouts, and output limits"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "typescript", "javascript", "shell"],
                    "description": "Script language"
                },
                "code": {
                    "type": "string",
                    "description": "Code to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Max execution time in seconds (default: 120)"
                }
            },
            "required": ["language", "code"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let lang = args["language"].as_str().unwrap_or("shell");
        let code = args["code"].as_str().unwrap_or("");
        let timeout_override = args["timeout"].as_u64();

        let mut config = self.config.clone();
        if let Some(t) = timeout_override {
            config.timeout = Duration::from_secs(t);
        }

        let tool = ExecuteCodeTool::with_config(config);
        let result = tool.execute_internal(lang, code).await?;

        Ok(ToolResult {
            tool_name: self.name().to_string(),
            success: result.success,
            output: serde_json::json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
                "truncated": result.truncated
            }),
            error: if result.success { None } else { Some(result.stderr) },
            duration_ms: result.duration_ms,
        })
    }
}

/// Parallel execution tool - run multiple commands simultaneously
pub struct ExecuteParallelTool {
    timeout_secs: u64,
}

impl ExecuteParallelTool {
    pub fn new() -> Self {
        Self { timeout_secs: 60 }
    }
}

impl Default for ExecuteParallelTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct CmdSpec {
    id: String,
    command: String,
    language: Option<String>,
}

#[async_trait]
impl Tool for ExecuteParallelTool {
    fn name(&self) -> &str { "execute_parallel" }

    fn description(&self) -> &str {
        "Execute multiple code blocks in parallel with independent results"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "command": {"type": "string"},
                            "language": {"type": "string"}
                        },
                        "required": ["id", "command"]
                    }
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout per command (default: 60)"
                }
            },
            "required": ["commands"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let cmds: Vec<CmdSpec> = serde_json::from_value(args["commands"].clone())?;
        let timeout_override = args["timeout"].as_u64().unwrap_or(self.timeout_secs);

        if cmds.is_empty() {
            return Ok(ToolResult {
                tool_name: self.name().to_string(),
                success: true,
                output: serde_json::json!({"results": [], "total": 0}),
                error: None,
                duration_ms: 0,
            });
        }

        let start = Instant::now();
        let futures: Vec<_> = cmds.iter().map(|cmd| async {
            let lang = cmd.language.as_deref().unwrap_or("shell");
            let exec_config = ExecutionConfig {
                timeout: Duration::from_secs(timeout_override),
                ..Default::default()
            };
            let tool = ExecuteCodeTool::with_config(exec_config);
            tool.execute_internal(lang, &cmd.command).await
        }).collect();

        let results = futures::future::join_all(futures).await;

        let mut all_success = true;
        let mut output_results = Vec::new();

        for (i, result) in results.into_iter().enumerate() {
            match result {
                Ok(r) => {
                    if !r.success { all_success = false; }
                    output_results.push(serde_json::json!({
                        "id": cmds[i].id,
                        "success": r.success,
                        "stdout": r.stdout,
                        "stderr": r.stderr,
                        "exit_code": r.exit_code,
                        "duration_ms": r.duration_ms,
                        "truncated": r.truncated
                    }));
                }
                Err(e) => {
                    all_success = false;
                    output_results.push(serde_json::json!({
                        "id": cmds[i].id,
                        "success": false,
                        "error": e.to_string()
                    }));
                }
            }
        }

        Ok(ToolResult {
            tool_name: self.name().to_string(),
            success: all_success,
            output: serde_json::json!({
                "results": output_results,
                "total": output_results.len(),
                "duration_ms": start.elapsed().as_millis() as u64
            }),
            error: if all_success { None } else { Some("Some commands failed".into()) },
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
