//! ObservationPack - handle + paged recall for large tool results.
//! Large outputs are archived and replaced with short handles in history,
//! saving replay tokens every future turn. Handles are stable and content
//! is retrievable via fetch_observation.

use crate::orchestrator::{Tool, ToolResult};
use async_trait::async_trait;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::RwLock;

static STORE: std::sync::LazyLock<RwLock<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn store_observation(id: &str, content: String) {
    STORE.write().unwrap().insert(id.to_string(), content);
}

pub fn get_observation(id: &str) -> Option<String> {
    STORE.read().unwrap().get(id).cloned()
}

pub fn observation_exists(id: &str) -> bool {
    STORE.read().unwrap().contains_key(id)
}

/// Generate a short stable id for an observation.
pub fn new_obs_id(tool_name: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // 8-char hex from nanos + tool prefix
    format!("obs_{}_{:x}", &tool_name[..tool_name.len().min(4)], nanos & 0xffff_ffff)
}

pub struct FetchObservationTool;

#[async_trait]
impl Tool for FetchObservationTool {
    fn name(&self) -> &str { "fetch_observation" }
    fn description(&self) -> &str { "Retrieve a paged slice of an archived large tool result by handle id. Use when a prior tool result was replaced with an observation handle." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Observation handle id e.g. obs_run_abc123"},
                "page": {"type": "integer", "description": "1-indexed page number (default 1)", "default": 1},
                "per_page": {"type": "integer", "description": "Lines per page (default 80, max 200)", "default": 80}
            },
            "required": ["id"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let id = args["id"].as_str().ok_or_else(|| anyhow!("Missing id"))?;
        let page = args.get("page").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
        let per_page = args.get("per_page").and_then(|v| v.as_u64()).unwrap_or(80).clamp(10, 200) as usize;

        let content = get_observation(id).ok_or_else(|| anyhow!("Unknown observation id: {}", id))?;
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let total_pages = (total_lines + per_page - 1) / per_page.max(1);
        let start = (page - 1) * per_page;
        if start >= total_lines {
            return Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: serde_json::json!({"error": format!("Page {} out of range (total_pages {})", page, total_pages), "total_pages": total_pages, "total_lines": total_lines}),
                error: Some(format!("Page {} out of range", page)),
                duration_ms: 0,
            });
        }
        let end = std::cmp::min(start + per_page, total_lines);
        let slice = lines[start..end].join("\n");
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: serde_json::json!({
                "id": id,
                "page": page,
                "per_page": per_page,
                "total_pages": total_pages,
                "total_lines": total_lines,
                "content": slice
            }),
            error: None,
            duration_ms: 0,
        })
    }
}
