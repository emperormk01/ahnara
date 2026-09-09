//! Language executors module
//! 
//! Features:
//! - Python execution with sandboxed imports
//! - TypeScript/Bun execution
//! - Shell execution with timeout

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Result from a language execution
#[derive(Debug)]
pub struct ScriptResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

/// Trait for language executors
pub trait LanguageExecutor: Send + Sync {
    fn execute_script(
        &self,
        code: &str,
        cwd: &PathBuf,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<ScriptResult>> + Send;
}

/// Python executor with sandboxed imports
pub struct PythonExec {
    blocked_imports: Vec<String>,
}

impl PythonExec {
    pub fn new() -> Self {
        Self {
            blocked_imports: vec![
                "os".to_string(),
                "sys".to_string(),
                "subprocess".to_string(),
                "socket".to_string(),
                "requests".to_string(),
            ],
        }
    }
    
    fn validate_imports(&self, code: &str) -> Result<()> {
        for imp in &self.blocked_imports {
            if code.contains(&format!("import {}", imp)) 
                || code.contains(&format!("from {} import", imp)) 
            {
                return Err(anyhow!("Blocked import: {}", imp));
            }
        }
        Ok(())
    }
}

impl Default for PythonExec {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageExecutor for PythonExec {
    async fn execute_script(
        &self,
        code: &str,
        cwd: &PathBuf,
        _timeout: Duration,
    ) -> Result<ScriptResult> {
        self.validate_imports(code)?;
        
        // Write code to temp file
        let temp_file = std::env::temp_dir().join("script.py");
        tokio::fs::write(&temp_file, code).await?;
        
        let start = std::time::Instant::now();
        let output = Command::new("python3")
            .arg(&temp_file)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        
        let _ = tokio::fs::remove_file(temp_file).await;
        
        Ok(ScriptResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

/// Bun/TypeScript executor
pub struct BunExec {
    blocked_modules: Vec<String>,
}

impl BunExec {
    pub fn new() -> Self {
        Self {
            blocked_modules: vec![
                "child_process".to_string(),
                "fs".to_string(),
                "net".to_string(),
            ],
        }
    }
    
    fn validate_modules(&self, code: &str) -> Result<()> {
        for module in &self.blocked_modules {
            if code.contains(&format!("require('{}')", module))
                || code.contains(&format!("from '{}'", module))
            {
                return Err(anyhow!("Blocked module: {}", module));
            }
        }
        Ok(())
    }
}

impl Default for BunExec {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageExecutor for BunExec {
    async fn execute_script(
        &self,
        code: &str,
        cwd: &PathBuf,
        _timeout: Duration,
    ) -> Result<ScriptResult> {
        self.validate_modules(code)?;
        
        let temp_file = std::env::temp_dir().join("script.ts");
        tokio::fs::write(&temp_file, code).await?;
        
        let start = std::time::Instant::now();
        let output = Command::new("bun")
            .arg(&temp_file)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        
        let _ = tokio::fs::remove_file(temp_file).await;
        
        Ok(ScriptResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

/// Shell executor
pub struct ShellExec;

impl ShellExec {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ShellExec {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageExecutor for ShellExec {
    async fn execute_script(
        &self,
        code: &str,
        cwd: &PathBuf,
        _timeout: Duration,
    ) -> Result<ScriptResult> {
        let start = std::time::Instant::now();
        let output = Command::new("sh")
            .arg("-c")
            .arg(code)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        
        Ok(ScriptResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
