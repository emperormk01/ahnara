//! Compaction System - Summarizes old messages to reduce context size

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;
use std::sync::RwLock;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::MemoryConfig;
use super::{SessionHistory, HistoryMessage};
use super::store::MemoryStore;
use std::sync::Arc;

/// Result of a compaction operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionResult {
    pub success: bool,
    pub original_messages: usize,
    pub compacted_messages: usize,
    pub summary: String,
    pub tokens_saved: usize,
    pub error: Option<String>,
}

/// Compaction summary record for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSummary {
    pub session_id: String,
    pub summary: String,
    pub original_messages: usize,
    pub compacted_messages: usize,
    pub tokens_saved: usize,
    pub created_at: u64,
}

/// Cooldown state for compaction
pub struct CompactionCooldown {
    last_compaction: RwLock<HashMap<String, u64>>,
    cooldown_secs: u64,
}

impl CompactionCooldown {
    pub fn new(cooldown_secs: u64) -> Self {
        Self {
            last_compaction: RwLock::new(HashMap::new()),
            cooldown_secs,
        }
    }

    /// Check if compaction is allowed for a session (cooldown expired)
    pub fn can_compact(&self, session_id: &str) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let last = self.last_compaction.read().unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0);
        
        now.saturating_sub(last) >= self.cooldown_secs
    }

    /// Mark that compaction just happened
    pub fn mark_compacted(&self, session_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.last_compaction.write().unwrap()
            .insert(session_id.to_string(), now);
    }
}

/// Compactor - handles summarization of old messages
pub struct Compactor {
    config: MemoryConfig,
    cooldown: CompactionCooldown,
    summaries_dir: PathBuf,
    store: Option<Arc<MemoryStore>>,
}

impl Compactor {
    pub fn new(config: MemoryConfig, data_dir: PathBuf) -> Self {
        let summaries_dir = data_dir.join("compaction_summaries");
        let _ = fs::create_dir_all(&summaries_dir);
        
        Self {
            cooldown: CompactionCooldown::new(config.compaction_cooldown_secs),
            summaries_dir,
            config,
            store: None,
        }
    }

    /// Attach a SQLite store for dual-writing compaction summaries
    pub fn with_store(mut self, store: Arc<MemoryStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Check if compaction should run for a session
    pub fn should_compact(&self, session_id: &str, message_count: usize) -> bool {
        if !self.config.compaction_enabled {
            return false;
        }
        
        if message_count < self.config.compaction_threshold {
            return false;
        }
        
        self.cooldown.can_compact(session_id)
    }

    /// Compact a session's history
    pub async fn compact(&self, session: &mut SessionHistory) -> Result<CompactionResult> {
        let original_count = session.messages.len();
        let keep_recent = self.config.compaction_keep_recent;
        
        // Nothing to compact if already under threshold
        if original_count <= keep_recent {
            return Ok(CompactionResult {
                success: true,
                original_messages: original_count,
                compacted_messages: original_count,
                summary: String::new(),
                tokens_saved: 0,
                error: None,
            });
        }
        
        // Take messages to compact (everything except last N)
        let to_compact: Vec<_> = session.messages.iter()
            .take(original_count - keep_recent)
            .cloned()
            .collect();
        
        if to_compact.is_empty() {
            return Ok(CompactionResult {
                success: true,
                original_messages: original_count,
                compacted_messages: keep_recent,
                summary: String::new(),
                tokens_saved: 0,
                error: None,
            });
        }
        
        // Build prompt for summarization
        let prompt = self.build_summary_prompt(&to_compact);
        
        // Call Pollinations.ai for summarization (free, no API key needed)
        match self.call_gemma(&prompt).await {
            Ok(summary) => {
                // Create summary message
                let summary_content = format!(
                    "[CONVERSATION SUMMARY]\n{}\n[END SUMMARY]",
                    summary.trim()
                );
                
                // Remove old messages BUT preserve the system prompt at position 0
                let drain_start = if session.messages.first().map(|m| m.role.as_str()) == Some("system") {
                    1
                } else {
                    0
                };
                let drain_end = original_count - keep_recent;
                if drain_end > drain_start {
                    session.messages.drain(drain_start..drain_end);
                }
                
                // Build summary history message
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                
                let summary_msg = HistoryMessage {
                    role: "system".to_string(),
                    content: summary_content,
                    timestamp: now,
                    tool_calls: None,
                };
                
                // Insert summary right after the system prompt (or at position 0 if none)
                let insert_at = if session.messages.first().map(|m| m.role.as_str()) == Some("system") {
                    1
                } else {
                    0
                };
                session.messages.insert(insert_at, summary_msg);
                
                // Estimate tokens saved (rough: 4 chars per token)
                let tokens_saved = (to_compact.iter()
                    .map(|m| m.content.len())
                    .sum::<usize>() / 4)
                    .saturating_sub(summary.len() / 4);
                
                // Mark cooldown
                self.cooldown.mark_compacted(&session.session_id);
                
                // Save summary record
                let summary_record = CompactionSummary {
                    session_id: session.session_id.clone(),
                    summary: summary.clone(),
                    original_messages: original_count,
                    compacted_messages: session.messages.len(),
                    tokens_saved,
                    created_at: now,
                };
                let _ = self.save_summary(&summary_record);

                // SQLite insert
                if let Some(ref store) = self.store {
                    if let Err(e) = store.insert_compaction_summary(
                        &session.session_id,
                        &summary,
                        original_count,
                        session.messages.len(),
                        tokens_saved,
                    ) {
                        tracing::warn!("Failed to insert compaction summary into SQLite: {}", e);
                    }
                }

                Ok(CompactionResult {
                    success: true,
                    original_messages: original_count,
                    compacted_messages: session.messages.len(),
                    summary,
                    tokens_saved,
                    error: None,
                })
            }
            Err(e) => {
                Ok(CompactionResult {
                    success: false,
                    original_messages: original_count,
                    compacted_messages: original_count,
                    summary: String::new(),
                    tokens_saved: 0,
                    error: Some(e.to_string()),
                })
            }
        }
    }

    /// Build prompt for summarization
    fn build_summary_prompt(&self, messages: &[HistoryMessage]) -> String {
        let mut prompt = String::from("Summarize this conversation concisely. Preserve key facts, decisions, and context.\n\n");
        
        for msg in messages {
            let role = match msg.role.as_str() {
                "user" => "User",
                "assistant" => "Assistant",
                "system" => "System",
                "tool" => "Tool",
                _ => &msg.role,
            };
            prompt.push_str(&format!("{}: {}\n", role, msg.content));
        }
        
        prompt
    }

    /// Call Auxlo gateway for summarization
    async fn call_gemma(&self, prompt: &str) -> Result<String> {
        let url = crate::providers::gateway_endpoint();

        let body = serde_json::json!({
            "model": crate::providers::gateway_model(),
            "messages": [
                {
                    "role": "system",
                    "content": "You are a concise summarizer. Summarize conversations briefly, preserving key facts, decisions, and context."
                },
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "max_tokens": 1500
        });
        
        let client = reqwest::Client::new();
        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
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

    /// Save compaction summary to disk
    fn save_summary(&self, summary: &CompactionSummary) -> Result<()> {
        let path = self.summaries_dir.join(format!("{}.json", summary.created_at));
        let json = serde_json::to_string_pretty(summary)?;
        fs::write(&path, json)
            .with_context(|| format!("Failed to write summary file: {:?}", path))?;
        Ok(())
    }

    /// Load all compaction summaries
    pub fn load_summaries(&self) -> Result<Vec<CompactionSummary>> {
        let mut summaries = Vec::new();
        
        if !self.summaries_dir.exists() {
            return Ok(summaries);
        }
        
        for entry in fs::read_dir(&self.summaries_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(summary) = serde_json::from_str::<CompactionSummary>(&json) {
                        summaries.push(summary);
                    }
                }
            }
        }
        
        // Sort by created_at descending
        summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        
        Ok(summaries)
    }
}
