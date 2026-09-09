//! Reflection System - Produces structured session analysis
//! Similar to Claude Code's Auto Dream feature

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::Arc;

use super::{HistoryMessage, SessionHistory};
use super::store::MemoryStore;

/// Reflection type classification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReflectionType {
    Bugfix,
    Feature,
    Research,
    Question,
    Habit,
    Preference,
    Other,
}

impl std::fmt::Display for ReflectionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReflectionType::Bugfix => write!(f, "bugfix"),
            ReflectionType::Feature => write!(f, "feature"),
            ReflectionType::Research => write!(f, "research"),
            ReflectionType::Question => write!(f, "question"),
            ReflectionType::Habit => write!(f, "habit"),
            ReflectionType::Preference => write!(f, "preference"),
            ReflectionType::Other => write!(f, "other"),
        }
    }
}

/// Structured reflection output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    #[serde(rename = "type")]
    pub reflection_type: ReflectionType,
    pub title: String,
    pub narrative: String,
    #[serde(rename = "userGoal")]
    pub user_goal: String,
    pub completed: String,
    #[serde(rename = "nextSteps")]
    pub next_steps: Vec<String>,
    #[serde(rename = "userPreferences", default, skip_serializing_if = "Option::is_none")]
    pub user_preferences: Option<String>,
    #[serde(rename = "approachThatWorked", default, skip_serializing_if = "Option::is_none")]
    pub approach_that_worked: Option<String>,
    #[serde(rename = "approachThatFailed", default, skip_serializing_if = "Option::is_none")]
    pub approach_that_failed: Option<String>,
    #[serde(rename = "behavioralNote", default, skip_serializing_if = "Option::is_none")]
    pub behavioral_note: Option<String>,
    #[serde(rename = "evidence", default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub session_id: String,
    pub message_count: usize,
    pub created_at: u64,
}

/// Reflector configuration
#[derive(Debug, Clone)]
pub struct ReflectorConfig {
    pub enabled: bool,
    pub min_messages: usize,
    pub cooldown_secs: u64,
    pub max_messages: usize,
    pub max_prompt_chars: usize,
}

impl Default for ReflectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_messages: 5,
            cooldown_secs: 300, // 5 minutes
            max_messages: 10,
            max_prompt_chars: 20_000,
        }
    }
}

const REFLECTION_PROMPT: &str = r#"You are recalling what happened in a recent conversation with your user. Output a JSON object capturing what you learned -- what the user wanted, what worked, what didn't, and what you should remember for next time.

Fields:
- type: bugfix | feature | research | question | habit | preference | other
- title: what this was about, in plain language
- userGoal: what the user was trying to accomplish
- narrative: your honest account of what happened -- what you tried, what worked, what broke
- completed: "true" | "false" | "partial"
- nextSteps: what remains to be done (max 3)
- userPreferences: anything the user explicitly said they like, dislike, or want done differently
- approachThatWorked: the strategy that actually succeeded (if any)
- approachThatFailed: the strategy that backfired -- something you should never repeat
- behavioralNote: if the user corrected you, got frustrated, or pushed back, what you need to do differently
- evidence: the specific conversation moments that support your recollection
- facts: array of {key, value} objects for any concrete facts worth remembering (e.g. API endpoints, config values, names, preferences). Empty array if none.
- observations: array of strings for notable things observed about the user, their environment, or their work patterns. Empty array if none.
- preferences: array of {category, preference, confidence} objects for user preferences you detected (confidence 0.0-1.0). Empty array if none.

Be concrete. "Install agent-browser" is useful. "Fix the issue" is not.

If the user said "stop doing X" or "don't format like Y", that goes in behavioralNote -- it's the most important kind of memory.

If nothing meaningful happened, return {"type":"other","title":"No significant learning","completed":"true","facts":[],"observations":[],"preferences":[]}

Conversation:
"#;

/// Reflector - produces structured session analysis
pub struct Reflector {
    config: ReflectorConfig,
    last_reflection: std::sync::RwLock<HashMap<String, u64>>,
    store: Option<Arc<MemoryStore>>,
}

use std::collections::HashMap;

impl Reflector {
    pub fn new(config: ReflectorConfig, _data_dir: PathBuf) -> Self {
        Self {
            config,
            last_reflection: std::sync::RwLock::new(HashMap::new()),
            store: None,
        }
    }

    /// Attach a SQLite store for reading/writing reflections
    pub fn with_store(mut self, store: Arc<MemoryStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Restore `last_reflection` timestamps from SQLite so cooldowns survive restarts.
    /// Call once at startup after the store is attached.
    pub fn restore_cooldowns(&self) {
        if let Some(ref store) = self.store {
            match store.get_latest_reflection_per_session() {
                Ok(map) => {
                    let mut lr = self.last_reflection.write().unwrap();
                    let count = map.len();
                    *lr = map;
                    tracing::info!(
                        "Restored {} reflection cooldown timestamps from SQLite",
                        count
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to restore reflection cooldowns: {}", e);
                }
            }
        }
    }

    /// Check if reflection should run for a session
    pub fn should_reflect(&self, session_id: &str, message_count: usize) -> bool {
        if !self.config.enabled {
            return false;
        }

        if message_count < self.config.min_messages {
            return false;
        }

        // Check cooldown
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let last = self
            .last_reflection
            .read()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0);

        now.saturating_sub(last) >= self.config.cooldown_secs
    }

    /// Reflect on a session and produce structured output
    pub async fn reflect(&self, session: &SessionHistory) -> Result<Option<Reflection>> {
        if !self.should_reflect(&session.session_id, session.messages.len()) {
            return Ok(None);
        }

        let latest_reflection = self
            .load_reflections(&session.session_id)?
            .into_iter()
            .next();

        if self.should_skip_existing(&latest_reflection, session.messages.len()) {
            self.mark_reflected(&session.session_id);
            return Ok(None);
        }

        let prompt = self.build_reflection_prompt(&session.messages);
        if prompt.trim().is_empty() {
            self.mark_reflected(&session.session_id);
            return Ok(None);
        }

        let reflection = self.get_reflection_with_retry(&prompt, &session.session_id, session.messages.len()).await?;

        if self.is_duplicate(&latest_reflection, &reflection) {
            self.mark_reflected(&session.session_id);
            return Ok(None);
        }

        self.save_reflection(&reflection)?;
        self.mark_reflected(&session.session_id);

        Ok(Some(reflection))
    }

    fn should_skip_existing(&self, latest: &Option<Reflection>, msg_count: usize) -> bool {
        match latest {
            Some(r) if r.message_count >= msg_count => true,
            _ => false,
        }
    }

    fn is_duplicate(&self, latest: &Option<Reflection>, next: &Reflection) -> bool {
        match latest {
            Some(existing) => self.is_duplicate_reflection(existing, next),
            None => false,
        }
    }

    async fn get_reflection_with_retry(&self, prompt: &str, session_id: &str, msg_count: usize) -> Result<Reflection> {
        let response = self.call_gateway(prompt).await?;
        
        match self.parse_reflection(&response, session_id, msg_count) {
            Ok(r) => Ok(r),
            Err(e) => {
                let retry_prompt = format!(
                    "{}\n\nIMPORTANT: Return ONLY a valid JSON object. No prose, no markdown fences, no explanation. Just the raw JSON object.",
                    prompt
                );
                let retry_response = self.call_gateway(&retry_prompt).await?;
                self.parse_reflection(&retry_response, session_id, msg_count)
                    .context(format!("Reflection parse failed after retry. Original error: {}", e))
            }
        }
    }

    fn mark_reflected(&self, session_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_reflection
            .write()
            .unwrap()
            .insert(session_id.to_string(), now);
    }

    fn is_duplicate_reflection(&self, existing: &Reflection, next: &Reflection) -> bool {
        existing
            .title
            .trim()
            .eq_ignore_ascii_case(next.title.trim())
            && existing
                .user_goal
                .trim()
                .eq_ignore_ascii_case(next.user_goal.trim())
            && existing
                .completed
                .trim()
                .eq_ignore_ascii_case(next.completed.trim())
    }

    /// Build the reflection prompt
    fn build_reflection_prompt(&self, messages: &[HistoryMessage]) -> String {
        let recent_messages = self.recent_messages(messages);
        if recent_messages.is_empty() {
            return String::new();
        }

        let mut prompt = self.reflection_prompt_header();

        for msg in recent_messages {
            let line = self.format_message_line(msg);
            if prompt.len() + line.len() > self.config.max_prompt_chars {
                prompt.push_str("[older content omitted to stay within reflection budget]\n");
                break;
            }
            prompt.push_str(&line);
        }

        prompt.push_str("\nRespond with ONLY the JSON object, no other text.");
        prompt
    }

    fn reflection_prompt_header(&self) -> String {
        REFLECTION_PROMPT.to_string()
    }

    fn format_message_line(&self, msg: &HistoryMessage) -> String {
        let role = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            "tool" => "Tool",
            _ => &msg.role,
        };

        let content = if msg.content.len() > 500 {
            format!("{}... [truncated]", &msg.content[..msg.content.floor_char_boundary(500)])
        } else {
            msg.content.clone()
        };

        format!("{}: {}\n", role, content)
    }

    fn recent_messages<'a>(&self, messages: &'a [HistoryMessage]) -> Vec<&'a HistoryMessage> {
        let max = self.config.max_messages.max(1);
        let start = messages.len().saturating_sub(max);
        messages[start..].iter().collect()
    }

    /// Call the native AI gateway for reflection
    async fn call_gateway(&self, prompt: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": crate::providers::gateway_model(),
            "messages": [
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "max_tokens": 1500
        });

        let client = reqwest::Client::new();
        let response = client
            .post(crate::providers::gateway_endpoint())
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("Failed to call AI gateway")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("AI gateway error: {} - {}", status, body);
        }

        let resp: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse AI gateway response")?;

        let text = resp["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        if text.is_empty() {
            anyhow::bail!("AI gateway returned empty content");
        }

        Ok(text)
    }

    /// Parse the reflection response
    fn parse_reflection(
        &self,
        response: &str,
        session_id: &str,
        message_count: usize,
    ) -> Result<Reflection> {
        let json_str = self.extract_json(response)?;
        let parsed: serde_json::Value = serde_json::from_str(&json_str)
            .with_context(|| format!("Failed to parse reflection JSON: {}", json_str))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let reflection = self.build_reflection_from_json(&parsed, session_id, message_count, now);
        self.store_extracted_items(&parsed, session_id, now);

        Ok(reflection)
    }

    fn build_reflection_from_json(
        &self,
        parsed: &serde_json::Value,
        session_id: &str,
        message_count: usize,
        now: u64,
    ) -> Reflection {
        Reflection {
            reflection_type: self.parse_type(&parsed["type"]),
            title: self.json_str_or(&parsed["title"], "Untitled session"),
            narrative: self.json_str_or(&parsed["narrative"], ""),
            user_goal: self.json_str_or(&parsed["userGoal"], "Unknown goal"),
            completed: self.json_str_or(&parsed["completed"], ""),
            next_steps: self.parse_next_steps(&parsed["nextSteps"]),
            user_preferences: Self::stringify_value(&parsed["userPreferences"]),
            approach_that_worked: Self::stringify_value(&parsed["approachThatWorked"]),
            approach_that_failed: Self::stringify_value(&parsed["approachThatFailed"]),
            behavioral_note: Self::stringify_value(&parsed["behavioralNote"]),
            evidence: Self::stringify_value(&parsed["evidence"]),
            session_id: session_id.to_string(),
            message_count,
            created_at: now,
        }
    }

    fn json_str_or(&self, val: &serde_json::Value, default: &str) -> String {
        val.as_str().unwrap_or(default).to_string()
    }

    /// Coerce a JSON value to Option<String> -- handles strings AND arrays
    fn stringify_value(val: &serde_json::Value) -> Option<String> {
        match val {
            serde_json::Value::Null | serde_json::Value::Object(_) => None,
            serde_json::Value::String(s) if s.is_empty() => None,
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(arr) => {
                let parts: Vec<String> = arr.iter().filter_map(|v| {
                    match v {
                        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        serde_json::Value::Bool(b) => Some(b.to_string()),
                        serde_json::Value::Object(_) => serde_json::to_string(v).ok(),
                        serde_json::Value::Array(_) => serde_json::to_string(v).ok(),
                        _ => None,
                    }
                }).collect();
                if parts.is_empty() { None } else { Some(parts.join(", ")) }
            }
            other => {
                let s = other.to_string();
                if s.is_empty() || s == "null" { None } else { Some(s) }
            }
        }
    }

    /// Attempt to repair truncated JSON by closing open structures
    fn try_repair_json(s: &str) -> Option<String> {
        let mut result = s.to_string();

        // Close any open string
        let mut in_string = false;
        let mut escaped = false;
        for ch in result.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = !in_string;
            }
        }
        if in_string {
            result.push('"');
        }

        // Count open braces/brackets and close them
        let mut brace_depth: i32 = 0;
        let mut bracket_depth: i32 = 0;
        let mut in_str = false;
        let mut esc = false;
        for ch in result.chars() {
            if esc { esc = false; continue; }
            if ch == '\\' && in_str { esc = true; continue; }
            if ch == '"' { in_str = !in_str; continue; }
            if in_str { continue; }
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                '[' => bracket_depth += 1,
                ']' => bracket_depth -= 1,
                _ => {}
            }
        }
        // Remove trailing comma if any
        let trimmed = result.trim_end();
        if trimmed.ends_with(',') {
            result = trimmed[..trimmed.len() - 1].to_string();
        }
        for _ in 0..bracket_depth { result.push(']'); }
        for _ in 0..brace_depth { result.push('}'); }

        // Verify it parses
        if serde_json::from_str::<serde_json::Value>(&result).is_ok() {
            Some(result)
        } else {
            None
        }
    }

    /// Extract JSON from response (handles markdown code blocks, etc.)
    fn extract_json(&self, response: &str) -> Result<String> {
        let trimmed = response.trim();

        // Try direct JSON parse first
        if trimmed.starts_with('{') {
            if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                return Ok(trimmed.to_string());
            }
            // Truncated -- try to repair
            if let Some(repaired) = Self::try_repair_json(trimmed) {
                return Ok(repaired);
            }
        }

        // Try to extract from markdown code block
        if let Some(start) = trimmed.find("```json") {
            let rest = &trimmed[start + 7..];
            if let Some(end) = rest.find("```") {
                return Ok(rest[..end].trim().to_string());
            }
            // Code block not closed -- try repair on the rest
            if let Some(repaired) = Self::try_repair_json(rest.trim()) {
                return Ok(repaired);
            }
        }

        // Try to find JSON object anywhere
        if let Some(start) = trimmed.find('{') {
            let rest = &trimmed[start..];
            if serde_json::from_str::<serde_json::Value>(rest).is_ok() {
                return Ok(rest.to_string());
            }
            if let Some(repaired) = Self::try_repair_json(rest) {
                return Ok(repaired);
            }
        }

        anyhow::bail!("Could not extract JSON from response: {}", trimmed)
    }

    /// Parse reflection type
    fn parse_type(&self, value: &serde_json::Value) -> ReflectionType {
        match value.as_str().map(|s| s.to_lowercase()).as_deref() {
            Some("bugfix") => ReflectionType::Bugfix,
            Some("feature") => ReflectionType::Feature,
            Some("research") => ReflectionType::Research,
            Some("question") => ReflectionType::Question,
            Some("habit") => ReflectionType::Habit,
            Some("preference") => ReflectionType::Preference,
            Some("other") | Some(_) | None => ReflectionType::Other,
        }
    }

    /// Parse next steps array
    fn parse_next_steps(&self, value: &serde_json::Value) -> Vec<String> {
        value
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Extract and store facts, observations, and preferences from a parsed reflection
    fn store_extracted_items(&self, parsed: &serde_json::Value, session_id: &str, now: u64) {
        if let Some(ref store) = self.store {
            self.store_facts(parsed, store);
            self.store_observations(parsed, store, session_id, now);
            self.store_preferences(parsed, store, now);
        }
    }

    fn store_facts(&self, parsed: &serde_json::Value, store: &MemoryStore) {
        let facts = match parsed["facts"].as_array() {
            Some(arr) => arr,
            None => return,
        };
        for fact in facts {
            let key = fact["key"].as_str().unwrap_or_default();
            let value = fact["value"].as_str().unwrap_or_default();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            if let Err(e) = store.set_fact(key, value, Some("reflection")) {
                tracing::warn!("Failed to store extracted fact '{}': {}", key, e);
            }
        }
    }

    fn store_observations(&self, parsed: &serde_json::Value, store: &MemoryStore, session_id: &str, now: u64) {
        let obs_arr = match parsed["observations"].as_array() {
            Some(arr) => arr,
            None => return,
        };
        for obs in obs_arr {
            let title = obs["title"].as_str().unwrap_or_default();
            let narrative = obs["narrative"].as_str().unwrap_or_default();
            if title.is_empty() || narrative.is_empty() {
                continue;
            }
            let observation = super::store::Observation {
                id: None,
                session_id: session_id.to_string(),
                obs_type: obs["type"].as_str().unwrap_or("general").to_string(),
                title: title.to_string(),
                narrative: narrative.to_string(),
                facts: obs["facts"].as_str().map(|s| s.to_string()),
                concepts: obs["concepts"].as_str().map(|s| s.to_string()),
                files: obs["files"].as_str().map(|s| s.to_string()),
                tool_name: None,
                created_at: now,
            };
            if let Err(e) = store.insert_observation(&observation) {
                tracing::warn!("Failed to store extracted observation '{}': {}", title, e);
            }
        }
    }

    fn store_preferences(&self, parsed: &serde_json::Value, store: &MemoryStore, now: u64) {
        let prefs_arr = match parsed["preferences"].as_array() {
            Some(arr) => arr,
            None => return,
        };
        for pref in prefs_arr {
            let category = pref["category"].as_str().unwrap_or("general");
            let preference = pref["preference"].as_str().unwrap_or_default();
            let confidence = pref["confidence"].as_f64().unwrap_or(0.8);
            if preference.is_empty() {
                continue;
            }
            let record = super::store::UserPreference {
                id: None,
                user_id: None,
                category: category.to_string(),
                preference: preference.to_string(),
                confidence,
                source: Some("reflection".to_string()),
                last_reinforced: now,
                created_at: now,
            };
            if let Err(e) = store.upsert_preference(&record) {
                tracing::warn!("Failed to store extracted preference '{}': {}", preference, e);
            }
        }
    }

    /// Save reflection to SQLite (single source of truth)
    fn save_reflection(&self, reflection: &Reflection) -> Result<()> {
        if let Some(ref store) = self.store {
            store.insert_reflection(reflection)
                .context("Failed to insert reflection into SQLite")?;
        } else {
            tracing::warn!("No SQLite store attached — reflection not persisted");
        }
        Ok(())
    }

    /// Load all reflections for a session from SQLite
    pub fn load_reflections(&self, session_id: &str) -> Result<Vec<Reflection>> {
        if let Some(ref store) = self.store {
            store.get_reflections(Some(session_id), 50)
        } else {
            Ok(Vec::new())
        }
    }

    /// Load all reflections across all sessions from SQLite
    pub fn load_all_reflections(&self) -> Result<Vec<Reflection>> {
        if let Some(ref store) = self.store {
            store.get_reflections(None, 1000)
        } else {
            Ok(Vec::new())
        }
    }
}