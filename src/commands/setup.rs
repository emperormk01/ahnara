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

    if quick {
        return quick_setup(&config_dir, telegram, discord);
    }

    if has_non_interactive_options(&non_interactive) {
        return non_interactive_setup(&config_dir, &config_path, &non_interactive);
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(bail_non_tty());
    }

    interactive_setup(&config_dir, &config_path, telegram, discord)
}

fn has_non_interactive_options(non_interactive: &NonInteractiveOptions) -> bool {
    non_interactive.provider.is_some()
        || non_interactive.model.is_some()
        || non_interactive.api_key.is_some()
        || non_interactive.telegram_token.is_some()
        || non_interactive.discord_token.is_some()
        || non_interactive.github_token.is_some()
}

fn interactive_setup(
    config_dir: &std::path::Path,
    config_path: &std::path::Path,
    telegram: bool,
    discord: bool,
) -> Result<()> {
    println!("This wizard will help you configure AHNARA.\n");

    if !config_dir.exists() {
        fs::create_dir_all(config_dir)?;
        fs::create_dir_all(config_dir.join("skills"))?;
        fs::create_dir_all(config_dir.join("memory"))?;
        println!("Created config directory: {:?}", config_dir);
    }

    let agent_name = prompt_agent_name()?;
    let (provider_name, api_base) = prompt_provider()?;
    let model = prompt_model()?;
    let api_key = prompt_api_key()?;
    let temperature = prompt_temperature()?;
    let (enable_telegram, telegram_token) = prompt_telegram(telegram)?;
    let (enable_discord, discord_token) = prompt_discord(discord)?;
    let (enable_github_mcp, github_token) = prompt_github_mcp()?;
    let extra_mcp_servers = prompt_extra_mcp_servers(enable_github_mcp)?;

    save_config(
        config_dir,
        config_path,
        &agent_name,
        &provider_name,
        &api_base,
        &model,
        &api_key,
        temperature,
        enable_telegram,
        telegram_token.as_deref(),
        enable_discord,
        discord_token.as_deref(),
        enable_github_mcp,
        github_token.as_deref(),
        &extra_mcp_servers,
    )?;

    println!("\nSetup complete! Config saved to: {:?}", config_path);
    println!("Run `ahnara` to start your agent.");

    Ok(())
}

fn prompt_agent_name() -> Result<String> {
    Ok(Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Agent name")
        .default("AHNARA".into())
        .interact_text()?)
}

fn prompt_provider() -> Result<(String, String)> {
    let providers = vec![
        "OpenAI", "Anthropic", "Google Gemini", "OpenRouter (multi-model)",
        "Groq", "NVIDIA", "Custom endpoint",
    ];

    let provider_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select your LLM provider")
        .items(&providers)
        .interact()?;

    match provider_idx {
        0 => Ok(("openai".into(), "https://api.openai.com/v1".into())),
        1 => Ok(("anthropic".into(), "https://api.anthropic.com/v1".into())),
        2 => Ok(("google".into(), "https://generativelanguage.googleapis.com/v1beta/openai".into())),
        3 => Ok(("openrouter".into(), "https://openrouter.ai/api/v1".into())),
        4 => Ok(("groq".into(), "https://api.groq.com/openai/v1".into())),
        5 => Ok(("nvidia".into(), "https://integrate.api.nvidia.com/v1".into())),
        6 => {
            let base: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("API base URL")
                .interact_text()?;
            Ok(("custom".into(), base))
        }
        _ => bail!("Invalid selection"),
    }
}

fn prompt_model() -> Result<String> {
    Ok(Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Model ID (e.g. gpt-4o, claude-3-5-sonnet-20241022, gemini-1.5-flash)")
        .interact_text()?)
}

fn prompt_api_key() -> Result<String> {
    Ok(Input::with_theme(&ColorfulTheme::default())
        .with_prompt("API Key")
        .interact_text()?)
}

fn prompt_temperature() -> Result<f32> {
    let temp_str: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Temperature (0.0-2.0)")
        .default("1.0".into())
        .interact_text()?;
    Ok(temp_str.parse().unwrap_or(1.0))
}

fn prompt_telegram(force_enable: bool) -> Result<(bool, Option<String>)> {
    let enable = force_enable || Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable Telegram?")
        .default(false)
        .interact()?;

    let token = if enable {
        Some(Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Telegram Bot Token")
            .allow_empty(true)
            .interact_text()?)
    } else {
        None
    };

    Ok((enable, token))
}

fn prompt_discord(force_enable: bool) -> Result<(bool, Option<String>)> {
    let enable = force_enable || Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable Discord?")
        .default(false)
        .interact()?;

    let token = if enable {
        Some(Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Discord Bot Token")
            .allow_empty(true)
            .interact_text()?)
    } else {
        None
    };

    Ok((enable, token))
}

fn prompt_github_mcp() -> Result<(bool, Option<String>)> {
    let enable = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable GitHub integration (MCP)?")
        .default(false)
        .interact()?;

    let token = if enable {
        println!("  To create a token: GitHub Settings > Developer settings > Personal access tokens");
        println!("  Required scopes: repo, read:org, read:user\n");
        Some(Input::with_theme(&ColorfulTheme::default())
            .with_prompt("GitHub Personal Access Token")
            .interact_text()?)
    } else {
        None
    };

    Ok((enable, token))
}

fn prompt_extra_mcp_servers(github_enabled: bool) -> Result<Vec<(String, String, Vec<String>)>> {
    let mut servers = Vec::new();
    if !github_enabled {
        let add_more = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Add other MCP servers? (You can also use /mcp add later)")
            .default(false)
            .interact()?;

        if add_more {
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

                let args = if args_str.is_empty() {
                    Vec::new()
                } else {
                    args_str.split_whitespace().map(String::from).collect()
                };

                servers.push((name, command, args));
            }
        }
    }
    Ok(servers)
}

fn save_config(
    config_dir: &std::path::Path,
    config_path: &std::path::Path,
    agent_name: &str,
    provider_name: &str,
    api_base: &str,
    model: &str,
    api_key: &str,
    temperature: f32,
    enable_telegram: bool,
    telegram_token: Option<&str>,
    enable_discord: bool,
    discord_token: Option<&str>,
    enable_github_mcp: bool,
    github_token: Option<&str>,
    extra_mcp_servers: &[(String, String, Vec<String>)],
) -> Result<()> {
    if github_token.is_none() {
        println!("\nYou can set tokens later with:");
        println!("  ahnara token set GITHUB_TOKEN <your-token>");
        println!("  Or use /token set GITHUB_TOKEN <your-token> in Telegram/Discord\n");
    }

    let config = generate_config(
        agent_name, provider_name, api_base, model, api_key, temperature,
        telegram_token, discord_token, github_token, extra_mcp_servers,
    );

    fs::write(config_path, &config)?;
    if let Err(e) = restrict_permissions(config_path) {
        eprintln!("Warning: could not restrict permissions on config: {e}");
    }
    println!("\nConfiguration saved to {:?}", config_path);

    if github_token.is_some() {
        let token_dir = config_dir.join("tokens.json");
        let mut tokens = serde_json::Map::new();
        if let Some(t) = github_token {
            tokens.insert("GITHUB_TOKEN".to_string(), serde_json::Value::String(t.into()));
        }
        let token_json = serde_json::to_string_pretty(&tokens)?;
        fs::write(&token_dir, token_json)?;
        println!("Tokens saved to {:?}", token_dir);
    }

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
        println!("  GitHub MCP: enabled");
    }
    if !extra_mcp_servers.is_empty() {
        println!("  Extra MCP servers: {}", extra_mcp_servers.len());
    }

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
    _env_var: &str,
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

    let provider_block = generate_provider_block(provider, api_base, api_key);
    let telegram_block = generate_telegram_block(telegram_token);
    let discord_block = generate_discord_block(discord_token);
    let mcp_block = generate_mcp_block(github_token, extra_mcp);

    format!(
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

[sub_agents]
enabled = true
min_complexity = 50
max_budget = 30000
max_concurrent = 5
timeout_secs = 60
fallback_on_error = true
track_cost = true

{}[memory]
database_path = "~/.ahnara/memory.db"
hot_cache_size = 1000

{}{}[server]
host = "0.0.0.0"
port = 18789

{}"#,
        agent_name, model_line, temperature, provider_block,
        telegram_block, discord_block, mcp_block
    )
}

fn generate_provider_block(provider: &str, api_base: &str, api_key: &str) -> String {
    if !provider.is_empty() && !api_base.is_empty() {
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
    }
}

fn generate_telegram_block(telegram_token: Option<&str>) -> String {
    let (enabled, token) = match telegram_token {
        Some(t) => (true, t),
        None => (false, ""),
    };
    format!(
        r#"[channels.telegram]
enabled = {}
token = "{}"
group_policy = "mention"

"#,
        enabled, token
    )
}

fn generate_discord_block(discord_token: Option<&str>) -> String {
    let (enabled, token) = match discord_token {
        Some(t) => (true, t),
        None => (false, ""),
    };
    format!(
        r#"[channels.discord]
enabled = {}
token = "{}"
allowed_guilds = []

"#,
        enabled, token
    )
}

fn generate_mcp_block(github_token: Option<&str>, extra_mcp: &[(String, String, Vec<String>)]) -> String {
    let mut mcp_servers = Vec::new();

    if let Some(token) = github_token {
        mcp_servers.push(format!(
            r#"[[mcp.servers]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = {{ GITHUB_TOKEN = "{}" }}"#,
            token
        ));
    }

    for (name, command, args) in extra_mcp {
        let args_str: Vec<String> = args.iter().map(|a| format!("\"{}\"", a)).collect();
        mcp_servers.push(format!(
            r#"[[mcp.servers]]
name = "{}"
command = "{}"
args = [{}]"#,
            name, command, args_str.join(", ")
        ));
    }

    if mcp_servers.is_empty() {
        String::new()
    } else {
        format!(
            r#"[mcp]
enabled = true

{}
"#,
            mcp_servers.join("\n\n")
        )
    }
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
    let api_base = resolve_provider_base(provider)?;
    let api_key = opts.api_key.as_deref().unwrap_or("");
    let telegram_token = opts.telegram_token.as_deref().filter(|t| !t.trim().is_empty());
    let discord_token = opts.discord_token.as_deref().filter(|t| !t.trim().is_empty());
    let github_token = opts.github_token.as_deref().filter(|t| !t.trim().is_empty());
    let extra_mcp = &[];

    let config = generate_config(
        "AHNARA", provider, api_base, model, api_key, 1.0,
        telegram_token, discord_token, github_token, extra_mcp,
    );

    fs::write(config_path, &config)?;
    if let Err(e) = restrict_permissions(config_path) {
        eprintln!("Warning: could not restrict permissions on config: {e}");
    }

    print_setup_summary(provider, model, telegram_token, discord_token, github_token, config_path, api_key);
    Ok(())
}

fn resolve_provider_base(provider: &str) -> Result<&'static str> {
    match provider {
        "nvidia" => Ok("https://integrate.api.nvidia.com/v1"),
        "openai" => Ok("https://api.openai.com/v1"),
        "anthropic" => Ok("https://api.anthropic.com/v1"),
        "openrouter" => Ok("https://openrouter.ai/api/v1"),
        "groq" => Ok("https://api.groq.com/openai/v1"),
        "deepseek" => Ok("https://api.deepseek.com/v1"),
        "" => Ok(""),
        other => bail!("Unsupported provider: {}. Use --base to specify a custom API base URL.", other),
    }
}

fn print_setup_summary(
    provider: &str,
    model: &str,
    telegram_token: Option<&str>,
    discord_token: Option<&str>,
    github_token: Option<&str>,
    config_path: &PathBuf,
    api_key: &str,
) {
    println!("\nSummary:");
    if !provider.is_empty() {
        println!("  Provider: {}", provider);
    }
    if !model.is_empty() {
        println!("  Model: {}", model);
    }
    println!("  Telegram: {}", if telegram_token.is_some() { "enabled" } else { "disabled" });
    println!("  Discord: {}", if discord_token.is_some() { "enabled" } else { "disabled" });
    println!("  GitHub MCP: {}", if github_token.is_some() { "enabled" } else { "disabled" });
    println!("Configuration saved to {:?}", config_path);
    println!("Next steps: Run `ahnara gateway` to start.");
    if api_key.is_empty() {
        println!("Set your API key: export OPENAI_API_KEY=your-key");
    }
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
        let _config_path = tmp.join("config.toml");
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
