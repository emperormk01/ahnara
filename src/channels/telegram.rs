//! Telegram channel adapter with full command support.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use teloxide::{
    dispatching::Dispatcher,
    net::Download,
    prelude::*,
    types::{ChatAction, ChatId, Update},
    utils::command::BotCommands,
    Bot,
};

use crate::agent::AgentCore;
use crate::channels::markdown::markdown_to_telegram;
use crate::config::TelegramConfig;
use crate::persona::shared::{
    load_current_persona, reset_persona, set_behavior, set_length, set_name, set_no_em_dashes,
    set_no_emojis, set_tone, toggle_no_em_dashes, toggle_no_emojis,
};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum Command {
    #[command(description = "View agent memory")]
    Memory,
    #[command(description = "Clear conversation history")]
    Clear,
    #[command(description = "List available tools")]
    Tools,
    #[command(description = "View token usage statistics")]
    Usage,
    #[command(description = "Recover from crashed session")]
    Recover,
    #[command(description = "Show help message")]
    Help,
    #[command(description = "Check bot status")]
    Status,
    #[command(description = "Toggle voice mode or set voice")]
    Voice,
    #[command(description = "Manage agent persona")]
    Persona,
    #[command(description = "Update ahnara to the latest version")]
    Update,
    #[command(description = "Start a coding session in isolated workspace")]
    Code,
    #[command(description = "Start a new session")]
    New,
    #[command(description = "Exit coding mode")]
    Normal,
    #[command(description = "View gateway logs")]
    Logs(String),
    #[command(description = "Manage scheduled jobs")]
    Schedule(String),
    #[command(description = "Override model/provider settings")]
    Model(String),
    #[command(description = "Manage MCP server integrations")]
    Mcp(String),
    #[command(description = "Manage secure API tokens")]
    Token(String),
}



/// Spawn a background task that sends the typing indicator every 4 seconds
/// until the returned guard is dropped (i.e. processing completes).
fn spawn_typing_loop(bot: &Bot, chat_id: i64) -> tokio::sync::oneshot::Sender<()> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let bot = bot.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));
        tokio::pin!(let cancel = rx;);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = bot.send_chat_action(ChatId(chat_id), ChatAction::Typing).await;
                }
                _ = &mut cancel => {
                    break;
                }
            }
        }
    });
    tx
}

#[derive(Debug, Clone)]
struct SessionState {
    message_count: u64,
    total_tokens: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    voice_mode: bool,
    voice_id: Option<String>,
    last_activity: chrono::DateTime<chrono::Utc>,
}

impl Default for SessionState {
    fn default() -> Self {
        let now = chrono::Utc::now();
        Self {
            message_count: 0,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            created_at: now,
            voice_mode: false,
            voice_id: None,
            last_activity: now,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ModelFlowState {
    /// User selected custom subtype, waiting for endpoint URL + model ID
    WaitingForEndpoint { subtype: String },
    /// User sent endpoint, waiting for API key
    WaitingForKey,
}

pub struct TelegramState {
    agent: Arc<AgentCore>,
    model_store: Arc<crate::memory::model_store::ModelStore>,
    sessions: RwLock<HashMap<i64, SessionState>>,
    code_mode: Arc<crate::memory::CodeModeStore>,
    config: TelegramConfig,
    /// Per-user flow state for multi-step model setup (ephemeral, lost on restart)
    pending_model_flows: RwLock<HashMap<i64, ModelFlowState>>,
    /// Adapter for mid-task message delivery
    message_adapter: Option<Arc<crate::tools::TelegramAdapter>>,
    /// Shared scheduler run log for /schedule command
    schedule_log: crate::scheduler::ScheduleRunLog,
}

impl TelegramState {
    pub fn new(
        agent: Arc<AgentCore>,
        model_store: Arc<crate::memory::model_store::ModelStore>,
        code_mode: Arc<crate::memory::CodeModeStore>,
        config: TelegramConfig,
        message_adapter: Option<Arc<crate::tools::TelegramAdapter>>,
        schedule_log: crate::scheduler::ScheduleRunLog,
    ) -> Self {
        Self {
            agent,
            model_store,
            sessions: RwLock::new(HashMap::new()),
            code_mode,
            config,
            pending_model_flows: RwLock::new(HashMap::new()),
            message_adapter,
            schedule_log,
        }
    }

    async fn get_or_create_session(&self, chat_id: i64) -> SessionState {
        let mut sessions = self.sessions.write().await;
        sessions
            .entry(chat_id)
            .or_insert_with(|| SessionState {
                created_at: chrono::Utc::now(),
                last_activity: chrono::Utc::now(),
                ..Default::default()
            })
            .clone()
    }

    async fn update_session(&self, chat_id: i64, tokens: Option<(u32, u32)>) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(&chat_id) {
            session.message_count += 1;
            session.last_activity = chrono::Utc::now();
            if let Some((prompt, completion)) = tokens {
                session.prompt_tokens += prompt as u64;
                session.completion_tokens += completion as u64;
                session.total_tokens += (prompt + completion) as u64;
            }
        }
    }

    async fn is_coding(&self, chat_id: i64) -> bool {
        let user_id = format!("{}", chat_id);
        self.agent.has_active_session("telegram-code", &user_id)
    }

    async fn enter_code_mode(&self, _chat_id: i64, _workspace: String) {
        // Code mode activation is handled by agent.set_system_prompt_override which persists to CodeModeStore
    }

    async fn exit_code_mode(&self, _chat_id: i64) {
        // Code mode deactivation is handled by agent.clear_system_prompt_override which removes from CodeModeStore
    }

    /// Reset per-user session stats (token counts, message counts) in the
    /// channel-side `SessionState`. Called by `/clear` and `/new` so a fresh
    /// session starts with zero usage.
    pub async fn clear_session(&self, chat_id: i64) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(
            chat_id,
            SessionState {
                created_at: chrono::Utc::now(),
                last_activity: chrono::Utc::now(),
                ..Default::default()
            },
        );
    }
}

pub async fn start(
    agent: Arc<AgentCore>,
    model_store: Arc<crate::memory::model_store::ModelStore>,
    code_mode: Arc<crate::memory::CodeModeStore>,
    config: Option<TelegramConfig>,
    _persona: crate::persona::PersonaConfig,
    message_router: Option<Arc<crate::tools::MessageRouter>>,
) -> Result<()> {
    let config = config.ok_or_else(|| anyhow!("Telegram config required"))?;
    if !config.enabled || config.token.is_empty() {
        tracing::info!("Telegram gateway disabled");
        return Ok(());
    }

    let bot = Bot::new(config.token.clone());

    // Verify that the bot token is valid and the bot is reachable
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bot.get_me(),
    ).await {
        Ok(Ok(me)) => tracing::info!("✅ Telegram bot verified: @{} (ID: {})", me.user.username.as_deref().unwrap_or("unknown"), me.user.id.0),
        Ok(Err(e)) => tracing::warn!("⚠️  Telegram bot verification failed (wrong token?): {e}"),
        Err(_) => tracing::warn!("⚠️  Telegram bot verification timed out (check network or token)"),
    }

    // Create Telegram adapter for mid-task message delivery
    let default_chat_id = config.allowed_users.first()
        .and_then(|u| u.parse::<i64>().ok());
    let tg_adapter = Arc::new(crate::tools::TelegramAdapter::new(
        bot.clone(),
        default_chat_id,
    ));

    if let Some(router) = message_router {
        router.register(tg_adapter.clone() as Arc<dyn crate::tools::PlatformAdapter>).await;
        tracing::info!("Telegram registered with message router");
    }

    let state = Arc::new(TelegramState::new(agent, model_store, code_mode, config, Some(tg_adapter), crate::scheduler::ScheduleRunLog::default()));

    let commands = vec![
        teloxide::types::BotCommand { command: "memory".into(), description: "View agent memory".into() },
        teloxide::types::BotCommand { command: "clear".into(), description: "Clear conversation history".into() },
        teloxide::types::BotCommand { command: "tools".into(), description: "List available tools".into() },
        teloxide::types::BotCommand { command: "usage".into(), description: "View token usage statistics".into() },
        teloxide::types::BotCommand { command: "recover".into(), description: "Recover from crashed session".into() },
        teloxide::types::BotCommand { command: "help".into(), description: "Show help message".into() },
        teloxide::types::BotCommand { command: "status".into(), description: "Check bot status".into() },
        teloxide::types::BotCommand { command: "voice".into(), description: "Toggle voice mode or set voice".into() },
        teloxide::types::BotCommand { command: "persona".into(), description: "Show or edit persona".into() },
        teloxide::types::BotCommand { command: "new".into(), description: "Start new session".into() },
        teloxide::types::BotCommand { command: "update".into(), description: "Update ahnara to the latest version".into() },
        teloxide::types::BotCommand { command: "code".into(), description: "Start a coding session in isolated workspace".into() },
        teloxide::types::BotCommand { command: "normal".into(), description: "Exit coding mode and return to normal persona".into() },
        teloxide::types::BotCommand { command: "model".into(), description: "Override model/provider settings".into() },
        teloxide::types::BotCommand { command: "mcp".into(), description: "Manage MCP server integrations".into() },
        teloxide::types::BotCommand { command: "token".into(), description: "Manage API tokens".into() },
        teloxide::types::BotCommand { command: "logs".into(), description: "View gateway logs".into() },
        teloxide::types::BotCommand { command: "schedule".into(), description: "Manage scheduled jobs".into() },
    ];
    let _ = bot.set_my_commands(commands).await;

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_command),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback_query))
        .branch(Update::filter_message().endpoint(handle_message));

    tokio::spawn(async move {
        Dispatcher::builder(bot, handler)
            .dependencies(dptree::deps![state])
            .build()
            .dispatch()
            .await;
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn render_persona() -> String {
    match load_current_persona() {
        Ok(persona) => format!(
            "Current persona\n\nName: {}\nLength: {}\nTone: {}\nNo em dashes: {}\nNo emojis: {}\n\nBehavior:\n{}\n\nCommands:\n/persona show\n/persona name <value>\n/persona behavior <value>\n/persona tone <professional|casual|technical|friendly>\n/persona length <concise|balanced|detailed>\n/persona no_emojis <on|off>\n/persona no_em_dashes <on|off>\n/persona reset",
            persona.name,
            persona.style.length,
            persona.style.tone,
            persona.style.formatting.no_em_dashes,
            persona.style.formatting.no_emojis,
            persona.behavior
        ),
        Err(e) => format!("Failed to load persona: {e}"),
    }
}

fn handle_persona_command_text(text: &str) -> String {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("/persona").unwrap_or("").trim();
    if rest.is_empty() || rest == "show" {
        return render_persona();
    }

    let mut parts = rest.splitn(2, ' ');
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    let result = match sub {
        "name" if !arg.is_empty() => set_name(arg).map(|_| "Persona name updated".to_string()),
        "behavior" if !arg.is_empty() => set_behavior(arg).map(|_| "Persona behavior updated".to_string()),
        "tone" if !arg.is_empty() => set_tone(arg).map(|_| format!("Persona tone set to {arg}")),
        "length" if !arg.is_empty() => set_length(arg).map(|_| format!("Persona length set to {arg}")),
        "no_emojis" if !arg.is_empty() => {
            let enabled = matches!(arg, "on" | "true" | "yes" | "1");
            set_no_emojis(enabled).map(|_| format!("Persona no_emojis set to {enabled}"))
        }
        "no_emojis" => toggle_no_emojis().map(|enabled| format!("Persona no_emojis set to {enabled}")),
        "no_em_dashes" if !arg.is_empty() => {
            let enabled = matches!(arg, "on" | "true" | "yes" | "1");
            set_no_em_dashes(enabled).map(|_| format!("Persona no_em_dashes set to {enabled}"))
        }
        "no_em_dashes" => toggle_no_em_dashes().map(|enabled| format!("Persona no_em_dashes set to {enabled}")),
        "reset" => reset_persona().map(|_| "Persona reset to defaults".to_string()),
        _ => Err(anyhow!("Invalid persona command. Use /persona show, name, behavior, tone, length, no_emojis, no_em_dashes, or reset")),
    };

    match result {
        Ok(msg) => format!("{}\n\n{}", msg, render_persona()),
        Err(e) => format!("{}\n\n{}", e, render_persona()),
    }
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<TelegramState>,
) -> ResponseResult<()> {
    let chat_id: i64 = msg.chat.id.0;
    let _typing_guard = spawn_typing_loop(&bot, chat_id);

    let response = match cmd {
        Command::Memory => handle_memory_cmd(&state, msg.text().unwrap_or("/memory")).await,
        Command::Clear => handle_clear_cmd(&state, chat_id).await,
        Command::Tools => handle_tools_cmd(&state).await,
        Command::Usage => handle_usage_cmd(&state, chat_id).await,
        Command::Recover => handle_recover_cmd(&state, chat_id).await,
        Command::Help => handle_help_cmd(),
        Command::Status => handle_status_cmd(&state, chat_id).await,
        Command::Voice => handle_voice_cmd(&state, chat_id).await,
        Command::Persona => handle_persona_command_text(msg.text().unwrap_or("/persona")),
        Command::Update => crate::commands::update::handle_update().await,
        Command::Code => handle_code_cmd(&state, chat_id).await,
        Command::Model(args) => {
            return handle_model_cmd(&bot, &state, chat_id, &msg, &args).await;
        }
        Command::Mcp(args) => {
            return handle_mcp_cmd(&bot, chat_id, &args, &state).await;
        }
        Command::Token(args) => {
            return handle_token_cmd(&bot, &state, chat_id, &msg, &args).await;
        }
        Command::Logs(args) => {
            return handle_logs_cmd(&bot, chat_id, &args).await;
        }
        Command::Schedule(args) => {
            return handle_schedule_cmd(&state, chat_id, &args).await;
        }
        Command::Normal => handle_normal_cmd(&state, chat_id).await,
        Command::New => handle_new_cmd(&state, chat_id).await,
    };

    send_markdown_message(&bot, chat_id, &response).await?;
    Ok(())
}

async fn handle_memory_cmd(state: &TelegramState, text: &str) -> String {
    state.agent.handle_memory_text(text).await
}

async fn handle_clear_cmd(state: &TelegramState, chat_id: i64) -> String {
    let user_id = format!("{}", chat_id);
    let session_id = state.agent.get_or_create_session_id("telegram", &user_id);
    state.agent.clear_session(&session_id).await;
    state.agent.reset_session_routing("telegram", &user_id);
    state.clear_session(chat_id).await;
    "Session cleared".to_string()
}

async fn handle_tools_cmd(state: &TelegramState) -> String {
    let tools = state.agent.list_tools();
    let mut list = String::from("Available tools\n\n");
    for tool in tools {
        list.push_str(&format!("- {}: {}\n", tool.name, tool.description));
    }
    list
}

async fn handle_usage_cmd(state: &TelegramState, chat_id: i64) -> String {
    let session = state.get_or_create_session(chat_id).await;
    let usage = state.agent.get_usage_stats().await;
    format!(
        "Current session\nMessages: {}\nTotal tokens: {}\nPrompt tokens: {}\nCompletion tokens: {}\n\nAll-time\nMessages: {}\nTokens: {}",
        session.message_count,
        session.total_tokens,
        session.prompt_tokens,
        session.completion_tokens,
        usage.total_messages,
        usage.total_tokens,
    )
}

async fn handle_recover_cmd(state: &TelegramState, chat_id: i64) -> String {
    let user_id = format!("{}", chat_id);
    let session_id = state.agent.get_or_create_session_id("telegram", &user_id);
    let _ = state.agent.recover_session(&session_id).await;
    "Session recovered".to_string()
}

fn handle_help_cmd() -> String {
    "Commands: /memory /clear /tools /usage /recover /status /voice /persona /new /update /code /normal"
        .to_string()
}

async fn handle_status_cmd(state: &TelegramState, chat_id: i64) -> String {
    let session = state.get_or_create_session(chat_id).await;
    let uptime = chrono::Utc::now() - session.created_at;
    format!(
        "Status: Online\nUptime: {}h {}m\nActive chats: {}\nModel: {}\nVoice mode: {}",
        uptime.num_hours(),
        uptime.num_minutes() % 60,
        state.sessions.read().await.len(),
        state.agent.model_name(),
        if session.voice_mode { "ON" } else { "OFF" },
    )
}

async fn handle_voice_cmd(state: &TelegramState, chat_id: i64) -> String {
    let mut sessions = state.sessions.write().await;
    let session = sessions.entry(chat_id).or_default();
    session.voice_mode = !session.voice_mode;
    format!(
        "Voice mode {}",
        if session.voice_mode { "ON" } else { "OFF" }
    )
}

async fn handle_code_cmd(state: &TelegramState, chat_id: i64) -> String {
    let user_id = format!("{}", chat_id);
    let session_id = state.agent.get_or_create_session_id("telegram-code", &user_id);
    let workspace = crate::commands::code::ensure_workspace(&session_id)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to create workspace: {}", e);
            std::path::PathBuf::from("/tmp/ahnara-code")
        });
    let _ = crate::commands::code::init_workspace(&workspace);
    let code_prompt = crate::commands::code::build_code_system_prompt(&workspace);
    state.agent.set_session_context("telegram", &user_id).await;
    state.agent.set_system_prompt_override(&session_id, code_prompt).await;
    state.enter_code_mode(chat_id, workspace.display().to_string()).await;
    format!(
        "Coding mode activated.\nWorkspace: {}\n\nSend your coding task as the next message. Use /normal to exit coding mode.",
        workspace.display()
    )
}

async fn handle_model_cmd(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    msg: &Message,
    args: &str,
) -> ResponseResult<()> {
    let user_id = msg.from().map(|u| u.id.0).unwrap_or(0);
    let user_id_str = user_id.to_string();

    if args.trim().is_empty() {
        let response = match crate::commands::model::handle_model(
            &state.model_store,
            "telegram",
            &user_id_str,
            args,
        ) {
            Ok(resp) => resp,
            Err(e) => format!("Error: {}", e),
        };

        let keyboard = crate::commands::model::provider_keyboard_json();
        let formatted = crate::channels::markdown::markdown_to_telegram(&response);
        let markup: teloxide::types::InlineKeyboardMarkup = serde_json::from_str(&keyboard).unwrap_or_default();
        let _ = bot
            .send_message(teloxide::types::ChatId(chat_id), &formatted)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(markup)
            .await;
        return Ok(());
    }

    let response = match crate::commands::model::handle_model(
        &state.model_store,
        "telegram",
        &user_id_str,
        args,
    ) {
        Ok(resp) => resp,
        Err(e) => format!("Error: {}", e),
    };
    send_markdown_message(bot, chat_id, &response).await?;
    Ok(())
}

async fn handle_mcp_cmd(
    bot: &Bot,
    chat_id: i64,
    args: &str,
    state: &TelegramState,
) -> ResponseResult<()> {
    let response = crate::commands::mcp::handle_mcp(args, Some(&state.agent))
        .await
        .unwrap_or_else(|e| format!("Error: {}", e));
    send_markdown_message(bot, chat_id, &response).await?;
    Ok(())
}

async fn handle_token_cmd(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    msg: &Message,
    args: &str,
) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        if crate::commands::token::contains_secret(text) {
            let _ = bot.delete_message(teloxide::types::ChatId(chat_id), msg.id).await;
        }
    }
    let response = crate::commands::token::handle_token(args)
        .unwrap_or_else(|e| format!("Error: {}", e));
    send_markdown_message(bot, chat_id, &response).await?;
    Ok(())
}

async fn handle_logs_cmd(bot: &Bot, chat_id: i64, args: &str) -> ResponseResult<()> {
    let response = crate::commands::logs::handle_logs(args).await;
    send_markdown_message(bot, chat_id, &response).await?;
    Ok(())
}

async fn handle_schedule_cmd(state: &TelegramState, chat_id: i64, args: &str) -> ResponseResult<()> {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.ahnara/config.toml"));
    let scheduler_manager = crate::tools::scheduler_tools::SchedulerManager::new(
        state.schedule_log.clone(),
        config_path.to_string_lossy().to_string(),
    );
    let response = crate::commands::schedule::handle_schedule(args, &scheduler_manager).await;
    send_markdown_message(&TelegramState::get_bot(), chat_id, &response).await?;
    Ok(())
}

fn handle_normal_cmd(state: &TelegramState, chat_id: i64) -> String {
    let user_id = format!("{}", chat_id);
    let code_session = state.agent.get_or_create_session_id("telegram-code", &user_id);
    state.agent.clear_system_prompt_override(&code_session);
    state.agent.reset_session_routing("telegram-code", &user_id);
    "Exited coding mode. Back to normal.".to_string()
}

async fn handle_new_cmd(state: &TelegramState, chat_id: i64) -> String {
    let user_id = format!("{}", chat_id);
    state.agent.reset_session_routing("telegram", &user_id);
    state.clear_session(chat_id).await;
    "New session started".to_string()
}

async fn handle_custom_subtype(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    q: &teloxide::types::CallbackQuery,
    data: &str,
) -> ResponseResult<()> {
    let subtype = data.strip_prefix("model:custom_subtype:").unwrap_or("openai-compatible");
    let label = if subtype == "anthropic" { "Anthropic-style" } else { "OpenAI-compatible" };

    let user_id_str = q.from.id.0.to_string();
    let mut ov = match state.model_store.get("telegram", &user_id_str) {
        Ok(Some(ov)) => ov,
        Ok(None) => crate::memory::model_store::UserModelOverride::default(),
        Err(e) => {
            tracing::error!("Model store get error: {e:?}");
            let _ = bot.answer_callback_query(q.id.clone()).text("Storage error").await;
            return Ok(());
        }
    };
    ov.provider_type = Some(format!("custom/{}", subtype));
    ov.base_url = None;
    ov.updated_at = crate::commands::model::now_secs();
    if let Err(e) = state.model_store.set("telegram", &user_id_str, &ov) {
        tracing::error!("Model store set error: {e:?}");
        let _ = bot.answer_callback_query(q.id.clone()).text("Storage error").await;
        return Ok(());
    }

    state.pending_model_flows.write().await.insert(chat_id, ModelFlowState::WaitingForEndpoint {
        subtype: subtype.to_string(),
    });

    let _ = bot.answer_callback_query(q.id.clone()).await;
    let _ = bot.send_message(
        teloxide::types::ChatId(chat_id),
        format!(
            "{} API format selected.\n\nSend your endpoint URL and model ID together, like:\nhttps://your-api.example.com/v1 model-name\n\nI'll auto-detect which is which.",
            label
        ),
    ).await;

    Ok(())
}

/// Handle Telegram callback queries (inline keyboard button presses).
async fn handle_callback_query(
    bot: Bot,
    q: teloxide::types::CallbackQuery,
    state: Arc<TelegramState>,
) -> ResponseResult<()> {
    let chat_id = match &q.message {
        Some(msg) => msg.chat.id.0,
        None => return Ok(()),
    };

    let data = q.data.as_deref().unwrap_or("");

    if !data.starts_with("model:") {
        return Ok(());
    }

    let user_id = q.from.id.0;

    if data.starts_with("model:custom_subtype:") {
        return handle_custom_subtype(&bot, &state, chat_id, &q, data).await;
    }

    match crate::commands::model::handle_callback(
        &state.model_store,
        "telegram",
        &user_id.to_string(),
        data,
    ) {
        Ok((response, keyboard, done)) => {
            let markup_str = keyboard.as_deref().unwrap_or("");
            let send_result = if markup_str.is_empty() {
                bot.send_message(teloxide::types::ChatId(chat_id), &response)
                    .await
            } else {
                let markup: teloxide::types::InlineKeyboardMarkup =
                    serde_json::from_str(markup_str).unwrap_or_default();
                bot.send_message(teloxide::types::ChatId(chat_id), &response)
                    .reply_markup(markup)
                    .await
            };
            if let Err(ref e) = send_result {
                tracing::error!("Callback message send failed: {}", e);
            }
            if done {
                let _ = bot.answer_callback_query(q.id).await;
            }
        }
        Err(e) => {
            let _ = bot
                .answer_callback_query(q.id)
                .text(format!("Error: {}", e))
                .await;
        }
    }

    Ok(())
}

async fn handle_model_flow(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    msg_id: teloxide::types::MessageId,
    text: &str,
    flow: ModelFlowState,
) -> anyhow::Result<()> {
    match flow {
        ModelFlowState::WaitingForEndpoint { subtype } => {
            handle_waiting_for_endpoint(bot, state, chat_id, text, &subtype).await
        }
        ModelFlowState::WaitingForKey => {
            handle_waiting_for_key(bot, state, chat_id, msg_id, text).await
        }
    }
}

async fn handle_waiting_for_endpoint(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    text: &str,
    subtype: &str,
) -> anyhow::Result<()> {
    use crate::commands::model;
    let user_id = format!("{}", chat_id);
    let store = &*state.model_store;

    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut url: Option<&str> = None;
    let mut model_id: Option<&str> = None;

    for token in &tokens {
        if token.contains("/v1") || token.starts_with("http://") || token.starts_with("https://") {
            url = Some(token);
        } else {
            model_id = Some(token);
        }
    }

    let url = url.ok_or_else(|| anyhow::anyhow!(
        "Could not detect a URL. Send both like:\n`https://your-api.example.com/v1 model-name`"
    ))?;
    let model_id = model_id.ok_or_else(|| anyhow::anyhow!(
        "Could not detect a model ID. Send both like:\n`https://your-api.example.com/v1 model-name`"
    ))?;

    let mut ov = store.get("telegram", &user_id)?.unwrap_or_default();
    ov.provider_type = Some(format!("custom/{}", subtype));
    ov.base_url = Some(url.to_string());
    ov.model_id = Some(model_id.to_string());
    ov.updated_at = model::now_secs();
    store.set("telegram", &user_id, &ov)?;

    state.pending_model_flows.write().await.insert(chat_id, ModelFlowState::WaitingForKey);

    let _ = send_markdown_message(
        bot,
        chat_id,
        &format!(
            "Endpoint saved: {}\nModel: {}\n\nNow send your API key (just the key, nothing else).\nI will delete your message after saving it.",
            url, model_id
        ),
    ).await?;

    Ok(())
}

async fn handle_waiting_for_key(
    bot: &Bot,
    state: &TelegramState,
    chat_id: i64,
    msg_id: teloxide::types::MessageId,
    text: &str,
) -> anyhow::Result<()> {
    use crate::commands::model;
    let user_id = format!("{}", chat_id);
    let store = &*state.model_store;

    let key = text.trim();
    if key.is_empty() || key.len() < 4 {
        state.pending_model_flows.write().await.insert(chat_id, ModelFlowState::WaitingForKey);
        let _ = send_markdown_message(bot, chat_id, "Key too short. Send your API key again.").await?;
        return Ok(());
    }

    let _ = bot.delete_message(teloxide::types::ChatId(chat_id), msg_id).await;

    let mut ov = store.get("telegram", &user_id)?.unwrap_or_default();
    let encrypted = store.encrypt_key(key)?;
    ov.encrypted_api_key = Some(encrypted);
    ov.updated_at = model::now_secs();
    store.set("telegram", &user_id, &ov)?;

    state.pending_model_flows.write().await.remove(&chat_id);

    let masked = model::mask_key(key);
    let summary = model::build_summary_for_user("telegram", &user_id, store)?;
    let _ = send_markdown_message(
        bot,
        chat_id,
        &format!(
            "API key saved: `{}`\n\n{}",
            masked, summary
        ),
    ).await?;

    Ok(())
}

async fn handle_message(bot: Bot, msg: Message, state: Arc<TelegramState>) -> ResponseResult<()> {
    let chat_id: i64 = msg.chat.id.0;

    // Track active chat for mid-task message delivery
    if let Some(ref adapter) = state.message_adapter {
        adapter.set_active_chat(chat_id);
    }

    // Intercept model setup flow BEFORE any other check
    let text = msg.text().unwrap_or("");

    // `/start` — welcome message with common tasks
    if text.trim() == "/start" {
        let first = msg.from().as_ref().map(|u| u.first_name.as_str()).unwrap_or("there");
        let welcome = format!(
            "Welcome, {first}! I am *AHNARA*, your AI agent.\n\n\
             \u{1f9e0} Chat naturally \u{2014} I remember our conversations.\n\
             \u{1f50d} Ask me to search the web, run code, or analyze files.\n\
             \u{1f4f1} Just message me here or on Discord.\n\n\
             *Get started:*\n\
             \u{2022} Just type a message to chat\n\
             \u{2022} /model \u{2014} choose your AI provider\n\
             \u{2022} /help \u{2014} see all commands\n\
             \u{2022} /status \u{2014} check my status\n\
             \u{2022} /token set <name> <value> — store a secure API token\n\n\
             \u{26a1} I am ready when you are!"
        );
        send_markdown_message(
            &bot,
            chat_id,
            &welcome,
        ).await?;
        return Ok(());
    }

    {
        let mut flows = state.pending_model_flows.write().await;
        if let Some(flow) = flows.remove(&chat_id) {
            drop(flows);
            if text.is_empty() {
                send_markdown_message(&bot, chat_id, "Please send a text message for the model setup flow.").await?;
                return Ok(());
            }
            match handle_model_flow(&bot, &state, chat_id, msg.id, text, flow).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!("Model flow error: {e:?}");
                    let _ = send_markdown_message(&bot, chat_id, &format!("Error: {}. Try /model again.", e)).await;
                    return Ok(());
                }
            }
        }
    }

    // Auto-delete messages containing secrets
    if !text.is_empty() && crate::commands::token::contains_secret(text) {
        let _ = bot.delete_message(teloxide::types::ChatId(chat_id), msg.id).await;
        send_markdown_message(&bot, chat_id, "Your message was deleted for security (it contained a token/secret). Use `/token set <name> <value>` to store tokens safely.").await?;
        return Ok(());
    }

    if text.trim_start().starts_with("/persona") {
        let response = handle_persona_command_text(text);
        send_markdown_message(&bot, chat_id, &response).await?;
        return Ok(());
    }

    if text.trim() == "/normal" {
        state.exit_code_mode(chat_id).await;
        let user_id = format!("{}", chat_id);
        let code_session = state.agent.get_or_create_session_id("telegram-code", &user_id);
        state.agent.clear_system_prompt_override(&code_session).await;
        state.agent.reset_session_routing("telegram-code", &user_id);
        send_markdown_message(&bot, chat_id, "Exited coding mode. Back to normal.").await?;
        return Ok(());
    }

    // ── Media handling: detect and download attachments ──
    let mut agent_message = String::new();
    let mut media_downloaded: Vec<String> = Vec::new();

    // Check for photo (largest size)
    if let Some(photo) = msg.photo() {
        let largest = photo.iter().max_by_key(|p| p.width * p.height).unwrap();
        match download_telegram_file(&bot, &largest.file.id, "images", &format!("{}.jpg", largest.file.id)).await {
            Ok(path) => {
                let caption = msg.caption().unwrap_or("").to_string();
                agent_message = if caption.is_empty() {
                    format!("User sent an image. File saved at: {}\n\nAnalyze this image to understand what the user sent. Use the analyze_image tool with this path.", path)
                } else {
                    format!("User sent an image with caption: \"{}\"\n\nFile saved at: {}\n\nAnalyze this image to understand what the user sent. Use the analyze_image tool with this path.", caption, path)
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download photo: {e:?}");
                agent_message = "User sent an image but it could not be downloaded.".to_string();
            }
        }
    }
    // Check for video
    else if let Some(video) = msg.video() {
        match download_telegram_file(&bot, &video.file.id, "videos", &video.file.unique_id).await {
            Ok(path) => {
                let caption = msg.caption().unwrap_or("").to_string();
                agent_message = if caption.is_empty() {
                    format!("User sent a video. File saved at: {}\n\nAnalyze this video to understand what the user sent. Use the analyze_video tool with this path.", path)
                } else {
                    format!("User sent a video with caption: \"{}\"\n\nFile saved at: {}\n\nAnalyze this video to understand what the user sent. Use the analyze_video tool with this path.", caption, path)
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download video: {e:?}");
                agent_message = "User sent a video but it could not be downloaded.".to_string();
            }
        }
    }
    // Check for animation (GIF/video note)
    else if let Some(animation) = msg.animation() {
        match download_telegram_file(&bot, &animation.file.id, "videos", &animation.file.unique_id).await {
            Ok(path) => {
                let caption = msg.caption().unwrap_or("").to_string();
                agent_message = if caption.is_empty() {
                    format!("User sent an animated GIF/video. File saved at: {}\n\nAnalyze this animation. Use analyze_video with this path.", path)
                } else {
                    format!("User sent an animated GIF/video with caption: \"{}\"\n\nFile saved at: {}", caption, path)
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download animation: {e:?}");
                agent_message = "User sent an animation but it could not be downloaded.".to_string();
            }
        }
    }
    // Check for document
    else if let Some(doc) = msg.document() {
        let filename = doc.file_name.as_deref().unwrap_or("unknown");
        let subdir = if filename.ends_with(".pdf") { "documents" } else { "files" };
        match download_telegram_file(&bot, &doc.file.id, subdir, filename).await {
            Ok(path) => {
                let caption = msg.caption().unwrap_or("").to_string();
                agent_message = if caption.is_empty() {
                    format!("User sent a file: {}\n\nFile saved at: {}\n\nUse the appropriate tool to analyze this file (read_document for PDFs, read_file for text, analyze_image for images).", filename, path)
                } else {
                    format!("User sent a file: {} with caption: \"{}\"\n\nFile saved at: {}\n\nUse the appropriate tool to analyze this file.", filename, caption, path)
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download document: {e:?}");
                agent_message = format!("User sent a file ({}) but it could not be downloaded.", filename);
            }
        }
    }
    // Check for audio
    else if let Some(audio) = msg.audio() {
        let filename = audio.file_name.as_deref().unwrap_or("audio.mp3");
        match download_telegram_file(&bot, &audio.file.id, "audio", filename).await {
            Ok(path) => {
                let caption = msg.caption().unwrap_or("").to_string();
                // Auto-transcribe audio
                let transcript_info = tokio::task::spawn_blocking({
                    let p = path.clone();
                    move || crate::tools::transcribe::transcribe_audio_sync(&p, "base", None)
                }).await.unwrap_or_else(|e| Err(format!("Transcription task failed: {e}")));

                agent_message = match (transcript_info, caption.is_empty()) {
                    (Ok(tr), true) => format!(
                        "User sent an audio file. File saved at: {}\n\n[Auto-transcription | {} | {:.1}s]\n{}",
                        path, tr.language, tr.duration, tr.text
                    ),
                    (Ok(tr), false) => format!(
                        "User sent an audio file with caption: \"{}\"\n\nFile saved at: {}\n\n[Auto-transcription | {} | {:.1}s]\n{}",
                        caption, path, tr.language, tr.duration, tr.text
                    ),
                    (Err(e), true) => format!(
                        "User sent an audio file. File saved at: {}\n\n(Transcription failed: {})\n\nUse the transcribe_audio tool to retry.",
                        path, e
                    ),
                    (Err(e), false) => format!(
                        "User sent an audio file with caption: \"{}\"\n\nFile saved at: {}\n\n(Transcription failed: {})\n\nUse the transcribe_audio tool to retry.",
                        caption, path, e
                    ),
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download audio: {e:?}");
                agent_message = "User sent audio but it could not be downloaded.".to_string();
            }
        }
    }
    // Check for voice message
    else if let Some(voice) = msg.voice() {
        match download_telegram_file(&bot, &voice.file.id, "audio", &format!("voice_{}.ogg", msg.id.0)).await {
            Ok(path) => {
                // Auto-transcribe voice message
                let transcript_info = tokio::task::spawn_blocking({
                    let p = path.clone();
                    move || crate::tools::transcribe::transcribe_audio_sync(&p, "base", None)
                }).await.unwrap_or_else(|e| Err(format!("Transcription task failed: {e}")));

                agent_message = match transcript_info {
                    Ok(tr) => format!(
                        "User sent a voice message. File saved at: {}\n\n[Auto-transcription | {} | {:.1}s]\n{}",
                        path, tr.language, tr.duration, tr.text
                    ),
                    Err(e) => format!(
                        "User sent a voice message. File saved at: {}\n\n(Transcription failed: {})\n\nUse the transcribe_audio tool to retry.",
                        path, e
                    ),
                };
                media_downloaded.push(path);
            }
            Err(e) => {
                tracing::warn!("Failed to download voice: {e:?}");
                agent_message = "User sent a voice message but it could not be downloaded.".to_string();
            }
        }
    }

    // If no media was attached, use text content
    if agent_message.is_empty() {
        agent_message = text.to_string();
    }

    // If still empty (empty text, no media), bail
    if agent_message.is_empty() {
        return Ok(());
    }

    let _typing_guard = spawn_typing_loop(&bot, chat_id);
    let _session = state.get_or_create_session(chat_id).await;
    let user_id = format!("{}", chat_id);
    let session_id = if state.is_coding(chat_id).await {
        state.agent.get_or_create_session_id("telegram-code", &user_id)
    } else {
        state.agent.get_or_create_session_id("telegram", &user_id)
    };
    state.agent.set_session_context("telegram", &user_id).await;

    // If an agent loop is already running for this session, inject the
    // message as a mid-loop intervention instead of starting a parallel
    // process() call. The running loop will pick it up on its next iteration.
    if state.agent.session_is_active(&session_id).await {
        let accepted = state.agent.inject_message(&session_id, agent_message.clone()).await;
        if accepted {
            send_markdown_message(
                &bot,
                chat_id,
                "Got it — I'll fold that into the current task.",
            )
            .await?;
            return Ok(());
        }
    }

    let response = state
        .agent
        .process(&agent_message, Some(&session_id))
        .await;
    state.update_session(chat_id, None).await;

    // Send text response
    if let Err(err) = send_markdown_message(&bot, chat_id, &response).await {
        tracing::warn!("Telegram send error: {err:?}");
    }

    // Drain and send any structured outputs
    let structured = state.agent.drain_structured_outputs();
    for output in structured {
        if let Err(err) = send_structured_output(&bot, chat_id, &output).await {
            tracing::warn!("Failed to send structured output: {err:?}");
        }
    }

    Ok(())
}

/// Download a file from Telegram and save it to the ahnara media directory.
/// Returns the absolute path to the saved file.
async fn download_telegram_file(
    bot: &Bot,
    file_id: &str,
    subdir: &str,
    filename: &str,
) -> anyhow::Result<String> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/root"));
    let media_dir = home.join(".ahnara/media").join(subdir);
    tokio::fs::create_dir_all(&media_dir).await?;

    let safe_name = filename
        .replace("..", "_")
        .replace("/", "_")
        .replace("\\", "_");
    let path = media_dir.join(&safe_name);

    let file = bot.get_file(file_id).await?;
    let mut writer = tokio::fs::File::create(&path).await?;
    bot.download_file(&file.path, &mut writer).await?;

    tracing::info!("Downloaded Telegram file to: {}", path.display());
    Ok(path.to_string_lossy().to_string())
}

async fn send_markdown_message(bot: &Bot, chat_id: i64, text: &str) -> ResponseResult<()> {
    for chunk in split_telegram_message(text, 3900) {
        let formatted = markdown_to_telegram(&chunk);
        let send_result = bot.send_message(ChatId(chat_id), &formatted)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
        if let Err(err) = send_result {
            tracing::warn!("MarkdownV2 send failed ({err:?}), retrying as plain text");
            let plain = strip_all_formatting(&chunk);
            bot.send_message(ChatId(chat_id), plain)
                .await?;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

fn strip_all_formatting(text: &str) -> String {
    text.to_string()
}

fn split_telegram_message(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return vec![text.to_string()];
    }

    chars
        .chunks(max_chars)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

/// Send a structured output to Telegram.
/// File/image/video: send as document/photo/video attachment.
/// JSON/CSV/markdown: write to temp file, send as document.
async fn send_structured_output(
    bot: &Bot,
    chat_id: i64,
    output: &crate::agent::StructuredOutput,
) -> anyhow::Result<()> {
    use teloxide::types::InputFile;

    match output.format.as_str() {
        "image" => send_image(bot, chat_id, output).await,
        "video" => send_video(bot, chat_id, output).await,
        "file" => send_file_attachment(bot, chat_id, output).await,
        "json" | "csv" | "markdown" => send_text_file(bot, chat_id, output).await,
        _ => {
            tracing::warn!("Unknown structured output format: {}", output.format);
            Ok(())
        }
    }
}

async fn send_image(bot: &Bot, chat_id: i64, output: &crate::agent::StructuredOutput) -> anyhow::Result<()> {
    use teloxide::types::InputFile;
    let path = output.content.as_str().unwrap_or_default();
    let photo = InputFile::file(path);
    bot.send_photo(teloxide::types::ChatId(chat_id), photo).await?;
    Ok(())
}

async fn send_video(bot: &Bot, chat_id: i64, output: &crate::agent::StructuredOutput) -> anyhow::Result<()> {
    use teloxide::types::InputFile;
    let path = output.content.as_str().unwrap_or_default();
    let video = InputFile::file(path);
    bot.send_video(teloxide::types::ChatId(chat_id), video).await?;
    Ok(())
}

async fn send_file_attachment(bot: &Bot, chat_id: i64, output: &crate::agent::StructuredOutput) -> anyhow::Result<()> {
    use teloxide::types::InputFile;
    let path = output.content.as_str().unwrap_or_default();
    let mut doc = InputFile::file(path);
    if let Some(ref name) = output.filename {
        doc = doc.file_name(name.clone());
    }
    bot.send_document(teloxide::types::ChatId(chat_id), doc).await?;
    Ok(())
}

async fn send_text_file(bot: &Bot, chat_id: i64, output: &crate::agent::StructuredOutput) -> anyhow::Result<()> {
    use teloxide::types::InputFile;
    
    let content = match output.format.as_str() {
        "json" => serde_json::to_string_pretty(&output.content).unwrap_or_else(|_| output.content.to_string()),
        _ => output.content.as_str().unwrap_or(&output.content.to_string()).to_string(),
    };
    
    let filename = output.filename.clone().unwrap_or_else(|| match output.format.as_str() {
        "json" => "output.json".into(),
        "csv" => "output.csv".into(),
        "markdown" => "output.md".into(),
        _ => "output.txt".into(),
    });
    
    let ext = filename.rsplit('.').next().unwrap_or("txt");
    let tmp = std::env::temp_dir().join(format!("ahnara_structured_{}.{}", uuid::Uuid::new_v4(), ext));
    std::fs::write(&tmp, &content)?;
    let doc = InputFile::file(&tmp).file_name(filename);
    let _ = bot.send_document(teloxide::types::ChatId(chat_id), doc).await;
    let _ = std::fs::remove_file(&tmp);
    
    Ok(())
}
