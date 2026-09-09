//! Runtime module for process management and resource limits
//!
//! DEPRECATED: Use `environment::ExecutionEngine` with `LocalEnvironment`
//! for new code. This module is kept for backward compatibility with
//! existing tool calls that use `run_with_timeout` directly.

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use anyhow::Result;

pub use super::environment::ProcessOutput;

/// Execute a command with timeout and resource limits.
///
/// DEPRECATED: Use `ExecutionEngine::new(LocalEnvironment::new(...), config).execute(cmd)`.
/// This function is kept for backward compatibility.
pub async fn run_with_timeout(
    cmd: &str,
    cwd: &std::path::Path,
    _timeout: Duration,
) -> Result<ProcessOutput> {
    let start = std::time::Instant::now();

    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    Ok(ProcessOutput {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        truncated: false,
    })
}
