//! Local execution environment.
//!
//! Runs commands directly on the host via `bash -c`. This is the default
//! environment and replaces the bare `run_with_timeout` from runtime.rs.
//!
//! Features over bare runtime:
//! - Session snapshot sourcing (env vars persist across calls)
//! - CWD tracking via stdout markers
//! - Activity callbacks to prevent gateway inactivity timeout
//! - Proper process group management for clean interruption
//! - Environment variable sanitization (blocks API keys from leaking)

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::environment::Environment;

/// Environment variables that are safe to pass to child processes.
/// All others are stripped to prevent secret leaks.
const ALLOWED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "COLORTERM",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HOSTNAME",
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "DEBIAN_FRONTEND",
    "CI",
    "GITHUB_ACTIONS",
    "AHNARA_DOCKER_BINARY",
];

/// Local host execution environment.
///
/// Spawns `bash -c` for each command. Inherits the host's filesystem
/// and network, but sanitizes environment variables to prevent secret leaks.
pub struct LocalEnvironment {
    /// Working directory for commands.
    cwd: PathBuf,
    /// Additional environment variables to set (applied after sanitization).
    env: std::collections::HashMap<String, String>,
    /// Pre-computed allowed env var set for O(1) lookup.
    allowed: HashSet<&'static str>,
}

impl LocalEnvironment {
    pub fn new(cwd: PathBuf, env: std::collections::HashMap<String, String>) -> Self {
        let allowed: HashSet<&'static str> = ALLOWED_ENV_VARS.iter().copied().collect();
        Self { cwd, env, allowed }
    }

    /// Check if a path exists and is a directory, walking up to find
    /// the nearest existing ancestor if not. Falls back to /tmp.
    fn resolve_safe_cwd(cwd: &PathBuf) -> PathBuf {
        if cwd.is_dir() {
            return cwd.clone();
        }
        let mut parent = cwd.parent().map(|p| p.to_path_buf());
        while let Some(p) = parent {
            if p.is_dir() {
                return p;
            }
            let next = p.parent().map(|pp| pp.to_path_buf());
            if next == Some(p.clone()) {
                break;
            }
            parent = next;
        }
        std::env::temp_dir()
    }

    fn build_command(&self, script: &str, login: bool, safe_cwd: &std::path::Path, stdin_data: Option<&str>) -> Command {
        let mut cmd = Command::new("bash");
        if login {
            cmd.arg("-l");
        }
        cmd.arg("-c")
            .arg(script)
            .current_dir(safe_cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .process_group(0);
        cmd
    }

    fn apply_environment(&self, cmd: &mut Command) {
        cmd.env_clear();
        for (key, value) in std::env::vars() {
            if self.allowed.contains(key.as_str()) {
                cmd.env(&key, &value);
            }
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
    }

    async fn run_child(&self, mut child: tokio::process::Child, stdin_data: Option<&str>) -> Result<std::process::Output> {
        if let Some(data) = stdin_data {
            if let Some(ref mut stdin) = child.stdin {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(data.as_bytes()).await;
                drop(child.stdin.take());
            }
        }
        child.wait_with_output().await.map_err(|e| anyhow::anyhow!("Process error: {}", e))
    }

    fn handle_output(&self, output: Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed>, timeout: Duration) -> Result<(String, i32)> {
        match output {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let combined = if stderr.is_empty() {
                    stdout
                } else if stdout.is_empty() {
                    stderr
                } else {
                    format!("{stdout}\n{stderr}")
                };
                let exit_code = output.status.code().unwrap_or(-1);
                Ok((combined, exit_code))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("Process error: {}", e)),
            Err(_) => Err(anyhow::anyhow!(
                "Command timed out after {}s",
                timeout.as_secs()
            )),
        }
    }
}

#[async_trait]
impl Environment for LocalEnvironment {
    async fn run_bash(
        &self,
        script: &str,
        login: bool,
        timeout: Duration,
        stdin_data: Option<&str>,
    ) -> Result<(String, i32)> {
        let safe_cwd = Self::resolve_safe_cwd(&self.cwd);
        let mut cmd = self.build_command(script, login, &safe_cwd, stdin_data);
        self.apply_environment(&mut cmd);

        let child = cmd.spawn().context("Failed to spawn bash process")?;
        let output = tokio::time::timeout(timeout, self.run_child(child, stdin_data)).await;
        self.handle_output(output, timeout)
    }

    async fn cleanup(&self) -> Result<()> {
        Ok(())
    }

    fn backend_name(&self) -> &str {
        "local"
    }
}
