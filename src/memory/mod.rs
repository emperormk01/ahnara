//! Memory Engine - with SQLite-backed persistence for sessions, reflections, and more

use anyhow::{Context, Result};
use lru::LruCache;
use std::sync::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::fs;

use crate::config::MemoryConfig;

pub mod compactor;
pub mod context;
pub mod model_store;
pub mod reflector;
pub mod store;

pub use compactor::{Compactor, CompactionResult};
pub use reflector::{Reflector, Reflection, ReflectorConfig};
pub use store::MemoryStore;

/// Memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub metadata: HashMap<String, String>,
    pub created_at: u64,
    pub accessed_at: u64,
    pub access_count: u32,
}

/// Simplified memory engine (hot cache only for now)
pub struct MemoryEngine {
    hot: RwLock<LruCache<String, MemoryEntry>>,
    db_path: PathBuf,
}

impl MemoryEngine {
    pub fn new(config: &MemoryConfig) -> Result<Self> {
        let hot = LruCache::new(NonZeroUsize::new(config.hot_cache_size).unwrap());
        let db_path = PathBuf::from(&config.database_path);
        
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create memory directory: {:?}", parent))?;
        }
        
        Ok(Self {
            hot: RwLock::new(hot),
            db_path,
        })
    }

    pub fn store(&self, key: &str, content: &str, metadata: Option<HashMap<String, String>>) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let entry = MemoryEntry {
            key: key.to_string(),
            content: content.to_string(),
            metadata: metadata.unwrap_or_default(),
            created_at: now,
            accessed_at: now,
            access_count: 1,
        };

        let mut hot = self.hot.write().unwrap();
        hot.put(key.to_string(), entry);
        
        Ok(())
    }

    pub fn retrieve(&self, key: &str) -> Option<MemoryEntry> {
        let mut hot = self.hot.write().unwrap();
        if let Some(entry) = hot.get_mut(key) {
            entry.accessed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            entry.access_count += 1;
            return Some(entry.clone());
        }
        None
    }

    pub fn hot_keys(&self) -> Vec<String> {
        let hot = self.hot.read().unwrap();
        hot.iter().map(|(k, _)| k.clone()).collect()
    }

    /// Filesystem path backing this engine's data directory.
    /// Sibling stores (sessions, models) share the same configured path.
    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    pub fn clear_hot(&self) {
        let mut hot = self.hot.write().unwrap();
        hot.clear();
    }
}

/// Session history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHistory {
    pub session_id: String,
    pub messages: Vec<HistoryMessage>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

impl SessionHistory {
    pub fn new(session_id: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        Self {
            session_id: session_id.to_string(),
            messages: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    pub fn add_message(&mut self, role: &str, content: &str, tool_calls: Option<Vec<serde_json::Value>>) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.messages.push(HistoryMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: now,
            tool_calls,
        });
        self.updated_at = now;
    }
}

/// SQLite-backed session store — replaces JSON file persistence.
/// Wraps a `MemoryStore` and delegates all reads/writes to SQLite.
pub struct SessionStore {
    store: Arc<MemoryStore>,
}

impl SessionStore {
    /// Create from an existing MemoryStore (preferred — shares the same connection).
    pub fn new_from_store(store: Arc<MemoryStore>) -> Result<Self> {
        Ok(Self { store })
    }

    /// Convenience constructor: opens its own MemoryStore at `db_path`.
    /// Use `new_from_store` when a MemoryStore already exists to avoid
    /// duplicate connections.
    pub fn new(db_path: &str) -> Result<Self> {
        let path = std::path::Path::new(db_path);
        let store = MemoryStore::new(path)
            .with_context(|| format!("Failed to open SQLite session store at {}", db_path))?;
        Ok(Self { store: Arc::new(store) })
    }

    /// Access the underlying MemoryStore (for reflection/activity queries).
    pub fn memory_store(&self) -> &Arc<MemoryStore> {
        &self.store
    }

    /// Save a session: upsert the session record, then replace all messages.
    /// This is correct because the in-memory `SessionHistory` is the source
    /// of truth during runtime; SQLite is the persistence layer.
    pub fn save(&self, session_id: &str, history: &SessionHistory) -> Result<()> {
        // Upsert session record
        self.store.create_session(session_id, "cli", None)?;
        // Replace messages (delete + re-insert)
        self.store.delete_messages_for_session(session_id)?;
        for msg in &history.messages {
            self.store.insert_message(session_id, msg)?;
        }
        self.store.update_session(session_id, history.messages.len())?;
        Ok(())
    }

    /// Load a single session from SQLite.
    pub fn load(&self, session_id: &str) -> Result<Option<SessionHistory>> {
        let record = match self.store.get_session(session_id)? {
            Some(r) => r,
            None => return Ok(None),
        };
        let messages = self.store.get_messages(session_id, None)?;
        Ok(Some(SessionHistory {
            session_id: record.session_id,
            messages,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }))
    }

    /// Load all sessions from SQLite.
    pub fn load_all(&self) -> Result<Vec<(String, SessionHistory)>> {
        let records = self.store.list_sessions(500)?;
        let mut result = Vec::new();
        for record in records {
            let messages = self.store.get_messages(&record.session_id, None)?;
            result.push((
                record.session_id.clone(),
                SessionHistory {
                    session_id: record.session_id,
                    messages,
                    created_at: record.created_at,
                    updated_at: record.updated_at,
                },
            ));
        }
        result.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        Ok(result)
    }

    /// Delete a session (and its cascaded messages) from SQLite.
    pub fn delete(&self, session_id: &str) -> Result<()> {
        self.store.delete_messages_for_session(session_id)?;
        self.store.delete_session(session_id)?;
        Ok(())
    }

    /// Count all sessions in SQLite.
    pub fn count(&self) -> Result<usize> {
        self.store.session_count()
    }
}


/// Persisted code mode state -- survives restarts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodeModeState {
    /// Map of session_key -> override system prompt text
    pub active_sessions: HashMap<String, String>,
}

/// Persistent store for code mode overrides
pub struct CodeModeStore {
    file_path: PathBuf,
    state: std::sync::RwLock<CodeModeState>,
}

impl CodeModeStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let dir = PathBuf::from(db_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create code mode directory: {:?}", dir))?;
        
        let file_path = dir.join("code_mode.json");
        
        let state = if file_path.exists() {
            let data = fs::read_to_string(&file_path)
                .with_context(|| format!("Failed to read code_mode.json: {:?}", file_path))?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            CodeModeState::default()
        };
        
        Ok(Self {
            file_path,
            state: std::sync::RwLock::new(state),
        })
    }
    
    /// Activate code mode for a session with the given override prompt
    pub fn activate(&self, session_key: &str, override_prompt: String) -> Result<()> {
        {
            let mut state = self.state.write().unwrap();
            state.active_sessions.insert(session_key.to_string(), override_prompt);
        }
        self.persist()
    }
    
    /// Deactivate code mode for a session
    pub fn deactivate(&self, session_key: &str) -> Result<()> {
        {
            let mut state = self.state.write().unwrap();
            state.active_sessions.remove(session_key);
        }
        self.persist()
    }
    
    /// Check if a session is in code mode, return the override prompt if so
    pub fn get_override(&self, session_key: &str) -> Option<String> {
        let state = self.state.read().unwrap();
        state.active_sessions.get(session_key).cloned()
    }
    
    /// Get all active code mode sessions
    pub fn active_sessions(&self) -> Vec<String> {
        let state = self.state.read().unwrap();
        state.active_sessions.keys().cloned().collect()
    }
    
    fn persist(&self) -> Result<()> {
        let state = self.state.read().unwrap();
        let json = serde_json::to_string_pretty(&*state)
            .context("Failed to serialize code mode state")?;
        fs::write(&self.file_path, json)
            .with_context(|| format!("Failed to write code_mode.json: {:?}", self.file_path))?;
        Ok(())
    }
}
