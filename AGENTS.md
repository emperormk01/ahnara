# AGENTS.md — Ahnara

Rust AI agent framework. Binary crate (`src/main.rs`), edition 2021. Repo: `emperormk01/Ahnara`, branch `master`.

## Build and verify (GitHub CI is the compiler)

No Rust toolchain in this environment. Never assume local `cargo` works. Push and read CI.

- Workflow: `.github/workflows/ci.yml` — runs `cargo check --all-targets`, `cargo clippy --all-targets -- -D clippy::correctness -D clippy::suspicious`, `cargo test` on every push and PR.
- `release.yml` builds release binaries on `v*` tags only.
- Clippy gate is deliberately scoped to correctness plus suspicious. Do not add blanket `-D warnings`: it denies ~150 style lints including API-redesign demands (`too_many_arguments`). Style stays advisory.
- After pushing, poll `GET /repos/emperormk01/Ahnara/actions/runs`, then fetch the failed job's logs via the jobs API. Iterate until green.

## Pre-flight before pushing (no compiler locally)

- Run tree-sitter parse check over `src/**/*.rs` if available (`tree-sitter` + `tree-sitter-rust` pip packages). It catches brace-level corruption but NOT type errors. Zero parse errors is necessary, not sufficient.
- Grep for the corruption signatures below.

## Known corruption pattern (Sep 2026 — master did not compile for a long stretch)

Botched refactors left orphaned code tails across many files: stray closing braces, duplicated blocks, match arms without a head, functions called but never defined. Signatures to grep for:

- Lone indented `}` immediately after a col-0 `}`.
- Indented `Ok(())` within 4 lines after a col-0 `}`.
- `match` arms (`=>`) with no enclosing `match`/`fn` head.
- Duplicate `fn help_text`, duplicate handler bodies, `scripts/` references that no longer exist.

When found, prefer rebuilding the orphaned arms as real handler functions over deleting (see `cli/memory.rs` Stats/Export and `commands/config.rs` Edit/Reset/Validate). Deleted code lost behavior twice already.

## Structural rules (learned from CI failures)

- Helper methods live in inherent `impl X` blocks. Only trait members go in `impl Trait for X` (E0407). See `providers/adapters/anthropic.rs` for the reference layout; mirror `openai.rs`/`gemini.rs`.
- `serde_json::json!` needs valid macro syntax: no bare identifiers as values (`{"text": url}` is illegal, use `format!`).
- Prefer `&Path` over `&PathBuf` in public signatures (`commands/code.rs` precedent).
- CLI dispatch pattern: `handle_<thing>` matches every enum variant and delegates to `handle_<verb>_<noun>` fns. A missing arm is E0004. Check `cli/mod.rs` enums against every dispatcher after editing.
- `SkillInstaller` API surface: `install`, `install_from_url`, `install_from_git` (sync), `uninstall`, `update`, `search`/`list_available` (`&mut self`), `is_installed`, `create_from_template`. `SkillCommands::Install.name` is `Option<String>`.
- `SkillMeta` has no `version` field. It has `compatibility`. Check `skills/mod.rs` before touching metadata fields.
- `ModelStore::new` creates dirs; anything writing under `~/.ahnara/` must `create_dir_all` first (`commands/model.rs:save_config` precedent). Tests run in parallel against real HOME: keep file side effects out of test bodies where possible.
- Anthropic adapter must map the `tool` role to `tool_result` content blocks. Dropping tool messages breaks multi-turn tool conversations silently.

## Token discipline (Sep 2026 — approved: caching, aging, budgets)

- Anthropic adapter pins prompt-cache breakpoints on the system block and the last tool (`cache_control: ephemeral`). System must stay in block form for the breakpoint; `adapter_tests.rs` asserts the shape. OpenAI-compatible endpoints cache identical prefixes automatically, nothing to do there.
- `age_tool_results` (`agent/mod.rs`) digests stale `tool`-role messages in the loop's working set: keeps newest 3 full, replaces older with `[aged:name]` digests. Idempotent (skips already-aged). Unit-tested.
- Per-tool output budgets: `Tool::output_budget()` defaults to 10_000; `WebSearchTool` 4_000, `WebFetchTool` 6_000. `ToolOrchestrator::output_budget(name)` resolves with default fallback. `AgentCore::execute_tool` truncates the message-facing string to `min(budget, tool_output_max_chars)`. Full outputs stay intact in `ToolResult` for structured-output collection.

## Agent loop conventions (`agent/mod.rs:process`)

- Request lifecycle is logged at info level: request received (session, length, preview), each tool execution (name, args preview, iteration), final response (iterations, tool count, length). "Did she search" must be answerable from logs.
- `detect_unexecuted_tool_call` guards the no-tool-call branch: output shaped like `{"name": ..., "parameters"|"arguments"|"args": ...}` triggers a system correction plus loop retry, bounded by `MAX_TOOL_JSON_RECOVERIES = 2`. Covered by unit tests in the file's `mod tests`. Small models emit tool calls as text; without this the raw JSON gets served as chat.

## Config conventions

- Single source: `src/config/mod.rs`. `default_true()` lives there as `pub(crate)`; other modules reference `crate::config::default_true` in serde attrs. No local duplicates.
- Dead config rule: a field must have a reader outside `config/mod.rs` plus the setup template, or it gets removed. Previous purge deleted `ToolsConfig` entirely plus 9 more fields.
- `[sub_agents]` is wired end to end: `TokenBudget::from_sub_agents` maps it into the cost-aware delegator (`coordination/mod.rs`). Keep template (`commands/setup.rs`), example (`config.example.toml`), and runtime in sync.
- Gateway fallback for background tasks (vision, compactor, reflector, extractor): `providers::gateway_endpoint()` / `gateway_model()`, overridable via `AHNARA_GATEWAY_URL` / `AHNARA_GATEWAY_MODEL`. No new hardcoded URLs.

## Channels

- Telegram gating lives in `message_allowed` (`channels/telegram.rs`): `group_policy` (Closed/Mention/Open) plus `allowed_users` whitelist. Group chats have negative IDs. Apply to both `handle_message` and `handle_command`.
- `webhook_url` is accepted but unimplemented; `start()` logs a warning and uses polling. Do not silently ignore new config surface the same way.
- Discord `allowed_guilds` enforced in `DiscordHandler::message`; DMs always pass. Handler holds `_code_mode` (dormant) — see below.
- Dormant plumbing kept deliberately: `_code_mode` fields on both channel states, `MemoryEngine::db_path()` accessor. Wire or delete, never duplicate.

## Identity and branding

- System prompt identity block (`persona/mod.rs`): "running on an independent AI agent framework called Ahnara. You were created by Emperor M.K (github.com/emperormk01/ahnara)."
- Default voice (`DEFAULT_BEHAVIOR` in `persona/mod.rs`): feminine, warm, sharp, a little playful. Tasteful always, never explicit. Default tone is `Friendly`. Lowercase-first texting, anti chatbot tell list, playful deflection (never technical) on "are you a bot" questions while "who made you" still gets the true answer. Tool-discipline sections below the voice block stay intact. Users override voice via persona config, so keep voice and discipline in separate blocks.
- No `Auxlo-xyz/ahnara` references anywhere (install scripts, update checker, docs, Cargo metadata). The org move is complete; do not regress.
- Git identity for this repo: `Emperor M.K <emperormk01@gmail.com>`.

## Test suite notes

- `cargo test`: 177 tests, all must pass. Known-sensitive areas: `commands/model.rs` store tests (need writable HOME), `adapter_tests.rs` (asserts exact content-block shapes).
- New behavior gets a test where one already exists for the module. Do not add `tests/` or `benches/` dirs; inline `#[cfg(test)]` modules only (keeps dev-deps empty).
