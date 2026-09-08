//! AHNARA - Ultra-High-Performance AI Agent Framework

mod agent;
mod auth;
mod capabilities;
mod channels;
mod checkpoints;
mod cli;
mod commands;
mod config;
mod context;
mod coordination;
mod error_recovery;
mod mcp;
mod memory;
mod orchestrator;
mod persona;
mod planner;
mod plugins;
mod providers;
mod runs;
mod scheduler;
mod skills;
mod tools;

use crate::checkpoints::CheckpointManager;
use std::sync::Arc;
use std::time::Instant;

use crate::auth::{AuthConfig, AuthState};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Commands};

/// Print a high-signal lifecycle message to stderr. Unlike `tracing::info!`,
/// this is NOT suppressed by log level -- it's the user-facing "what's
/// happening" channel. Used for: gateway started, telegram connected,
/// config loaded, ready, etc.
///
/// Goes to stderr so it never interleaves with stdout (e.g. the `chat`
/// command's response goes to stdout; if we logged to stdout we'd corrupt
/// the user-visible output).
#[macro_export]
macro_rules! bann {
    ($($arg:tt)*) => {{
        eprintln!($($arg)*);
    }}
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse_args();

    // Initialize logging with file appender (writes to both stderr and ~/.ahnara/logs/)
    // Default: only warnings+ reach stderr. info+ still goes to the rotating log file
    // under ~/.ahnara/logs/. The `--debug` flag raises the global level for both
    // destinations. `RUST_LOG` always wins if set, which is the standard escape hatch
    // for operators and for `ahnara logs` follow mode.
    let level = if args.debug {
        "debug".to_string()
    } else {
        std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string())
    };
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".ahnara/logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/ahnara/logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "gateway.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    use tracing_subscriber::prelude::*;
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    let file_layer = tracing_subscriber::fmt::layer().with_writer(non_blocking).with_ansi(false);
    tracing_subscriber::registry()
        .with(EnvFilter::new(level))
        .with(stderr_layer)
        .with(file_layer)
        .init();

    // Handle commands
    match args.command {
        Commands::Gateway { port, host } => {
            run_gateway(&host, port).await?;
        }

        Commands::Chat {
            message,
            model,
            stream,
        } => {
            commands::chat::handle_chat(message, model, stream).await?;
        }

        Commands::Setup {
            quick,
            telegram,
            discord,
            provider,
            model,
            api_key,
            telegram_token,
            discord_token,
            github_token,
        } => {
            let opts = commands::setup::NonInteractiveOptions {
                provider,
                model,
                api_key,
                telegram_token,
                discord_token,
                github_token,
            };
            commands::setup::handle_setup_with(quick, telegram, discord, opts)?;
        }

        Commands::Config { action } => {
            commands::config::handle_config(action)?;
        }

        Commands::Skill { action } => {
            commands::skill::handle_skill(action).await?;
        }

        Commands::Provider { action } => {
            commands::provider::handle_provider(action).await?;
        }

        Commands::Persona { action } => {
            commands::persona::handle_persona(action)?;
        }

        Commands::Status { delegation } => {
            commands::status::handle_status(delegation)?;
        }

        Commands::Code { task, project, session } => {
            commands::code::handle_code(task, project, session).await?;
        }

        Commands::Run {
            skill,
            args: run_args,
        } => {
            commands::run::handle_run(skill, run_args).await?;
        }

        Commands::Plan { goal, output } => {
            commands::plan::handle_plan(goal, output).await?;
        }

        Commands::RunPlan { path, db } => {
            commands::plan::handle_run_plan(path, db).await?;
        }

        Commands::Runs { action, db } => {
            commands::runs::handle_runs(action, db).await?;
        }

        Commands::Capabilities { json } => {
            let _ = commands::capabilities::handle_capabilities(json).await;
        }

        Commands::Update => {
            let result = commands::update::handle_update().await;
            println!("{}", result);
        }

        Commands::Mcp { args } => {
            let args_str = args.join(" ");
            match crate::commands::mcp::handle_mcp(&args_str, None).await {
                Ok(result) => println!("{}", result),
                Err(e) => eprintln!("Error: {}", e),
            }
        }

        Commands::Token { args } => {
            let arg_str = args.join(" ");
            match crate::commands::token::handle_token(&arg_str) {
                Ok(resp) => println!("{}", resp),
                Err(e) => eprintln!("Error: {}", e),
            }
            return Ok(());
        }

        Commands::Stop => {
            commands::stop::handle_stop()?;
        }

        Commands::Memory { action } => {
            cli::memory::handle_memory_command(&action)?;
        }

        Commands::Model { model_id, base, key, reset, show } => {
            let session_db = dirs::home_dir()
                .map(|h| h.join(".ahnara/sessions"))
                .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
            let session_db_parent = session_db.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara"));
            let model_store = memory::model_store::ModelStore::new(&session_db_parent)?;
            let user_id = "cli";
            let channel = "cli";
            if reset {
                if model_store.delete(channel, user_id)? {
                    println!("Model override cleared.");
                } else {
                    println!("No model override was set.");
                }
            } else if show || (model_id.is_none() && base.is_none() && key.is_none()) {
                let resp = commands::model::handle_model(&model_store, channel, user_id, "")?;
                println!("{}", resp);
            } else {
                let mut args = Vec::new();
                if let Some(ref b) = base {
                    args.push("url".to_string());
                    args.push(b.clone());
                }
                if let Some(ref k) = key {
                    args.push("key".to_string());
                    args.push(k.clone());
                }
                if let Some(ref m) = model_id {
                    args.push("id".to_string());
                    args.push(m.clone());
                }
                let resp = commands::model::handle_model(&model_store, channel, user_id, &args.join(" "))?;
                println!("{}", resp);
            }
        }

        Commands::Logs { lines, filter, errors, clear } => {
            let mut args_parts = Vec::new();
            if clear {
                args_parts.push("--clear".to_string());
            } else {
                args_parts.push(lines.to_string());
                if errors {
                    args_parts.push("--errors".to_string());
                }
                if let Some(f) = filter {
                    args_parts.push("--filter".to_string());
                    args_parts.push(f);
                }
            }
            let result = commands::logs::handle_logs(&args_parts.join(" ")).await;
            println!("{}", result);
        }

        Commands::Schedule { args } => {
            if args.is_empty() || args[0] == "list" {
                let config_path = dirs::home_dir()
                    .map(|h| h.join(".ahnara/config.toml"))
                    .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara/config.toml"));
                let config = config::AppConfig::load(&config_path.to_string_lossy())?;
                if config.scheduler.jobs.is_empty() {
                    println!("No scheduled jobs configured.");
                } else {
                    println!("Scheduled Jobs\n");
                    for job in &config.scheduler.jobs {
                        println!("  {} ({})\n    Cron: {}\n    Prompt: {}\n    Timeout: {}s\n",
                            job.name,
                            if job.enabled { "enabled" } else { "disabled" },
                            job.cron,
                            job.prompt.chars().take(80).collect::<String>(),
                            job.timeout_secs,
                        );
                    }
                }
            } else {
                let config_path = dirs::home_dir()
                    .map(|h| h.join(".ahnara/config.toml"))
                    .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara/config.toml"));
                let run_log = scheduler::create_run_log(&[]);
                let manager = tools::SchedulerManager::new(
                    run_log,
                    config_path.to_string_lossy().to_string(),
                );
                let result = commands::schedule::handle_schedule(&args.join(" "), &manager).await;
                println!("{}", result);
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct AppState {
    agent: Arc<agent::AgentCore>,
}

async fn run_gateway(host: &str, port: u16) -> anyhow::Result<()> {
    let auth_config = AuthConfig {
        api_key: std::env::var("AHNARA_API_KEY").ok(),
        require_auth: std::env::var("AHNARA_REQUIRE_AUTH")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false),
    };
    let auth_state = Arc::new(AuthState::new(auth_config));

    bann!("\x1b[36m🦞 AHNARA v{}\x1b[0m initializing...", env!("CARGO_PKG_VERSION"));

    let start = Instant::now();

    // Load config
    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;
    let config =
        config::AppConfig::load(config_path.to_str().unwrap_or("~/.ahnara/config.toml"))?;

    // Expand tilde in database path
    let session_db = shellexpand::tilde(&config.memory.database_path).into_owned();
    let session_db_parent = std::path::Path::new(&session_db).parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara"));

    // Initialize core components
    let memory = Arc::new(memory::MemoryEngine::new(&config.memory)?);
    let providers = Arc::new(providers::ProviderPool::new(config.providers.clone()));

    if config.providers.providers.is_empty() {
        bann!("\x1b[33m⚠  No AI provider configured.\x1b[0m");
        bann!("   Run `ahnara setup` or add a provider to ~/.ahnara/config.toml");
        bann!("   Example:");
        bann!("     [providers]");
        bann!("     active = \"openai\"");
        bann!("     [[providers.providers]]");
        bann!("     name = \"openai\"");
        bann!("     api_key = \"sk-...\"");
        bann!("     api_base = \"https://api.openai.com/v1\"");
        bann!("");
    }

    // Initialize SQLite memory store (must come before orchestrator for session tools)
    let db_path = std::path::Path::new(&session_db);
    let memory_store = match memory::MemoryStore::new(db_path) {
        Ok(store) => {
            info!("SQLite memory store initialized at {}", session_db);
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("Failed to init SQLite memory store: {}", e);
            None
        }
    };

    let mut raw_orchestrator = orchestrator::ToolOrchestrator::new();
    raw_orchestrator.register_code_tools();
    raw_orchestrator.register_vision_tools();
    // Register session history tools if memory store is available
    if let Some(ref ms) = memory_store {
        raw_orchestrator.register_session_tools(ms.clone());
        raw_orchestrator.register_transcribe_tool();
    }
    let mut raw_plugins = plugins::PluginManager::new(config.plugins.clone());
    raw_plugins.set_tools(raw_orchestrator.list_tools());
    let plugins = Arc::new(raw_plugins);
    raw_orchestrator.set_plugins(plugins.clone());

    // Create message router for cross-platform proactive messaging
    let mut message_router = tools::MessageRouter::new();
    if config.channels.telegram.enabled {
        message_router.set_default_platform("telegram".to_string());
    }
    raw_orchestrator.register_send_message_tool(message_router.clone());

    // Create shared context state for sub-agent tool (updated per-request by agent)
    let subagent_context: Arc<parking_lot::RwLock<(Option<String>, Option<String>)>> =
        Arc::new(parking_lot::RwLock::new((None, None)));

    // Create sub-agent coordinator placeholder (populated after agent init)
    let coordinator: Arc<tokio::sync::RwLock<Option<Arc<coordination::AgentCoordinator>>>> =
        Arc::new(tokio::sync::RwLock::new(None));

    // Initialize model store early so sub-agent tool can read user overrides
    let model_store = Arc::new(memory::model_store::ModelStore::new(&session_db_parent)?);

    raw_orchestrator.register_subagent_tool(coordinator.clone(), model_store.clone(), subagent_context.clone());

    // Schedule run log -- shared between orchestrator (tool) and agent (system prompt)
    let schedule_log = scheduler::create_run_log(&config.scheduler.jobs);
    raw_orchestrator.register_schedule_tool(schedule_log.clone());

    // Scheduler manager for runtime CRUD on scheduled jobs
    let config_path_str = config_path.to_string_lossy().to_string();
    let mut scheduler_manager = tools::SchedulerManager::new(schedule_log.clone(), config_path_str.clone());
    // CronHandle placeholder -- the live scheduler gets stored here after agent creation
    let cron_handle: tools::CronHandle = Arc::new(tokio::sync::Mutex::new(None));

    // Shared blackboard for multi-agent coordination
    let blackboard = coordination::SharedBlackboard::new();

    let orchestrator = Arc::new(raw_orchestrator);
    if config.mcp.enabled {
        let count = orchestrator.register_mcp_tools(&config.mcp).await?;
        info!("{} Registered {} MCP tools", "", count);
    }

    // Initialize persistent session store backed by SQLite
    let session_store = match &memory_store {
        Some(ms) => Arc::new(memory::SessionStore::new_from_store(ms.clone())?),
        None => Arc::new(memory::SessionStore::new(&session_db)?),
    };
    let code_mode = Arc::new(memory::CodeModeStore::new(&session_db)?);
    let checkpoint_manager = Arc::new(CheckpointManager::new(&session_db)?);

    plugins.run_lifecycle(plugins::HookEvent::Startup).await;

    let agent = Arc::new(agent::AgentCore::new(
        memory,
        providers.clone(),
        orchestrator.clone(),
        config.clone(),
        session_store.clone(),
        code_mode.clone(),
        model_store.clone(),
        plugins.clone(),
        checkpoint_manager.clone(),
        subagent_context.clone(),
        Some(schedule_log.clone()),
        memory_store.clone(),
    )?);

    // Run JSON-to-SQLite migration if store is available
    if let Some(ref ms) = memory_store {
        let sessions_dir = std::path::Path::new(&session_db).parent()
            .map(|p| p.join("sessions"))
            .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara/sessions"));
        if sessions_dir.exists() {
            match agent.migrate_json_sessions_to_sqlite(ms, &sessions_dir) {
                Ok(count) => {
                    if count > 0 {
                        info!("Migrated {} sessions from JSON to SQLite", count);
                    }
                }
                Err(e) => {
                    tracing::warn!("JSON session migration failed (non-fatal): {}", e);
                }
            }
        }
    }

    // Load persisted sessions (also bootstraps last_activity from SQLite updated_at)
    agent.load_sessions().await?;
    
    // Now initialize the sub-agent coordinator with all dependencies
    let coordinator_instance = Arc::new(coordination::AgentCoordinator::new(
        agent.clone(),
        providers.clone(),
        orchestrator.clone(),
        session_store.clone(),
        config.clone(),
    ));
    // Load persisted cost-aware delegator state (budget + delegation history)
    // and inject it into the coordinator. This wires `record_usage`, `budget_status`,
    // `set_sub_agents_enabled`, `set_min_complexity`, `set_max_budget`, and `stats`
    // to actually persist across restarts instead of being in-memory only.
    let delegation_state_path = session_db_parent.join("delegation_state.json");
    let loaded_delegator =
        coordination::cost_aware_delegation::CostAwareDelegator::load_or_default(&delegation_state_path);
    {
        let stats = loaded_delegator.stats();
        tracing::info!(
            "Loaded cost-aware delegator: {} decisions in history, {} tokens used",
            stats.total_analyzed,
            stats.budget_used
        );
    }
    coordinator_instance.set_cost_aware_delegator(loaded_delegator);
    {
        let mut coord_guard = coordinator.write().await;
        *coord_guard = Some(coordinator_instance.clone());
    }
    // Persist delegator state on Ctrl-C / SIGTERM so the budget survives restarts.
    {
        let coordinator_cell = coordinator.clone();
        let path_for_save = delegation_state_path.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            if let Some(coord) = coordinator_cell.read().await.clone() {
                let snapshot = coord.cost_aware_delegator_snapshot().await;
                if let Err(e) = snapshot.save(&path_for_save) {
                    tracing::warn!("Failed to persist cost-aware delegator on shutdown: {}", e);
                } else {
                    tracing::info!("Persisted cost-aware delegator state to {}", path_for_save.display());
                }
            }
        });
    }
    tracing::info!("Sub-agent coordinator initialized");

    // Register blackboard tools (requires coordinator to be initialized)
    orchestrator.register_blackboard_tools(blackboard.clone(), coordinator.clone());

    let cron_scheduler =
        scheduler::CronScheduler::start(agent.clone(), config.scheduler.clone(), schedule_log.clone()).await?;
    if let Some(cs) = cron_scheduler {
        *cron_handle.lock().await = Some(cs);
    }
    scheduler_manager.set_live_scheduler(agent.clone(), cron_handle.clone());
    orchestrator.register_schedule_management_tools(scheduler_manager);

    bann!("\x1b[32m⚡ Core initialized in {:?}\x1b[0m", start.elapsed());

    // Zombie child reaper - reap orphaned child processes every 30s
    #[cfg(unix)]
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            unsafe {
                while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
            }
        }
    });

    // Spawn reflection monitor background task
    let monitor_agent = agent.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;

            // Check for sessions needing reflection
            let sessions = monitor_agent.get_sessions_needing_reflection().await;

            for session_key in sessions {
                tracing::info!(
                    "Auto-reflection triggered for inactive session: {}",
                    session_key
                );
                if let Some(reflection) = monitor_agent.run_reflection(&session_key).await {
                    tracing::info!(
                        "Auto-reflection complete: {} - {}",
                        reflection.reflection_type.to_string().to_lowercase(),
                        reflection.title
                    );
                }
            }
        }
    });

    // Start Telegram channel if enabled
    let model_store_discord = model_store.clone();
    let code_mode_discord = code_mode.clone();
    let _discord_handle = if config.channels.discord.enabled {
        let discord_agent = agent.clone();
        let discord_config = config.channels.discord.clone();
        bann!("💬 Starting Discord gateway...");

        // Create Discord adapter for mid-task message delivery
        let discord_http = Arc::new(serenity::http::Http::new(&discord_config.token));
        let discord_adapter = Arc::new(tools::DiscordAdapter::new(discord_http));
        message_router.register(discord_adapter.clone() as Arc<dyn tools::PlatformAdapter>).await;

        Some(tokio::spawn(async move {
            if let Err(e) = channels::discord::start(discord_agent, model_store_discord, code_mode_discord, Some(discord_config), Some(discord_adapter)).await {
                tracing::error!("Discord error: {}", e);
            }
        }))
    } else {
        None
    };

    let _tg_handle = if config.channels.telegram.enabled {
        let tg_agent = agent.clone();
        let tg_config = config.channels.telegram.clone();
        let tg_persona = config.persona.clone();
        let tg_router = Arc::new(message_router);
        bann!("📱 Starting Telegram gateway...");
        Some(tokio::spawn(async move {
            if let Err(e) = channels::telegram::start(tg_agent, model_store.clone(), code_mode.clone(), Some(tg_config), tg_persona, Some(tg_router)).await {
                tracing::error!("Telegram error: {}", e);
            }
        }))
    } else {
        None
    };

    let state = AppState {
        agent: agent.clone(),
    };

    let auth_middleware_state = auth_state.clone();
    let auth_middleware = axum::middleware::from_fn(move |req: Request, next: Next| {
        let auth_state = auth_middleware_state.clone();
        async move {
            if let Err(e) = auth_state.verify_bearer_token(
                req.headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok()),
            ) {
                return Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"error": "Unauthorized", "message": e.to_string()})
                            .to_string(),
                    ))
                    .unwrap();
            }
            next.run(req).await
        }
    });

    // Build HTTP router
    let app = Router::new()
        .route("/health", axum::routing::get(|| async { "OK" }))
        .route("/", axum::routing::get(dashboard_handler))
        .route("/chat", axum::routing::post(chat_handler))
        .route("/api/chat", axum::routing::post(chat_handler))
        .route("/api/status", axum::routing::get(status_handler))
        .route(
            "/api/capabilities",
            axum::routing::get(capabilities_handler),
        )
        .route("/api/skills", axum::routing::get(list_skills_handler))
        .route("/api/reflect", axum::routing::post(reflect_handler))
        .route(
            "/api/reflections",
            axum::routing::get(list_reflections_handler),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(auth_middleware)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    bann!("🌐 HTTP server listening on http://{}", addr);
    bann!("\x1b[1;32m✅ Ready in {:?}\x1b[0m -- API at http://{}/", start.elapsed(), addr);

    axum::serve(listener, app).await?;

    Ok(())
}

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    response: String,
}

async fn chat_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<ChatRequest>,
) -> axum::Json<ChatResponse> {
    let agent = state.agent;
    let response = agent.process(&req.message, None).await;
    axum::Json(ChatResponse { response })
}

async fn status_handler(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "model": state.agent.model_name().to_string(),
        "uptime": "running"
    }))
}

async fn capabilities_handler(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    axum::Json(state.agent.capability_manifest().as_json())
}

async fn list_skills_handler() -> axum::Json<Vec<String>> {
    let skills_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ahnara")
        .join("skills");
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let skill_file = entry.path().join("SKILL.md");
            if skill_file.is_file() {
                if let Ok(content) = std::fs::read_to_string(&skill_file) {
                    if let Ok(skill) = crate::skills::Skill::parse(&content) {
                        names.push(skill.name().to_string());
                        continue;
                    }
                }
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names.sort();
    axum::Json(names)
}

#[derive(Deserialize)]
struct ReflectRequest {
    session_id: Option<String>,
}

async fn reflect_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<ReflectRequest>,
) -> axum::Json<serde_json::Value> {
    let agent = state.agent;
    let session_key = req.session_id.unwrap_or_else(|| "default".to_string());

    match agent.run_reflection(&session_key).await {
        Some(reflection) => axum::Json(
            serde_json::to_value(&reflection)
                .unwrap_or_else(|_| serde_json::json!({"error": "Failed to serialize reflection"})),
        ),
        None => axum::Json(serde_json::json!({
            "error": "Reflection not triggered (check min_messages or cooldown)"
        })),
    }
}

async fn list_reflections_handler(
    State(state): State<AppState>,
) -> axum::Json<Vec<serde_json::Value>> {
    let agent = state.agent;
    match agent.get_all_reflections() {
        Some(reflections) => axum::Json(
            reflections
                .iter()
                .map(|r| serde_json::to_value(r).unwrap_or_default())
                .collect(),
        ),
        None => axum::Json(vec![]),
    }
}

async fn dashboard_handler() -> impl axum::response::IntoResponse {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>AHNARA Gateway</title>
<style>
:root { color-scheme: dark; }
body { font-family: system-ui, sans-serif; background: #0f1117; color: #e1e4e8; margin: 0; padding: 2rem; }
.container { max-width: 700px; margin: 0 auto; }
h1 { color: #58a6ff; font-size: 1.8rem; margin-bottom: 0.2rem; }
.tagline { color: #8b949e; margin-bottom: 2rem; }
.card { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 1.5rem; margin-bottom: 1.5rem; }
.card h2 { margin-top: 0; font-size: 1.25rem; color: #c9d1d9; }
.card p { color: #8b949e; line-height: 1.6; }
.status-dot { display: inline-block; width: 10px; height: 10px; border-radius: 50%; margin-right: 8px; }
.status-ok { background: #3fb950; }
.status-off { background: #f85149; }
.status-warn { background: #d29922; }
table { width: 100%; border-collapse: collapse; }
td { padding: 0.5rem 0; border-bottom: 1px solid #21262d; }
td:first-child { color: #8b949e; }
td:last-child { text-align: right; color: #c9d1d9; font-weight: 500; }
a, a:visited { color: #58a6ff; }
code { background: #21262d; padding: 2px 8px; border-radius: 4px; font-size: 0.9em; }
.endpoint { color: #7ee787; font-family: monospace; font-size: 0.9em; }
pre { background: #161b22; border: 1px solid #30363d; border-radius: 6px; padding: 1rem; overflow-x: auto; font-size: 0.85rem; }
pre code { background: none; padding: 0; }
.footer { text-align: center; margin-top: 4rem; color: #484f58; font-size: 0.8rem; }
</style>
</head>
<body>
<div class="container">
<h1>🦞 AHNARA Gateway</h1>
<p class="tagline">AI Agent Framework — now running and ready for requests.</p>

<div class="card">
<h2>🔗 API Endpoints</h2>
<table>
<tr><td>Health</td><td><span class="endpoint">GET /health</span></td></tr>
<tr><td>Status</td><td><span class="endpoint">GET /api/status</span></td></tr>
<tr><td>Capabilities</td><td><span class="endpoint">GET /api/capabilities</span></td></tr>
<tr><td>Skills</td><td><span class="endpoint">GET /api/skills</span></td></tr>
<tr><td>Chat</td><td><span class="endpoint">POST /api/chat</span></td></tr>
<tr><td>Reflect</td><td><span class="endpoint">POST /api/reflect</span></td></tr>
</table>
</div>

<div class="card">
<h2>📱 Channels</h2>
<p>Connect via <strong>Telegram</strong> or <strong>Discord</strong> to interact with the agent. Configured in <code>~/.ahnara/config.toml</code></p>
</div>

<div class="card">
<h2>💬 Quick Chat</h2>
<textarea id="chat-input" placeholder="Type a message..." style="width:100%;height:80px;background:#0d1117;color:#e1e4e8;border:1px solid #30363d;border-radius:6px;padding:10px;font-size:1rem;resize:vertical;"></textarea>
<button onclick="sendMessage()" style="margin-top:10px;background:#238636;color:#fff;border:none;border-radius:6px;padding:10px 20px;cursor:pointer;font-size:1rem;">Send</button>
<pre id="response" style="margin-top:10px;min-height:40px;"><code>Response will appear here...</code></pre>
</div>

<div class="footer">
<p>AHNARA &copy; 2025–2026 &middot; AI Agent Framework</p>
</div>
</div>
<script>
async function sendMessage() {
  const input = document.getElementById('chat-input');
  const response = document.getElementById('response');
  const msg = input.value.trim();
  if (!msg) return;
  response.textContent = 'Thinking...';
  try {
    const res = await fetch('/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message: msg })
    });
    const data = await res.json();
    response.textContent = data.response || JSON.stringify(data, null, 2);
  } catch (err) {
    response.textContent = 'Error: ' + err.message;
  }
}
</script>
</body>
</html>"#;

    axum::response::Html(html)
}