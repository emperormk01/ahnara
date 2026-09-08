//! Persistent run database for auditability and replay.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct RunDatabase {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub kind: String,
    pub goal: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub metadata: Value,
}

impl RunDatabase {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create run db directory {}", parent.display())
            })?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open run database {}", path.display()))?;
        let db = Self {
            path,
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ahnara")
            .join("runs.db")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                goal TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS run_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                event_type TEXT NOT NULL,
                message TEXT NOT NULL,
                payload_json TEXT NOT NULL DEFAULT '{}',
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS plan_steps (
                run_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                description TEXT NOT NULL,
                tool TEXT,
                status TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                result_json TEXT NOT NULL DEFAULT '{}',
                error TEXT,
                PRIMARY KEY(run_id, step_id),
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_run_events_run_id ON run_events(run_id, id);
            CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at);
            "#,
        )?;
        Ok(())
    }

    pub fn start_run(&self, kind: &str, goal: &str, metadata: Value) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().expect("run db mutex poisoned");
        conn.execute(
            "INSERT INTO runs (id, kind, goal, status, started_at, metadata_json) VALUES (?1, ?2, ?3, 'running', ?4, ?5)",
            params![id, kind, goal, now, metadata.to_string()],
        )?;
        Ok(id)
    }

    pub fn finish_run(&self, run_id: &str, status: &str, metadata: Option<Value>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().expect("run db mutex poisoned");
        if let Some(metadata) = metadata {
            conn.execute(
                "UPDATE runs SET status = ?1, finished_at = ?2, metadata_json = ?3 WHERE id = ?4",
                params![status, now, metadata.to_string(), run_id],
            )?;
        } else {
            conn.execute(
                "UPDATE runs SET status = ?1, finished_at = ?2 WHERE id = ?3",
                params![status, now, run_id],
            )?;
        }
        Ok(())
    }

    pub fn log_event(
        &self,
        run_id: &str,
        event_type: &str,
        message: &str,
        payload: Value,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().expect("run db mutex poisoned");
        conn.execute(
            "INSERT INTO run_events (run_id, timestamp, event_type, message, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, now, event_type, message, payload.to_string()],
        )?;
        Ok(())
    }

    pub fn create_step(
        &self,
        run_id: &str,
        step_id: &str,
        description: &str,
        tool: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO plan_steps (run_id, step_id, description, tool, status) VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![run_id, step_id, description, tool],
        )?;
        Ok(())
    }

    pub fn update_step(
        &self,
        run_id: &str,
        step_id: &str,
        status: &str,
        result: Value,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().expect("run db mutex poisoned");
        if status == "running" {
            conn.execute(
                "UPDATE plan_steps SET status = ?1, started_at = COALESCE(started_at, ?2), result_json = ?3, error = ?4 WHERE run_id = ?5 AND step_id = ?6",
                params![status, now, result.to_string(), error, run_id, step_id],
            )?;
        } else {
            conn.execute(
                "UPDATE plan_steps SET status = ?1, finished_at = ?2, result_json = ?3, error = ?4 WHERE run_id = ?5 AND step_id = ?6",
                params![status, now, result.to_string(), error, run_id, step_id],
            )?;
        }
        Ok(())
    }

    pub fn list_runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, kind, goal, status, started_at, finished_at, metadata_json FROM runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let metadata_json: String = row.get(6)?;
            Ok(RunRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                goal: row.get(2)?,
                status: row.get(3)?,
                started_at: row.get(4)?,
                finished_at: row.get(5)?,
                metadata: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<RunRecord>> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        conn.query_row(
            "SELECT id, kind, goal, status, started_at, finished_at, metadata_json FROM runs WHERE id = ?1",
            params![run_id],
            |row| {
                let metadata_json: String = row.get(6)?;
                Ok(RunRecord {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    goal: row.get(2)?,
                    status: row.get(3)?,
                    started_at: row.get(4)?,
                    finished_at: row.get(5)?,
                    metadata: serde_json::from_str(&metadata_json).unwrap_or(Value::Null),
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_events(&self, run_id: &str) -> Result<Vec<(String, String, String, Value)>> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT timestamp, event_type, message, payload_json FROM run_events WHERE run_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            let payload_json: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                serde_json::from_str(&payload_json).unwrap_or(Value::Null),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<(String, String, String, Option<String>, Option<String>)>> {
        let conn = self.conn.lock().expect("run db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT step_id, status, description, tool, error FROM plan_steps WHERE run_id = ?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_run_lifecycle() {
        let path = std::env::temp_dir().join(format!("ahnara-runs-{}.db", uuid::Uuid::new_v4()));
        let db = RunDatabase::open(&path).unwrap();
        let run_id = db
            .start_run("plan", "test goal", serde_json::json!({"a": 1}))
            .unwrap();
        db.create_step(&run_id, "inspect", "Inspect files", Some("read_file"))
            .unwrap();
        db.update_step(
            &run_id,
            "inspect",
            "success",
            serde_json::json!({"ok": true}),
            None,
        )
        .unwrap();
        db.log_event(&run_id, "note", "hello", serde_json::json!({}))
            .unwrap();
        db.finish_run(&run_id, "success", None).unwrap();

        let runs = db.list_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(db.get_events(&run_id).unwrap().len(), 1);
        assert_eq!(db.get_steps(&run_id).unwrap().len(), 1);
        let _ = std::fs::remove_file(path);
    }
}
