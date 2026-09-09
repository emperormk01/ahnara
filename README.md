# ahnara

**Ultra-High-Performance AI Agent Framework**

![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)

![License: PolyForm Noncommercial](https://img.shields.io/badge/License-PolyForm%20Noncommercial-blue.svg)

---

## Introduction

ahnara is a high-performance, Rust-native AI agent framework designed for building intelligent, autonomous systems that can plan, execute, and interact across multiple channels. It combines zero-cost abstractions, parallel tool execution, and multi-provider support to deliver real-time responses with minimal latency.

Key architectural choices include:

- **Rust native** -- zero-cost abstractions, no GC pauses
- **DAG tool execution** -- independent tools run in parallel
- **3-tier memory** -- LRU cache + SQLite + vector search
- **Multi-provider** -- NVIDIA, OpenAI, Anthropic, OpenRouter, Groq, and more
- **Skill system** -- compatible with [agentskills.io](https://agentskills.io)
- **Zero-copy streaming** -- direct SSE passthrough for real-time responses
- **Multi-channel** -- Telegram, Discord, and HTTP API with optional bearer auth
- **MCP client** -- connect to stdio MCP servers and use their tools
- **Planner DAG** -- structured task planning with auditable run database
- **Plugin hooks** -- lifecycle event system for external plugins
- **Cron scheduler** -- autonomous recurring jobs inside the gateway
- **Context pruning** -- smart conversation history management
- **Persona system** -- customizable agent personality and behavior
- **Code mode** -- isolated coding agent with its own workspace

---

## Concepts You'll Need

ahnara operates on a core mental model where the agent:

**Planning and execution.** The agent uses a structured DAG (Directed Acyclic Graph) to plan tasks, breaking down goals into actionable steps. It executes tools in parallel when possible to maximize efficiency.

**Memory and context.** The agent remembers context across sessions using a 3-tier memory system: a fast LRU cache for recent data, SQLite for persistent storage, and vector search for semantic retrieval.

**Adaptation and behavior.** The agent adapts its behavior through persona and skill configurations, allowing it to customize its personality and capabilities for different tasks.

**Streaming and interaction.** The agent streams responses in real-time via Server-Sent Events (SSE) for low-latency interaction, enabling smooth, conversational exchanges.

---

## Getting Started in 5 Minutes

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/emperormk01/Ahnara/master/get.sh | bash
```

Downloads a pre-built static binary for your platform (Linux or macOS, x86_64 or aarch64). No Rust required.

### 2. Configure a provider

ahnara works with any OpenAI-compatible endpoint. Pick one:

**NVIDIA NIM (free tier available at build.nvidia.com):**
```bash
export NVIDIA_API_KEY="nvapi-..."
```

**OpenRouter (any model, pay-per-token):**
```bash
export OPENROUTER_API_KEY="sk-or-v1-..."
```

**Any custom endpoint:**
```bash
export OPENAI_API_KEY="your-key"
# Then edit ~/.ahnara/config.toml and set:
# api_base = "https://your-endpoint/v1"
```

### 3. First chat

```bash
ahnara chat "What's the capital of France?"
```

The agent responds immediately. Each message stays in a session -- quit and restart, the conversation continues.

### 4. Start the gateway (persistent agent)

```bash
ahnara gateway
```

This starts ahnara as a server on port 18789. From here you can:
- Connect Telegram/Discord bots
- Call the HTTP API from other apps
- Run scheduled cron jobs
- Let autonomous tasks execute in the background

### 5. Connect Telegram (optional, 2 minutes)

```toml
# Add to ~/.ahnara/config.toml:
[channels.telegram]
enabled = true
# Set token via env var: export TELEGRAM_BOT_TOKEN="..."
```

Create a bot via [@BotFather](https://t.me/BotFather), paste the token, restart the gateway. Message your bot and ahnara responds.

### What next?

- `ahnara skill search <keyword>` -- install community skills (web scrapers, automation workflows)
- `ahnara code "fix the login bug"` -- spawn an isolated coding agent with its own workspace
- `/model list` in chat -- switch to any model mid-conversation

---

## Features

- **Rust native** -- zero-cost abstractions, no GC pauses
- **DAG tool execution** -- independent tools run in parallel
- **3-tier memory** -- LRU cache + SQLite + vector search
- **Multi-provider** -- NVIDIA, OpenAI, Anthropic, OpenRouter, Groq, and more
- **Skill system** -- compatible with [agentskills.io](https://agentskills.io)
- **Zero-copy streaming** -- direct SSE passthrough for real-time responses
- **Multi-channel** -- Telegram, Discord, and HTTP API with optional bearer auth
- **MCP client** -- connect to stdio MCP servers and use their tools
- **Planner DAG** -- structured task planning with auditable run database
- **Plugin hooks** -- lifecycle event system for external plugins
- **Cron scheduler** -- autonomous recurring jobs inside the gateway
- **Context pruning** -- smart conversation history management
- **Persona system** -- customizable agent personality and behavior
- **Code mode** -- isolated coding agent with its own workspace

---

## Daily Usage

The commands you'll actually use day to day:

```bash
# Check system status
ahnara status

# Install a skill from the registry
ahnara skill search stock
ahnara skill install stock-analyzer

# Switch model mid-session
/model list          # in chat
/model
```

---

## CLI Commands

| Command | Description |
| --- | --- |
| `ahnara gateway` | Start the gateway server (default port 18789) |
| `ahnara chat [message]` | Chat with the agent (interactive REPL or one-shot) |
| `ahnara setup` | Interactive setup wizard |
| `ahnara status` | Show system status |
| `ahnara code [task]` | Start a coding session in an isolated workspace |
| `ahnara model [id]` | Override model/provider settings for your session |
| `ahnara skill <sub>` | Manage skills (list, view, create, install, search, browse) |
| `ahnara provider <sub>` | Manage providers (list, test) |
| `ahnara persona <sub>` | Manage persona (show, set, list) |
| `ahnara config <sub>` | Manage configuration (show, set, get, edit) |
| `ahnara mcp <sub>` | Manage MCP servers (list, add, remove, enable, disable, tools) |
| `ahnara capabilities` | Show runtime capability manifest |
| `ahnara plan <goal>` | Create a structured task plan from a goal |
| `ahnara run-plan <file>` | Execute a structured task plan DAG |
| `ahnara runs <sub>` | Inspect persistent run history (list, show, export) |
| `ahnara run <skill>` | Run a skill |
| `ahnara update` | Self-update to latest version |

### In-Chat Commands

| Command | Description |
| --- | --- |
| `/token` | Set/list/remove/get/forget API keys |
| `/mcp` | Add/remove/list/enable/disable MCP servers |
| `/model` | Switch LLM model per session |
| `/code` | Enter coding agent mode |
| `/normal` | Exit coding mode, return to normal chat |
| `/memory` | View agent memory |
| `/stop` | Stop current agent operation |

All commands work in Telegram, Discord, and CLI.

---

## Gateway API

```bash
ahnara gateway --port 8080
```

**Authentication**: Off by default. Set `ahnara_REQUIRE_AUTH=true` and `ahnara_API_KEY=<secret>` to require bearer auth on all routes except `/health`.

| Endpoint | Method | Description |
| --- | --- | --- |
| `/health` | GET | Health check |
| `/chat` | POST | Chat with ahnara |
| `/stream` | POST | Streaming chat (SSE) |
| `/skills` | GET | List installed skills |
| `/tools` | GET | List available tools |
| `/api/capabilities` | GET | Runtime capability manifest |
| `/api/reflect` | GET | Reflect current state |
| `/api/reflections` | GET | List reflections |
| `/api/sessions/:id/history` | GET | Session history |

---

## Configuration

Config file: `file ~/.ahnara/config.toml`

```toml
[agent]
name = "ahnara"
default_model = "stepfun-ai/step-3.5-flash"
temperature = 1.0
max_tokens = 8192
recent_history_turns = 10
context_window_tokens = 20000
tool_output_max_chars = 4000

[providers.primary]
name = "nvidia"
api_base = "https://integrate.api.nvidia.com/v1"
# api_key via NVIDIA_API_KEY env var

[memory]
database_path = "~/.ahnara/memory.db"
hot_cache_size = 1000

[channels.telegram]
enabled = false
# token via TELEGRAM_BOT_TOKEN env var

[channels.discord]
enabled = false
# token via DISCORD_BOT_TOKEN env var

[server]
port = 18789
```

### Environment Variables

| Variable | Description |
| --- | --- |
| `NVIDIA_API_KEY` | NVIDIA API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `GROQ_API_KEY` | Groq API key |
| `TELEGRAM_BOT_TOKEN` | Telegram bot token |
| `DISCORD_BOT_TOKEN` | Discord bot token |
| `ahnara_REQUIRE_AUTH` | Require bearer auth on API routes |
| `ahnara_API_KEY` | API key for bearer auth |

---

## MCP Client

Connect to stdio MCP servers and use their tools natively.

```toml
[mcp]
enabled = true

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/workspace"]
tool_prefix = "fs"
timeout_secs = 30
```

| Field | Description |
| --- | --- |
| `name` | Unique server name |
| `command` | Stdio MCP server command |
| `args` | Command arguments |
| `env` | Optional environment variables |
| `tool_prefix` | Optional local tool prefix override |
| `include_tools` | Optional allowlist of remote tool names |
| `exclude_tools` | Optional denylist of remote tool names |
| `timeout_secs` | Per-request timeout |

---

## Skills Hub / Taps

Merge skills from multiple registry manifests. The official Auxlo registry is enabled by default.

```bash
ahnara skill tap list
ahnara skill tap add community https://example.com/manifest.json --priority 10
ahnara skill tap add pinned https://example.com/manifest.json --sha256 <hash>
ahnara skill search debugging
ahnara skill browse
```

---

## Plugin Hooks

Run external plugin commands on lifecycle events. Plugins receive JSON on stdin and may return JSON on stdout to rewrite messages, rewrite tool args, or cancel tools.

```toml
[plugins]
enabled = true
timeout_secs = 10

[[plugins.plugins]]
name = "audit-log"
enabled = true
command = "python3"
args = ["/path/to/audit.py"]
hooks = ["startup", "before_message", "after_message", "before_tool", "after_tool"]
timeout_secs = 5
```

Supported hooks: `startup`, `before_message`, `after_message`, `before_tool`, `after_tool`, `shutdown`.

---

## Cron Scheduler

Run autonomous recurring jobs inside the gateway process.

```toml
[scheduler]
enabled = true

[[scheduler.jobs]]
name = "daily-summary"
cron = "0 0 9 * * *"
prompt = "Review active sessions and produce a daily summary."
session_id = "scheduler:daily-summary"
enabled = true
run_on_startup = false
timeout_secs = 300
```

Cron expressions use six-field seconds format: `sec min hour day month weekday`.

---

## Planner DAG

Structured task planning with auditable execution.

```bash
ahnara plan "Fix failing auth tests" --output auth-plan.json
ahnara run-plan auth-plan.json
ahnara runs list
ahnara runs show <run-id>
```

---

## Tool Approval Policy

- `ahnara_APPROVAL_MODE=smart|manual|off` (default: `smart`)
- Smart mode blocks critical destructive patterns, requires approval for high-risk shell/network commands, and blocks private/local URLs to reduce SSRF risk.

---

## Skill Development

Skills are markdown-based instruction sets compatible with [agentskills.io](https://agentskills.io/specification).

```markdown
skill-name/
├── SKILL.md          # Required: metadata + instructions
├── scripts/          # Optional: executable code
├── references/       # Optional: documentation
└── assets/           # Optional: templates, resources
```

```markdown
---
name: my-skill
description: What this skill does and when to use it.
allowed-tools: Bash(python:*) Read
---

# My Skill

Instructions for the AI agent...
```

---

## Architecture

```markdown
┌─────────────────────────────────────────────────────────┐
│                      AgentCore                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │
│  │  Provider   │  │   Memory    │  │ Orchestrator│    │
│  │    Pool     │  │   Engine    │  │   (DAG)     │    │
│  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌─────────┐ │    │
│  │ │ Primary │ │  │ │   LRU   │ │  │ │ Tools   │ │    │
│  │ │Fallbacks│ │  │ │ SQLite  │ │  │ │ (para.) │ │    │
│  │ └─────────┘ │  │ │ Vector  │ │  │ └─────────┘ │    │
│  └─────────────┘ │ └─────────┘ │  └─────────────┘    │
│                   └─────────────┘                      │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │
│  │   Skills    │  │  Channels   │  │  Streaming  │    │
│  │  Registry   │  │ TG/Discord  │  │  (SSE)      │    │
│  └─────────────┘  └─────────────┘  └─────────────┘    │
└─────────────────────────────────────────────────────────┘
```

---

## Performance

| Metric | Value |
| --- | --- |
| Binary Size | \~21 MB |
| Startup Time | \~12ms |
| Chat Latency | &lt;100ms |
| First Stream Token | &lt;200ms |
| Tests | 71 passing |
| Lines of Rust | \~17,700 |

---

## Roadmap

- [ ] More MCP server integrations (filesystem, Brave search, memory, fetch, postgres)

- [ ] Provider-specific request adapters (Anthropic, Gemini, Cohere native formats)

- [ ] Rate limiting and retry with exponential backoff

- [ ] Streaming partial tool call reassembly

- [ ] Accurate token counting (tiktoken-rs integration)

- [ ] Multi-user / multi-session support

- [ ] Web UI dashboard

- [ ] Voice input/output (Whisper STT + TTS)

- [ ] Docker support

- [ ] Webhook support for external service integrations

---

## Troubleshooting

### "System message must be at the beginning" (after upgrade to v0.4.7)

Fixed in v0.4.7. This occurred when conversation compaction inserted a mid-conversation system message that some providers reject. Run `/update` to get the fix.

### API key not found

ahnara checks environment variables in order:

---

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing`)
3. Commit changes (`git commit -m 'Add amazing feature'`)
4. Push to branch (`git push origin feature/amazing`)
5. Open a Pull Request

---

## License

PolyForm Noncommercial 1.0.0 - see [LICENSE](LICENSE) file.

---

**Built with love by [Emperor M.K](https://github.com/emperormk01)**