use anyhow::Result;
use clap::Subcommand;
use crate::memory::store::MemoryStore;

#[derive(Subcommand)]
pub enum MemorySubcommand {
    /// List all facts
    Facts,
    /// Recall a specific fact
    Recall {
        /// Fact key
        key: String,
    },
    /// Store a fact
    Remember {
        /// Fact key
        key: String,
        /// Fact value
        value: String,
    },
    /// Full-text search all memory
    Search {
        /// Search query
        query: String,
        /// Max results per category
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// List recent reflections
    Reflections {
        /// Max results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// List recent sessions
    Sessions,
    /// List user preferences
    Preferences,
    /// List observations
    Observations {
        /// Filter by observation type
        #[arg(short, long)]
        r#type: Option<String>,
        /// Max results
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Show memory statistics
    Stats,
    /// Export all memory as JSON
    Export,
}

fn open_store() -> Result<MemoryStore> {
    let config_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".ahnara")
        .join("config.toml");

    let config = crate::config::AppConfig::load(
        config_path.to_str().unwrap_or("~/.ahnara/config.toml"),
    )?;

    let db_path = shellexpand::tilde(&config.memory.database_path).into_owned();
    let db_path = std::path::PathBuf::from(&db_path);

    if !db_path.exists() {
        anyhow::bail!("Memory database not found at {}", db_path.display());
    }
    MemoryStore::new(&db_path)
}

pub fn handle_memory_command(subcmd: &MemorySubcommand) -> Result<()> {
    let store = open_store()?;

    match subcmd {
        MemorySubcommand::Facts => handle_facts(&store),
        MemorySubcommand::Recall { key } => handle_recall(&store, key),
        MemorySubcommand::Remember { key, value } => handle_remember(&store, key, value),
        MemorySubcommand::Search { query, limit } => handle_search(&store, query, *limit),
        MemorySubcommand::Reflections { limit } => handle_reflections(&store, *limit),
        MemorySubcommand::Sessions => handle_sessions(&store),
        MemorySubcommand::Preferences => handle_preferences(&store),
        MemorySubcommand::Observations { r#type, limit } => handle_observations(&store, r#type.as_deref(), *limit),
        MemorySubcommand::Stats => handle_stats(&store),
        MemorySubcommand::Export => handle_export(&store),
    }
}

fn handle_facts(store: &MemoryStore) -> Result<()> {
    let facts = store.list_facts()?;
    if facts.is_empty() {
        println!("No facts stored.");
        return Ok(());
    }
    for f in &facts {
        println!(
            "{}: {} (source: {}, confidence: {:.0}%, v{})",
            f.key,
            f.value,
            f.source.as_deref().unwrap_or("unknown"),
            f.confidence * 100.0,
            f.version,
        );
    }
    Ok(())
}

fn handle_recall(store: &MemoryStore, key: &str) -> Result<()> {
    match store.get_fact(key)? {
        Some(f) => println!("{}: {}", f.key, f.value),
        None => println!("No fact found for key: {}", key),
    }
    Ok(())
}

fn handle_remember(store: &MemoryStore, key: &str, value: &str) -> Result<()> {
    store.set_fact(key, value, Some("cli"))?;
    println!("Remembered: {} = {}", key, value);
    Ok(())
}

fn handle_search(store: &MemoryStore, query: &str, limit: usize) -> Result<()> {
    let results = store.search_all(query, limit)?;
    let total = results.reflections.len()
        + results.observations.len()
        + results.facts.len()
        + results.summaries.len();
    if total == 0 {
        println!("No results for: {}", query);
        return Ok(());
    }

    if !results.reflections.is_empty() {
        println!("== Reflections ({} ={})", results.reflections.len(), "");
        for r in &results.reflections {
            println!("  [{}] {}", r.reflection_type, r.narrative);
        }
    }
    if !results.observations.is_empty() {
        println!("== Observations ({})", results.observations.len());
        for o in &results.observations {
            println!("  [{}] {}: {}", o.obs_type, o.title, o.narrative);
        }
    }
    if !results.facts.is_empty() {
        println!("== Facts ({})", results.facts.len());
        for f in &results.facts {
            println!("  {}: {} [v{}]", f.key, f.value, f.version);
        }
    }
    if !results.summaries.is_empty() {
        println!("== Session Summaries ({})", results.summaries.len());
        for s in &results.summaries {
            println!("  [{}] {}", s.session_id, s.summary);
        }
    }
    println!("\n{} total results", total);
    Ok(())
}

fn handle_reflections(store: &MemoryStore, limit: usize) -> Result<()> {
    let reflections = store.get_reflections(None, limit)?;
    if reflections.is_empty() {
        println!("No reflections found.");
        return Ok(());
    }
    for r in &reflections {
        println!("[{}] {}", r.reflection_type, r.narrative);
        if !r.user_goal.is_empty() {
            println!("  goal: {}", r.user_goal);
        }
    }
    Ok(())
}

fn handle_sessions(store: &MemoryStore) -> Result<()> {
    let count = store.session_count()?;
    println!("{} sessions in memory", count);
    Ok(())
}

fn handle_preferences(store: &MemoryStore) -> Result<()> {
    let prefs = store.get_preferences(None)?;
    if prefs.is_empty() {
        println!("No preferences tracked.");
        return Ok(());
    }
    for p in &prefs {
        println!(
            "{}: {} ({}, confidence: {:.0}%)",
            p.category,
            p.preference,
            p.source.as_deref().unwrap_or("unknown"),
            p.confidence * 100.0
        );
    }
    Ok(())
}

fn handle_observations(store: &MemoryStore, r#type: Option<&str>, limit: usize) -> Result<()> {
    let observations = match r#type {
        Some(t) => store.get_observations_by_type(t, limit)?,
        None => store.get_recent_observations(limit)?,
    };
    if observations.is_empty() {
        println!("No observations found.");
        return Ok(());
    }
    for o in &observations {
        println!("[{}] {}: {}", o.obs_type, o.title, o.narrative);
    }
    Ok(())
}

fn handle_stats(store: &MemoryStore) -> Result<()> {
    let sessions = store.session_count()?;
    let reflections = store.reflection_count()?;
    let facts = store.fact_count()?;
    let observations = store.observation_count()?;
    println!(
        "Sessions:      {}\nReflections:   {}\nFacts:         {}\nObservations:  {}",
        sessions, reflections, facts, observations
    );
    Ok(())
}

fn handle_export(store: &MemoryStore) -> Result<()> {
    let facts = store.list_facts()?;
    let prefs = store.get_preferences(None)?;
    let reflections = store.get_reflections(None, 1000)?;
    let observations = store.get_recent_observations(1000)?;

    let export = serde_json::json!({
        "facts": facts,
        "preferences": prefs,
        "reflections": reflections,
        "observations": observations,
    });
    println!("{}", serde_json::to_string_pretty(&export)?);
    Ok(())
}
