//! SQLite-backed memory store
//!
//! Replaces JSON file persistence with a single SQLite database using WAL mode.
//! Tables: sessions, messages, reflections, facts, user_preferences, observations,
//! compaction_summaries.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{HistoryMessage, SessionHistory};

// ── Schema ──────────────────────────────────────────────────────────────────

const SCHEMA_SQL: &str = "
-- Sessions table
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    channel TEXT NOT NULL DEFAULT 'cli',
    user_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0,
    user_goal TEXT,
    completed TEXT,
    next_steps TEXT
);

-- Messages table
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    tool_calls TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);

-- Reflections table
CREATE TABLE IF NOT EXISTS reflections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    reflection_type TEXT NOT NULL,
    title TEXT NOT NULL,
    narrative TEXT NOT NULL,
    user_goal TEXT,
    completed TEXT,
    next_steps TEXT,
    user_preferences TEXT,
    approach_that_worked TEXT,
    approach_that_failed TEXT,
    behavioral_note TEXT,
    evidence TEXT,
    message_count INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE INDEX IF NOT EXISTS idx_reflections_session ON reflections(session_id);
CREATE INDEX IF NOT EXISTS idx_reflections_type ON reflections(reflection_type);

-- Facts table
CREATE TABLE IF NOT EXISTS facts (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    source TEXT,
    confidence REAL NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- User preferences table
CREATE TABLE IF NOT EXISTS user_preferences (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT,
    category TEXT NOT NULL,
    preference TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0.6,
    source TEXT,
    last_reinforced INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(user_id, category, preference)
);
CREATE INDEX IF NOT EXISTS idx_preferences_user ON user_preferences(user_id);

-- Observations table
CREATE TABLE IF NOT EXISTS observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    obs_type TEXT NOT NULL,
    title TEXT NOT NULL,
    narrative TEXT NOT NULL,
    facts TEXT,
    concepts TEXT,
    files TEXT,
    tool_name TEXT,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE INDEX IF NOT EXISTS idx_observations_type ON observations(obs_type);
CREATE INDEX IF NOT EXISTS idx_observations_session ON observations(session_id);

-- Compaction summaries
CREATE TABLE IF NOT EXISTS compaction_summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    original_messages INTEGER NOT NULL,
    compacted_messages INTEGER NOT NULL,
    tokens_saved INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);

-- Active session routing (channel+user -> current session)
CREATE TABLE IF NOT EXISTS session_routing (
    channel TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (channel, user_id)
);

-- FTS5 virtual tables for search
CREATE VIRTUAL TABLE IF NOT EXISTS reflections_fts USING fts5(
    title, narrative, user_goal, completed,
    content='reflections',
    content_rowid='id'
);

CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts USING fts5(
    title, narrative,
    content='observations',
    content_rowid='id'
);

CREATE VIRTUAL TABLE IF NOT EXISTS summaries_fts USING fts5(
    summary,
    content='compaction_summaries',
    content_rowid='id'
);
";

// Triggers to keep FTS in sync
const FTS_TRIGGERS_SQL: &str = "
CREATE TRIGGER IF NOT EXISTS reflections_ai AFTER INSERT ON reflections BEGIN
    INSERT INTO reflections_fts(rowid, title, narrative, user_goal, completed)
    VALUES (new.id, new.title, new.narrative, new.user_goal, new.completed);
END;

CREATE TRIGGER IF NOT EXISTS reflections_ad AFTER DELETE ON reflections BEGIN
    INSERT INTO reflections_fts(reflections_fts, rowid, title, narrative, user_goal, completed)
    VALUES ('delete', old.id, old.title, old.narrative, old.user_goal, old.completed);
END;

CREATE TRIGGER IF NOT EXISTS reflections_au AFTER UPDATE ON reflections BEGIN
    INSERT INTO reflections_fts(reflections_fts, rowid, title, narrative, user_goal, completed)
    VALUES ('delete', old.id, old.title, old.narrative, old.user_goal, old.completed);
    INSERT INTO reflections_fts(rowid, title, narrative, user_goal, completed)
    VALUES (new.id, new.title, new.narrative, new.user_goal, new.completed);
END;

CREATE TRIGGER IF NOT EXISTS observations_ai AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, title, narrative)
    VALUES (new.id, new.title, new.narrative);
END;

CREATE TRIGGER IF NOT EXISTS observations_ad AFTER DELETE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, title, narrative)
    VALUES ('delete', old.id, old.title, old.narrative);
END;

CREATE TRIGGER IF NOT EXISTS observations_au AFTER UPDATE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, title, narrative)
    VALUES ('delete', old.id, old.title, old.narrative);
    INSERT INTO observations_fts(rowid, title, narrative)
    VALUES (new.id, new.title, new.narrative);
END;

CREATE TRIGGER IF NOT EXISTS summaries_ai AFTER INSERT ON compaction_summaries BEGIN
    INSERT INTO summaries_fts(rowid, summary) VALUES (new.id, new.summary);
END;

CREATE TRIGGER IF NOT EXISTS summaries_ad AFTER DELETE ON compaction_summaries BEGIN
    INSERT INTO summaries_fts(summaries_fts, rowid, summary) VALUES ('delete', old.id, old.summary);
END;
";

// ── Record types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub channel: String,
    pub user_id: Option<String>,
    pub message_count: i64,
    pub updated_at: u64,
    pub created_at: u64,
    pub user_goal: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
}

/// A message record from SQLite (for search results)
#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactRecord {
    pub key: String,
    pub value: String,
    pub source: Option<String>,
    pub confidence: f64,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreference {
    pub id: Option<i64>,
    pub user_id: Option<String>,
    pub category: String,
    pub preference: String,
    pub confidence: f64,
    pub source: Option<String>,
    pub last_reinforced: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: Option<i64>,
    pub session_id: String,
    pub obs_type: String,
    pub title: String,
    pub narrative: String,
    pub facts: Option<String>,
    pub concepts: Option<String>,
    pub files: Option<String>,
    pub tool_name: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResults {
    pub reflections: Vec<super::reflector::Reflection>,
    pub observations: Vec<Observation>,
    pub facts: Vec<FactRecord>,
    pub summaries: Vec<CompactionSummaryRecord>,
}

// ── Store ───────────────────────────────────────────────────────────────────

pub struct MemoryStore {
    conn: Mutex<Connection>,
}

impl MemoryStore {
    pub fn new(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create DB directory: {:?}", parent))?;
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open SQLite DB: {:?}", db_path))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA foreign_keys=ON;
            PRAGMA busy_timeout=5000;
            ",
        )
        .context("Failed to set PRAGMAs")?;

        let store = Self { conn: Mutex::new(conn) };
        store.run_migrations()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing)
    #[cfg(test)]
    pub fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "
            PRAGMA foreign_keys=ON;
            ",
        )?;
        let store = Self { conn: Mutex::new(conn) };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_SQL)
            .context("Failed to run schema migrations")?;
        conn.execute_batch(FTS_TRIGGERS_SQL)
            .context("Failed to create FTS triggers")?;
        Ok(())
    }

    // ── Sessions ─────────────────────────────────────────────────────────

    pub fn create_session(
        &self,
        session_id: &str,
        channel: &str,
        user_id: Option<&str>,
    ) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions (session_id, channel, user_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, channel, user_id, now, now],
        )?;
        Ok(())
    }

    pub fn update_session(&self, session_id: &str, message_count: usize) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1, message_count = ?2 WHERE session_id = ?3",
            params![now, message_count as i64, session_id],
        )?;
        Ok(())
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, channel, user_id, created_at, updated_at, message_count,
                    user_goal, completed, next_steps
             FROM sessions WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![session_id], Self::row_to_session)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<SessionRecord> {
        Ok(SessionRecord {
            session_id: row.get(0)?,
            channel: row.get(1)?,
            user_id: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            message_count: row.get(5)?,
            user_goal: row.get(6)?,
            completed: row.get(7)?,
            next_steps: row.get(8)?,
        })
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, channel, user_id, created_at, updated_at, message_count,
                    user_goal, completed, next_steps
             FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], Self::row_to_session)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn session_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn reflection_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reflections",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Search messages by content keyword, optionally scoped to a session.
    pub fn search_messages(&self, query: &str, session_id: Option<&str>, limit: usize) -> Result<Vec<MessageRecord>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query);
        
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match session_id {
            Some(sid) => (
                "SELECT session_id, role, content, timestamp FROM messages
                 WHERE content LIKE ?1 AND session_id = ?2
                 ORDER BY timestamp DESC LIMIT ?3",
                vec![Box::new(pattern), Box::new(sid.to_string()), Box::new(limit as i64)],
            ),
            None => (
                "SELECT session_id, role, content, timestamp FROM messages
                 WHERE content LIKE ?1
                 ORDER BY timestamp DESC LIMIT ?2",
                vec![Box::new(pattern), Box::new(limit as i64)],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let mut result = Vec::new();
        
        while let Some(row) = rows.next()? {
            result.push(MessageRecord {
                session_id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                timestamp: row.get(3)?,
            });
        }

        Ok(result)
    }

    pub fn fact_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM facts",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn observation_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM observations",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    // ── Messages ─────────────────────────────────────────────────────────

    pub fn insert_message(&self, session_id: &str, msg: &HistoryMessage) -> Result<i64> {
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default());

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp, tool_calls)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, msg.role, msg.content, msg.timestamp, tool_calls_json],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_messages(
        &self,
        session_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<HistoryMessage>> {
        let sql = match limit {
            Some(_) => {
                "SELECT role, content, timestamp, tool_calls FROM messages
                 WHERE session_id = ?1 ORDER BY id ASC LIMIT ?2"
            }
            None => {
                "SELECT role, content, timestamp, tool_calls FROM messages
                 WHERE session_id = ?1 ORDER BY id ASC"
            }
        };

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;

        let rows = if let Some(lim) = limit {
            stmt.query_map(params![session_id, lim as i64], parse_history_message)?
        } else {
            stmt.query_map(params![session_id], parse_history_message)?
        };

        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn get_recent_messages(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<Vec<HistoryMessage>> {
        // We need most recent N but ordered ASC for the consumer
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT role, content, timestamp, tool_calls FROM (
                SELECT role, content, timestamp, tool_calls, id
                FROM messages WHERE session_id = ?1
                ORDER BY id DESC LIMIT ?2
             ) sub ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![session_id, n as i64], parse_history_message)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn message_count(&self, session_id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    // ── Reflections ──────────────────────────────────────────────────────

    const INSERT_REFLECTION_SQL: &str = "INSERT INTO reflections (
                session_id, reflection_type, title, narrative, user_goal,
                completed, next_steps, user_preferences, approach_that_worked,
                approach_that_failed, behavioral_note, evidence,
                message_count, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)";

    pub fn insert_reflection(&self, reflection: &super::reflector::Reflection) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let params = Self::reflection_params(reflection);
        conn.execute(
            Self::INSERT_REFLECTION_SQL,
            rusqlite::params_from_iter(params.iter()),
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn reflection_params(reflection: &super::reflector::Reflection) -> Vec<Box<dyn rusqlite::types::ToSql>> {
        let next_steps_json = serde_json::to_string(&reflection.next_steps).unwrap_or_default();
        vec![
            Box::new(reflection.session_id.clone()),
            Box::new(reflection.reflection_type.to_string()),
            Box::new(reflection.title.clone()),
            Box::new(reflection.narrative.clone()),
            Box::new(reflection.user_goal.clone()),
            Box::new(reflection.completed.clone()),
            Box::new(next_steps_json),
            Box::new(reflection.user_preferences.clone()),
            Box::new(reflection.approach_that_worked.clone()),
            Box::new(reflection.approach_that_failed.clone()),
            Box::new(reflection.behavioral_note.clone()),
            Box::new(reflection.evidence.clone()),
            Box::new(reflection.message_count as i64),
            Box::new(reflection.created_at as i64),
        ]
    }

    pub fn get_reflections(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<super::reflector::Reflection>> {
        let (sql, param_values): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match session_id {
            Some(sid) => (
                "SELECT session_id, reflection_type, title, narrative, user_goal,
                        completed, next_steps, user_preferences, approach_that_worked,
                        approach_that_failed, behavioral_note, evidence,
                        message_count, created_at
                 FROM reflections WHERE session_id = ?1
                 ORDER BY created_at DESC LIMIT ?2",
                vec![
                    Box::new(sid.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(limit as i64),
                ],
            ),
            None => (
                "SELECT session_id, reflection_type, title, narrative, user_goal,
                        completed, next_steps, user_preferences, approach_that_worked,
                        approach_that_failed, behavioral_note, evidence,
                        message_count, created_at
                 FROM reflections ORDER BY created_at DESC LIMIT ?1",
                vec![Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>],
            ),
        };

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
            parse_reflection(row)
        })?;

        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn search_reflections(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<super::reflector::Reflection>> {
        let fts_query = format!("{}*", query.replace('"', "\"\""));
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT r.session_id, r.reflection_type, r.title, r.narrative, r.user_goal,
                    r.completed, r.next_steps, r.user_preferences, r.approach_that_worked,
                    r.approach_that_failed, r.behavioral_note, r.evidence,
                    r.message_count, r.created_at
             FROM reflections r
             INNER JOIN reflections_fts fts ON r.id = fts.rowid
             WHERE reflections_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
            parse_reflection(row)
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    // ── Facts ────────────────────────────────────────────────────────────

    pub fn set_fact(
        &self,
        key: &str,
        value: &str,
        source: Option<&str>,
    ) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO facts (key, value, source, confidence, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1.0, ?4, ?5)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                source = excluded.source,
                updated_at = excluded.updated_at",
            params![key, value, source, now, now],
        )?;
        Ok(())
    }

    pub fn get_fact(&self, key: &str) -> Result<Option<FactRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key, value, source, confidence, created_at, updated_at
             FROM facts WHERE key = ?1",
        )?;
        let mut rows = stmt.query_map(params![key], |row| {
            Ok(FactRecord {
                key: row.get(0)?,
                value: row.get(1)?,
                source: row.get(2)?,
                confidence: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_facts(&self) -> Result<Vec<FactRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key, value, source, confidence, created_at, updated_at
             FROM facts ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FactRecord {
                key: row.get(0)?,
                value: row.get(1)?,
                source: row.get(2)?,
                confidence: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn delete_fact(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn
            .execute("DELETE FROM facts WHERE key = ?1", params![key])?;
        Ok(())
    }

    // ── User Preferences ─────────────────────────────────────────────────

    const UPSERT_PREFERENCE_SQL: &str = "INSERT INTO user_preferences (user_id, category, preference, confidence, source, last_reinforced, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(user_id, category, preference) DO UPDATE SET
                confidence = MAX(user_preferences.confidence, excluded.confidence),
                source = COALESCE(excluded.source, user_preferences.source),
                last_reinforced = excluded.last_reinforced";

    pub fn upsert_preference(&self, pref: &UserPreference) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            Self::UPSERT_PREFERENCE_SQL,
            params![
                pref.user_id,
                pref.category,
                pref.preference,
                pref.confidence,
                pref.source,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn get_preferences(&self, user_id: Option<&str>) -> Result<Vec<UserPreference>> {
        let (sql, param_values): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match user_id {
            Some(uid) => (
                "SELECT id, user_id, category, preference, confidence, source, last_reinforced, created_at
                 FROM user_preferences WHERE user_id = ?1 ORDER BY confidence DESC",
                vec![Box::new(uid.to_string()) as Box<dyn rusqlite::types::ToSql>],
            ),
            None => (
                "SELECT id, user_id, category, preference, confidence, source, last_reinforced, created_at
                 FROM user_preferences ORDER BY confidence DESC",
                vec![],
            ),
        };

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
            Ok(UserPreference {
                id: Some(row.get(0)?),
                user_id: row.get(1)?,
                category: row.get(2)?,
                preference: row.get(3)?,
                confidence: row.get(4)?,
                source: row.get(5)?,
                last_reinforced: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn reinforce_preference(
        &self,
        user_id: &str,
        category: &str,
        preference: &str,
    ) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE user_preferences
             SET confidence = MIN(confidence + 0.1, 1.0),
                 last_reinforced = ?1
             WHERE user_id = ?2 AND category = ?3 AND preference = ?4",
            params![now, user_id, category, preference],
        )?;
        Ok(())
    }

    // ── Observations ─────────────────────────────────────────────────────

    pub fn insert_observation(&self, obs: &Observation) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO observations (session_id, obs_type, title, narrative, facts, concepts, files, tool_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                obs.session_id,
                obs.obs_type,
                obs.title,
                obs.narrative,
                obs.facts,
                obs.concepts,
                obs.files,
                obs.tool_name,
                obs.created_at as i64,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn search_observations(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Observation>> {
        let fts_query = format!("{}*", query.replace('"', "\"\""));
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT o.id, o.session_id, o.obs_type, o.title, o.narrative,
                    o.facts, o.concepts, o.files, o.tool_name, o.created_at
             FROM observations o
             INNER JOIN observations_fts fts ON o.id = fts.rowid
             WHERE observations_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
            parse_observation(row)
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    const SEARCH_FACTS_SQL: &str = "SELECT f.key, f.value, f.source, f.confidence, f.created_at, f.updated_at
             FROM facts f
             WHERE f.key LIKE ?1
             ORDER BY f.updated_at DESC
             LIMIT ?2";

    pub fn search_facts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FactRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(Self::SEARCH_FACTS_SQL)?;
        let rows = stmt.query_map(params![format!("{}%", query), limit as i64], Self::row_to_fact)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    fn row_to_fact(row: &rusqlite::Row) -> rusqlite::Result<FactRecord> {
        Ok(FactRecord {
            key: row.get(0)?,
            value: row.get(1)?,
            source: row.get(2)?,
            confidence: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    const SEARCH_SUMMARIES_SQL: &str = "SELECT cs.session_id, cs.summary, cs.original_messages,
                    cs.compacted_messages, cs.tokens_saved, cs.created_at
             FROM compaction_summaries cs
             INNER JOIN summaries_fts fts ON cs.id = fts.rowid
             WHERE summaries_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2";

    pub fn search_summaries(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CompactionSummaryRecord>> {
        let fts_query = format!("{}*", query.replace('"', "\"\""));
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(Self::SEARCH_SUMMARIES_SQL)?;
        let rows = stmt.query_map(params![fts_query, limit as i64], Self::row_to_summary)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    fn row_to_summary(row: &rusqlite::Row) -> rusqlite::Result<CompactionSummaryRecord> {
        Ok(CompactionSummaryRecord {
            session_id: row.get(0)?,
            summary: row.get(1)?,
            original_messages: row.get(2)?,
            compacted_messages: row.get(3)?,
            tokens_saved: row.get(4)?,
            created_at: row.get(5)?,
        })
    }

    /// Unified full-text search across all memory types
    pub fn search_all(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<MemorySearchResults> {
        let reflections = self.search_reflections(query, limit)?;
        let observations = self.search_observations(query, limit)?;
        let facts = self.search_facts(query, limit)?;
        let summaries = self.search_summaries(query, limit)?;
        Ok(MemorySearchResults {
            reflections,
            observations,
            facts,
            summaries,
        })
    }

    pub fn get_observations_by_type(
        &self,
        obs_type: &str,
        limit: usize,
    ) -> Result<Vec<Observation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, obs_type, title, narrative, facts, concepts, files, tool_name, created_at
             FROM observations WHERE obs_type = ?1
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![obs_type, limit as i64], |row| {
            parse_observation(row)
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn get_recent_observations(&self, limit: usize) -> Result<Vec<Observation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, obs_type, title, narrative, facts, concepts, files, tool_name, created_at
             FROM observations ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            parse_observation(row)
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    // ── Compaction Summaries ─────────────────────────────────────────────

    pub fn insert_compaction_summary(
        &self,
        session_id: &str,
        summary: &str,
        original: usize,
        compacted: usize,
        tokens_saved: usize,
    ) -> Result<i64> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO compaction_summaries (session_id, summary, original_messages, compacted_messages, tokens_saved, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                summary,
                original as i64,
                compacted as i64,
                tokens_saved as i64,
                now as i64,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_compaction_summaries(&self, limit: usize) -> Result<Vec<CompactionSummaryRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, summary, original_messages, compacted_messages, tokens_saved, created_at
             FROM compaction_summaries ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(CompactionSummaryRecord {
                session_id: row.get(0)?,
                summary: row.get(1)?,
                original_messages: row.get(2)?,
                compacted_messages: row.get(3)?,
                tokens_saved: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    // ── Bulk Import (for JSON migration) ─────────────────────────────────

    pub fn import_session(&self, session: &SessionHistory) -> Result<()> {
        self.create_session(&session.session_id, "cli", None)?;
        for msg in &session.messages {
            self.insert_message(&session.session_id, msg)?;
        }
        let count = self.message_count(&session.session_id)?;
        self.update_session(&session.session_id, count)?;
        Ok(())
    }

    // ── Session activity queries (for bootstrapping in-memory state) ────

    pub fn get_all_session_updated_at(&self) -> Result<std::collections::HashMap<String, u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT session_id, updated_at FROM sessions")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (k, v) = r?;
            map.insert(k, v);
        }
        Ok(map)
    }

    pub fn get_latest_reflection_per_session(&self) -> Result<std::collections::HashMap<String, u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, MAX(created_at) FROM reflections GROUP BY session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (k, v) = r?;
            map.insert(k, v);
        }
        Ok(map)
    }

    pub fn delete_messages_for_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])?;
        Ok(())
    }

    // ── Session routing (channel+user -> active session) ─────────────────

    pub fn get_active_session_id(&self, channel: &str, user_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id FROM session_routing WHERE channel = ?1 AND user_id = ?2",
        )?;
        let mut rows = stmt.query_map(params![channel, user_id], |row| {
            Ok(row.get::<_, String>(0)?)
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn set_active_session(&self, channel: &str, user_id: &str, session_id: &str) -> Result<()> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO session_routing (channel, user_id, session_id, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![channel, user_id, session_id, now],
        )?;
        Ok(())
    }

    pub fn clear_active_session(&self, channel: &str, user_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM session_routing WHERE channel = ?1 AND user_id = ?2",
            params![channel, user_id],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSummaryRecord {
    pub session_id: String,
    pub summary: String,
    pub original_messages: i64,
    pub compacted_messages: i64,
    pub tokens_saved: i64,
    pub created_at: i64,
}

// ── Row parsers ─────────────────────────────────────────────────────────────

fn parse_history_message(row: &rusqlite::Row) -> rusqlite::Result<HistoryMessage> {
    let tool_calls_str: Option<String> = row.get(3)?;
    let tool_calls = tool_calls_str.and_then(|s| serde_json::from_str(&s).ok());

    Ok(HistoryMessage {
        role: row.get(0)?,
        content: row.get(1)?,
        timestamp: row.get(2)?,
        tool_calls,
    })
}

fn parse_reflection(
    row: &rusqlite::Row,
) -> rusqlite::Result<super::reflector::Reflection> {
    use super::reflector::{Reflection, ReflectionType};

    let type_str: String = row.get(1)?;
    let reflection_type = parse_reflection_type(&type_str);

    let next_steps_str: Option<String> = row.get(6)?;
    let next_steps: Vec<String> = next_steps_str
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    Ok(Reflection {
        reflection_type,
        title: row.get(2)?,
        narrative: row.get(3)?,
        user_goal: row.get(4)?,
        completed: row.get(5)?,
        next_steps,
        user_preferences: row.get(7)?,
        approach_that_worked: row.get(8)?,
        approach_that_failed: row.get(9)?,
        behavioral_note: row.get(10)?,
        evidence: row.get(11)?,
        session_id: row.get(0)?,
        message_count: row.get::<_, i64>(12)? as usize,
        created_at: row.get::<_, i64>(13)? as u64,
    })
}

fn parse_reflection_type(type_str: &str) -> super::reflector::ReflectionType {
    use super::reflector::ReflectionType;
    match type_str {
        "bugfix" => ReflectionType::Bugfix,
        "feature" => ReflectionType::Feature,
        "research" => ReflectionType::Research,
        "question" => ReflectionType::Question,
        "habit" => ReflectionType::Habit,
        "preference" => ReflectionType::Preference,
        _ => ReflectionType::Other,
    }
}

fn parse_observation(row: &rusqlite::Row) -> rusqlite::Result<Observation> {
    Ok(Observation {
        id: Some(row.get(0)?),
        session_id: row.get(1)?,
        obs_type: row.get(2)?,
        title: row.get(3)?,
        narrative: row.get(4)?,
        facts: row.get(5)?,
        concepts: row.get(6)?,
        files: row.get(7)?,
        tool_name: row.get(8)?,
        created_at: row.get::<_, i64>(9)? as u64,
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> MemoryStore {
        MemoryStore::new_in_memory().unwrap()
    }

    #[test]
    fn test_create_and_get_session() {
        let store = test_store();
        store.create_session("sess1", "telegram", Some("user1")).unwrap();
        let session = store.get_session("sess1").unwrap().unwrap();
        assert_eq!(session.session_id, "sess1");
        assert_eq!(session.channel, "telegram");
        assert_eq!(session.user_id.as_deref(), Some("user1"));
    }

    #[test]
    fn test_insert_and_get_messages() {
        let store = test_store();
        store.create_session("sess1", "cli", None).unwrap();

        let msg = HistoryMessage {
            role: "user".into(),
            content: "hello".into(),
            timestamp: 1000,
            tool_calls: None,
        };
        store.insert_message("sess1", &msg).unwrap();

        let msg2 = HistoryMessage {
            role: "assistant".into(),
            content: "hi there".into(),
            timestamp: 1001,
            tool_calls: None,
        };
        store.insert_message("sess1", &msg2).unwrap();

        let messages = store.get_messages("sess1", None).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn test_recent_messages() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();

        for i in 0..10 {
            store
                .insert_message(
                    "s1",
                    &HistoryMessage {
                        role: "user".into(),
                        content: format!("msg{}", i),
                        timestamp: i,
                        tool_calls: None,
                    },
                )
                .unwrap();
        }

        let recent = store.get_recent_messages("s1", 3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "msg7");
        assert_eq!(recent[2].content, "msg9");
    }

    #[test]
    fn test_facts_crud() {
        let store = test_store();
        store.set_fact("model", "gpt-4o", Some("config")).unwrap();
        store.set_fact("version", "0.8.7", None).unwrap();

        let fact = store.get_fact("model").unwrap().unwrap();
        assert_eq!(fact.value, "gpt-4o");
        assert_eq!(fact.source.as_deref(), Some("config"));

        let facts = store.list_facts().unwrap();
        assert_eq!(facts.len(), 2);

        store.delete_fact("model").unwrap();
        assert!(store.get_fact("model").unwrap().is_none());
    }

    #[test]
    fn test_facts_upsert() {
        let store = test_store();
        store.set_fact("key1", "old", None).unwrap();
        store.set_fact("key1", "new", Some("update")).unwrap();
        let fact = store.get_fact("key1").unwrap().unwrap();
        assert_eq!(fact.value, "new");
    }

    #[test]
    fn test_preferences_crud() {
        let store = test_store();
        let pref = UserPreference {
            id: None,
            user_id: Some("user1".into()),
            category: "tone".into(),
            preference: "concise".into(),
            confidence: 0.6,
            source: Some("observed".into()),
            last_reinforced: 1000,
            created_at: 1000,
        };
        store.upsert_preference(&pref).unwrap();

        let prefs = store.get_preferences(Some("user1")).unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].preference, "concise");

        store.reinforce_preference("user1", "tone", "concise").unwrap();
        let prefs = store.get_preferences(Some("user1")).unwrap();
        assert!(prefs[0].confidence > 0.6);
    }

    #[test]
    fn test_observations_crud() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();

        let obs = Observation {
            id: None,
            session_id: "s1".into(),
            obs_type: "bugfix".into(),
            title: "Fixed null pointer".into(),
            narrative: "The issue was in the parser".into(),
            facts: Some(r#"["parser bug","null check"]"#.into()),
            concepts: Some(r#"["rust","error-handling"]"#.into()),
            files: Some(r#"["src/parser.rs"]"#.into()),
            tool_name: None,
            created_at: 1000,
        };
        store.insert_observation(&obs).unwrap();

        let results = store.get_observations_by_type("bugfix", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fixed null pointer");
    }

    #[test]
    fn test_compaction_summaries() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();
        store
            .insert_compaction_summary("s1", "Test summary", 50, 10, 5000)
            .unwrap();

        let summaries = store.get_compaction_summaries(10).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].summary, "Test summary");
        assert_eq!(summaries[0].tokens_saved, 5000);
    }

    #[test]
    fn test_session_count_and_list() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();
        store.create_session("s2", "telegram", None).unwrap();
        store.create_session("s3", "discord", None).unwrap();

        assert_eq!(store.session_count().unwrap(), 3);

        let sessions = store.list_sessions(2).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_delete_cascades_messages() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();
        store
            .insert_message(
                "s1",
                &HistoryMessage {
                    role: "user".into(),
                    content: "test".into(),
                    timestamp: 1000,
                    tool_calls: None,
                },
            )
            .unwrap();

        store.delete_session("s1").unwrap();
        assert!(store.get_session("s1").unwrap().is_none());
        assert_eq!(store.get_messages("s1", None).unwrap().len(), 0);
    }

    #[test]
    fn test_search_reflections_fts() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();

        use super::super::reflector::{Reflection, ReflectionType};
        let reflection = Reflection {
            reflection_type: ReflectionType::Bugfix,
            title: "Fixed authentication timeout".into(),
            narrative: "The auth token was expiring too quickly".into(),
            user_goal: "Fix login failures".into(),
            completed: "true".into(),
            next_steps: vec![],
            user_preferences: None,
            approach_that_worked: Some("increased token TTL".into()),
            approach_that_failed: None,
            behavioral_note: None,
            evidence: None,
            session_id: "s1".into(),
            message_count: 10,
            created_at: 1000,
        };
        store.insert_reflection(&reflection).unwrap();

        let results = store.search_reflections("authentication", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fixed authentication timeout");
    }

    #[test]
    fn test_search_observations_fts() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();

        let obs = Observation {
            id: None,
            session_id: "s1".into(),
            obs_type: "bugfix".into(),
            title: "Memory leak in worker pool".into(),
            narrative: "Workers were not being cleaned up".into(),
            facts: None,
            concepts: None,
            files: None,
            tool_name: None,
            created_at: 1000,
        };
        store.insert_observation(&obs).unwrap();

        let results = store.search_observations("memory leak", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_import_session() {
        let store = test_store();
        let mut session = SessionHistory::new("test-session");
        session.add_message("user", "hello", None);
        session.add_message("assistant", "hi", None);

        store.import_session(&session).unwrap();

        let record = store.get_session("test-session").unwrap().unwrap();
        assert_eq!(record.message_count, 2);

        let messages = store.get_messages("test-session", None).unwrap();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_search_facts_like() {
        let store = test_store();
        store.set_fact("rust_version", "1.75.0", Some("config")).unwrap();
        store.set_fact("python_version", "3.12", Some("config")).unwrap();

        let results = store.search_facts("rust", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "rust_version");
    }

    #[test]
    fn test_search_summaries_fts() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();
        store.insert_compaction_summary("s1", "Discussed authentication flow with JWT tokens", 50, 10, 5000).unwrap();
        store.insert_compaction_summary("s1", "Set up database migrations", 30, 5, 2000).unwrap();

        let results = store.search_summaries("authentication", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("authentication"));
    }

    #[test]
    fn test_search_all_unified() {
        let store = test_store();
        store.create_session("s1", "cli", None).unwrap();

        use super::super::reflector::{Reflection, ReflectionType};
        let reflection = Reflection {
            reflection_type: ReflectionType::Feature,
            title: "Implemented cache layer".into(),
            narrative: "Added Redis caching for API responses".into(),
            user_goal: "Improve performance".into(),
            completed: "true".into(),
            next_steps: vec![],
            user_preferences: None,
            approach_that_worked: Some("Redis TTL caching".into()),
            approach_that_failed: None,
            behavioral_note: None,
            evidence: None,
            session_id: "s1".into(),
            message_count: 15,
            created_at: 1000,
        };
        store.insert_reflection(&reflection).unwrap();

        let obs = Observation {
            id: None,
            session_id: "s1".into(),
            obs_type: "feature".into(),
            title: "Cache invalidation pattern".into(),
            narrative: "Implemented write-through cache invalidation".into(),
            facts: None,
            concepts: None,
            files: None,
            tool_name: None,
            created_at: 1001,
        };
        store.insert_observation(&obs).unwrap();

        store.set_fact("cache_backend", "redis", Some("config")).unwrap();

        let results = store.search_all("cache", 10).unwrap();
        assert_eq!(results.reflections.len(), 1);
        assert_eq!(results.observations.len(), 1);
        assert_eq!(results.facts.len(), 1);
    }
}
