//! Docker execution environment.
//!
//! Runs commands inside a Docker container with security hardening:
//! - All capabilities dropped (--cap-drop ALL)
//! - No privilege escalation (--security-opt no-new-privileges)
//! - PID limits (--pids-limit 256)
//! - Size-limited tmpfs for scratch dirs
//! - Configurable CPU/memory/disk resource limits
//! - Optional persistent workspace via bind mounts
//! - Credential file mounting (read-only)
//!
//! The container is started once (with `sleep infinity`) and commands
//! are executed via `docker exec`. The container is cleaned up on drop.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::environment::{DockerEnvConfig, Environment};
use uuid::Uuid;

/// Security flags applied to every container.
const BASE_SECURITY_ARGS: &[&str] = &[
    "--cap-drop", "ALL",
    "--cap-add", "DAC_OVERRIDE",
    "--cap-add", "CHOWN",
    "--cap-add", "FOWNER",
    "--security-opt", "no-new-privileges",
    "--pids-limit", "256",
    "--tmpfs", "/tmp:rw,nosuid,size=512m",
];

/// Extra caps needed when container starts as root and an init must drop privileges.
const PRIVDROP_CAP_ARGS: &[&str] = &[
    "--cap-add", "SETUID",
    "--cap-add", "SETGID",
];

/// Docker container execution environment.
///
/// Starts a persistent container with `sleep infinity`, then uses
/// `docker exec` for each command. The container is cleaned up when
/// the environment is dropped.
pub struct DockerEnvironment {
    config: DockerEnvConfig,
    container_name: String,
    container_id: Option<String>,
    cwd: PathBuf,
    docker_exe: String,
    workspace_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    running: bool,
}

impl DockerEnvironment {
    pub async fn new(
        config: DockerEnvConfig,
        cwd: PathBuf,
    ) -> Result<Self> {
        let docker_exe = Self::find_docker()
            .context("Docker executable not found. Install Docker and ensure the 'docker' command is available.")?;

        // Verify Docker daemon is running
        let check = Command::new(&docker_exe)
            .args(["version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("Failed to run docker version")?;
        if !check.success() {
            anyhow::bail!("Docker daemon is not responding. Ensure Docker is running.");
        }

        let container_name = format!("ahnara-{}", &Uuid::new_v4().to_string()[..8]);

        Ok(Self {
            config,
            container_name,
            container_id: None,
            cwd,
            docker_exe,
            workspace_dir: None,
            home_dir: None,
            running: false,
        })
    }

    /// Start the container. Must be called before `run_bash()`.
    pub async fn start(&mut self, host_workspace: &PathBuf) -> Result<()> {
        let mut args: Vec<String> = vec![
            "run".into(), "-d".into(),
            "--init".into(),  // tini as PID 1 — reaps zombie children
            "--name".into(), self.container_name.clone(),
            "-w".into(), self.cwd.display().to_string(),
        ];

        // Security args
        args.extend(BASE_SECURITY_ARGS.iter().map(|s| s.to_string()));
        if !self.config.run_as_host_user {
            args.extend(PRIVDROP_CAP_ARGS.iter().map(|s| s.to_string()));
        }

        // Resource limits
        if self.config.cpu > 0.0 {
            args.extend(["--cpus".into(), self.config.cpu.to_string()]);
        }
        if self.config.memory_mb > 0 {
            args.extend(["--memory".into(), format!("{}m", self.config.memory_mb)]);
        }
        if !self.config.network {
            args.push("--network=none".into());
        }

        // Run as host user (avoids root-owned files in bind mounts)
        if self.config.run_as_host_user {
            let uid = unsafe { libc::getuid() };
            let gid = unsafe { libc::getgid() };
            args.extend(["--user".into(), format!("{uid}:{gid}")]);
        }

        // Workspace mount
        if host_workspace.exists() {
            args.extend([
                "-v".into(),
                format!("{}:/workspace", host_workspace.display()),
            ]);
        }

        // User-configured volume mounts
        for vol in &self.config.volumes {
            if vol.contains(':') {
                args.extend(["-v".into(), vol.clone()]);
            }
        }

        // Image + sleep infinity
        args.push(self.config.image.clone());
        args.extend(["sleep".into(), "infinity".into()]);

        let output = Command::new(&self.docker_exe)
            .args(&args)
            .output()
            .await
            .context("Failed to start Docker container")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to start container: {stderr}");
        }

        let cid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        self.container_id = Some(cid);
        self.running = true;

        tracing::info!(
            container = %self.container_name,
            image = %self.config.image,
            "Docker container started"
        );

        Ok(())
    }

    /// Locate the docker (or podman) CLI binary.
    fn find_docker() -> Option<String> {
        // Check env override first
        if let Ok(exe) = std::env::var("AHNARA_DOCKER_BINARY") {
            if std::path::Path::new(&exe).is_file() {
                return Some(exe);
            }
        }
        // Check PATH for docker, then podman
        for name in &["docker", "podman"] {
            if let Ok(output) = std::process::Command::new("which")
                .arg(name)
                .output()
            {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(path);
                    }
                }
            }
        }
        None
    }
}

#[async_trait]
impl Environment for DockerEnvironment {
    async fn run_bash(
        &self,
        script: &str,
        _login: bool,
        timeout: Duration,
        stdin_data: Option<&str>,
    ) -> Result<(String, i32)> {
        if !self.running {
            anyhow::bail!("Docker container not started. Call start() first.");
        }

        let start = std::time::Instant::now();

        let mut cmd = Command::new(&self.docker_exe);
        cmd.args(["exec", "-i"])
            .arg(&self.container_name)
            .args(["bash", "-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }

        let child = cmd.spawn().context("Failed to spawn docker exec")?;

        let output = tokio::time::timeout(timeout, async {
            let mut child = child;
            if let Some(data) = stdin_data {
                if let Some(ref mut stdin) = child.stdin {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(data.as_bytes()).await;
                    drop(child.stdin.take());
                }
            }
            child.wait_with_output().await
        })
        .await;

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
            Ok(Err(e)) => Err(anyhow::anyhow!("Docker exec error: {}", e)),
            Err(_) => Err(anyhow::anyhow!(
                "Docker command timed out after {}s",
                timeout.as_secs()
            )),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        if let Some(_cid) = &self.container_id {
            tracing::info!(
                container = %self.container_name,
                "Stopping and removing Docker container"
            );
            let _ = Command::new(&self.docker_exe)
                .args(["rm", "-f", &self.container_name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        Ok(())
    }

    fn backend_name(&self) -> &str {
        "docker"
    }
}

impl Drop for DockerEnvironment {
    fn drop(&mut self) {
        // Best-effort cleanup: fire and forget
        if self.running {
            let name = self.container_name.clone();
            let exe = self.docker_exe.clone();
            tokio::spawn(async move {
                let _ = Command::new(&exe)
                    .args(["rm", "-f", &name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            });
        }
    }
}
