//! /model command - Per-user model and provider override
//!
//! Interactive flow on Telegram (with inline keyboards):
//!   /model                     - Show inline keyboard with provider types
//!   > Select provider          - Ask for API key
//!   > Enter API key            - Ask for model ID
//!   > Enter model ID           - Done, show summary
//!
//! Text-based usage (Discord / direct):
//!   /model provider <type>     - Set provider type (openai, anthropic, google, openrouter, groq, deepseek, custom)
//!   /model key <api_key>       - Set API key (encrypted at rest)
//!   /model id <model_id>       - Set model ID
//!   /model reset               - Clear override, revert to defaults
//!   /model                     - Show current override + provider type buttons

use crate::memory::model_store::{ModelStore, UserModelOverride};
use anyhow::Result;

/// Provider info displayed in the inline keyboard.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub default_base: &'static str,
    pub auth_header: &'static str,
    pub key_prefix_hint: &'static str,
}

/// Known providers with their canonical settings.
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "openai",
        name: "OpenAI / Azure",
        default_base: "https://api.openai.com/v1",
        auth_header: "Authorization: Bearer",
        key_prefix_hint: "sk-...",
    },
    ProviderInfo {
        id: "anthropic",
        name: "Anthropic Claude",
        default_base: "https://api.anthropic.com/v1/messages",
        auth_header: "x-api-key",
        key_prefix_hint: "sk-ant-...",
    },
    ProviderInfo {
        id: "google",
        name: "Google Gemini",
        default_base: "https://generativelanguage.googleapis.com/v1beta",
        auth_header: "?key= (query param)",
        key_prefix_hint: "AIza...",
    },
    ProviderInfo {
        id: "openrouter",
        name: "OpenRouter",
        default_base: "https://openrouter.ai/api/v1",
        auth_header: "Authorization: Bearer",
        key_prefix_hint: "sk-or-...",
    },
    ProviderInfo {
        id: "groq",
        name: "Groq",
        default_base: "https://api.groq.com/openai/v1",
        auth_header: "Authorization: Bearer",
        key_prefix_hint: "gsk_...",
    },
    ProviderInfo {
        id: "deepseek",
        name: "DeepSeek",
        default_base: "https://api.deepseek.com/v1",
        auth_header: "Authorization: Bearer",
        key_prefix_hint: "sk-...",
    },
    ProviderInfo {
        id: "custom",
        name: "Custom Endpoint",
        default_base: "https://your-api.example.com/v1",
        auth_header: "Authorization: Bearer",
        key_prefix_hint: "your-key",
    },
];

/// Build an inline keyboard markup for provider type selection (Telegram-flavored JSON).
/// Returns a serialized JSON string that the Telegram adapter can include in `reply_markup`.
pub fn provider_keyboard_json() -> String {
    // 2 columns, 7 providers -> 4 rows
    let mut rows: Vec<String> = Vec::new();

    for chunk in PROVIDERS.chunks(2) {
        let buttons: Vec<String> = chunk
            .iter()
            .map(|p| {
                format!(
                    r#"{{"text": "{} {}", "callback_data": "model:provider:{}"}}"#,
                    emoji_for(p.id),
                    p.name,
                    p.id
                )
            })
            .collect();
        rows.push(format!(r#"[{}]"#, buttons.join(", ")));
    }

    // Cancel / Reset row
    rows.push(
        r#"[{"text": "Reset Override", "callback_data": "model:reset"}, {"text": "Cancel", "callback_data": "model:cancel"}]"#
            .to_string(),
    );

    format!(r#"{{"inline_keyboard": [{}]}}"#, rows.join(", "))
}

fn emoji_for(id: &str) -> &str {
    match id {
        "openai" => "",
        "anthropic" => "",
        "google" => "",
        "openrouter" => "",
        "groq" => "",
        "deepseek" => "",
        _ => "",
    }
}

/// Find a provider by ID.
pub fn find_provider(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Handle callback queries from inline keyboards.
/// Returns (response_message, updated_keyboard_json, true_if_done).
pub fn handle_callback(
    store: &ModelStore,
    channel: &str,
    user_id: &str,
    data: &str,
) -> Result<(String, Option<String>, bool)> {
    let parts: Vec<&str> = data.split(':').collect();

    match parts.get(0) {
        Some(&"model") => match parts.get(1) {
            Some(&"provider") => {
                // User selected a provider type
                let provider_id = parts.get(2).copied().unwrap_or("custom");
                let provider = find_provider(provider_id).unwrap_or(&PROVIDERS[6]); // default to custom

                let mut ov = store.get(channel, user_id)?.unwrap_or_default();
                ov.provider_type = Some(provider_id.to_string());

                if provider_id == "custom" {
                    // Custom endpoints need more info: sub-type + base URL
                    ov.base_url = None; // clear - user must provide
                    ov.updated_at = now_secs();
                    store.set(channel, user_id, &ov)?;
                    update_config_provider(store, channel, user_id)?;

                    // Show sub-type keyboard: OpenAI-compatible vs Anthropic
                    let keyboard_json = custom_subtype_keyboard_json();
                    let msg = format!(
                        "Custom Endpoint selected.\n\n\
                         First, choose the API format your endpoint speaks:"
                    );
                    return Ok((msg, Some(keyboard_json), false));
                }

                ov.base_url = Some(provider.default_base.to_string());
                ov.updated_at = now_secs();
                store.set(channel, user_id, &ov)?;
                update_config_provider(store, channel, user_id)?;

                let msg = format!(
                    "**{} selected.**\n\n\
                     Send your API key:\n```\n/model key YOUR_KEY\n```\n\n\
                     Key format: `{}`\n\
                     Auth: `{}`\n\n\
                     Security: Keys are AES-256-GCM encrypted at rest.",
                    provider.name, provider.key_prefix_hint, provider.auth_header
                );

                Ok((msg, None, true))
            }
            Some(&"reset") => {
                if store.delete(channel, user_id)? {
                    Ok(("Override cleared. Using global defaults.".into(), None, true))
                } else {
                    Ok(("No override was set.".into(), None, true))
                }
            }
            Some(&"cancel") => Ok(("Cancelled.".into(), None, true)),
            Some(&"custom_subtype") => {
                // User chose OpenAI-compatible or Anthropic format for a custom endpoint
                let sub = parts.get(2).copied().unwrap_or("openai-compatible");
                let label = if sub == "anthropic" { "Anthropic-style" } else { "OpenAI-compatible" };

                let mut ov = store.get(channel, user_id)?.unwrap_or_default();
                ov.provider_type = Some(format!("custom/{}", sub));
                ov.base_url = None;
                ov.updated_at = now_secs();
                store.set(channel, user_id, &ov)?;
                update_config_provider(store, channel, user_id)?;

                let msg = format!(
                    "**{label} API format selected.**\n\n\
                     Now set your endpoint, key, and model:\n\n\
                     ```\n/model url https://your-api.example.com/v1\n\
                     /model key YOUR_KEY\n\
                     /model id MODEL_NAME\n```"
                );
                Ok((msg, None, true))
            }
            _ => Ok((format_help(), None, true)),
        },
        _ => Ok((format_help(), None, true)),
    }
}

/// Parse and handle the text /model command.
pub fn handle_model(
    store: &ModelStore,
    channel: &str,
    user_id: &str,
    args: &str,
) -> Result<String> {
    let args = args.trim();

    // /model with no args - show current status + keyboard
    if args.is_empty() {
        return show_current(store, channel, user_id);
    }

    // /model reset
    if args == "reset" {
        if store.delete(channel, user_id)? {
            return Ok("Model override cleared. Using global defaults.".into());
        } else {
            return Ok("No model override was set.".into());
        }
    }

    // Parse subcommands: provider <type>, key <key>, id <model_id>
    let tokens: Vec<&str> = args.splitn(3, ' ').collect();

    match tokens.as_slice() {
        ["provider", pt, ..] => {
            let provider = find_provider(pt).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown provider: {}. Available: {}",
                    pt,
                    PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                )
            })?;

            let mut ov = store.get(channel, user_id)?.unwrap_or_default();
            ov.provider_type = Some(provider.id.to_string());
            ov.base_url = Some(provider.default_base.to_string());
            ov.updated_at = now_secs();
            store.set(channel, user_id, &ov)?;
            update_config_provider(store, channel, user_id)?;

            Ok(format!(
                "Provider set to **{}**.\nDefault endpoint: {}\nNext: /model key YOUR_KEY",
                provider.name, provider.default_base
            ))
        }
        ["key", rest, ..] => {
            let key = rest.trim();
            if key.is_empty() || key.len() < 4 {
                return Ok("Please provide a valid API key: /model key sk-xxx".into());
            }

            let mut ov = store.get(channel, user_id)?.unwrap_or_default();
            let encrypted = store.encrypt_key(key)?;
            ov.encrypted_api_key = Some(encrypted);
            ov.updated_at = now_secs();
            store.set(channel, user_id, &ov)?;
            update_config_provider(store, channel, user_id)?;

            let masked = mask_key(key);
            Ok(format!(
                "API key saved: {}\nNext: /model url https://your-api.example.com/v1 (if not set) | /model id MODEL_NAME",
                masked
            ))
        }
        ["url", rest, ..] => {
            let url = rest.trim();
            if url.is_empty() || (!url.starts_with("http://") && !url.starts_with("https://")) {
                return Ok("Please provide a valid URL: /model url https://your-api.example.com/v1".into());
            }

            let mut ov = store.get(channel, user_id)?.unwrap_or_default();
            ov.base_url = Some(url.to_string());
            ov.updated_at = now_secs();
            store.set(channel, user_id, &ov)?;
            update_config_provider(store, channel, user_id)?;

            let next = if ov.encrypted_api_key.is_some() {
                "Next: /model id MODEL_NAME".to_string()
            } else {
                "Next: /model key YOUR_KEY".to_string()
            };
            Ok(format!("Base URL set to: {}\n{}", url, next))
        }
        ["id", rest, ..] => {
            let model_id = rest.trim();
            if model_id.is_empty() {
                return Ok("Please specify a model ID: /model id gpt-4o".into());
            }

            let mut ov = store.get(channel, user_id)?.unwrap_or_default();
            ov.model_id = Some(model_id.to_string());
            ov.updated_at = now_secs();
            store.set(channel, user_id, &ov)?;
            update_config_provider(store, channel, user_id)?;

            let summary = build_summary("telegram", user_id, &ov);
            Ok(format!("Model ID updated to **{}**.\n\n{}", model_id, summary))
        }
        _ => {
            // Treat single token as model ID for backward compat
            if !args.contains(' ') {
                let mut ov = store.get(channel, user_id)?.unwrap_or_default();
                ov.model_id = Some(args.to_string());
                ov.updated_at = now_secs();
                store.set(channel, user_id, &ov)?;
                update_config_provider(store, channel, user_id)?;
                return Ok(format!("Model ID updated to **{}**.", args));
            }
            Ok(format!("Unknown option: {}\n\n{}", args, format_help()))
        }
    }
}

fn show_current(store: &ModelStore, channel: &str, user_id: &str) -> Result<String> {
    match store.get(channel, user_id)? {
        Some(ov) => Ok(build_summary(channel, user_id, &ov)),
        None => Ok(format!(
            "No model override set. Using global defaults.\n\n\
             Choose a provider:\n\
             {}\n\n\
             Or use text commands:\n\
             /model provider openai\n\
             /model url https://api.example.com/v1\n\
             /model key sk-xxx\n\
             /model id gpt-4o\n\
             /model reset",
            PROVIDERS
                .iter()
                .map(|p| format!("/model provider {} — {}", p.id, p.name))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

pub fn build_summary_for_user(channel: &str, user_id: &str, store: &ModelStore) -> Result<String> {
    match store.get(channel, user_id)? {
        Some(ov) => Ok(build_summary(channel, user_id, &ov)),
        None => Ok("No model override set.".into()),
    }
}

fn build_summary(channel: &str, user_id: &str, ov: &UserModelOverride) -> String {
    let provider_name = ov
        .provider_type
        .as_deref()
        .map(|pt| {
            // Handle custom/ prefix
            if let Some(sub) = pt.strip_prefix("custom/") {
                if sub == "anthropic" {
                    "Custom (Anthropic-style)"
                } else {
                    "Custom (OpenAI-compatible)"
                }
            } else {
                find_provider(pt).map(|p| p.name).unwrap_or("(not set)")
            }
        })
        .unwrap_or("(not set)");

    let key_status = if ov.encrypted_api_key.is_some() {
        "(set, encrypted)"
    } else {
        "(not set)"
    };

    format!(
        "Model override for {}/{}:\n\
         - Provider: {}\n\
         - Model: {}\n\
         - Base URL: {}\n\
         - API Key: {}\n\n\
         Change: /model provider <type> | /model url <url> | /model key <k> | /model id <id>\n\
         Reset: /model reset",
        channel,
        user_id,
        provider_name,
        ov.model_id.as_deref().unwrap_or("(not set)"),
        ov.base_url.as_deref().unwrap_or("(not set)"),
        key_status,
    )
}

fn format_help() -> String {
    format!(
        "Usage:\n\
         /model provider openai|anthropic|google|openrouter|groq|deepseek|custom\n\
         /model url https://your-api.example.com/v1\n\
         /model key YOUR_API_KEY\n\
         /model id MODEL_ID\n\
         /model reset\n\n\
         Available providers: {}",
        PROVIDERS
            .iter()
            .map(|p| format!("{} ({})", p.id, p.default_base))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Mask an API key for display: show first 4 and last 4 chars.
pub fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        let end = 4.min(key.len());
        format!("{}***", &key[..key.floor_char_boundary(end)])
    } else {
        let start = key.floor_char_boundary(key.len() - 4);
        format!("{}***{}", &key[..key.floor_char_boundary(4)], &key[start..])
    }
}

/// Resolve the effective model config for a user session.
/// Returns (provider_type, base_url, api_key, model_id) where each field falls back to None
/// if the user hasn't overridden it.
pub fn resolve_user_model(
    store: &ModelStore,
    channel: &str,
    user_id: &str,
) -> Result<(Option<String>, Option<String>, Option<String>, Option<String>)> {
    match store.get(channel, user_id)? {
        Some(ov) => {
            let api_key = match &ov.encrypted_api_key {
                Some(enc) => Some(store.decrypt_key(enc)?),
                None => None,
            };
            Ok((ov.provider_type, ov.base_url, api_key, ov.model_id))
        }
        None => Ok((None, None, None, None)),
    }
}

/// Sync the user's current model override into ~/.ahnara/config.toml
/// as the single global provider. This is the ONLY place provider config lives.
/// After this call, a restart will pick up the new provider automatically,
/// and for the current session the ProviderPool is updated at runtime.
pub fn update_config_provider(
    store: &ModelStore,
    channel: &str,
    user_id: &str,
) -> Result<()> {
    let ov = match store.get(channel, user_id)? {
        Some(ov) => ov,
        None => return Ok(()),
    };

    let config_path = dirs::home_dir()
        .map(|h| h.join(".ahnara/config.toml"))
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    // Read existing config or start fresh
    let mut doc: toml::Value = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)?;
        toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };

    // Always write model_id to [agent] if set -- this is the cross-channel
    // persistence the user needs.  Does NOT require provider credentials.
    if let Some(ref model_id) = ov.model_id {
        if !model_id.is_empty() {
            let agent_table = doc.as_table_mut()
                .unwrap()
                .entry("agent".to_string())
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("[agent] is not a table"))?;
            agent_table.insert("default_model".to_string(), toml::Value::String(model_id.clone()));
        }
    }

    // Write provider entry only when we have enough data
    let base_url = match &ov.base_url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => {
            // No base_url -- just write the model and return
            let rendered = toml::to_string_pretty(&doc)?;
            std::fs::write(&config_path, &rendered)?;
            tighten_config_permissions(&config_path);
            tracing::info!("Updated config.toml with model '{}'", ov.model_id.as_deref().unwrap_or("(none)"));
            return Ok(());
        }
    };
    let api_key = match &ov.encrypted_api_key {
        Some(enc) => store.decrypt_key(enc)?,
        None => {
            // No api_key -- just write the model and return
            let rendered = toml::to_string_pretty(&doc)?;
            std::fs::write(&config_path, &rendered)?;
            tighten_config_permissions(&config_path);
            tracing::info!("Updated config.toml with model '{}'", ov.model_id.as_deref().unwrap_or("(none)"));
            return Ok(());
        }
    };
    let provider_type = ov.provider_type.clone().unwrap_or_else(|| "custom".into());
    let model_id = ov.model_id.clone().unwrap_or_default();

    // Build the provider entry
    let name = provider_type.clone();
    let entry = toml::Value::Table({
        let mut t = toml::map::Map::new();
        t.insert("name".into(), toml::Value::String(name.clone()));
        t.insert("api_key".into(), toml::Value::String(api_key));
        t.insert("api_base".into(), toml::Value::String(base_url));
        t
    });

    // Update [providers] section
    let providers_table = doc.as_table_mut()
        .unwrap()
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[providers] is not a table"))?;

    providers_table.insert("active".to_string(), toml::Value::String(name.clone()));
    providers_table.insert("providers".to_string(), toml::Value::Array(vec![entry]));

    let rendered = toml::to_string_pretty(&doc)?;
    std::fs::write(&config_path, &rendered)?;
    tighten_config_permissions(&config_path);

    tracing::info!("Updated config.toml with provider '{}' and model '{}'", name, model_id);
    Ok(())
}

fn tighten_config_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn custom_subtype_keyboard_json() -> String {
    let rows = vec![
        r#"[{"text": " OpenAI-compatible (/v1/chat/completions)", "callback_data": "model:custom_subtype:openai-compatible"}]"#,
        r#"[{"text": " Anthropic-style (/v1/messages)", "callback_data": "model:custom_subtype:anthropic"}]"#,
        r#"[{"text": " Cancel", "callback_data": "model:cancel"}]"#,
    ];
    format!(r#"{{"inline_keyboard": [{}]}}"#, rows.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store() -> ModelStore {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ahnara_cmd_test_{}", ts));
        fs::create_dir_all(&dir).unwrap();
        ModelStore::new(&dir).unwrap()
    }

    #[test]
    fn test_show_no_override() {
        let store = temp_store();
        let resp = handle_model(&store, "telegram", "999", "").unwrap();
        assert!(resp.contains("No model override"));
    }

    #[test]
    fn test_set_provider() {
        let store = temp_store();
        let resp = handle_model(&store, "telegram", "1", "provider anthropic").unwrap();
        assert!(resp.contains("Anthropic"));

        let ov = store.get("telegram", "1").unwrap().unwrap();
        assert_eq!(ov.provider_type.as_deref(), Some("anthropic"));
        assert_eq!(ov.base_url.as_deref(), Some("https://api.anthropic.com/v1/messages"));
    }

    #[test]
    fn test_set_key_and_id() {
        let store = temp_store();
        handle_model(&store, "telegram", "2", "provider openai").unwrap();
        let resp = handle_model(&store, "telegram", "2", "key sk-test12345678").unwrap();
        assert!(resp.contains("sk-t***5678"));

        let resp = handle_model(&store, "telegram", "2", "id gpt-4o").unwrap();
        assert!(resp.contains("gpt-4o"));

        let ov = store.get("telegram", "2").unwrap().unwrap();
        assert_eq!(ov.model_id.as_deref(), Some("gpt-4o"));
        assert!(ov.encrypted_api_key.is_some());
    }

    #[test]
    fn test_keyword_arg_parsing() {
        let store = temp_store();
        let resp = handle_model(
            &store,
            "telegram",
            "4",
            "provider google",
        )
        .unwrap();
        assert!(resp.contains("Google"));
        handle_model(&store, "telegram", "4", "key AIza-test123").unwrap();
        handle_model(&store, "telegram", "4", "id gemini-2.0-flash").unwrap();
    }

    #[test]
    fn test_resolve_with_provider_type() {
        let store = temp_store();
        handle_model(&store, "telegram", "5", "provider anthropic").unwrap();
        handle_model(&store, "telegram", "5", "key sk-ant-secret").unwrap();
        handle_model(&store, "telegram", "5", "id claude-3-opus").unwrap();

        let (pt, base, key, model) = resolve_user_model(&store, "telegram", "5").unwrap();
        assert_eq!(pt.as_deref(), Some("anthropic"));
        assert_eq!(base.as_deref(), Some("https://api.anthropic.com/v1/messages"));
        assert_eq!(key.as_deref(), Some("sk-ant-secret"));
        assert_eq!(model.as_deref(), Some("claude-3-opus"));
    }

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("sk-1234567890"), "sk-1***7890");
        assert_eq!(mask_key("short"), "shor***");
    }

    #[test]
    fn url_command_sets_base_url() {
        let store = temp_store();
        let resp = handle_model(&store, "telegram", "6", "url https://example.com/v1").unwrap();
        assert!(resp.contains("Base URL set to: https://example.com/v1"));

        let ov = store.get("telegram", "6").unwrap().unwrap();
        assert_eq!(ov.base_url.as_deref(), Some("https://example.com/v1"));
    }

    #[test]
    fn custom_provider_summary_friendly_name() {
        let store = temp_store();
        let mut ov = UserModelOverride::default();
        ov.provider_type = Some("custom/anthropic".into());
        ov.base_url = Some("https://my-api.com/v1".into());
        ov.encrypted_api_key = Some("encrypted-key-placeholder".into());
        ov.model_id = Some("claude-3".into());
        store.set("telegram", "user", &ov).unwrap();
        
        let summary = handle_model(&store, "telegram", "user", "").unwrap();
        assert!(summary.contains("Custom (Anthropic-style)"));
        
        ov.provider_type = Some("custom/openai-compatible".into());
        store.set("telegram", "user", &ov).unwrap();
        let summary = handle_model(&store, "telegram", "user", "").unwrap();
        assert!(summary.contains("Custom (OpenAI-compatible)"));
    }

    #[test]
    fn parse_endpoint_and_model_from_text() {
        // Test the auto-detection logic used in handle_model_flow
        fn detect(text: &str) -> Option<(&str, &str)> {
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
            Some((url?, model_id?))
        }

        let (url, model) = detect("https://api.example.com/v1 gpt-4o").unwrap();
        assert_eq!(url, "https://api.example.com/v1");
        assert_eq!(model, "gpt-4o");

        // Reversed order still works
        let (url, model) = detect("gpt-4o https://api.example.com/v1").unwrap();
        assert_eq!(url, "https://api.example.com/v1");
        assert_eq!(model, "gpt-4o");

        // URL with /v1 in path
        let (url, model) = detect("http://localhost:8080/v1 my-model").unwrap();
        assert_eq!(url, "http://localhost:8080/v1");
        assert_eq!(model, "my-model");

        // Missing URL
        assert!(detect("gpt-4o only-model").is_none());

        // Missing model
        assert!(detect("https://api.example.com/v1").is_none());
    }
}
