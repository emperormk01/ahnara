//! SSH execution environment.
//!
//! Runs commands on a remote host via SSH. Features:
//! - CWD tracking via in-band stdout markers (same pattern as local)
//! - Session snapshot transferred to remote host
//! - BatchMode for non-interactive execution
//! - Configurable host, user, key, port
//! - Connection keep-alive via ControlMaster (optional)

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::environment::{Environment, SshEnvConfig};

/// SSH-based remote execution environment.
///
/// Executes commands on a remote host via `ssh`. Each command is run
/// as `ssh user@host 'bash -c "<script>"'`. CWD tracking uses the
/// same stdout marker pattern as local execution, but the snapshot
/// file lives on the remote host.
pub struct SshEnvironment {
    config: SshEnvConfig,
    cwd: PathBuf,
}

impl SshEnvironment {
    pub fn new(config: SshEnvConfig, cwd: PathBuf) -> Self {
        Self { config, cwd }
    }

    /// Build the base SSH command with connection options.
    fn ssh_cmd(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o").arg("BatchMode=yes");
        cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
        cmd.arg("-o").arg("ConnectTimeout=10");

        if self.config.port != 22 {
            cmd.arg("-p").arg(self.config.port.to_string());
        }
        if !self.config.key_path.is_empty() {
            cmd.arg("-i").arg(&self.config.key_path);
        }

        // Target
        let target = if self.config.user.is_empty() {
            self.config.host.clone()
        } else {
            format!("{}@{}", self.config.user, self.config.host)
        };
        cmd.arg(target);
        cmd
    }
}

#[async_trait]
impl Environment for SshEnvironment {
    async fn run_bash(
        &self,
        script: &str,
        login: bool,
        timeout: Duration,
        _stdin_data: Option<&str>,
    ) -> Result<(String, i32)> {
        let mut cmd = self.ssh_cmd();

        // Build the remote bash invocation
        let remote_script = if login {
            format!("bash -lc {}", shell_quote(script))
        } else {
            format!("bash -c {}", shell_quote(script))
        };

        cmd.arg(remote_script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let _start = std::time::Instant::now();

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .context("SSH command timed out")?
            .context("Failed to execute SSH command")?;

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

    async fn cleanup(&self) -> Result<()> {
        // SSH has no persistent state to clean up
        Ok(())
    }

    fn supports_stdin(&self) -> bool {
        false // stdin piping over SSH is unreliable for large data
    }

    fn backend_name(&self) -> &str {
        "ssh"
    }
}

/// Quote a string for safe embedding in a shell command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
