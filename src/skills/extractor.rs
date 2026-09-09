//! Skill Extractor - Auto-generates reusable skills from task experience.
//!
//! Monitors tool call patterns during task execution and generates SKILL.md
//! files when trigger conditions are met:
//!   - 5+ tool calls in a single task
//!   - Error recovery (failure followed by success)
//!   - User correction detected in message
//!   - Repeated tool pattern across sessions

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::memory::Reflection;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Record of one tool call inside the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTraceEntry {
    pub tool_name: String,
    pub arguments: String,
    pub result_summary: String,
    pub success: bool,
    pub iteration: usize,
}

/// Why extraction fired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExtractionTrigger {
    HighToolCount { count: usize },
    ErrorRecovery { failed_tool: String, recovered_via: String },
    UserCorrection,
    RepeatedPattern { pattern: String, occurrences: usize },
}

impl std::fmt::Display for ExtractionTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HighToolCount { count } => write!(f, "high-tool-count ({count} calls)"),
            Self::ErrorRecovery { failed_tool, recovered_via } => {
                write!(f, "error-recovery ({failed_tool} -> {recovered_via})")
            }
            Self::UserCorrection => write!(f, "user-correction"),
            Self::RepeatedPattern { pattern, occurrences } => {
                write!(f, "repeated-pattern ({pattern} seen {occurrences}x)")
            }
        }
    }
}

/// The generated skill ready for disk.
#[derive(Debug, Clone)]
pub struct ExtractedSkill {
    pub name: String,
    pub description: String,
    pub trigger: ExtractionTrigger,
    pub skill_md: String,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExtractorConfig {
    pub enabled: bool,
    pub min_tool_calls: usize,
    pub cooldown_secs: u64,
    pub pattern_threshold: usize,
    pub skills_dir: PathBuf,
}

impl Default for ExtractorConfig {
    fn default() -> Self {
        let skills_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ahnara")
            .join("skills");
        Self {
            enabled: true,
            min_tool_calls: 5,
            cooldown_secs: 600,
            pattern_threshold: 2,
            skills_dir,
        }
    }
}

// ---------------------------------------------------------------------------
// SkillExtractor
// ---------------------------------------------------------------------------

pub struct SkillExtractor {
    config: ExtractorConfig,
    last_extraction: RwLock<HashMap<String, u64>>,
    patterns: RwLock<HashMap<String, usize>>,
    patterns_path: PathBuf,
}

impl SkillExtractor {
    pub fn new(config: ExtractorConfig) -> Self {
        let patterns_path = config.skills_dir.join(".patterns.json");
        let patterns = Self::load_patterns(&patterns_path);
        Self {
            config,
            last_extraction: RwLock::new(HashMap::new()),
            patterns: RwLock::new(patterns),
            patterns_path,
        }
    }

    // -- persistence helpers -------------------------------------------------

    fn load_patterns(path: &PathBuf) -> HashMap<String, usize> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_patterns(&self) {
        let patterns = self.patterns.read().unwrap();
        if let Ok(json) = serde_json::to_string_pretty(&*patterns) {
            let _ = fs::create_dir_all(self.patterns_path.parent().unwrap());
            let _ = fs::write(&self.patterns_path, json);
        }
    }

    // -- cooldown ------------------------------------------------------------

    fn can_extract(&self, session_id: &str) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let last = self
            .last_extraction
            .read()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0);
        now.saturating_sub(last) >= self.config.cooldown_secs
    }

    fn mark_extracted(&self, session_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_extraction
            .write()
            .unwrap()
            .insert(session_id.to_string(), now);
    }

    // -- pattern tracking ----------------------------------------------------

    /// Sorted, deduplicated tool names joined by `+`.
    fn compute_pattern(trace: &[ToolTraceEntry]) -> String {
        let mut tools: Vec<String> = trace.iter().map(|t| t.tool_name.clone()).collect();
        tools.sort();
        tools.dedup();
        tools.join("+")
    }

    // -- trigger detection ---------------------------------------------------

    pub fn check_triggers(
        &self,
        trace: &[ToolTraceEntry],
        user_message: &str,
    ) -> Option<ExtractionTrigger> {
        if trace.is_empty() {
            return None;
        }

        // 1) High tool count
        if trace.len() >= self.config.min_tool_calls {
            return Some(ExtractionTrigger::HighToolCount { count: trace.len() });
        }

        // 2) Error recovery: any failure followed by a later success
        for i in 0..trace.len() {
            if !trace[i].success {
                for j in (i + 1)..trace.len() {
                    if trace[j].success {
                        return Some(ExtractionTrigger::ErrorRecovery {
                            failed_tool: trace[i].tool_name.clone(),
                            recovered_via: trace[j].tool_name.clone(),
                        });
                    }
                }
            }
        }

        // 3) User correction
        if Self::is_correction(user_message) {
            return Some(ExtractionTrigger::UserCorrection);
        }

        // 4) Repeated pattern across sessions
        let pattern = Self::compute_pattern(trace);
        let count = self
            .patterns
            .read()
            .unwrap()
            .get(&pattern)
            .copied()
            .unwrap_or(0);
        if count >= self.config.pattern_threshold {
            return Some(ExtractionTrigger::RepeatedPattern {
                pattern,
                occurrences: count,
            });
        }

        None
    }

    fn is_correction(message: &str) -> bool {
        let lower = message.to_lowercase();
        [
            "no,", "no.", "wrong", "actually", "that's not", "that is not",
            "not what i", "not right", "incorrect", "try again", "fix that",
            "redo", "do it again", "not what i meant", "you misunderstood",
            "that's incorrect", "fix this", "change that", "stop,",
            "that's wrong", "that is wrong",
        ]
        .iter()
        .any(|p| lower.starts_with(p) || lower.contains(p))
    }

    // -- extraction ----------------------------------------------------------

    pub async fn extract(
        &self,
        trace: &[ToolTraceEntry],
        trigger: &ExtractionTrigger,
        session_id: &str,
        reflection: Option<&Reflection>,
    ) -> Result<ExtractedSkill> {
        let prompt = self.build_prompt(trace, trigger, reflection);
        let raw = self.call_llm(&prompt).await?;

        let skill_md = self.normalize(&raw, trace, trigger, session_id);
        let name = Self::field(&skill_md, "name").unwrap_or_else(|| Self::slug(trace));
        let description = Self::field(&skill_md, "description")
            .unwrap_or_else(|| format!("Auto-extracted skill: {trigger}"));

        Ok(ExtractedSkill {
            name,
            description,
            trigger: trigger.clone(),
            skill_md,
        })
    }

    /// Save to `skills_dir/<name>/SKILL.md`, skipping if it already exists.
    pub fn save(&self, skill: &ExtractedSkill) -> Result<Option<PathBuf>> {
        let dir = self.config.skills_dir.join(&skill.name);
        if dir.exists() {
            tracing::debug!("Skill '{}' already exists, skipping", skill.name);
            return Ok(None);
        }
        fs::create_dir_all(&dir)?;
        let path = dir.join("SKILL.md");
        fs::write(&path, &skill.skill_md)?;
        Ok(Some(path))
    }

    /// Full pipeline: cooldown -> trigger -> extract -> save.
    pub async fn run(
        &self,
        trace: &[ToolTraceEntry],
        session_id: &str,
        reflection: Option<&Reflection>,
        user_message: &str,
    ) -> Result<Option<PathBuf>> {
        if !self.config.enabled || trace.is_empty() {
            return Ok(None);
        }

        if !self.can_extract(session_id) {
            tracing::debug!("Skill extraction cooldown active for {}", session_id);
            return Ok(None);
        }

        let trigger = match self.check_triggers(trace, user_message) {
            Some(t) => t,
            None => return Ok(None),
        };

        tracing::info!("Skill extraction triggered for {}: {}", session_id, trigger);

        // Update pattern tracker
        let pattern = Self::compute_pattern(trace);
        {
            let mut patterns = self.patterns.write().unwrap();
            *patterns.entry(pattern).or_insert(0) += 1;
        }
        self.save_patterns();

        // Skip if a slug-named folder already exists
        let slug = Self::slug(trace);
        if self.config.skills_dir.join(&slug).exists() {
            tracing::debug!("Similar skill '{}' exists, skipping", slug);
            self.mark_extracted(session_id);
            return Ok(None);
        }

        match self.extract(trace, &trigger, session_id, reflection).await {
            Ok(skill) => {
                let path = self.save(&skill)?;
                self.mark_extracted(session_id);
                if let Some(ref p) = path {
                    tracing::info!("Auto-extracted skill '{}' -> {:?}", skill.name, p);
                }
                Ok(path)
            }
            Err(e) => {
                tracing::warn!("Skill extraction failed: {e}");
                self.mark_extracted(session_id);
                Ok(None)
            }
        }
    }

    // -- LLM -----------------------------------------------------------------

    async fn call_llm(&self, prompt: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": crate::providers::gateway_model(),
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": 1500
        });
        let client = reqwest::Client::new();
        let resp = client
            .post(crate::providers::gateway_endpoint())
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(90))
            .send()
            .await
            .context("Failed to call AI gateway")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("AI gateway error: {status} - {body}");
        }
        let data: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse AI gateway response")?;
        let text = data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            anyhow::bail!("AI gateway returned empty content");
        }
        Ok(text)
    }

    // -- prompt construction -------------------------------------------------

    fn build_prompt(
        &self,
        trace: &[ToolTraceEntry],
        trigger: &ExtractionTrigger,
        reflection: Option<&Reflection>,
    ) -> String {
        let mut p = String::from(
            "You are an AI skill generator. Analyze the task below and produce a reusable \
             SKILL.md file.\n\n",
        );

        p.push_str(&format!("## Trigger\n{trigger}\n\n## Tool Call Trace\n"));
        for (i, t) in trace.iter().enumerate() {
            let status = if t.success { "OK" } else { "FAIL" };
            let args_short = truncate(&t.arguments, 200);
            p.push_str(&format!(
                "{}. {} [{}]\n   args: {}\n   result: {}\n",
                i + 1,
                t.tool_name,
                status,
                args_short,
                truncate(&t.result_summary, 150),
            ));
        }

        if let Some(r) = reflection {
            if let Ok(json) = serde_json::to_string(r) {
                p.push_str(&format!("\n## Session Reflection\n{json}\n"));
            }
        }

        p.push_str(
            "\n## Output Format\n\
             Output ONLY a valid SKILL.md starting with `---` YAML frontmatter.\n\
             Required frontmatter fields: name (slug-case, max 64 chars), description (1-200 chars).\n\
             Body must include:\n\
             - `## When to Use`\n\
             - `## Instructions` (numbered, concrete, reproducible steps)\n\
             - `## Tools Required` (bullet list of tool names)\n\
             No preamble, no explanation, no code fences around the output.\n",
        );
        p
    }

    // -- response normalization ----------------------------------------------

    fn normalize(
        &self,
        raw: &str,
        trace: &[ToolTraceEntry],
        trigger: &ExtractionTrigger,
        session_id: &str,
    ) -> String {
        let cleaned = Self::strip_fences(raw);
        if Self::looks_valid(&cleaned) {
            return cleaned;
        }
        self.fallback(trace, trigger, session_id)
    }

    fn strip_fences(raw: &str) -> String {
        let mut s = raw.trim().to_string();
        if s.starts_with("```") {
            if let Some(nl) = s.find('\n') {
                s = s[nl + 1..].to_string();
            }
            if let Some(pos) = s.rfind("```") {
                s = s[..pos].to_string();
            }
        }
        s.trim().to_string()
    }

    fn looks_valid(content: &str) -> bool {
        content.starts_with("---")
            && content.get(3..).map_or(false, |r| r.contains("---"))
            && Self::field(content, "name").is_some()
            && Self::field(content, "description").is_some()
    }

    fn field(md: &str, key: &str) -> Option<String> {
        if !md.starts_with("---") {
            return None;
        }
        let end = md[3..].find("---")? + 3;
        for line in md[3..end].lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix(&format!("{key}:")) {
                let val = rest.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
        None
    }

    /// Deterministic fallback SKILL.md when the LLM response is unusable.
    fn fallback(
        &self,
        trace: &[ToolTraceEntry],
        trigger: &ExtractionTrigger,
        session_id: &str,
    ) -> String {
        let name = Self::slug(trace);
        let tools: Vec<String> = trace
            .iter()
            .map(|t| t.tool_name.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let steps: Vec<String> = trace
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. Use `{}` -- {}", i + 1, t.tool_name, Self::summarize(t)))
            .collect();
        let now = chrono::Utc::now().format("%Y-%m-%d");

        format!(
            "\
---
name: {name}
description: \"Auto-extracted skill: {trigger}\"
category: auto-extracted
metadata:
  source: skill-extractor
  trigger: \"{trigger}\"
  session: \"{session_id}\"
  extracted_at: \"{now}\"
---

## When to Use

Use this skill when a task requires the tool sequence: {tool_list}.

## Instructions

{steps_block}

## Tools Required

{tools_block}
",
            name = name,
            trigger = trigger,
            session_id = session_id,
            now = now,
            tool_list = tools.join(", "),
            steps_block = steps.join("\n"),
            tools_block = tools.iter().map(|t| format!("- `{t}`")).collect::<Vec<_>>().join("\n"),
        )
    }

    // -- slug / helpers ------------------------------------------------------

    fn slug(trace: &[ToolTraceEntry]) -> String {
        let unique: Vec<String> = trace
            .iter()
            .map(|t| t.tool_name.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let base = if unique.len() <= 2 {
            unique.join("-")
        } else {
            format!("{}-{}-and-{}-more", unique[0], unique[1], unique.len() - 2)
        };
        to_slug(&base)
    }

    fn summarize(t: &ToolTraceEntry) -> String {
        let args: serde_json::Value =
            serde_json::from_str(&t.arguments).unwrap_or(serde_json::Value::Null);
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            return format!("search for '{}'", truncate(q, 50));
        }
        if let Some(c) = args.get("code").and_then(|v| v.as_str()) {
            let first = c.lines().next().unwrap_or(c);
            return format!("run `{}`", truncate(first, 60));
        }
        if let Some(u) = args.get("url").and_then(|v| v.as_str()) {
            return format!("access {}", truncate(u, 60));
        }
        format!("execute {}", t.tool_name)
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn to_slug(s: &str) -> String {
    let slug: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    slug.split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(n: usize) -> Vec<ToolTraceEntry> {
        (0..n)
            .map(|i| ToolTraceEntry {
                tool_name: format!("tool_{}", i % 3),
                arguments: "{}".into(),
                result_summary: "ok".into(),
                success: true,
                iteration: i,
            })
            .collect()
    }

    fn failing_trace() -> Vec<ToolTraceEntry> {
        vec![
            ToolTraceEntry {
                tool_name: "web_search".into(),
                arguments: "{}".into(),
                result_summary: "error".into(),
                success: false,
                iteration: 1,
            },
            ToolTraceEntry {
                tool_name: "browser_open".into(),
                arguments: "{}".into(),
                result_summary: "ok".into(),
                success: true,
                iteration: 2,
            },
        ]
    }

    #[test]
    fn high_tool_count() {
        let ext = SkillExtractor::new(ExtractorConfig::default());
        let t = trace(6);
        let trig = ext.check_triggers(&t, "do something");
        assert!(matches!(trig, Some(ExtractionTrigger::HighToolCount { count: 6 })));
    }

    #[test]
    fn no_trigger_few_tools() {
        let ext = SkillExtractor::new(ExtractorConfig::default());
        let t = trace(2);
        assert!(ext.check_triggers(&t, "hello").is_none());
    }

    #[test]
    fn empty_trace_no_trigger() {
        let ext = SkillExtractor::new(ExtractorConfig::default());
        assert!(ext.check_triggers(&[], "anything").is_none());
    }

    #[test]
    fn error_recovery_trigger() {
        let ext = SkillExtractor::new(ExtractorConfig::default());
        let t = failing_trace();
        let trig = ext.check_triggers(&t, "just do it");
        assert!(matches!(
            trig,
            Some(ExtractionTrigger::ErrorRecovery { .. })
        ));
    }

    #[test]
    fn user_correction_trigger() {
        let ext = SkillExtractor::new(ExtractorConfig::default());
        let t = trace(2);
        let trig = ext.check_triggers(&t, "no, that's wrong, use docker instead");
        assert!(matches!(trig, Some(ExtractionTrigger::UserCorrection)));
    }

    #[test]
    fn slug_generation() {
        let t = trace(3);
        let s = SkillExtractor::slug(&t);
        assert!(!s.is_empty());
        assert!(!s.contains(' '));
        assert_eq!(s, s.to_lowercase());
    }

    #[test]
    fn to_slug_basic() {
        assert_eq!(to_slug("Hello World!"), "hello-world");
        assert_eq!(to_slug("  foo--bar  "), "foo-bar");
        assert_eq!(to_slug("a_b_c"), "a-b-c");
    }

    #[test]
    fn pattern_computation() {
        let t = trace(5);
        let p = SkillExtractor::compute_pattern(&t);
        // tools 0,1,2,0,1 -> sorted+dedup -> 0+1+2
        assert_eq!(p, "tool_0+tool_1+tool_2");
    }
}
