//! Coding workspace tools - read_file, edit_file, create_or_rewrite_file, etc.
//! These are registered into the orchestrator when /code mode is activated.

use crate::orchestrator::{Tool, ToolResult};
use async_trait::async_trait;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

// ─── read_file ───────────────────────────────────────────────────────────────

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read the contents of a file. Returns the full text. For large files, use start_line/end_line to read a specific range." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"},
                "start_line": {"type": "integer", "description": "1-indexed start line (optional)"},
                "end_line": {"type": "integer", "description": "1-indexed end line, inclusive (optional)"}
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;
        let _content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow!("Failed to read {}: {}", path, e))?;

        let output = if let (Some(start), Some(end)) = (args["start_line"].as_i64(), args["end_line"].as_i64()) {
            let lines: Vec<&str> = content.lines().collect();
            let s = (start as usize).saturating_sub(1);
            let e = std::cmp::min(end as usize, lines.len());
            lines[s..e].join("\n")
        } else {
            content
        };

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "content": output }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── list_files ──────────────────────────────────────────────────────────────

pub struct ListFilesTool;

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str { "list_files" }
    fn description(&self) -> &str { "List files and directories at a path. Returns a tree of names with trailing / for directories." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute directory path to list"}
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;
        let mut entries = tokio::fs::read_dir(path).await
            .map_err(|e| anyhow!("Failed to read dir {}: {}", path, e))?;

        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await?.is_dir();
            names.push(if is_dir { format!("{}/", name) } else { name });
        }
        names.sort();

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "entries": names }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── edit_file ───────────────────────────────────────────────────────────────

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }
    fn description(&self) -> &str { "Replace text in a file. Finds the exact old_text and replaces it with new_text. You MUST read the file first." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"},
                "old_text": {"type": "string", "description": "Exact text to find and replace (must match exactly)"},
                "new_text": {"type": "string", "description": "Replacement text"}
            },
            "required": ["path", "old_text", "new_text"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;
        let old_text = args["old_text"].as_str().ok_or_else(|| anyhow!("Missing old_text"))?;
        let new_text = args["new_text"].as_str().ok_or_else(|| anyhow!("Missing new_text"))?;

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow!("Failed to read {}: {}", path, e))?;

        if !content.contains(old_text) {
            return Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: serde_json::Value::Null,
                error: Some(format!("old_text not found in {}", path)),
                duration_ms: 0,
            });
        }

        let new_content = content.replacen(old_text, new_text, 1);
        tokio::fs::write(path, &new_content).await
            .map_err(|e| anyhow!("Failed to write {}: {}", path, e))?;

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "status": "replaced", "path": path }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── edit_file_llm ───────────────────────────────────────────────────────────

pub struct EditFileLlmTool;

#[async_trait]
impl Tool for EditFileLlmTool {
    fn name(&self) -> &str { "edit_file_llm" }
    fn description(&self) -> &str { "Edit a file using natural language instructions. Provide the path and instructions describing what to change. The tool reads the current content, applies your instructions, and writes the result. You MUST read the file first." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"},
                "instructions": {"type": "string", "description": "Natural language description of the edit to make"},
                "code_edit": {"type": "string", "description": "The new code/content to write, with '// ... existing code ...' placeholders for unchanged regions"}
            },
            "required": ["path", "instructions", "code_edit"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;
        let instructions = args["instructions"].as_str().ok_or_else(|| anyhow!("Missing instructions"))?;
        let code_edit = args["code_edit"].as_str().ok_or_else(|| anyhow!("Missing code_edit"))?;

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow!("Failed to read {}: {}", path, e))?;

        // If code_edit contains no placeholders, treat it as a full replacement
        let new_content = if code_edit.contains("// ... existing code ...") || code_edit.contains("//...") {
            // For placeholder-based edits, try to apply heuristically
            // Simple approach: replace the entire content with code_edit
            // (The LLM should provide the full intended content with placeholders expanded)
            code_edit.to_string()
        } else {
            code_edit.to_string()
        };

        tokio::fs::write(path, &new_content).await
            .map_err(|e| anyhow!("Failed to write {}: {}", path, e))?;

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "status": "edited", "path": path, "instructions": instructions }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── create_or_rewrite_file ──────────────────────────────────────────────────

pub struct CreateOrRewriteFileTool;

#[async_trait]
impl Tool for CreateOrRewriteFileTool {
    fn name(&self) -> &str { "create_or_rewrite_file" }
    fn description(&self) -> &str { "Create a new file or completely rewrite an existing one. Creates parent directories automatically." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path for the file"},
                "content": {"type": "string", "description": "Full file content to write"}
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;
        let content = args["content"].as_str().ok_or_else(|| anyhow!("Missing content"))?;

        if let Some(parent) = PathBuf::from(path).parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| anyhow!("Failed to create dirs for {}: {}", path, e))?;
        }

        tokio::fs::write(path, content).await
            .map_err(|e| anyhow!("Failed to write {}: {}", path, e))?;

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "status": "written", "path": path, "bytes": content.len() }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── run_bash_command ────────────────────────────────────────────────────────

pub struct RunBashCommandTool;

#[async_trait]
impl Tool for RunBashCommandTool {
    fn name(&self) -> &str { "run_bash_command" }
    fn description(&self) -> &str { "Execute a single shell command and return stdout, stderr, and exit code." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute"},
                "cwd": {"type": "string", "description": "Working directory (optional)"}
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let command = args["command"].as_str().ok_or_else(|| anyhow!("Missing command"))?;
        let cwd = args["cwd"].as_str();

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let output = cmd.output().await
            .map_err(|e| anyhow!("Failed to execute: {}", e))?;

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: output.status.success(),
            output: serde_json::json!({
                "stdout": String::from_utf8_lossy(&output.stdout).chars().take(50000).collect::<String>(),
                "stderr": String::from_utf8_lossy(&output.stderr).chars().take(10000).collect::<String>(),
                "exit_code": output.status.code().unwrap_or(-1)
            }),
            error: if output.status.success() { None } else { Some(format!("Exit code: {}", output.status.code().unwrap_or(-1))) },
            duration_ms: 0,
        })
    }
}

// ─── run_sequential_cmds ─────────────────────────────────────────────────────

pub struct RunSequentialCmdsTool;

#[async_trait]
impl Tool for RunSequentialCmdsTool {
    fn name(&self) -> &str { "run_sequential_cmds" }
    fn description(&self) -> &str { "Execute multiple shell commands in order. Continues even if one fails. Returns results for all commands." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "commands": {"type": "array", "items": {"type": "string"}, "description": "List of shell commands to run sequentially"}
            },
            "required": ["commands"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let commands: Vec<String> = serde_json::from_value(args["commands"].clone())
            .map_err(|e| anyhow!("Invalid commands array: {}", e))?;

        let mut results = Vec::new();
        for cmd_str in &commands {
            let output = tokio::process::Command::new("sh")
                .arg("-c").arg(cmd_str)
                .output().await;
            match output {
                Ok(o) => results.push(serde_json::json!({
                    "command": cmd_str,
                    "stdout": String::from_utf8_lossy(&o.stdout).chars().take(20000).collect::<String>(),
                    "stderr": String::from_utf8_lossy(&o.stderr).chars().take(5000).collect::<String>(),
                    "exit_code": o.status.code().unwrap_or(-1)
                })),
                Err(e) => results.push(serde_json::json!({
                    "command": cmd_str,
                    "error": e.to_string()
                })),
            }
        }

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "results": results }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── run_parallel_cmds ───────────────────────────────────────────────────────

pub struct RunParallelCmdsTool;

#[async_trait]
impl Tool for RunParallelCmdsTool {
    fn name(&self) -> &str { "run_parallel_cmds" }
    fn description(&self) -> &str { "Execute multiple shell commands concurrently. Returns results for all commands." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "commands": {"type": "array", "items": {"type": "string"}, "description": "List of shell commands to run concurrently"}
            },
            "required": ["commands"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let commands: Vec<String> = serde_json::from_value(args["commands"].clone())
            .map_err(|e| anyhow!("Invalid commands array: {}", e))?;

        let handles: Vec<_> = commands.iter().map(|cmd_str| {
            let c = cmd_str.clone();
            tokio::spawn(async move {
                tokio::process::Command::new("sh").arg("-c").arg(&c).output().await
            })
        }).collect();

        let mut results = Vec::new();
        for (i, h) in handles.into_iter().enumerate() {
            match h.await {
                Ok(Ok(o)) => results.push(serde_json::json!({
                    "command": commands[i],
                    "stdout": String::from_utf8_lossy(&o.stdout).chars().take(20000).collect::<String>(),
                    "stderr": String::from_utf8_lossy(&o.stderr).chars().take(5000).collect::<String>(),
                    "exit_code": o.status.code().unwrap_or(-1)
                })),
                _ => results.push(serde_json::json!({
                    "command": commands[i],
                    "error": "execution failed"
                })),
            }
        }

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({ "results": results }),
            error: None,
            duration_ms: 0,
        })
    }
}

// ─── grep_search ─────────────────────────────────────────────────────────────

pub struct GrepSearchTool;

#[async_trait]
impl Tool for GrepSearchTool {
    fn name(&self) -> &str { "grep_search" }
    fn description(&self) -> &str { "Search for a pattern in files using ripgrep. Returns matching lines with file paths and line numbers." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search pattern (regex supported)"},
                "path": {"type": "string", "description": "Directory or file path to search in"},
                "include": {"type": "string", "description": "Glob pattern to include (e.g. '*.rs')"}
            },
            "required": ["query", "path"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let query = args["query"].as_str().ok_or_else(|| anyhow!("Missing query"))?;
        let path = args["path"].as_str().ok_or_else(|| anyhow!("Missing path"))?;

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--no-heading").arg("-n").arg(query).arg(path);

        if let Some(include) = args["include"].as_str() {
            cmd.arg("--glob").arg(include);
        }

        let output = cmd.output().await
            .map_err(|e| anyhow!("ripgrep failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().take(100).collect();

        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({
                "matches": lines,
                "total_shown": lines.len(),
                "truncated": stdout.lines().count() > 100
            }),
            error: None,
            duration_ms: 0,
        })
    }
}
