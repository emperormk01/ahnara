//! Execution environment abstraction for ahnara.
//!
//! Provides a unified interface for running commands in different
//! environments: local host, Docker containers, or SSH remote hosts.
//! Inspired by Hermes Agent's BaseEnvironment pattern with session
//! snapshots and CWD tracking.
//!
//! Key design decisions:
//! - Each command spawns a fresh `bash -c` process (no persistent shell)
//! - Session state (env vars, functions, aliases) persists via snapshot file
//! - CWD persists via in-band stdout markers parsed after each command
//! - ProcessHandle trait unifies local subprocesses and Docker exec

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Process output
// ---------------------------------------------------------------------------

/// Unified output from any execution environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Environment trait
// ---------------------------------------------------------------------------

/// Core abstraction for execution environments.
///
/// Implementors provide `run()` which spawns a bash process in the target
/// environment (local, Docker, SSH). The base trait provides `execute()`
/// which wraps commands with snapshot sourcing, CWD tracking, and timeout.
#[async_trait]
pub trait Environment: Send + Sync {
    /// Low-level bash spawn. Subclasses implement this.
    /// Returns raw stdout+stderr combined, and exit code.
    async fn run_bash(
        &self,
        script: &str,
        login: bool,
        timeout: Duration,
        stdin_data: Option<&str>,
    ) -> Result<(String, i32)>;

    /// Cleanup backend resources (container, connection, etc).
    async fn cleanup(&self) -> Result<()>;

    /// Whether this environment supports interactive stdin.
    fn supports_stdin(&self) -> bool {
        true
    }

    /// Human-readable backend name.
    fn backend_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Execution engine (wraps any Environment with session state)
// ---------------------------------------------------------------------------

/// Configuration for the execution engine.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    pub timeout: Duration,
    pub max_output_chars: usize,
    pub workspace_root: PathBuf,
    pub env: HashMap<String, String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_output_chars: 100_000,
            workspace_root: PathBuf::from("/workspace"),
            env: HashMap::new(),
        }
    }
}

/// Wraps an Environment with session management and CWD tracking.
pub struct ExecutionEngine<E: Environment> {
    env: E,
    config: ExecutionConfig,
    session_id: String,
    cwd: PathBuf,
    snapshot_path: PathBuf,
    snapshot_ready: bool,
}

impl<E: Environment> ExecutionEngine<E> {
    pub fn new(env: E, config: ExecutionConfig) -> Self {
        let session_id = Uuid::new_v4().to_string()[..12].to_string();
        let snap_nonce = Uuid::new_v4().to_string()[..8].to_string();
        let snapshot_path = std::env::temp_dir()
            .join(format!("aux-snap-{session_id}-{snap_nonce}.sh"));
        let cwd = config.workspace_root.clone();

        Self {
            env,
            config,
            session_id,
            cwd,
            snapshot_path,
            snapshot_ready: false,
        }
    }

    /// Capture login shell environment into a snapshot file.
    /// Called once after construction.
    pub async fn init_session(&mut self) -> Result<()> {
        let snap = self.snapshot_path.display();
        let cwd = self.cwd.display();
        let marker = self.cwd_marker();

        let bootstrap = format!(
            "export -p > {snap} 2>/dev/null || true\n\
             declare -f | grep -vE '^_[^_]' >> {snap} 2>/dev/null || true\n\
             alias -p >> {snap} 2>/dev/null || true\n\
             echo 'shopt -s expand_aliases' >> {snap}\n\
             echo 'set +e' >> {snap}\n\
             echo 'set +u' >> {snap}\n\
             builtin cd {cwd} 2>/dev/null || true\n\
             printf '\\n{marker}%s{marker}\\n' \"$(pwd -P)\""
        );

        match self.env.run_bash(&bootstrap, true, Duration::from_secs(30), None).await {
            Ok((output, _)) => {
                self.snapshot_ready = true;
                self.update_cwd(&output);
                tracing::debug!(
                    session = %self.session_id,
                    cwd = %self.cwd.display(),
                    "Session snapshot created"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session = %self.session_id,
                    error = %e,
                    "Session snapshot failed, falling back to login shell per command"
                );
                self.snapshot_ready = false;
            }
        }
        Ok(())
    }

    /// Execute a command with session state, CWD tracking, and timeout.
    pub async fn execute(&mut self, command: &str) -> Result<ProcessOutput> {
        let start = std::time::Instant::now();
        let script = self.wrap_command(command);
        let login = !self.snapshot_ready;

        let (raw_output, exit_code) = self.env
            .run_bash(&script, login, self.config.timeout, None)
            .await
            .context("Failed to execute command")?;

        // Strip CWD marker from visible output
        let (visible, cwd_from_output) = self.extract_cwd(&raw_output);
        let truncated = visible.len() > self.config.max_output_chars;
        let stdout = if truncated {
            format!(
                "{}... [truncated {} chars]",
                &visible[..self.config.max_output_chars],
                visible.len() - self.config.max_output_chars
            )
        } else {
            visible.to_string()
        };

        if let Some(new_cwd) = cwd_from_output {
            self.cwd = PathBuf::from(&new_cwd);
            tracing::debug!(cwd = %new_cwd, "CWD updated");
        }

        Ok(ProcessOutput {
            success: exit_code == 0,
            exit_code,
            stdout,
            stderr: String::new(), // combined with stdout in most backends
            duration_ms: start.elapsed().as_millis() as u64,
            truncated,
        })
    }

    /// Get current working directory.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// Set working directory for next command.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// Parse CWD marker from output and update internal state.
    fn update_cwd(&mut self, output: &str) {
        let (_, new_cwd) = self.extract_cwd(output);
        if let Some(cwd) = new_cwd {
            self.cwd = PathBuf::from(cwd);
        }
    }

    /// Cleanup the environment.
    pub async fn cleanup(&self) -> Result<()> {
        // Remove snapshot file
        if self.snapshot_path.exists() {
            let _ = tokio::fs::remove_file(&self.snapshot_path).await;
        }
        self.env.cleanup().await
    }

    // -- internals --

    fn cwd_marker(&self) -> String {
        format!("__AHNARA_CWD_{}__", self.session_id)
    }

    fn wrap_command(&self, command: &str) -> String {
        let escaped = command.replace('\'', "'\\''");
        let snap = self.snapshot_path.display();
        let marker = self.cwd_marker();

        let mut parts = Vec::new();

        // Source snapshot if available
        if self.snapshot_ready {
            parts.push(format!(
                "source {snap} >/dev/null 2>&1 || true"
            ));
        }

        // cd to working directory
        parts.push(format!(
            "builtin cd -- '{}' || exit 126",
            self.cwd.display()
        ));

        // Run the actual command
        parts.push(format!("eval '{escaped}'"));
        parts.push("__aux_ec=$?".to_string());

        // Re-dump env vars to snapshot
        if self.snapshot_ready {
            parts.push(format!(
                "export -p > {snap} 2>/dev/null || true"
            ));
        }

        // Emit CWD marker
        parts.push(format!(
            "printf '\\n{marker}%s{marker}\\n' \"$(pwd -P)\""
        ));
        parts.push("exit $__aux_ec".to_string());

        parts.join("\n")
    }

    fn extract_cwd<'a>(&self, output: &'a str) -> (&'a str, Option<String>) {
        let marker = self.cwd_marker();
        if let Some(start) = output.find(&marker) {
            let after_start = &output[start + marker.len()..];
            if let Some(end) = after_start.find(&marker) {
                let new_cwd = after_start[..end].trim().to_string();
                // Return everything before the marker as visible output
                let visible = output[..start].trim_end();
                return (visible, Some(new_cwd));
            }
        }
        (output.trim_end(), None)
    }
}

// ---------------------------------------------------------------------------
// Environment config enum (for config.toml deserialization)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentType {
    Local,
    Docker,
    Ssh,
}

impl Default for EnvironmentType {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DockerEnvConfig {
    pub image: String,
    #[serde(default)]
    pub cpu: f64,
    #[serde(default)]
    pub memory_mb: u64,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub run_as_host_user: bool,
    #[serde(default = "default_true")]
    pub network: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshEnvConfig {
    pub host: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    #[serde(default)]
    pub environment: EnvironmentType,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_max_output")]
    pub max_output_chars: usize,
    #[serde(default)]
    pub docker: DockerEnvConfig,
    #[serde(default)]
    pub ssh: SshEnvConfig,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            environment: EnvironmentType::Local,
            timeout: default_timeout(),
            max_output_chars: default_max_output(),
            docker: DockerEnvConfig::default(),
            ssh: SshEnvConfig::default(),
        }
    }
}

fn default_timeout() -> u64 {
    120
}

fn default_max_output() -> usize {
    100_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::local_env::LocalEnvironment;

    #[test]
    fn extract_cwd_parses_marker() {
        let config = ExecutionConfig {
            workspace_root: PathBuf::from("/workspace"),
            ..Default::default()
        };
        let env = LocalEnvironment::new(PathBuf::from("/workspace"), HashMap::new());
        let mut engine = ExecutionEngine::new(env, config);
        // Manually set snapshot_ready so marker is generated
        engine.snapshot_ready = true;

        let marker = engine.cwd_marker();
        let output = format!("some output\n{marker}/home/user/project{marker}\n");
        let (visible, cwd) = engine.extract_cwd(&output);
        assert_eq!(visible, "some output");
        assert_eq!(cwd, Some("/home/user/project".to_string()));
    }

    #[test]
    fn extract_cwd_returns_none_when_no_marker() {
        let config = ExecutionConfig::default();
        let env = LocalEnvironment::new(PathBuf::from("/workspace"), HashMap::new());
        let engine = ExecutionEngine::new(env, config);

        let (visible, cwd) = engine.extract_cwd("just some output\n");
        assert_eq!(visible, "just some output");
        assert_eq!(cwd, None);
    }

    #[test]
    fn update_cwd_changes_directory() {
        let config = ExecutionConfig {
            workspace_root: PathBuf::from("/workspace"),
            ..Default::default()
        };
        let env = LocalEnvironment::new(PathBuf::from("/workspace"), HashMap::new());
        let mut engine = ExecutionEngine::new(env, config);
        engine.snapshot_ready = true;

        let marker = engine.cwd_marker();
        let output = format!("{marker}/new/path{marker}\n");
        engine.update_cwd(&output);
        assert_eq!(engine.cwd(), &PathBuf::from("/new/path"));
    }

    #[tokio::test]
    async fn local_env_executes_command() {
        let env = LocalEnvironment::new(PathBuf::from("/tmp"), HashMap::new());
        let (output, code) = env
            .run_bash("echo hello", false, Duration::from_secs(5), None)
            .await
            .unwrap();
        assert_eq!(code, 0);
        assert!(output.contains("hello"));
    }

    #[tokio::test]
    async fn local_env_captures_exit_code() {
        let env = LocalEnvironment::new(PathBuf::from("/tmp"), HashMap::new());
        let (_, code) = env
            .run_bash("exit 42", false, Duration::from_secs(5), None)
            .await
            .unwrap();
        assert_eq!(code, 42);
    }

    #[tokio::test]
    async fn local_env_blocks_timeout() {
        let env = LocalEnvironment::new(PathBuf::from("/tmp"), HashMap::new());
        let result = env
            .run_bash("sleep 10", false, Duration::from_millis(100), None)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn execution_engine_full_loop() {
        let config = ExecutionConfig {
            workspace_root: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(10),
            ..Default::default()
        };
        let local = LocalEnvironment::new(PathBuf::from("/tmp"), HashMap::new());
        let mut engine = ExecutionEngine::new(local, config);

        engine.init_session().await.unwrap();

        let result = engine.execute("echo test123").await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test123"));

        // CWD tracking: execute cd and verify next command sees it
        let result = engine.execute("cd /var && pwd").await.unwrap();
        assert!(result.success);
        assert_eq!(engine.cwd(), &PathBuf::from("/var"));

        engine.cleanup().await.unwrap();
    }

    #[test]
    fn terminal_config_defaults() {
        let config = TerminalConfig::default();
        assert_eq!(config.environment, EnvironmentType::Local);
        assert_eq!(config.timeout, 120);
        assert_eq!(config.max_output_chars, 100_000);
    }

    #[test]
    fn terminal_config_deserialize() {
        let toml = r#"
            environment = "docker"
            timeout = 60
            [docker]
            image = "python:3.12"
            memory_mb = 256
        "#;
        let config: TerminalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.environment, EnvironmentType::Docker);
        assert_eq!(config.timeout, 60);
        assert_eq!(config.docker.image, "python:3.12");
        assert_eq!(config.docker.memory_mb, 256);
    }
}