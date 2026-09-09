//! Sandbox configuration and enforcement.
//!
//! Provides resource limits, blocked patterns, and workspace restrictions
//! that apply across all execution environments. This is the safety layer
//! that prevents destructive operations regardless of backend.

use anyhow::Result;
use std::path::PathBuf;

/// Enhanced sandbox configuration with environment-aware validation.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub max_output_chars: usize,
    pub max_memory_mb: Option<u64>,
    pub workspace_root: Option<PathBuf>,
    pub blocked_patterns: Vec<String>,
    pub blocked_imports: Vec<String>,
    pub blocked_modules: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_output_chars: 100_000,
            max_memory_mb: Some(512),
            workspace_root: None,
            blocked_patterns: vec![
                "rm -rf /".into(),
                "(){:|:&};:".into(),
                "mkfs".into(),
                "dd if=".into(),
                "/etc/passwd".into(),
                "/etc/shadow".into(),
            ],
            blocked_imports: vec![
                "os".into(),
                "sys".into(),
                "subprocess".into(),
                "socket".into(),
                "requests".into(),
                "urllib".into(),
                "http.client".into(),
            ],
            blocked_modules: vec![
                "child_process".into(),
                "fs".into(),
                "net".into(),
            ],
        }
    }
}

impl SandboxConfig {
    pub fn validate_command(&self, code: &str) -> Result<(), String> {
        for pattern in &self.blocked_patterns {
            if code.contains(pattern) {
                return Err(format!("Blocked pattern detected: {}", pattern));
            }
        }
        Ok(())
    }

    pub fn validate_python(&self, code: &str) -> Result<(), String> {
        for imp in &self.blocked_imports {
            if code.contains(&format!("import {}", imp))
                || code.contains(&format!("from {} import", imp))
            {
                return Err(format!("Blocked Python import: {}", imp));
            }
        }
        Ok(())
    }

    pub fn validate_js(&self, code: &str) -> Result<(), String> {
        for module in &self.blocked_modules {
            if code.contains(&format!("require('{}')", module))
                || code.contains(&format!("from '{}'", module))
                || code.contains(&format!("import from '{}'", module))
            {
                return Err(format!("Blocked JS module: {}", module));
            }
        }
        Ok(())
    }

    pub fn truncate_output(&self, output: &str) -> (String, bool) {
        if output.len() > self.max_output_chars {
            let truncated = format!(
                "{}... [truncated {} chars]",
                &output[..self.max_output_chars],
                output.len() - self.max_output_chars
            );
            (truncated, true)
        } else {
            (output.to_string(), false)
        }
    }

    pub fn is_within_workspace(&self, path: &PathBuf) -> bool {
        if let Some(ref root) = self.workspace_root {
            path.starts_with(root)
        } else {
            true
        }
    }
}
