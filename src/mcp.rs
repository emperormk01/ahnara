//! Minimal MCP stdio client.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{timeout, Duration};

use crate::config::{McpConfig, McpServerConfig};
use crate::orchestrator::{Tool, ToolResult};

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct McpResponse {
    id: Option<Value>,
    result: Option<Value>,
    error: Option<McpError>,
}

#[derive(Debug, Deserialize)]
struct McpError {
    code: i64,
    message: String,
}

struct McpClientInner {
    name: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout_secs: u64,
    child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    reader: Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: AtomicU64,
}

#[derive(Clone)]
pub struct McpClient {
    inner: Arc<McpClientInner>,
}

impl McpClient {
    pub async fn start(config: &McpServerConfig) -> Result<Self> {
        if config.name.trim().is_empty() {
            return Err(anyhow!("MCP server name cannot be empty"));
        }
        if config.command.trim().is_empty() {
            return Err(anyhow!("MCP server command cannot be empty"));
        }

        let mut command = Command::new(&config.command);
        command.args(&config.args);
        command.envs(&config.env);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::inherit());

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server {}", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server {} missing stdin", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server {} missing stdout", config.name))?;

        let client = Self {
            inner: Arc::new(McpClientInner {
                name: config.name.clone(),
                command: config.command.clone(),
                args: config.args.clone(),
                env: config.env.clone(),
                timeout_secs: config.timeout_secs.max(1),
                child: Mutex::new(child),
                stdin: Mutex::new(stdin),
                reader: Mutex::new(BufReader::new(stdout)),
                request_id: AtomicU64::new(1),
            }),
        };

        let _ = client.initialize().await?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    async fn initialize(&self) -> Result<Value> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "ahnara",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(result)
    }

    async fn next_id(&self) -> u64 {
        self.inner.request_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id().await;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let response = timeout(
            Duration::from_secs(self.inner.timeout_secs),
            self.send_and_read_response(payload, Some(id)),
        )
        .await
        .map_err(|_| anyhow!("MCP request {} timed out for {}", method, self.inner.name))??;

        if let Some(error) = response.error {
            return Err(anyhow!(
                "MCP {} error {}: {}",
                method,
                error.code,
                error.message
            ));
        }
        response
            .result
            .ok_or_else(|| anyhow!("MCP {} returned no result", method))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut stdin = self.inner.stdin.lock().await;
        stdin.write_all(payload.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn send_and_read_response(
        &self,
        payload: Value,
        expected_id: Option<u64>,
    ) -> Result<McpResponse> {
        {
            let mut stdin = self.inner.stdin.lock().await;
            stdin.write_all(payload.to_string().as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        let mut reader = self.inner.reader.lock().await;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).await?;
            if bytes == 0 {
                return Err(anyhow!("MCP server {} closed stdout", self.inner.name));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response: McpResponse = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid MCP response from {}", self.inner.name))?;
            if let Some(expected) = expected_id {
                if response.id.as_ref().and_then(Value::as_u64) != Some(expected) {
                    continue;
                }
            }
            return Ok(response);
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(tools
            .into_iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?.to_string();
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                Some(McpToolInfo {
                    name,
                    description,
                    input_schema,
                })
            })
            .collect())
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .await
    }
}

impl Drop for McpClientInner {
    fn drop(&mut self) {
        let _ = self.child.get_mut().start_kill();
    }
}

pub struct McpTool {
    client: McpClient,
    local_name: String,
    remote_name: String,
    description: String,
    parameters: Value,
}

impl McpTool {
    pub fn new(client: McpClient, local_name: String, remote: McpToolInfo) -> Self {
        Self {
            client,
            local_name,
            remote_name: remote.name,
            description: remote.description,
            parameters: remote.input_schema,
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.local_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let output = self.client.call_tool(&self.remote_name, args).await?;
        Ok(ToolResult {
            tool_name: self.local_name.clone(),
            success: true,
            output,
            error: None,
            duration_ms: 0,
        })
    }
}

#[derive(Default)]
pub struct McpRegistry {
    clients: RwLock<Vec<McpClient>>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn load_tools(&self, config: &McpConfig) -> Result<Vec<Arc<dyn Tool>>> {
        if !config.enabled {
            return Ok(Vec::new());
        }

        let mut registered = Vec::new();
        let mut clients = self.clients.write().await;

        for server in &config.servers {
            let client = McpClient::start(server).await?;
            let tools = client.list_tools().await?;
            for tool in tools {
                if !is_tool_enabled(server, &tool.name) {
                    continue;
                }
                let local_name = make_local_tool_name(server, &tool.name);
                registered.push(Arc::new(McpTool::new(client.clone(), local_name, tool)) as Arc<dyn Tool>);
            }
            clients.push(client);
        }

        Ok(registered)
    }
}

fn is_tool_enabled(server: &McpServerConfig, remote_name: &str) -> bool {
    if !server.include_tools.is_empty() && !server.include_tools.iter().any(|n| n == remote_name) {
        return false;
    }
    !server.exclude_tools.iter().any(|n| n == remote_name)
}

fn make_local_tool_name(server: &McpServerConfig, remote_name: &str) -> String {
    let prefix = server
        .tool_prefix
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(&server.name);
    format!(
        "mcp_{}_{}",
        sanitize_tool_name(prefix),
        sanitize_tool_name(remote_name)
    )
}

fn sanitize_tool_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.chars() {
        let valid = ch.is_ascii_alphanumeric() || ch == '_';
        if valid {
            out.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_mcp_tool_names() {
        assert_eq!(sanitize_tool_name("GitHub Tools"), "github_tools");
        assert_eq!(sanitize_tool_name("search/repo"), "search_repo");
    }

    #[test]
    fn filters_tools() {
        let mut server = McpServerConfig {
            name: "demo".into(),
            include_tools: vec!["allowed".into()],
            ..Default::default()
        };
        assert!(is_tool_enabled(&server, "allowed"));
        assert!(!is_tool_enabled(&server, "blocked"));
        server.include_tools.clear();
        server.exclude_tools = vec!["blocked".into()];
        assert!(is_tool_enabled(&server, "allowed"));
        assert!(!is_tool_enabled(&server, "blocked"));
    }

    #[test]
    fn builds_prefixed_local_names() {
        let server = McpServerConfig {
            name: "github".into(),
            ..Default::default()
        };
        assert_eq!(
            make_local_tool_name(&server, "search/repo"),
            "mcp_github_search_repo"
        );
    }
}
