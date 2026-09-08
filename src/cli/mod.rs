//! AHNARA CLI - User-friendly command interface

pub mod memory;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ahnara")]
#[command(author, version, about, long_about = None)]
#[command(next_line_help = true)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, global = true, default_value = "~/.ahnara/config.toml")]
    pub config: PathBuf,

    /// Enable debug logging
    #[arg(short, long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the gateway server
    Gateway {
        #[arg(short, long, default_value = "18789")]
        port: u16,
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
    },
    /// Chat with the agent
    Chat {
        message: Option<String>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        stream: bool,
    },
    /// Interactive setup wizard
    Setup {
        #[arg(short, long)]
        quick: bool,
        #[arg(long)]
        telegram: bool,
        #[arg(long)]
        discord: bool,
        /// Non-interactive: provider name (nvidia|openai|anthropic|...)
        #[arg(long)]
        provider: Option<String>,
        /// Non-interactive: model ID (e.g. gpt-4o, claude-3, gemma-7b)
        #[arg(long)]
        model: Option<String>,
        /// Non-interactive: API key for the provider
        #[arg(long)]
        api_key: Option<String>,
        /// Non-interactive: Telegram bot token
        #[arg(long)]
        telegram_token: Option<String>,
        /// Non-interactive: Discord bot token
        #[arg(long)]
        discord_token: Option<String>,
        /// Non-interactive: GitHub token
        #[arg(long)]
        github_token: Option<String>,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },
    /// Manage skills
    Skill {
        #[command(subcommand)]
        action: SkillCommands,
    },
    /// Manage providers
    Provider {
        #[command(subcommand)]
        action: ProviderCommands,
    },
    /// Manage persona
    Persona {
        #[command(subcommand)]
        action: PersonaCommands,
    },
    /// Show system status
    Status {
        /// Show delegation statistics
        #[arg(long)]
        delegation: bool,
    },
    /// Start a coding session in an isolated workspace
    Code {
        /// Initial message/task for the coding agent
        #[arg(trailing_var_arg = true)]
        task: Vec<String>,

        /// Project name for the workspace
        #[arg(short, long)]
        project: Option<String>,

        /// Resume an existing coding session
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Override model/provider settings for your session
    /// Usage: ahnara model [model_id] [--base URL] [--key API_KEY]
    Model {
        /// Model ID (e.g. gpt-4o, claude-3, gemma-7b)
        model_id: Option<String>,
        /// Provider base URL (e.g. https://api.openai.com/v1)
        #[arg(long)]
        base: Option<String>,
        /// API key for the provider
        #[arg(long)]
        key: Option<String>,
        /// Clear model override and revert to defaults
        #[arg(long)]
        reset: bool,
        /// Show current override without changing it
        #[arg(long)]
        show: bool,
    },
    /// Run a skill
    Run {
        skill: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Create a structured task plan from a goal
    Plan {
        /// Goal to turn into a plan skeleton
        goal: String,
        /// Output plan JSON path
        #[arg(short, long, default_value = "ahnara-plan.json")]
        output: PathBuf,
    },
    /// Execute a structured task plan DAG
    RunPlan {
        /// Plan JSON/YAML path
        path: PathBuf,
        /// Run database path
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Inspect persistent run history
    Runs {
        #[command(subcommand)]
        action: RunsCommands,
        /// Run database path
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Show runtime capability manifest
    Capabilities {
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage MCP server integrations
    Mcp {
        /// Subcommand: list, add, remove, enable, disable, tools, help
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Manage secure API tokens
    Token {
        /// Token subcommand and args (e.g. "set github_token ghp_xxxx")
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Update ahnara to the latest version
    Update,
    /// Stop the gateway server
    Stop,
    /// Inspect and manage agent memory
    Memory {
        #[command(subcommand)]
        action: memory::MemorySubcommand,
    },
    /// View gateway logs
    Logs {
        /// Number of lines to show
        #[arg(default_value_t = 50)]
        lines: usize,
        /// Filter lines containing this pattern
        #[arg(short, long)]
        filter: Option<String>,
        /// Show only errors and warnings
        #[arg(short, long)]
        errors: bool,
        /// Clear the log file
        #[arg(long)]
        clear: bool,
    },
    /// Manage scheduled jobs
    Schedule {
        /// Subcommand: list, add, remove, enable, disable, info
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum RunsCommands {
    /// List recent runs
    List {
        /// Maximum number of runs to list
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Show one run with steps and events
    Show {
        /// Run id
        id: String,
    },

    /// Export a run as JSON
    Export {
        /// Run id
        id: String,
        /// Output file; prints to stdout if omitted
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Replay metadata for a run
    Replay {
        /// Run id
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show {
        /// Output format
        #[arg(short, long, default_value = "toml")]
        format: String,
    },

    /// Set a configuration value
    Set {
        /// Configuration key (e.g., "agent.temperature")
        key: String,

        /// Configuration value
        value: String,
    },

    /// Get a configuration value
    Get {
        /// Configuration key
        key: String,
    },

    /// Edit configuration in editor
    Edit,

    /// Reset to defaults
    Reset {
        /// Confirm reset
        #[arg(short, long)]
        yes: bool,
    },

    /// Validate configuration
    Validate,
}

#[derive(Subcommand)]
pub enum SkillCommands {
    /// List installed skills
    List {
        /// Show detailed info
        #[arg(short = 'l', long)]
        detailed: bool,
    },

    /// Search for skills in the registry
    Search {
        /// Search query
        query: String,
    },

    /// Install a skill from registry or URL
    Install {
        /// Skill name to install from registry
        name: Option<String>,

        /// Install from GitHub URL
        #[arg(short, long)]
        url: Option<String>,

        /// Install from git repository
        #[arg(short, long)]
        git: Option<String>,
    },

    /// Uninstall a skill
    Uninstall {
        /// Skill name
        name: String,
    },

    /// Create a new skill
    Create {
        /// Skill name
        name: String,

        /// Description
        #[arg(short = 't', long)]
        description: Option<String>,
    },

    /// Update a skill from registry
    Update {
        /// Skill name
        name: String,
    },

    /// Browse available skills in registry
    Browse,

    /// Show skill info
    Info {
        /// Skill name
        name: String,
    },

    /// Manage skill registry taps
    Tap {
        #[command(subcommand)]
        action: SkillTapCommands,
    },
}

#[derive(Subcommand)]
pub enum SkillTapCommands {
    /// List configured skill taps
    List,

    /// Add a skill tap manifest URL
    Add {
        /// Tap name
        name: String,

        /// Manifest URL
        url: String,

        /// Optional manifest sha256 checksum
        #[arg(long)]
        sha256: Option<String>,

        /// Higher priority taps win on duplicate skill names
        #[arg(long, default_value_t = 0)]
        priority: i32,
    },

    /// Remove a skill tap
    Remove {
        /// Tap name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum ProviderCommands {
    /// List all available providers
    List,

    /// Show current active provider
    Active,

    /// Switch to a different provider
    Use {
        /// Provider name to switch to
        name: String,
    },

    /// Add a new provider
    Add {
        /// Provider name
        name: String,

        /// API base URL
        #[arg(long)]
        base: String,

        /// API key (will prompt if not provided)
        #[arg(short, long)]
        key: Option<String>,
    },

    /// Remove a provider
    Remove {
        /// Provider name to remove
        name: String,
    },

    /// Test provider connection
    Test {
        /// Provider name (tests all if not specified)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PersonaCommands {
    /// Show current persona
    Show,

    /// Edit persona
    Edit,

    /// Set persona name
    Name {
        /// New name
        name: String,
    },

    /// Set persona behavior
    Behavior {
        /// Behavior instructions
        text: String,
    },

    /// Set response style
    Style {
        /// Length: concise, balanced, detailed
        #[arg(short, long)]
        length: Option<String>,

        /// Tone: professional, casual, technical, friendly
        #[arg(short, long)]
        tone: Option<String>,

        /// No em dashes
        #[arg(long)]
        no_em_dashes: bool,

        /// No emojis
        #[arg(long)]
        no_emojis: bool,
    },

    /// Load persona from file
    Load {
        /// Path to PERSONA.md file
        file: String,
    },

    /// Save current persona to file
    Save {
        /// Output file path
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Reset to default persona
    Reset,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
