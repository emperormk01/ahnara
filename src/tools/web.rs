//! Web Tools - search, browser automation, X/Twitter

use async_trait::async_trait;
use anyhow::{anyhow, Result};
use std::process::Command;

use crate::orchestrator::{Tool, ToolResult};

// =====================================================
// WEB SEARCH TOOL (webserp CLI - multi-engine, no API key)
// =====================================================

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }

    fn output_budget(&self) -> usize { 4_000 }
    
    fn description(&self) -> &str { 
        "Search the web using multiple engines (Google, DuckDuckGo, Brave, Yahoo, Mojeek, Startpage, Presearch) in parallel. No API key required. Returns JSON results with titles, URLs, and snippets."
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results per engine (default: 10)",
                    "default": 10
                },
                "engines": {
                    "type": "string",
                    "description": "Comma-separated engine list (default: all). Options: google, duckduckgo, brave, yahoo, mojeek, startpage, presearch"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let query = args["query"].as_str()
            .ok_or_else(|| anyhow!("Missing query parameter"))?;
        
        let max_results = args.get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);
        
        let engines = args.get("engines")
            .and_then(|v| v.as_str());
        
        // Auto-install webserp if missing
        let has_webserp = Command::new("which")
            .arg("webserp")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        
        if !has_webserp {
            tracing::info!("webserp not found, auto-installing via pip...");
            let install = Command::new("pip")
                .args(["install", "webserp"])
                .output();
            match install {
                Ok(o) if o.status.success() => {
                    tracing::info!("webserp installed successfully");
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    return Ok(ToolResult {
                        tool_name: "web_search".into(),
                        success: false,
                        output: serde_json::json!({
                            "error": format!("Failed to install webserp: {}", stderr.trim()),
                            "fix": "Run manually: pip install webserp"
                        }),
                        error: Some(stderr.trim().to_string()),
                        duration_ms: 0,
                    });
                }
                Err(e) => {
                    return Ok(ToolResult {
                        tool_name: "web_search".into(),
                        success: false,
                        output: serde_json::json!({
                            "error": format!("pip not available: {}", e),
                            "fix": "Install pip, then run: pip install webserp"
                        }),
                        error: Some(e.to_string()),
                        duration_ms: 0,
                    });
                }
            }
        }
        
        // Build command
        let mut cmd = Command::new("webserp");
        cmd.arg(query);
        cmd.arg("--max-results").arg(max_results.to_string());
        if let Some(eng) = engines {
            cmd.arg("--engines").arg(eng);
        }
        
        let output = cmd.output()
            .map_err(|e| anyhow!("Failed to run webserp: {}", e))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(ToolResult {
                tool_name: "web_search".into(),
                success: false,
                output: serde_json::json!({
                    "error": format!("webserp failed: {}", stderr.trim()),
                    "query": query
                }),
                error: Some(stderr.trim().to_string()),
                duration_ms: 0,
            });
        }
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        
        // Parse webserp JSON output
        let parsed: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| serde_json::json!({
                "raw": stdout.to_string()
            }));
        
        let results = parsed.get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        
        let unresponsive = parsed.get("unresponsive_engines")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // DDG-lite fallback: webserp's engines serve bot-tier empty pages to
        // datacenter IPs (Google JS-shell, DDG/Yahoo/Startpage no-result shells;
        // only Brave currently parses). lite.duckduckgo.com still serves real
        // results, so an empty webserp response triggers one direct fetch here.
        let (provider, results) = if results.is_empty() {
            match ddg_lite_search(query, max_results).await {
                Ok(fb) if !fb.is_empty() => {
                    tracing::info!(
                        "webserp empty, ddg-lite fallback returned {} results",
                        fb.len()
                    );
                    ("ddg-lite-fallback", fb)
                }
                _ => ("webserp", results),
            }
        } else {
            ("webserp", results)
        };
        
        Ok(ToolResult {
            tool_name: "web_search".into(),
            success: true,
            output: serde_json::json!({
                "query": query,
                "provider": provider,
                "total": results.len(),
                "unresponsive_engines": unresponsive,
                "results": results
            }),
            error: None,
            duration_ms: 0,
        })
    }
}

/// Direct DuckDuckGo Lite search (no JS, no key). Backup for when webserp's
/// scraped engines return empty pages.
async fn ddg_lite_search(query: &str, max_results: u64) -> Result<Vec<serde_json::Value>> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let html = client
        .get("https://lite.duckduckgo.com/lite/")
        .query(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_ddg_lite(&html, max_results as usize))
}

/// Parse DDG-lite HTML: `a.result-link` anchors (href carries `uddg=<pct-url>`)
/// each followed by a `td.result-snippet` cell. Pure regex, no HTML dep.
fn parse_ddg_lite(html: &str, max_results: usize) -> Vec<serde_json::Value> {
    let link_re =
        regex::Regex::new(r#"(?s)<a\b[^>]*class='result-link'[^>]*>(.*?)</a>"#).unwrap();
    let href_re = regex::Regex::new(r#"href="([^"]+)""#).unwrap();
    let snip_re =
        regex::Regex::new(r#"(?s)<td class='result-snippet'>(.*?)</td>"#).unwrap();
    let tag_re = regex::Regex::new(r#"<[^>]+>"#).unwrap();

    let links: Vec<(String, String)> = link_re
        .captures_iter(html)
        .filter_map(|c| {
            let anchor = c.get(0)?.as_str();
            let title = tag_re.replace_all(&c[1], "").trim().to_string();
            let href = href_re.captures(anchor)?.get(1)?.as_str().to_string();
            let url = lite_result_url(&href)?;
            if title.is_empty() {
                return None;
            }
            Some((title, url))
        })
        .collect();
    let snippets: Vec<String> = snip_re
        .captures_iter(html)
        .map(|c| {
            tag_re
                .replace_all(&c[1], "")
                .replace("&nbsp;", " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();

    links
        .into_iter()
        .zip(snippets.into_iter().chain(std::iter::repeat(String::new())))
        .take(max_results)
        .map(|((title, url), snippet)| {
            serde_json::json!({
                "title": title,
                "url": url,
                "content": snippet,
                "engine": "duckduckgo-lite"
            })
        })
        .collect()
}

/// Extract the real URL from a DDG-lite redirect href (`uddg=<pct-encoded>`).
fn lite_result_url(href: &str) -> Option<String> {
    let start = href.find("uddg=")? + 5;
    let end = href[start..].find('&').map(|i| start + i).unwrap_or(href.len());
    let decoded = percent_decode(&href[start..end]);
    if decoded.starts_with("http") {
        Some(decoded)
    } else {
        None
    }
}

/// Minimal percent-decoder (no extra dep for one query param).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes.get(i + 1)).and_then(|b| (*b as char).to_digit(16)),
                (bytes.get(i + 2)).and_then(|b| (*b as char).to_digit(16)),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// =====================================================
// BROWSER TOOLS (uses agent-browser by Vercel)
// =====================================================

fn ensure_agent_browser() -> Result<()> {
    let has_ab = Command::new("which")
        .arg("agent-browser")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_ab {
        tracing::info!("agent-browser not found, installing via npm...");
        let install = Command::new("npm")
            .args(["install", "-g", "agent-browser"])
            .output();
        match install {
            Ok(o) if o.status.success() => {
                tracing::info!("agent-browser installed via npm");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                return Err(anyhow!(
                    "Failed to install agent-browser: {}\nFix: npm install -g agent-browser",
                    stderr.trim()
                ));
            }
            Err(e) => {
                return Err(anyhow!(
                    "npm not available: {}\nFix: install Node.js/npm, then run: npm install -g agent-browser",
                    e
                ));
            }
        }

        // Install the Chromium browser engine that agent-browser needs
        tracing::info!("Installing Chromium for agent-browser...");
        let browser_install = Command::new("npx")
            .args(["playwright", "install", "chromium"])
            .output();
        match browser_install {
            Ok(o) if o.status.success() => {
                tracing::info!("Chromium installed for agent-browser");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("Chromium install warning: {}", stderr.trim());
            }
            Err(e) => {
                tracing::warn!("Could not install Chromium: {}", e);
            }
        }
    }
    Ok(())
}

fn run_agent_browser(args: &[&str]) -> Result<(bool, String)> {
    let output = Command::new("agent-browser")
        .args(args)
        .output()
        .map_err(|e| anyhow!("Failed to run agent-browser: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() && !stdout.is_empty() {
        return Ok((false, if stderr.is_empty() { stdout } else { stderr }));
    }
    Ok((output.status.success(), stdout))
}

pub struct BrowserOpenTool;

#[async_trait]
impl Tool for BrowserOpenTool {
    fn name(&self) -> &str { "browser_open" }
    fn description(&self) -> &str {
        "Open a URL in the agent-browser for subsequent automation (click, fill, screenshot)"
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to open" }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let url = args["url"].as_str()
            .ok_or_else(|| anyhow!("Missing url parameter"))?;
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["open", url])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_open failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserSnapshotTool;

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn name(&self) -> &str { "browser_snapshot" }
    fn description(&self) -> &str {
        "Get accessibility tree snapshot of current page with element refs (@e1, @e2, etc.) for interaction via click or fill"
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "browser".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["snapshot", "-i"])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_snapshot failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserClickTool;

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str { "browser_click" }
    fn description(&self) -> &str {
        "Click an element by CSS selector on the current page. Use browser_snapshot first to discover elements."
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector (e.g., 'button', 'a.link', 'input[type=submit]')" }
            },
            "required": ["selector"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "browser".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let selector = args["selector"].as_str()
            .ok_or_else(|| anyhow!("Missing selector parameter"))?;
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["click", selector])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_click failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserFillTool;

#[async_trait]
impl Tool for BrowserFillTool {
    fn name(&self) -> &str { "browser_fill" }
    fn description(&self) -> &str {
        "Fill text into an input field by CSS selector (clears existing value first). Triggers input/change events."
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector for the input field" },
                "text": { "type": "string", "description": "Text to fill" }
            },
            "required": ["selector", "text"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "browser".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let selector = args["selector"].as_str()
            .ok_or_else(|| anyhow!("Missing selector parameter"))?;
        let text = args["text"].as_str()
            .ok_or_else(|| anyhow!("Missing text parameter"))?;
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["fill", selector, text])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_fill failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserScreenshotTool;

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str { "browser_screenshot" }
    fn description(&self) -> &str {
        "Take a PNG screenshot of the current page"
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to save screenshot (default: /tmp/screenshot.png)",
                    "default": "/tmp/screenshot.png"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "browser".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("/tmp/screenshot.png");
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["screenshot", path, "--full-page"])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_screenshot failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserGetTool;

#[async_trait]
impl Tool for BrowserGetTool {
    fn name(&self) -> &str { "browser_get" }
    fn description(&self) -> &str {
        "Get content from the current page: text, html, url, or title"
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "what": {
                    "type": "string",
                    "description": "What to get: text, html, url, title",
                    "enum": ["text", "html", "url", "title"]
                }
            },
            "required": ["what"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "browser".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        let what = args["what"].as_str()
            .ok_or_else(|| anyhow!("Missing what parameter"))?;
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["get", what])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: if ok { None } else { Some("browser_get failed".into()) },
            duration_ms,
        })
    }
}

pub struct BrowserCloseTool;

#[async_trait]
impl Tool for BrowserCloseTool {
    fn name(&self) -> &str { "browser_close" }
    fn description(&self) -> &str {
        "Close the agent-browser session"
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        let start = std::time::Instant::now();
        let (ok, data) = run_agent_browser(&["close"])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: ok,
            output: serde_json::json!({"result": data}),
            error: None,
            duration_ms,
        })
    }
}

// =====================================================
// X/TWITTER TOOL (uses vxtwitter API)
// =====================================================

pub struct XFetchTool;

#[async_trait]
impl Tool for XFetchTool {
    fn name(&self) -> &str { "x_fetch" }
    fn description(&self) -> &str { "Fetch tweet/user info from X/Twitter via vxtwitter API (no auth required)" }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "string",
                    "description": "What to fetch: tweet or user",
                    "enum": ["tweet", "user"]
                },
                "id": {
                    "type": "string",
                    "description": "Tweet ID or username (e.g., 123456789 or elonmusk)"
                }
            },
            "required": ["type", "id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let fetch_type = args["type"].as_str()
            .ok_or_else(|| anyhow!("Missing type parameter"))?;
        let id = args["id"].as_str()
            .ok_or_else(|| anyhow!("Missing id parameter"))?;
        
        let url = match fetch_type {
            "tweet" => format!("https://api.vxtwitter.com/Twitter/status/{}", id),
            "user" => format!("https://api.vxtwitter.com/{}", id),
            _ => return Err(anyhow!("Invalid type: must be 'tweet' or 'user'"))
        };
        
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; AHNARA/0.1)")
            .build()?;
        
        let resp = client.get(&url).send().await?;
        
        if !resp.status().is_success() {
            return Ok(ToolResult {
                tool_name: self.name().into(),
                success: false,
                output: serde_json::json!({
                    "error": format!("HTTP {}", resp.status()),
                    "url": url
                }),
                error: Some(format!("HTTP {}", resp.status())),
                duration_ms: 0,
            });
        }
        
        let data: serde_json::Value = resp.json().await?;
        
        Ok(ToolResult {
            tool_name: self.name().into(),
            success: true,
            output: data,
            error: None,
            duration_ms: 0,
        })
    }
}

// =====================================================
// WEB FETCH TOOL (agent-browser by Vercel)
// =====================================================

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "web_fetch" }
    fn output_budget(&self) -> usize { 6_000 }
    fn description(&self) -> &str {
        "Fetch a web page and return its content as text/markdown. Uses agent-browser (by Vercel). For reading page content, articles, documentation. No API key required."
    }
    
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let url = args["url"].as_str()
            .ok_or_else(|| anyhow!("Missing url parameter"))?;
        
        if let Err(e) = ensure_agent_browser() {
            return Ok(ToolResult {
                tool_name: "web_fetch".into(),
                success: false,
                output: serde_json::json!({"error": e.to_string()}),
                error: Some(e.to_string()),
                duration_ms: 0,
            });
        }
        
        let start = std::time::Instant::now();
        
        let (ok, _) = run_agent_browser(&["open", url])?;
        if !ok {
            let duration_ms = start.elapsed().as_millis() as u64;
            return Ok(ToolResult {
                tool_name: "web_fetch".into(),
                success: false,
                output: serde_json::json!({"error": "Failed to open URL", "url": url}),
                error: Some("Failed to open URL".into()),
                duration_ms,
            });
        }
        
        let (_ok, _) = run_agent_browser(&["wait", "--load", "networkidle"])?;
        let (ok, text) = run_agent_browser(&["get", "text"])?;
        let duration_ms = start.elapsed().as_millis() as u64;
        
        Ok(ToolResult {
            tool_name: "web_fetch".into(),
            success: ok && !text.trim().is_empty(),
            output: serde_json::json!({
                "url": url,
                "content": text,
                "char_count": text.len(),
                "duration_ms": duration_ms
            }),
            error: None,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LITE_SAMPLE: &str = r#"
        <a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fasync%2F&amp;rut=abc" class='result-link'>Async <b>Rust</b> Guide</a>
        <td class='result-snippet'>Learn <b>async</b> programming in Rust fast.</td>
        <a rel="nofollow" href="//duckduckgo.com/about.html" class='result-link'>About</a>
    "#;

    #[test]
    fn lite_parser_extracts_title_url_snippet() {
        let out = parse_ddg_lite(LITE_SAMPLE, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["title"], "Async Rust Guide");
        assert_eq!(out[0]["url"], "https://example.com/async/");
        assert_eq!(out[0]["content"], "Learn async programming in Rust fast.");
        assert_eq!(out[0]["engine"], "duckduckgo-lite");
    }

    #[test]
    fn lite_parser_respects_max_results() {
        assert!(parse_ddg_lite(LITE_SAMPLE, 0).is_empty());
    }

    #[test]
    fn percent_decode_handles_escapes() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com%2Fa+b"),
            "https://example.com/a b"
        );
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("trailing%"), "trailing%");
    }

    #[test]
    fn lite_result_url_rejects_non_http() {
        assert!(lite_result_url("//duckduckgo.com/about.html").is_none());
        assert_eq!(
            lite_result_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fx.com%2F&rut=1").as_deref(),
            Some("https://x.com/")
        );
    }
}
