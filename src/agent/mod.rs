//! Agent Core - Central processing unit for the agent framework

pub mod history;

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::capabilities::CapabilityManifest;
use crate::checkpoints::CheckpointManager;
use crate::config::AppConfig;
use crate::context::{build_pruned_messages};
use crate::memory::{
    CodeModeStore,
    CompactionResult, Compactor, HistoryMessage, MemoryEngine, Reflection, Reflector,
    ReflectorConfig, SessionHistory, SessionStore,
    store::MemoryStore,
};
use crate::orchestrator::ToolOrchestrator;
use crate::persona::{shared::load_current_persona, PersonaConfig};
use crate::plugins::{HookEvent, PluginManager};
use crate::providers::{CompletionRequest, Message, ProviderPool, ToolCall};
use crate::skills::{ExtractorConfig, SkillExtractor, ToolTraceEntry};
use regex::Regex;
use tokio::sync::mpsc;

/// Usage statistics
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub total_messages: u64,
    pub total_tokens: u64,
}

/// Mid-loop intervention: a user message injected while the agent is running.
#[derive(Debug, Clone)]
pub struct Intervention {
    pub message: String,
}

/// Global registry of active intervention channels per session.
/// Channels send interventions into the running agent loop.
pub struct InterventionRegistry {
    senders: RwLock<HashMap<String, mpsc::Sender<Intervention>>>,
}

impl InterventionRegistry {
    pub fn new() -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
        }
    }

    /// Register a session as actively running (call at loop start).
    pub async fn register(&self, session_key: &str) -> mpsc::Receiver<Intervention> {
        let (tx, rx) = mpsc::channel(16);
        let mut senders = self.senders.write().await;
        senders.insert(session_key.to_string(), tx);
        rx
    }

    /// Unregister when loop ends.
    pub async fn unregister(&self, session_key: &str) {
        let mut senders = self.senders.write().await;
        senders.remove(session_key);
    }

    /// Inject a message into a running agent loop. Returns false if session is not active.
    pub async fn inject(&self, session_key: &str, message: String) -> bool {
        let senders = self.senders.read().await;
        if let Some(tx) = senders.get(session_key) {
            tx.send(Intervention { message }).await.is_ok()
        } else {
            false
        }
    }

    /// Check if a session currently has an active loop.
    pub async fn is_active(&self, session_key: &str) -> bool {
        let senders = self.senders.read().await;
        senders.contains_key(session_key)
    }
}

/// Tool info
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

/// Build a strict nudge message injected when the agent has made too many
/// tool calls without updating the user.
pub(crate) fn build_nudge_message(tool_call_count: u32) -> String {
    format!(
        "[NUDGE] You have made {} tool calls without updating the user. \
         You MUST now call the `send_message` tool to report your current \
         progress to the user. Be concise — state what you have done so far \
         and what remains. Then continue working on the task until it is \
         complete. Do NOT stop after the message — keep going.",
        tool_call_count
    )
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Core agent state
pub struct AgentCore {
    config: AppConfig,
    memory: Arc<MemoryEngine>,
    providers: Arc<ProviderPool>,
    orchestrator: Arc<ToolOrchestrator>,
    persona: PersonaConfig,
    usage: std::sync::atomic::AtomicU64,
    pub sessions: RwLock<HashMap<String, SessionHistory>>,
    session_store: Arc<SessionStore>,
    code_mode: Arc<CodeModeStore>,
    model_store: Arc<crate::memory::model_store::ModelStore>,
    compactor: Arc<Compactor>,
    reflector: Arc<Reflector>,
    plugins: Arc<PluginManager>,
    checkpoint_manager: Arc<CheckpointManager>,
    extractor: Arc<SkillExtractor>,
    /// Intervention registry for mid-loop message injection
    pub intervention_registry: Arc<InterventionRegistry>,
    /// Last activity timestamp per session (epoch seconds)
    last_activity: RwLock<HashMap<String, u64>>,
    /// Channel name of the current request (set per-request)
    current_channel: parking_lot::RwLock<Option<String>>,
    /// User ID of the current request (set per-request)
    current_user_id: parking_lot::RwLock<Option<String>>,
    /// Shared context for sub-agent tool: (channel, user_id), updated per-request
    subagent_context: Arc<parking_lot::RwLock<(Option<String>, Option<String>)>>,
    /// When true, skip loading persona from disk and use config.persona directly.
    /// Used by /code mode to enforce the coding agent persona.
    override_system_prompt: Arc<RwLock<Option<String>>>,
    /// Shared run log from the cron scheduler (if started)
    schedule_log: Option<crate::scheduler::ScheduleRunLog>,
    /// SQLite memory store for cross-session context
    memory_store: Option<Arc<MemoryStore>>,
    /// Structured outputs collected during the last process() call
    last_structured_outputs: parking_lot::RwLock<Vec<StructuredOutput>>,
}

/// A structured output artifact the agent produced.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructuredOutput {
    pub format: String,
    pub filename: Option<String>,
    pub content: serde_json::Value,
}

impl AgentCore {
    pub fn new(
        // This is a hack to avoid modifying the whole signature
        memory: Arc<MemoryEngine>,
        providers: Arc<ProviderPool>,
        orchestrator: Arc<ToolOrchestrator>,
        config: AppConfig,
        session_store: Arc<SessionStore>,
        code_mode: Arc<CodeModeStore>,
        model_store: Arc<crate::memory::model_store::ModelStore>,
        plugins: Arc<PluginManager>,
        checkpoint_manager: Arc<CheckpointManager>,
        subagent_context: Arc<parking_lot::RwLock<(Option<String>, Option<String>)>>,
        schedule_log: Option<crate::scheduler::ScheduleRunLog>,
        memory_store: Option<Arc<MemoryStore>>,
    ) -> Result<Self> {
        let persona = load_current_persona().unwrap_or_else(|err| {
            tracing::warn!(
                "Failed to load current persona, using config persona: {}",
                err
            );
            config.persona.clone()
        });
        let data_dir = PathBuf::from(shellexpand::tilde(&config.memory.database_path).into_owned())
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("~/.ahnara"));

        let compactor = {
            let c = Compactor::new(config.memory.clone(), data_dir.clone());
            if let Some(ref ms) = memory_store {
                Arc::new(c.with_store(ms.clone()))
            } else {
                Arc::new(c)
            }
        };

        let reflector_config = ReflectorConfig {
            enabled: config.memory.reflection_enabled,
            min_messages: config.memory.reflection_min_messages,
            cooldown_secs: config.memory.reflection_cooldown_secs,
            max_messages: config.agent.recent_history_turns * 2,
            max_prompt_chars: (config.agent.context_window_tokens as usize).min(20_000),
        };
        let reflector = {
            let r = Reflector::new(reflector_config, data_dir.clone());
            if let Some(ref ms) = memory_store {
                let r = r.with_store(ms.clone());
                r.restore_cooldowns();
                Arc::new(r)
            } else {
                Arc::new(r)
            }
        };

        let extractor = Arc::new(SkillExtractor::new(ExtractorConfig::default()));

        Ok(Self {
            config,
            memory,
            providers,
            orchestrator,
            persona,
            usage: std::sync::atomic::AtomicU64::new(0),
            sessions: RwLock::new(HashMap::new()),
            session_store,
            code_mode,
            model_store,
            compactor,
            reflector,
            plugins,
            checkpoint_manager,
            extractor,
            intervention_registry: Arc::new(InterventionRegistry::new()),
            last_activity: RwLock::new(HashMap::new()),
            current_channel: parking_lot::RwLock::new(Option::<String>::None),
            current_user_id: parking_lot::RwLock::new(Option::<String>::None),
            subagent_context,
            override_system_prompt: Arc::new(RwLock::new(None)),
            schedule_log,
            memory_store,
            last_structured_outputs: parking_lot::RwLock::new(Vec::new()),
        })
    }

    /// Drain structured outputs from the last process() call.
    pub fn drain_structured_outputs(&self) -> Vec<StructuredOutput> {
        let mut outputs = self.last_structured_outputs.write();
        std::mem::take(&mut *outputs)
    }

    /// Collect any structured output from a tool result string.
    fn collect_structured_output(&self, result_str: &str) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(result_str) {
            if v.get("__structured_output__").and_then(|b| b.as_bool()).unwrap_or(false) {
                let output = StructuredOutput {
                    format: v["format"].as_str().unwrap_or("json").to_string(),
                    filename: v["filename"].as_str().map(|s| s.to_string()),
                    content: v["content"].clone(),
                };
                self.last_structured_outputs.write().push(output);
            }
        }
    }

    /// Check if there's an active session routing for (channel, user_id).
    /// Used by channels to detect code mode without constructing the key.
    pub fn has_active_session(&self, channel: &str, user_id: &str) -> bool {
        if let Some(ref ms) = self.memory_store {
            return ms.get_active_session_id(channel, user_id)
                .ok()
                .flatten()
                .is_some();
        }
        false
    }

    /// Check if there's an active session routing for (channel, user_id).
    /// Used by channels to detect code mode without constructing the key.
    pub async fn load_sessions(&self) -> Result<()> {
        let persisted = self.session_store.load_all()?;
        let mut sessions = self.sessions.write().await;
        let mut last_activity = self.last_activity.write().await;
        for (key, history) in persisted {
            // Bootstrap last_activity from the session's persisted updated_at
            // so reflection doesn't fire on every session immediately after restart.
            last_activity.insert(key.clone(), history.updated_at);
            sessions.insert(key, history);
        }
        tracing::info!(
            " Loaded {} sessions from disk (last_activity bootstrapped for all)",
            sessions.len()
        );
        Ok(())
    }

    /// Get the active session ID for a (channel, user_id) pair, or create a new
    /// UUID-based session.  Session keys use the format `{channel}:{user_id}:{uuid}`.
    ///
    /// Routing is persisted in the `session_routing` SQLite table so it survives
    /// restarts.  A new UUID is generated only when no active session exists for
    /// the pair.
    pub fn get_or_create_session_id(&self, channel: &str, user_id: &str) -> String {
        // Check if there's already an active session routed for this pair
        if let Some(ref ms) = self.memory_store {
            match ms.get_active_session_id(channel, user_id) {
                Ok(Some(session_id)) => return session_id,
                Ok(None) => {} // fall through to create
                Err(e) => {
                    tracing::warn!("Failed to look up active session: {}", e);
                }
            }
        }

        // Generate a new UUID-based session key
        let uuid = uuid::Uuid::new_v4().to_string();
        let short_uuid = &uuid[..8]; // first 8 hex chars for readability
        let session_id = format!("{}:{}:{}", channel, user_id, short_uuid);

        // Persist the routing
        if let Some(ref ms) = self.memory_store {
            if let Err(e) = ms.set_active_session(channel, user_id, &session_id) {
                tracing::warn!("Failed to set active session routing: {}", e);
            }
        }

        session_id
    }

    /// Reset the session routing for a (channel, user_id) pair so the next
    /// message creates a fresh session.  Used by /new and /clear commands.
    pub fn reset_session_routing(&self, channel: &str, user_id: &str) {
        if let Some(ref ms) = self.memory_store {
            match ms.get_active_session_id(channel, user_id) {
                Ok(Some(old_id)) => {
                    tracing::info!(
                        "Resetting session routing for {}:{} (was: {})",
                        channel, user_id, old_id
                    );
                }
                _ => {}
            }
            // Remove the routing so get_or_create generates a new UUID
            if let Err(e) = ms.clear_active_session(channel, user_id) {
                tracing::warn!("Failed to clear session routing: {}", e);
            }
        }
    }

    /// Migrate existing JSON session files to SQLite store.
    /// Non-fatal: errors on individual files are logged and skipped.
    pub fn migrate_json_sessions_to_sqlite(
        &self,
        store: &MemoryStore,
        json_dir: &std::path::Path,
    ) -> Result<usize> {
        use std::fs;
        let mut migrated = 0;
        if !json_dir.exists() {
            return Ok(0);
        }
        for entry in fs::read_dir(json_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                match fs::read_to_string(&path) {
                    Ok(json) => {
                        match serde_json::from_str::<SessionHistory>(&json) {
                            Ok(session) => {
                                if let Err(e) = store.import_session(&session) {
                                    tracing::warn!("Failed to migrate session {:?}: {}", path, e);
                                } else {
                                    migrated += 1;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse session {:?}: {}", path, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read {:?}: {}", path, e);
                    }
                }
            }
        }
        Ok(migrated)
    }

    /// Enable override mode to skip loading persona from disk.
    /// Used by /code mode to enforce the coding agent persona.
    /// Set a system prompt override, completely replacing the normal persona.
    /// Used by /code mode to inject the pure coding agent prompt.
    pub async fn set_system_prompt_override(&self, session_key: &str, prompt: String) {
        // Persist to disk so it survives restarts
        if let Err(e) = self.code_mode.activate(session_key, prompt.clone()) {
            tracing::warn!("Failed to persist code mode override: {}", e);
        }
        let mut override_prompt = self.override_system_prompt.write().await;
        *override_prompt = Some(prompt);
    }

    pub async fn clear_system_prompt_override(&self, session_key: &str) {
        // Remove from disk
        if let Err(e) = self.code_mode.deactivate(session_key) {
            tracing::warn!("Failed to remove persisted code mode: {}", e);
        }
        let mut override_prompt = self.override_system_prompt.write().await;
        *override_prompt = None;
    }
    
    /// Check if a session has a persisted code mode override
    pub fn get_persisted_code_override(&self, session_key: &str) -> Option<String> {
        self.code_mode.get_override(session_key)
    }

    /// Return the active session key for a (channel, user_id) pair WITHOUT
    /// creating a new one. Returns `None` if no active session is routed.
    /// Used by channels to look up the intervention target key so they can
    /// poke a running agent loop without forcing a new session.
    pub fn session_key_for(&self, channel: &str, user_id: &str) -> Option<String> {
        self.memory_store
            .as_ref()
            .and_then(|ms| ms.get_active_session_id(channel, user_id).ok().flatten())
    }

    /// Get the active session key (creating one if needed) and return the
    /// current `intervention_registry`. Channels use this to inject messages
    /// into a running agent loop.
    pub fn intervention_registry(&self) -> &InterventionRegistry {
        &self.intervention_registry
    }

    /// Inject a user message into a running agent loop. Returns `true` if
    /// the message was accepted (a loop is running for that session),
    /// `false` if no loop is active.
    pub async fn inject_message(
        &self,
        session_key: &str,
        message: String,
    ) -> bool {
        self.intervention_registry.inject(session_key, message).await
    }

    /// Returns `true` if a session currently has an active agent loop.
    pub async fn session_is_active(&self, session_key: &str) -> bool {
        self.intervention_registry.is_active(session_key).await
    }

    /// Process a message with tool execution loop
    pub async fn set_session_context(&self, channel: &str, user_id: &str) {
        *self.current_channel.write() = Some(channel.to_string());
        *self.current_user_id.write() = Some(user_id.to_string());
        // Sync to shared context so sub-agent tool can read it
        *self.subagent_context.write() = (Some(channel.to_string()), Some(user_id.to_string()));
    }

    pub async fn process(&self, message: &str, session_id: Option<&str>) -> String {
        let session_key = session_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "default".to_string());

        let effective_message = self
            .plugins
            .process_message_hooks(
                HookEvent::BeforeMessage,
                Some(&session_key),
                message.to_string(),
            )
            .await;

        let history = self.get_history(&session_key).await;
        let reflections = self.get_reflections(&session_key).unwrap_or_else(Vec::new);

        let mut system_prompt = self.build_system_prompt(&session_key).await;
        tracing::debug!("=== SYSTEM PROMPT START (first 500 chars) ===");
        tracing::debug!("{}", &system_prompt[..system_prompt.len().min(500)]);
        tracing::debug!("=== SYSTEM PROMPT END ===");
        
        self.append_reflections(&mut system_prompt, &reflections);
        self.append_scheduled_jobs(&mut system_prompt);

        let mut messages = build_pruned_messages(
            system_prompt,
            &history,
            effective_message.clone(),
            self.config.agent.recent_history_turns,
            self.config.agent.context_window_tokens,
        );

        // Register intervention channel for this session
        let mut intervention_rx = self.intervention_registry.register(&session_key).await;

        // Tool execution loop
        let mut iterations = 0;
        let max_iterations = self.config.agent.max_tool_iterations as usize;
        let nudge_threshold = self.config.agent.nudge_after_tool_calls;
        let mut total_tool_calls: u32 = 0;
        let mut final_response: String;
        let mut tool_trace: Vec<ToolTraceEntry> = Vec::new();

        loop {
            iterations += 1;
            // Drain any mid-loop interventions (user messages injected while running)
            while let Ok(intervention) = intervention_rx.try_recv() {
                messages.push(Message::new("user", &intervention.message));
                tracing::debug!(session = %session_key, "Intervention injected");
            }
            if iterations > max_iterations {
                final_response = "Error: Max tool iterations reached".into();
                break;
            }

            let code_only = {
                let lock = self.override_system_prompt.read().await;
                lock.is_some()
            };
            let request = self.build_request(&messages, code_only);

            match self.providers.complete(request).await {
                Ok(response) => {
                    // Update usage
                    if let Some(usage) = &response.usage {
                        self.usage.fetch_add(
                            usage.total_tokens as u64,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }

                    // Check if there are tool calls
                    if let Some(tool_calls) = &response.tool_calls {
                        if !tool_calls.is_empty() {
                            tracing::debug!(count = tool_calls.len(), "Model returned tool_calls");

                            // Add assistant message with tool calls
                            messages.push(Message::with_tool_calls("assistant", tool_calls.clone()));

                            // Execute each tool
                            for tool_call in tool_calls {
                                let result = self.execute_tool(tool_call).await;
                                self.collect_structured_output(&result);
                                total_tool_calls += 1;
                                let success = !result.starts_with("Tool error:");
                                let summary = if result.len() > 500 {
                                    format!("{}...", &result[..result.floor_char_boundary(500)])
                                } else {
                                    result.clone()
                                };
                                tool_trace.push(ToolTraceEntry {
                                    tool_name: tool_call.function.name.clone(),
                                    arguments: tool_call.function.arguments.clone(),
                                    result_summary: summary,
                                    success,
                                    iteration: iterations,
                                });

                                // Add tool result as a message (truncated + spilled if oversized)
                                messages.push(Message::tool_result(
                                    &tool_call.id,
                                    &tool_call.function.name,
                                    &crate::context::spill_tool_output(
                                        &result,
                                        self.config.agent.tool_output_max_chars,
                                        &tool_call.function.name,
                                    ),
                                ));
                            }

                            // Check if nudge threshold crossed
                            if nudge_threshold > 0 && total_tool_calls >= nudge_threshold {
                                let nudge = build_nudge_message(total_tool_calls);
                                messages.push(Message::new("user", &nudge));
                                total_tool_calls = 0;
                                tracing::info!("Nudge injected after {} tool calls", nudge_threshold);
                            }

                            tracing::debug!(count = messages.len(), "Continuing loop");
                            // Continue loop to get next response
                            continue;
                        }
                    }

                    // No tool calls - this is the final response
                    final_response = response.content;
                    break;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("No active provider") {
                        tracing::error!("No active provider configured");
                        final_response = "I can't respond right now because no AI provider is configured. \
                            Please run `ahnara setup` to configure a provider, or set one up in ~/.ahnara/config.toml".into();
                    } else {
                        tracing::error!("Provider error: {}", e);
                        final_response = format!("Error: {}", e);
                    }
                    break;
                }
            }
        }

        // Unregister intervention channel
        self.intervention_registry.unregister(&session_key).await;

        // Store in history
        self.add_to_history(&session_key, "user", &effective_message)
            .await;
        self.add_to_history(&session_key, "assistant", &final_response)
            .await;

        // Run compaction check after assistant response
        if let Some(result) = self.run_post_compaction_check(&session_key).await {
            tracing::info!(
                "Session compacted: {} messages saved ~{} tokens",
                result.compacted_messages,
                result.tokens_saved
            );
        }

        // Spawn background skill extraction (non-blocking)
        if !tool_trace.is_empty() {
            let trace_clone = tool_trace.clone();
            let session_clone = session_key.clone();
            let extractor = Arc::clone(&self.extractor);
            let reflection = self.get_reflections(&session_key).and_then(|mut r| r.pop());
            let user_msg = effective_message.clone();
            tokio::spawn(async move {
                if let Err(e) = extractor
                    .run(&trace_clone, &session_clone, reflection.as_ref(), &user_msg)
                    .await
                {
                    tracing::warn!("Skill extraction background task failed: {e}");
                }
            });
        }

        // Filter out <thought></thought> blocks including content using regex
        let re = Regex::new(r"(?s)<thought>.*?</thought>").unwrap();
        let filtered_response = re.replace_all(&final_response, "").to_string();
        let filtered_response = self
            .plugins
            .process_message_hooks(
                HookEvent::AfterMessage,
                Some(&session_key),
                filtered_response,
            )
            .await;

        // Update last activity timestamp for the session
        self.touch_activity(&session_key).await;

        filtered_response
    }

    async fn execute_tool(&self, tool_call: &ToolCall) -> String {
        let args: serde_json::Value =
            serde_json::from_str(&tool_call.function.arguments).unwrap_or(serde_json::Value::Null);

        tracing::info!(
            " Executing tool: {} with args: {}",
            tool_call.function.name,
            args
        );

        // Execute via orchestrator
        let result = self
            .orchestrator
            .execute_tool(&tool_call.function.name, args)
            .await;

        if result.success {
            serde_json::to_string(&result.output).unwrap_or_else(|_| result.output.to_string())
        } else {
            format!(
                "Tool error: {}",
                result.error.as_ref().unwrap_or(&"Unknown error".into())
            )
        }
    }

    fn append_reflections(&self, system_prompt: &mut String, reflections: &[super::memory::reflector::Reflection]) {
        if reflections.is_empty() {
            return;
        }
        system_prompt.push_str("\n\n## Your Memory\n\nYou remember the following from past conversations with this user. This is not reference material -- these are things you experienced and learned firsthand. Speak and act accordingly. If you learned something didn't work, don't repeat it. If the user told you their preference, you already know it. If a task was left unfinished, you know what remains.\n");
        for r in reflections.iter().take(3) {
            let outcome = match r.completed.as_str() {
                "true" => "This was completed.",
                "false" => "This was NOT finished.",
                "partial" => "This was partially done.",
                other => other,
            };
            let next = if !r.next_steps.is_empty() {
                format!("\nStill remaining: {}", r.next_steps.join("; "))
            } else {
                String::new()
            };
            let behavioral = r.behavioral_note.as_ref().map(|n| format!("\nThe user was clear about this: {}", n)).unwrap_or_default();
            let avoid = r.approach_that_failed.as_ref().map(|a| format!("\nWhat didn't work: {}", a)).unwrap_or_default();
            let use_str = r.approach_that_worked.as_ref().map(|a| format!("\nWhat worked: {}", a)).unwrap_or_default();
            let prefs = r.user_preferences.as_ref().map(|p| format!("\nTheir preferences: {}", p)).unwrap_or_default();
            let evidence = r.evidence.as_ref().map(|e| format!("\nEvidence: {}", e)).unwrap_or_default();
            system_prompt.push_str(&format!(
                "\n**{}** -- The user was trying to: {}. {}{}{}{}{}{}{}",
                r.title, r.user_goal, outcome, next, behavioral, avoid, use_str, prefs, evidence,
            ));
            system_prompt.push('\n');
        }
    }

    fn append_scheduled_jobs(&self, system_prompt: &mut String) {
        let log = match &self.schedule_log {
            Some(log) => log,
            None => return,
        };
        let guard = match log.read() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_empty() {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        system_prompt.push_str("\n\n## Your Scheduled Jobs\n\nYou have set up these recurring tasks. You remember creating them and you are aware of their recent status.\n");
        for entry in guard.values() {
            let status = if entry.last_run_at == 0 {
                "never ran".to_string()
            } else {
                let ago = format_duration(now.saturating_sub(entry.last_run_at));
                let outcome = if entry.last_success { "success" } else { "failed" };
                format!("{} ago | {} | run #{}", ago, outcome, entry.run_count)
            };
            let result_line = if !entry.last_result_summary.is_empty() {
                format!("\n  Result: \"{}\"", entry.last_result_summary)
            } else {
                String::new()
            };
            let enabled_str = if entry.enabled { "" } else { " [DISABLED]" };
            system_prompt.push_str(&format!(
                "\n- {}{} (cron: {})\n  Task: \"{}\"\n  Status: {}{}",
                entry.name, enabled_str, entry.cron, entry.prompt_summary, status, result_line,
            ));
        }
    }

    async fn get_history(&self, session_key: &str) -> Vec<HistoryMessage> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_key) {
            session.messages.clone()
        } else {
            vec![]
        }
    }

    pub async fn add_to_history(&self, session_key: &str, role: &str, content: &str) {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(session_key.to_string())
            .or_insert_with(|| SessionHistory::new(session_key));

        session.add_message(role, content, None);

        // Note: removed the old 50 message limit - compaction handles this now

        let session_clone = session.clone();
        drop(sessions);

        let _ = self.session_store.save(session_key, &session_clone);
    }

    /// Run post-compaction check after assistant response
    /// Returns the compaction result if compaction was triggered
    pub async fn run_post_compaction_check(&self, session_key: &str) -> Option<CompactionResult> {
        // Get current message count
        let message_count = {
            let sessions = self.sessions.read().await;
            sessions
                .get(session_key)
                .map(|s| s.messages.len())
                .unwrap_or(0)
        };

        // Check if compaction should run
        if !self.compactor.should_compact(session_key, message_count) {
            return None;
        }

        tracing::info!(
            "Compaction triggered for session {} ({} messages >= threshold {})",
            session_key,
            message_count,
            self.config.memory.compaction_threshold
        );

        // Get mutable session and compact
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_key) {
            match self.compactor.compact(session).await {
                Ok(result) => {
                    if result.success {
                        tracing::info!(
                            "Compaction complete: {} -> {} messages, ~{} tokens saved",
                            result.original_messages,
                            result.compacted_messages,
                            result.tokens_saved
                        );

                        // Save compacted session
                        let session_clone = session.clone();
                        drop(sessions);
                        let _ = self.session_store.save(session_key, &session_clone);

                        return Some(result);
                    } else {
                        tracing::warn!(
                            "Compaction failed: {}",
                            result
                                .error
                                .as_ref()
                                .unwrap_or(&"Unknown error".to_string())
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Compaction error: {}", e);
                }
            }
        }

        None
    }

    fn build_request(&self, messages: &[Message], code_only: bool) -> CompletionRequest {
        let all_tools: Vec<crate::providers::ToolDefinition> = self
            .orchestrator
            .get_definitions()
            .into_iter()
            .map(|t| crate::providers::ToolDefinition {
                tool_type: t.tool_type,
                function: crate::providers::FunctionDefinition {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: t.function.parameters,
                },
            })
            .collect();

        let tools: Vec<crate::providers::ToolDefinition> = if code_only {
            all_tools
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.function.name.as_str(),
                        "read_file"
                            | "list_files"
                            | "edit_file"
                            | "edit_file_llm"
                            | "create_or_rewrite_file"
                            | "run_bash_command"
                            | "run_sequential_cmds"
                            | "run_parallel_cmds"
                            | "grep_search"
                    )
                })
                .collect()
        } else {
            all_tools
        };

        // Resolve per-user model override (model name, base_url, api_key)
        let (effective_model, user_base_url, user_api_key) = {
            let channel = self.current_channel.read();
            let user_id = self.current_user_id.read();
            match (channel.as_deref(), user_id.as_deref()) {
                (Some(ch), Some(uid)) => {
                    match crate::commands::model::resolve_user_model(&self.model_store, ch, uid) {
                        Ok((_provider_type, base_url, api_key, Some(model))) => {
                            tracing::info!(
                                "Using user model override: {} base={:?}",
                                model, base_url
                            );
                            (model, base_url, api_key)
                        }
                        _ => (self.config.agent.default_model.clone(), None, None),
                    }
                }
                _ => (self.config.agent.default_model.clone(), None, None),
            }
        };

        let is_override = self.current_channel.read().is_some();
        tracing::info!("Using model: {} (user_override: {})", effective_model, is_override);

        CompletionRequest {
            model: effective_model,
            messages: messages.to_vec(),
            temperature: Some(self.config.agent.temperature),
            max_tokens: Some(self.config.agent.max_tokens),
            tools: Some(tools),
            stream: None,
            base_url: user_base_url,
            api_key: user_api_key,
        }
    }

    pub fn capability_manifest(&self) -> CapabilityManifest {
        CapabilityManifest::new(&self.config, Some(&self.orchestrator))
    }

    async fn build_system_prompt(&self, _session_key: &str) -> String {
        use crate::persona::SystemPromptBuilder;

        let tools = self.orchestrator.get_definitions();
        let capability_summary = self.capability_manifest().prompt_summary();
        let mcp_summaries = self.orchestrator.mcp_summaries();

        // Check if there's an override system prompt (e.g., /code mode)
        {
            let override_lock = self.override_system_prompt.read().await;
            if let Some(ref override_prompt) = *override_lock {
                tracing::info!("[build_system_prompt] OVERRIDE active - bypassing persona, using custom prompt ({} chars)", override_prompt.len());
                let mut prompt = format!("{}\n\n{}", override_prompt, capability_summary);
                prompt.push_str(&format!(
                    "\n\n## Your Identity\nYou are powered by the `{}` LLM.",
                    self.resolved_model_name()
                ));
                if !mcp_summaries.is_empty() {
                    prompt.push_str("\n\n## Connected Integrations (MCP)\n");
                    for s in &mcp_summaries {
                        prompt.push_str(&format!("- {}\n", s));
                    }
                }
                return prompt;
            }
        }

        // Normal persona flow
        tracing::info!("[build_system_prompt] No override - using normal persona flow");
        let persona = load_current_persona().unwrap_or_else(|err| {
            tracing::warn!(
                "Failed to reload current persona, using cached persona: {}",
                err
            );
            self.persona.clone()
        });
        let base_prompt = SystemPromptBuilder::new(persona)
            .with_tools(&tools)
            .with_skills(&[])
            .build();

        let mut prompt = format!("{}\n\n{}", base_prompt, capability_summary);
        prompt.push_str(&format!(
            "\n\n## Your Identity\nYou are powered by the `{}` LLM. If the user asks what model you are or what LLM is running you, tell them this exact model name.",
            self.resolved_model_name()
        ));
        if !mcp_summaries.is_empty() {
            prompt.push_str("\n\n## Connected Integrations (MCP)\n");
            for s in &mcp_summaries {
                prompt.push_str(&format!("- {}\n", s));
            }
        }

        // Inject available token names (never values)
        let token_names = crate::commands::token::list_token_names();
        if !token_names.is_empty() {
            prompt.push_str("\n\n## Available Tokens\n");
            prompt.push_str("The following named tokens are securely stored. You know their names but never their values. ");
            prompt.push_str("Never attempt to display, log, guess, or reveal token values. ");
            prompt.push_str("When the user asks to manage tokens, guide them through `/token set <name> <value>` or `/token remove <name>`.\n");
            for name in &token_names {
                prompt.push_str(&format!("- `{}`\n", name));
            }
        }

        // Inject cross-session memory context (reflections, facts, observations, preferences)
        if let Some(ref ms) = self.memory_store {
            let user_id = self.current_user_id.read();
            let ctx = crate::memory::context::ContextIndex::new(ms.clone());
            match ctx.generate(user_id.as_deref()) {
                Ok(cross_ctx) if !cross_ctx.is_empty() => {
                    tracing::info!(
                        "[build_system_prompt] Injecting cross-session context ({} chars)",
                        cross_ctx.len()
                    );
                    prompt.push_str("\n\n");
                    prompt.push_str(&cross_ctx);
                }
                Ok(_) => {
                    tracing::debug!("[build_system_prompt] No cross-session context to inject");
                }
                Err(e) => {
                    tracing::warn!("[build_system_prompt] Failed to generate cross-session context: {}", e);
                }
            }
        }

        // Inject session search guidance if session tools are available
        if self.memory_store.is_some() {
            prompt.push_str("\n\n## Session Memory\n");
            prompt.push_str(
                "You have access to the full history of past conversations via `list_sessions` and `search_sessions` tools. \
                 Use them proactively and silently:\\n\
                 - When the user references something not in your current context (past projects, old decisions, previous configs, \
                   'as we discussed', 'remember when', 'that thing from last time'), call `search_sessions` before responding.\\n\
                 - Use retrieved context to inform your response naturally — as if you simply remembered it.\\n\
                 - Never tell the user you searched sessions. Never quote raw search results. Never mention the tool by name.\\n\
                 - The goal is seamless continuity: the user should feel like you have a continuous memory across all sessions."
            );
        }

        // Audio transcription guidance
        prompt.push_str("\n\n## Audio Transcription\n");
        prompt.push_str(
            "You have a `transcribe_audio` tool for converting audio/voice files to text using a local Whisper model.\\n\
             - Audio and voice messages from Telegram are auto-transcribed when received. The transcript is included in the message.\\n\
             - If auto-transcription failed or you need a different model/language, call `transcribe_audio` manually.\\n\
             - Use model_size 'base' for speed, 'small' or 'medium' for better accuracy, 'large-v3' for best quality (slow).\\n\
             - When you receive an auto-transcript, treat it as the user's spoken words — respond to the content naturally.\\n\
             - Never mention transcription internals (model names, Whisper, etc.) to the user unless asked."
        );

        prompt
    }

    pub async fn memory_summary(&self) -> String {
        self.handle_memory_text("/memory").await
    }

    pub async fn handle_memory_text(&self, text: &str) -> String {
        let parts: Vec<&str> = text.trim().splitn(3, ' ').collect();
        let sub = parts.get(1).copied().unwrap_or("");

        let ms = match &self.memory_store {
            Some(ms) => ms,
            None => return "Memory store not available.".into(),
        };

        match sub {
            "facts" => {
                let facts = match ms.list_facts() {
                    Ok(f) => f,
                    Err(e) => return format!("Error: {}", e),
                };
                if facts.is_empty() {
                    return "No facts stored.".into();
                }
                let mut out = format!("{} facts:\n", facts.len());
                for f in &facts {
                    out.push_str(&format!(
                        "- {}: {} ({})\n",
                        f.key,
                        f.value,
                        f.source.as_deref().unwrap_or("unknown")
                    ));
                }
                out
            }
            "recall" => {
                let key = parts.get(2).copied().unwrap_or("");
                if key.is_empty() {
                    return "Usage: /memory recall <key>".into();
                }
                match ms.get_fact(key) {
                    Ok(Some(f)) => format!("{}: {}", f.key, f.value),
                    Ok(None) => format!("No fact found for: {}", key),
                    Err(e) => format!("Error: {}", e),
                }
            }
            "remember" => {
                let rest = parts.get(2).copied().unwrap_or("");
                let (key, value) = match rest.split_once(' ') {
                    Some((k, v)) => (k, v),
                    None => return "Usage: /memory remember <key> <value>".into(),
                };
                match ms.set_fact(key, value, Some("telegram")) {
                    Ok(()) => format!("Remembered: {} = {}", key, value),
                    Err(e) => format!("Error: {}", e),
                }
            }
            "search" => {
                let query = parts.get(2).copied().unwrap_or("");
                if query.is_empty() {
                    return "Usage: /memory search <query>".into();
                }
                match ms.search_all(query, 5) {
                    Ok(r) => {
                        let total = r.reflections.len()
                            + r.observations.len()
                            + r.facts.len()
                            + r.summaries.len();
                        if total == 0 {
                            return format!("No results for: {}", query);
                        }
                        let mut out = String::new();
                        for f in &r.facts {
                            out.push_str(&format!("[fact] {}: {}\n", f.key, f.value));
                        }
                        for o in &r.observations {
                            out.push_str(&format!("[obs] {}: {}\n", o.title, o.narrative));
                        }
                        for refl in &r.reflections {
                            out.push_str(&format!("[reflect] {}\n", refl.narrative));
                        }
                        out.push_str(&format!("\n{} results", total));
                        out
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "stats" => {
                let sessions = ms.session_count().unwrap_or(0);
                let reflections = ms.reflection_count().unwrap_or(0);
                let facts = ms.fact_count().unwrap_or(0);
                let observations = ms.observation_count().unwrap_or(0);
                format!(
                    "Memory stats\nSessions: {}\nReflections: {}\nFacts: {}\nObservations: {}",
                    sessions, reflections, facts, observations
                )
            }
            "preferences" => {
                let prefs = match ms.get_preferences(None) {
                    Ok(p) => p,
                    Err(e) => return format!("Error: {}", e),
                };
                if prefs.is_empty() {
                    return "No preferences tracked.".into();
                }
                let mut out = String::new();
                for p in &prefs {
                    out.push_str(&format!(
                        "- {}: {} ({:.0}%)\n",
                        p.category, p.preference, p.confidence * 100.0
                    ));
                }
                out
            }
            "reflections" => {
                let reflections = match ms.get_reflections(None, 10) {
                    Ok(r) => r,
                    Err(e) => return format!("Error: {}", e),
                };
                if reflections.is_empty() {
                    return "No reflections stored.".into();
                }
                let mut out = format!("{} reflections:\n", reflections.len());
                for r in &reflections {
                    out.push_str(&format!(
                        "- [{}] {}\n",
                        r.reflection_type, r.narrative
                    ));
                }
                out
            }
            "" | "sessions" => {
                if let Some(ref ms) = self.memory_store {
                    match ms.list_sessions(50) {
                        Ok(records) if records.is_empty() => {
                            return "No sessions found.".into();
                        }
                        Ok(records) => {
                            let mut summary = String::new();
                            for r in &records {
                                summary.push_str(&format!(
                                    "Session: {} | channel: {} | msgs: {} | updated: {}\n",
                                    r.session_id, r.channel, r.message_count, r.updated_at
                                ));
                            }
                            return summary;
                        }
                        Err(e) => return format!("Error listing sessions: {}", e),
                    }
                }
                "Memory store not available.".into()
            }
            _ => {
                "Memory commands:\n\
                 /memory - session list\n\
                 /memory facts - list all facts\n\
                 /memory recall <key> - get a fact\n\
                 /memory remember <key> <value> - store a fact\n\
                 /memory search <query> - search memory\n\
                 /memory reflections - recent reflections\n\
                 /memory stats - memory statistics\n\
                 /memory preferences - user preferences"
                    .into()
            }
        }
    }

    /// Clear a session from memory and disk
    pub async fn clear_session(&self, session_id: &str) {
        // Remove from in-memory sessions
        self.sessions.write().await.remove(session_id);

        // Delete from disk
        if let Err(e) = self.session_store.delete(session_id) {
            tracing::warn!("Failed to delete session file: {}", e);
        }

        tracing::info!("Cleared session: {}", session_id);
    }

    pub async fn recover_session(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    pub async fn new_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.to_string(), SessionHistory::new(session_id));
        let session = sessions.get(session_id).cloned()
            .ok_or_else(|| anyhow::anyhow!("session not found after insert: {}", session_id))?;
        drop(sessions);
        self.session_store.save(session_id, &session)?;
        Ok(())
    }

    pub fn list_tools(&self) -> Vec<ToolInfo> {
        self.orchestrator
            .get_definitions()
            .into_iter()
            .map(|t| ToolInfo {
                name: t.function.name,
                description: t.function.description,
            })
            .collect()
    }

    pub async fn get_usage_stats(&self) -> Usage {
        Usage {
            total_messages: 0,
            total_tokens: self.usage.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn model_name(&self) -> &str {
        &self.config.agent.default_model
    }

    /// Resolve the actual model name for the current session, checking per-user overrides.
    pub fn resolved_model_name(&self) -> String {
        let channel = self.current_channel.read();
        let user_id = self.current_user_id.read();
        match (channel.as_deref(), user_id.as_deref()) {
            (Some(ch), Some(uid)) => {
                match crate::commands::model::resolve_user_model(&self.model_store, ch, uid) {
                    Ok((_pt, _base, _key, Some(model))) => model,
                    _ => self.config.agent.default_model.clone(),
                }
            }
            _ => self.config.agent.default_model.clone(),
        }
    }

    /// Run reflection on a session
    /// Can be triggered explicitly or after significant activity
    pub async fn run_reflection(&self, session_key: &str) -> Option<Reflection> {
        // Get current message count
        let message_count = {
            let sessions = self.sessions.read().await;
            sessions
                .get(session_key)
                .map(|s| s.messages.len())
                .unwrap_or(0)
        };

        // Check if reflection should run
        if !self.reflector.should_reflect(session_key, message_count) {
            return None;
        }

        tracing::info!(
            "Reflection triggered for session {} ({} messages)",
            session_key,
            message_count
        );

        // Get session and reflect
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_key) {
            match self.reflector.reflect(session).await {
                Ok(Some(reflection)) => {
                    tracing::info!(
                        "Reflection complete: {} - {}",
                        reflection.reflection_type.to_string().to_lowercase(),
                        reflection.title
                    );
                    return Some(reflection);
                }
                Ok(None) => {
                    tracing::debug!("Reflection skipped for session {}", session_key);
                }
                Err(e) => {
                    tracing::error!("Reflection error: {}", e);
                }
            }
        }

        None
    }

    /// Update last activity timestamp for a session
    pub async fn touch_activity(&self, session_key: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut last_activity = self.last_activity.write().await;
        last_activity.insert(session_key.to_string(), now);
    }

    /// Get sessions that have been inactive for longer than reflection_interval_secs
    /// and have enough messages to qualify for reflection
    pub async fn get_sessions_needing_reflection(&self) -> Vec<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let interval_secs = self.config.memory.reflection_interval_secs;
        let min_messages = self.config.memory.reflection_min_messages;

        let sessions = self.sessions.read().await;
        let last_activity = self.last_activity.read().await;

        let mut result = Vec::new();

        for (key, session) in sessions.iter() {
            let last = last_activity.get(key).copied().unwrap_or(0);
            let inactive_secs = now.saturating_sub(last);

            if inactive_secs < interval_secs || session.messages.len() < min_messages {
                continue;
            }

            if !self.reflector.should_reflect(key, session.messages.len()) {
                continue;
            }

            if let Ok(reflections) = self.reflector.load_reflections(key) {
                if let Some(latest) = reflections.first() {
                    if latest.message_count >= session.messages.len() {
                        continue;
                    }
                }
            }

            result.push(key.clone());
        }

        result
    }

    /// Get reflections for a session
    pub fn get_reflections(&self, session_key: &str) -> Option<Vec<Reflection>> {
        self.reflector.load_reflections(session_key).ok()
    }

    /// Get all reflections across all sessions
    pub fn get_all_reflections(&self) -> Option<Vec<Reflection>> {
        self.reflector.load_all_reflections().ok()
    }

    pub async fn create_checkpoint(&self, session_id: &str, label: Option<&str>) -> Result<String> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            self.checkpoint_manager
                .create_checkpoint(session_id, session, label)
        } else {
            Err(anyhow::anyhow!("Session not found: {}", session_id))
        }
    }

    pub async fn rollback_session(&self, session_id: &str, checkpoint_id: &str) -> Result<()> {
        let history = self
            .checkpoint_manager
            .rollback(session_id, checkpoint_id)?;
        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.to_string(), history);
        Ok(())
    }

    /// Hot-reload MCP integrations from config on disk.
    /// Returns a user-friendly summary of what was loaded.
    pub async fn reload_mcp(&self) -> String {
        match self.orchestrator.reload_mcp().await {
            Ok((count, summaries)) => {
                if count == 0 {
                    "MCP reload complete. No tools loaded (check config).".to_string()
                } else {
                    let mut msg = format!("MCP reload complete: {} tools loaded.\n", count);
                    for s in &summaries {
                        msg.push_str(&format!("- {}\n", s));
                    }
                    msg
                }
            }
            Err(e) => format!("MCP reload failed: {}", e),
        }
    }

    /// List MCP tool names for a given server name
    pub fn mcp_tools_for(&self, server_name: &str) -> Vec<String> {
        let prefix = format!("mcp_{}_", server_name);
        self.orchestrator.tools_with_prefix(&prefix)
    }
}
