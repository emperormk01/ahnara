//! Cross-Session Context Injection
//!
//! Generates a condensed context block from persistent memory (reflections,
//! preferences, facts, observations) that gets injected into the system prompt
//! at session start. This gives the agent awareness of prior sessions.

use std::sync::Arc;

use anyhow::Result;

use super::store::{FactRecord, MemoryStore, Observation, UserPreference};
use super::reflector::Reflection;

pub struct ContextIndex {
    store: Arc<MemoryStore>,
}

impl ContextIndex {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }

    /// Generate a context block for injection into the system prompt.
    /// Returns an empty string if there's nothing relevant or context is disabled.
    pub fn generate(&self, user_id: Option<&str>) -> Result<String> {
        let mut sections = Vec::new();

        self.collect_reflections(&mut sections)?;
        self.collect_preferences(user_id, &mut sections)?;
        self.collect_facts(&mut sections)?;
        self.collect_observations(&mut sections)?;

        if sections.is_empty() {
            return Ok(String::new());
        }

        Ok(format!(
            "[CROSS-SESSION MEMORY]\n{}\n[END MEMORY]",
            sections.join("\n\n")
        ))
    }

    fn collect_reflections(&self, sections: &mut Vec<String>) -> Result<()> {
        let reflections = self.store.get_reflections(None, 5)?;
        if !reflections.is_empty() {
            sections.push(Self::format_reflections(&reflections));
        }
        Ok(())
    }

    fn collect_preferences(&self, user_id: Option<&str>, sections: &mut Vec<String>) -> Result<()> {
        let uid = match user_id {
            Some(u) => u,
            None => return Ok(()),
        };
        let prefs = self.store.get_preferences(Some(uid))?;
        let confirmed: Vec<_> = prefs
            .into_iter()
            .filter(|p| p.confidence >= 0.8)
            .collect();
        if !confirmed.is_empty() {
            sections.push(Self::format_preferences(&confirmed));
        }
        Ok(())
    }

    fn collect_facts(&self, sections: &mut Vec<String>) -> Result<()> {
        let facts = self.store.list_facts()?;
        if !facts.is_empty() {
            sections.push(Self::format_facts(&facts));
        }
        Ok(())
    }

    fn collect_observations(&self, sections: &mut Vec<String>) -> Result<()> {
        for obs_type in &["decision", "gotcha", "how_it_works"] {
            let obs = self.store.get_observations_by_type(obs_type, 3)?;
            if !obs.is_empty() {
                sections.push(Self::format_observations(obs_type, &obs));
            }
        }
        Ok(())
    }

    fn format_reflections(reflections: &[Reflection]) -> String {
        let mut out = String::from("## Recent Session Reflections\n");
        for r in reflections {
            out.push_str(&format!(
                "- [{}] {}: {} (Goal: {})\n",
                r.reflection_type, r.title, r.narrative, r.user_goal
            ));
        }
        out
    }

    fn format_preferences(prefs: &[UserPreference]) -> String {
        let mut out = String::from("## Confirmed User Preferences\n");
        for p in prefs {
            out.push_str(&format!(
                "- {} ({}): {} [confidence: {:.0}%]\n",
                p.category,
                p.preference,
                p.source.as_deref().unwrap_or(""),
                p.confidence * 100.0
            ));
        }
        out
    }

    fn format_facts(facts: &[FactRecord]) -> String {
        let mut out = String::from("## Known Facts\n");
        for f in facts {
            out.push_str(&format!("- {}: {}\n", f.key, f.value));
        }
        out
    }

    fn format_observations(obs_type: &str, obs: &[Observation]) -> String {
        let label = obs_type.replace('_', " ").to_uppercase();
        let mut out = format!("## {}s\n", label);
        for o in obs {
            out.push_str(&format!("- {}: {}\n", o.title, o.narrative));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::store::MemoryStore;

    #[test]
    fn test_empty_context() {
        let store = Arc::new(MemoryStore::new_in_memory().unwrap());
        let ctx = ContextIndex::new(store);
        let result = ctx.generate(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_context_with_facts() {
        let store = Arc::new(MemoryStore::new_in_memory().unwrap());
        store.set_fact("project", "ahnara", Some("manual")).unwrap();
        store.set_fact("language", "rust", Some("manual")).unwrap();

        let ctx = ContextIndex::new(store);
        let result = ctx.generate(None).unwrap();
        assert!(result.contains("[CROSS-SESSION MEMORY]"));
        assert!(result.contains("Known Facts"));
        assert!(result.contains("ahnara"));
        assert!(result.contains("[END MEMORY]"));
    }

    #[test]
    fn test_context_with_reflections() {
        use crate::memory::reflector::{Reflection, ReflectionType};

        let store = Arc::new(MemoryStore::new_in_memory().unwrap());
        store.create_session("s1", "telegram", None).unwrap();

        let reflection = Reflection {
            session_id: "s1".into(),
            reflection_type: ReflectionType::Feature,
            title: "Added SQLite support".into(),
            narrative: "Migrated from JSON to SQLite for persistent memory".into(),
            user_goal: "Better performance".into(),
            completed: "yes".into(),
            next_steps: vec!["Add FTS5".into()],
            user_preferences: None,
            approach_that_worked: Some("rusqlite with WAL".into()),
            approach_that_failed: None,
            behavioral_note: None,
            evidence: None,
            message_count: 10,
            created_at: 1000,
        };
        store.insert_reflection(&reflection).unwrap();

        let ctx = ContextIndex::new(store);
        let result = ctx.generate(None).unwrap();
        assert!(result.contains("Recent Session Reflections"));
        assert!(result.contains("Added SQLite support"));
    }
}
