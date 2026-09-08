//! Setup wizard

use anyhow::{bail, Result};
use dialoguer::{theme::ColorfulTheme, Input, Select, Confirm};
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;

/// Non-interactive configuration values. When any field is set, the wizard
/// skips all `dialoguer` prompts and writes a config deterministically.
#[derive(Debug, Default, Clone)]
pub struct NonInteractiveOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub telegram_token: Option<String>,
    pub discord_token: Option<String>,
    pub github_token: Option<String>,
}

pub fn handle_setup(quick: bool, telegram: bool, discord: bool) -> Result<()> {
    let env_opts = NonInteractiveOptions {
        provider: std::env::var("AHNARA_PROVIDER").ok(),
        model: std::env::var("AHNARA_MODEL").ok(),
        api_key: std::env::var("AHNARA_API_KEY").ok().or_else(|| {
            // Common fallback: NVIDIA, OpenAI, Anthropic provider-specific vars
            std::env::var("NVIDIA_API_KEY").ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        }),
        telegram_token: std::env::var("AHNARA_TELEGRAM_TOKEN").ok(),
        discord_token: std::env::var("AHNARA_DISCORD_TOKEN").ok(),
        github_token: std::env::var("AHNARA_GITHUB_TOKEN").ok(),
    };
    let has_env = env_opts.provider.is_some()
        || env_opts.model.is_some()
        || env_opts.api_key.is_some()
        || env_opts.telegram_token.is_some()
        || env_opts.discord_token.is_some()
        || env_opts.github_token.is_some();
    if has_env {
        return handle_setup_with(quick, telegram, discord, env_opts);
    }
    handle_setup_with(quick, telegram, discord, NonInteractiveOptions::default())
}

/// Like `handle_setup` but accepts non-interactive overrides. Used when the
/// caller has already collected config values from the user (CLI flags, env
/// vars, or a web onboarding flow).
pub fn handle_setup_with(
    quick: bool,
    telegram: bool,
    discord: bool,
    non_interactive: NonInteractiveOptions,
) -> Result<()> {
    println!("\nAHNARA Setup Wizard\n");

    let config_dir = dirs::home_dir()
        .map(|h| h.join(".ahnara"))
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    let config_path = config_dir.join("config.toml");

    // Route 1: explicit quick flag → existing quick_setup
    if quick {
        return quick_setup(&config_dir, telegram, discord);
    }

    // Route 2: any non-interactive option provided → build config without prompts
    let has_non_interactive = non_interactive.provider.is_some()
        || non_interactive.model.is_some()
        || non_interactive.api_key.is_some()
        || non_interactive.telegram_token.is_some()
        || non_interactive.discord_token.is_some()
        || non_interactive.github_token.is_some();
    if has_non_interactive {
        return non_interactive_setup(&config_dir, &config_path, &non_interactive);
    }

    // Route 3: TTY check. Refuse to run the interactive wizard without a
    // terminal -- dialoguer will block forever or read EOF on EOF-only stdin.
    if !std::io::stdin().is_terminal() {
        return Err(bail_non_tty());
    }
    if !std::io::stdout().is_terminal() {
        return Err(bail_non_tty());
    }

    // Interactive setup
    println!("This wizard will help you configure AHNARA.\n");

    // Create config directory
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)?;
        fs::create_dir_all(config_dir.join("skills"))?;
        fs::create_dir_all(config_dir.join("memory"))?;
        println!("Created config directory: {:?}", config_dir);
    }
    
    // Agent name
    let agent_name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Agent name")
        .default("AHNARA".into())
        .interact_text()?;
    
    // Provider selection
    let providers = vec![
        "OpenAI",
        "Anthropic",
        "Google Gemini",
        "OpenRouter (multi-model)",
        "Groq",
        "NVIDIA",
        "Custom endpoint",
    ];
    
    let provider_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select your LLM provider")
        .items(&providers)
        .interact()?;
    
    let (provider_name, api_base) = match provider_idx {
        0 => ("openai", "https://api.openai.com/v1".to_string()),
        1 => ("anthropic", "https://api.anthropic.com/v1".to_string()),
        2 => ("google", "https://generativelanguage.googleapis.com/v1beta/openai".to_string()),
        3 => ("openrouter", "https://openrouter.ai/api/v1".to_string()),
        4 => ("groq", "https://api.groq.com/openai/v1".to_string()),
        5 => ("nvidia", "https://integrate.api.nvidia.com/v1".to_string()),
        6 => {
            let base: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("API base URL")
                .interact_text()?;
            ("custom", base)
        },
        _ => bail!("Invalid selection"),
    };

    // Model ID
    let model: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Model ID (e.g. gpt-4o, claude-3-5-sonnet-20241022, gemini-1.5-flash)")
        .interact_text()?;
    
    // API Key
    let api_key: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("API Key")
        .interact_text()?;
    
    // Temperature
    let temp_str: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Temperature (0.0-2.0)")
        .default("1.0".into())
        .interact_text()?;
    let temperature: f32 = temp_str.parse().unwrap_or(1.0);
    
    // Channels
    let enable_telegram = telegram || Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable Telegram?")
        .default(false)
        .interact()?;
    
    let telegram_token = if enable_telegram {
        let token: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Telegram Bot Token")
            .allow_empty(true)
            .interact_text()?;
        Some(token)
    } else {
        None
    };
    
    let enable_discord = discord || Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable Discord?")
        .default(false)
        .interact()?;
    
    let discord_token = if enable_discord {
        let token: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Discord Bot Token")
            .allow_empty(true)
            .interact_text()?;
        Some(token)
    } else {
        None
    };
    
    // MCP Integrations
    let enable_github_mcp = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable GitHub integration (MCP)?")
        .default(false)
        .interact()?;
    
    let github_token = if enable_github_mcp {
        println!("  To create a token: GitHub Settings > Developer settings > Personal access tokens");
        println!("  Required scopes: repo, read:org, read:user\n");
        let token: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("GitHub Personal Access Token")
            .interact_text()?;
        Some(token)
    } else {
        None
    };
    
    // Extra MCP servers
    let mut extra_mcp_servers: Vec<(String, String, Vec<String>)> = Vec::new();
    if !enable_github_mcp {
        let add_more_mcp = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Add other MCP servers? (You can also use /mcp add later)")
            .default(false)
            .interact()?;
        
        if add_more_mcp {
            loop {
                println!("\nAdd MCP server (leave name empty to finish):");
                let name: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("  Server name (e.g. filesystem, slack)")
                    .allow_empty(true)
                    .interact_text()?;
                
                if name.is_empty() {
                    break;
                }
                
                let command: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("  Command (e.g. npx -y @modelcontextprotocol/server-filesystem)")
                    .interact_text()?;
                
                let args_str: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("  Arguments (space-separated, e.g. /home/workspace)")
                    .allow_empty(true)
                    .interact_text()?;
                
                let args: Vec<String> = if args_str.is_empty() {
                    Vec::new()
                } else {
                    args_str.split_whitespace().map(String::from).collect()
                };
                
                extra_mcp_servers.push((name, command, args));
            }
        }
    }
    
    // Token management
    if github_token.is_none() {
        println!("\nYou can set tokens later with:");
        println!("  ahnara token set GITHUB_TOKEN <your-token>");
        println!("  Or use /token set GITHUB_TOKEN <your-token> in Telegram/Discord\n");
    }
    
    // Generate config
    let config = generate_config(
        &agent_name,
        provider_name,
        &api_base,
        &model,
        &api_key,
        temperature,
        telegram_token.as_deref(),
        discord_token.as_deref(),
        github_token.as_deref(),
        &extra_mcp_servers,
    );
    
    fs::write(&config_path, &config)?;
    if let Err(e) = restrict_permissions(&config_path) {
        eprintln!("Warning: could not restrict permissions on config: {e}");
    }
    println!("\nConfiguration saved to {:?}", config_path);
    
    // Save token store
    if github_token.is_some() {
        let token_dir = config_dir.join("tokens.json");
        let mut tokens = serde_json::Map::new();
        if let Some(ref t) = github_token {
            tokens.insert("GITHUB_TOKEN".to_string(), serde_json::Value::String(t.clone()));
        }
        let token_json = serde_json::to_string_pretty(&tokens)?;
        fs::write(&token_dir, token_json)?;
        println!("Tokens saved to {:?}", token_dir);
    }
    
    // Summary
    println!("\nSummary:");
    println!("  Agent: {}", agent_name);
    println!("  Provider: {} ({})", provider_name, model);
    println!("  Temperature: {}", temperature);
    if enable_telegram {
        println!("  Telegram: enabled");
    }
    if enable_discord {
        println!("  Discord: enabled");
    }
    if enable_github_mcp {
        println!("  GitHub MCP: enabled (26 tools)");
    }
    if !extra_mcp_servers.is_empty() {
        println!("  Extra MCP servers: {}", extra_mcp_servers.len());
    }
    
    println!("\nSetup complete! Run `ahnara gateway` to start.");
    
    Ok(())
}

fn quick_setup(config_dir: &PathBuf, telegram: bool, discord: bool) -> Result<()> {
    if !config_dir.exists() {
        fs::create_dir_all(config_dir)?;
        fs::create_dir_all(config_dir.join("skills"))?;
        fs::create_dir_all(config_dir.join("memory"))?;
    }
    
    let config = generate_config(
        "AHNARA",
        "",
        "",
        "",
        "",
        1.0,
        if telegram { Some("") } else { None },
        if discord { Some("") } else { None },
        None,
        &[],
    );
    
    let config_path = config_dir.join("config.toml");
    fs::write(&config_path, &config)?;
    if let Err(e) = restrict_permissions(&config_path) {
        eprintln!("Warning: could not restrict permissions on config: {e}");
    }

    println!("Quick setup complete: {:?}", config_path);
    println!("  Configure your model: ahnara model --provider <name> --key <api_key> <model_id>");
    println!("  Or from Telegram/Discord: /model");
    println!("  Run: ahnara gateway");
    
    Ok(())
}

#[cfg(unix)]
pub fn restrict_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub fn restrict_permissions(path: &std::path::Path) -> std::io::Result<()> {
    // On non-Unix we don't have a portable chmod equivalent. Best-effort:
    // mark the file readonly. This won't hide secrets from admin users but
    // it stops accidental world-writes.
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
}

/// Decide what to actually write into `[providers.primary].api_key`.
///
/// The user can pass a real key in any of three ways:
/// 1. They typed it in the interactive wizard.
/// 2. They passed `--api-key` on the command line.
/// 3. They exported `AHNARA_API_KEY` in their environment.
///
/// In all three cases we write the literal key into `config.toml`.
/// If the key is empty (or matches a known placeholder string), we write
/// the canonical placeholder so the user can search for it later and run
/// `ahnara token set` to fill in the real value. This is the
/// "non-secret default" path -- the file no longer contains a blank
/// string and it doesn't pretend the key is set.
fn sanitize_api_key(
    key: &str,
    env_var: &str,
) -> String {
    const PLACEHOLDER: &str = "<set via ahnara token or AHNARA_API_KEY env>";
    if key.trim().is_empty() {
        return PLACEHOLDER.to_string();
    }
    // If the user typed the placeholder or any common no-op, treat as empty.
    let trimmed = key.trim();
    let known_placeholders = ["<set via ahnara token>", "<set via env>", "<set later>", "changeme", "your-key-here", "TODO"];
    if known_placeholders.contains(&trimmed) {
        return PLACEHOLDER.to_string();
    }
    key.to_string()
}

/// Return the placeholder string so callers can reference it in user-facing output.
pub fn api_key_placeholder() -> &'static str {
    "<set via ahnara token or AHNARA_API_KEY env>"
}

fn generate_config(
    agent_name: &str,
    provider: &str,
    api_base: &str,
    model: &str,
    api_key: &str,
    temperature: f32,
    telegram_token: Option<&str>,
    discord_token: Option<&str>,
    github_token: Option<&str>,
    extra_mcp: &[(String, String, Vec<String>)],
) -> String {
    let model_line = if model.is_empty() {
        String::new()
    } else {
        format!("default_model = \"{}\"\n", model)
    };

    let provider_block = if !provider.is_empty() && !api_base.is_empty() {
        format!(
            r#"[[providers.providers]]
name = "{}"
api_base = "{}"
api_key = "{}"  # set via ahnara token or env var
"#,
            provider, api_base, sanitize_api_key(api_key, "AHNARA_API_KEY")
        )
    } else {
        String::new()
    };

    let mut config = format!(
        r#"# AHNARA Configuration

[agent]
name = "{}"
{}max_tokens = 8192
temperature = {}
max_tool_iterations = 100
context_window_tokens = 20000
timezone = "UTC"

[providers]
connection_pool_size = 32
request_timeout_secs = 120

{}[memory]
database_path = "~/.ahnara/memory.db"
hot_cache_size = 1000
session_max_messages = 100
consolidation_interval_secs = 300

[channels.telegram]
enabled = {}
token = "{}"
group_policy = "mention"

[channels.discord]
enabled = {}
token = "{}"
group_policy = "mention"
allowed_guilds = []

[channels.slack]
enabled = false
bot_token = ""
app_token = ""

[tools]
exec_enabled = true
exec_timeout_secs = 60
restrict_to_workspace = true
web_search_enabled = true
web_search_provider = "brave"

[server]
host = "0.0.0.0"
port = 18789
cors_enabled = true

[mcp]
enabled = true
"#,
        agent_name,
        model_line,
        temperature,
        provider_block,
        telegram_token.is_some() && !telegram_token.unwrap_or("").trim().is_empty(),
        telegram_token.unwrap_or("").trim(),
        discord_token.is_some() && !discord_token.unwrap_or("").trim().is_empty(),
        discord_token.unwrap_or("").trim()
    );

    // Add GitHub MCP server if token provided
    if github_token.is_some() {
        config.push_str(&format!(r#"
[[mcp.servers]]
name = "github"
command = "mcp-server-github"
args = []
tool_prefix = "github"
timeout_secs = 30

[mcp.servers.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "{}"
"#, github_token.unwrap()));
    }

    // Add extra MCP servers
    for (name, command, args) in extra_mcp {
        let args_str: Vec<String> = args.iter().map(|a| format!("\"{}\"", a)).collect();
        config.push_str(&format!(r#"
[[mcp.servers]]
name = "{}"
command = "{}"
args = [{}]
tool_prefix = "{}"
timeout_secs = 30
"#, name, command, args_str.join(", "), name));
    }

    config
}

fn bail_non_tty() -> anyhow::Error {
    anyhow::anyhow!({
        "The wizard requires a terminal. Run with: ssh -t, script(1), or set the new non-interactive flags/env vars."
    })
}

fn non_interactive_setup(
    config_dir: &PathBuf,
    config_path: &PathBuf,
    opts: &NonInteractiveOptions,
) -> Result<()> {
    if !config_dir.exists() {
        fs::create_dir_all(config_dir)?;
        fs::create_dir_all(config_dir.join("skills"))?;
        fs::create_dir_all(config_dir.join("memory"))?;
    }

    let provider = opts.provider.as_deref().unwrap_or("");
    let model = opts.model.as_deref().unwrap_or("");
    let api_base = match opts.provider.as_deref() {
        Some("nvidia") => "https://integrate.api.nvidia.com/v1",
        Some("openai") => "https://api.openai.com/v1",
        Some("anthropic") => "https://api.anthropic.com/v1",
        Some("openrouter") => "https://openrouter.ai/api/v1",
        Some("groq") => "https://api.groq.com/openai/v1",
        Some("deepseek") => "https://api.deepseek.com/v1",
        Some(other) if !other.is_empty() => bail!("Unsupported provider: {}. Use --base to specify a custom API base URL.", other),
        _ => "",
    };
    let api_key = opts.api_key.as_deref().unwrap_or("");
    let telegram_token = opts.telegram_token.as_deref();
    let discord_token = opts.discord_token.as_deref();
    let github_token = opts.github_token.as_deref();
    let extra_mcp = &[];

    let config = generate_config(
        "AHNARA",
        provider,
        api_base,
        model,
        api_key,
        1.0,
        if telegram_token.is_some() && !telegram_token.unwrap_or("").trim().is_empty() { Some(telegram_token.unwrap()) } else { None },
        if discord_token.is_some() && !discord_token.unwrap_or("").trim().is_empty() { Some(discord_token.unwrap()) } else { None },
        if github_token.is_some() && !github_token.unwrap_or("").trim().is_empty() { Some(github_token.unwrap()) } else { None },
        extra_mcp,
    );

    fs::write(config_path, &config)?;
    if let Err(e) = restrict_permissions(config_path) {
        eprintln!("Warning: could not restrict permissions on config: {e}");
    }

    let mut enabled_telegram = false;
    let mut enabled_discord = false;
    let mut enabled_github = false;
    if telegram_token.is_some() && !telegram_token.unwrap_or("").trim().is_empty() {
        enabled_telegram = true;
    }
    if discord_token.is_some() && !discord_token.unwrap_or("").trim().is_empty() {
        enabled_discord = true;
    }
    if github_token.is_some() && !github_token.unwrap_or("").trim().is_empty() {
        enabled_github = true;
    }

    println!("\nSummary:");
    if !provider.is_empty() {
        println!("  Provider: {}", provider);
    }
    if !model.is_empty() {
        println!("  Model: {}", model);
    }
    println!("  Telegram: {}", if enabled_telegram { "enabled" } else { "disabled" });
    println!("  Discord: {}", if enabled_discord { "enabled" } else { "disabled" });
    println!("  GitHub MCP: {}", if enabled_github { "enabled" } else { "disabled" });
    println!("Configuration saved to {:?}", config_path);
    println!("Next steps: Run `ahnara gateway` to start.");
    if api_key.is_empty() {
        println!("Set your API key: export OPENAI_API_KEY=your-key");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn make_opts() -> NonInteractiveOptions {
        NonInteractiveOptions {
            provider: Some("openai".into()),
            model: Some("gpt-4o".into()),
            api_key: Some("sk-test".into()),
            telegram_token: None,
            discord_token: None,
            github_token: None,
        }
    }

    #[test]
    fn non_interactive_setup_writes_config() {
        let tmp = env::temp_dir().join(format!("ahnara-setup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let config_path = tmp.join("config.toml");
        let opts = make_opts();

        non_interactive_setup(&tmp, &config_path, &opts).expect("setup should succeed");

        assert!(config_path.exists(), "config.toml must be created");
        let body = fs::read_to_string(&config_path).unwrap();
        assert!(body.contains("openai"), "config must contain provider name");
        assert!(body.contains("sk-test"), "config must contain api key");
        assert!(body.contains("gpt-4o"), "config must contain model");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn handle_setup_with_non_interactive_opts_skips_tty() {
        // Even with stdin closed (the test harness has no TTY), passing any
        // non-interactive option must succeed without ever calling bail_non_tty.
        let tmp = env::temp_dir().join(format!("ahnara-setup-no-tty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let config_path = tmp.join("config.toml");

        let result = handle_setup_with(false, false, false, make_opts());
        assert!(result.is_ok(), "non-interactive path must not require a TTY: {result:?}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bail_non_tty_message_mentions_terminal() {
        let err = bail_non_tty();
        let msg = err.to_string();
        assert!(msg.contains("terminal"), "bail message must mention 'terminal', got: {msg}");
    }

    #[test]
    fn sanitize_api_key_redacts_empty_and_placeholder() {
        // Empty key becomes the canonical placeholder
        assert!(sanitize_api_key("", "X").contains("set via ahnara token"));
        // Known no-op placeholders also become the canonical placeholder
        assert_eq!(sanitize_api_key("changeme", "X"), api_key_placeholder());
        assert_eq!(sanitize_api_key("TODO", "X"), api_key_placeholder());
        assert_eq!(sanitize_api_key("your-key-here", "X"), api_key_placeholder());
        // A real key is returned as-is
        assert_eq!(sanitize_api_key("sk-abcdef123456", "X"), "sk-abcdef123456");
    }

    #[test]
    fn restrict_permissions_creates_file_with_600() {
        let tmp = env::temp_dir().join(format!("ahnara-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("config.toml");
        fs::write(&p, "secret=1").unwrap();
        // Apply restrictive perms
        restrict_permissions(&p).expect("chmod should succeed on unix");
        // Verify on unix only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "config file must be chmod 600, got {mode:o}");
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn interactive_config_does_not_contain_real_key_when_none_provided() {
        let tmp = env::temp_dir().join(format!("ahnara-no-key-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let config_path = tmp.join("config.toml");
        let opts = NonInteractiveOptions {
            provider: Some("openai".into()),
            model: Some("gpt-4o".into()),
            api_key: None, // user did not pass --api-key
            telegram_token: None,
            discord_token: None,
            github_token: None,
        };
        non_interactive_setup(&tmp, &config_path, &opts).expect("setup should succeed");
        let body = fs::read_to_string(&config_path).unwrap();
        // The actual key should NOT be present (it was never provided), and the
        // placeholder should be in its place so the user can search for it.
        assert!(
            body.contains(api_key_placeholder()),
            "config must contain the placeholder when no api key was given, body: {body}"
        );
        // And it must NOT contain a stray real key (e.g. from env or default).
        assert!(!body.contains("sk-live"), "config must not contain a hardcoded key");
        let _ = fs::remove_dir_all(&tmp);
    }
}
