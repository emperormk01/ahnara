//! /token command - Secure encrypted token/secret management
//!
//! Usage:
//!   /token                    - List all stored token names (values never shown)
//!   /token set <name> <value> - Store an encrypted token
//!   /token remove <name>      - Remove a token
//!   /token get <name>         - Retrieve token value (internal/agent use only)
//!   /token help               - Show usage
//!
//! Tokens are stored encrypted with AES-256-GCM in ~/.ahnara/tokens.enc.
//! The LLM never sees token values -- only names are injected into the system prompt.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TokenStore {
    tokens: HashMap<String, String>,
}

fn ahnara_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| "/root".into())
        .join(".ahnara")
}

fn store_path() -> PathBuf {
    ahnara_dir().join("tokens.enc")
}

fn salt_path() -> PathBuf {
    ahnara_dir().join(".token_key_salt")
}

fn derive_cipher() -> Result<Aes256Gcm> {
    let dir = ahnara_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create ahnara dir: {:?}", &dir))?;

    let sp = salt_path();
    if !sp.exists() {
        let mut salt = [0u8; 32];
        for (i, byte) in salt.iter_mut().enumerate() {
            *byte = ((std::process::id() as u64)
                .wrapping_mul(0x9E3779B97F4A7C15)
                .wrapping_add(i as u64)
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64) as u8;
        }
        fs::write(&sp, &salt)
            .with_context(|| format!("Failed to write salt: {:?}", &sp))?;
    }

    let salt = fs::read(&sp)
        .with_context(|| format!("Failed to read salt: {:?}", &sp))?;

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ahnara".into());

    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    hasher.update(&salt);
    let key_bytes = hasher.finalize();

    Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow!("Failed to create cipher: {}", e))
}

fn encrypt_value(cipher: &Aes256Gcm, plaintext: &str) -> Result<String> {
    use base64::Engine;
    let mut nonce_bytes = [0u8; 12];
    for (i, byte) in nonce_bytes.iter_mut().enumerate() {
        *byte = ((std::process::id() as u64)
            .wrapping_mul(0x9E3779B97F4A7C15)
            .wrapping_add(
                i as u64
                    + std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64,
            )) as u8;
    }
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;
    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

fn decrypt_value(cipher: &Aes256Gcm, encoded: &str) -> Result<String> {
    use base64::Engine;
    let combined = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| anyhow!("Invalid base64: {}", e))?;
    if combined.len() < 12 {
        return Err(anyhow!("Encrypted data too short"));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed: {}", e))?;
    String::from_utf8(plaintext).map_err(|e| anyhow!("Invalid UTF-8: {}", e))
}

fn load_store(cipher: &Aes256Gcm) -> Result<TokenStore> {
    let path = store_path();
    if !path.exists() {
        return Ok(TokenStore::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read tokens: {:?}", &path))?;
    let stored: TokenStore =
        serde_json::from_str(&raw).with_context(|| "Failed to parse tokens file")?;

    let mut tokens = HashMap::new();
    for (name, enc_val) in &stored.tokens {
        match decrypt_value(cipher, enc_val) {
            Ok(val) => { tokens.insert(name.clone(), val); }
            Err(e) => {
                tracing::warn!("Failed to decrypt token '{}': {}", name, e);
            }
        }
    }
    Ok(TokenStore { tokens })
}

fn save_store(cipher: &Aes256Gcm, store: &TokenStore) -> Result<()> {
    let path = store_path();
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create dir: {:?}", dir))?;

    let mut encrypted = TokenStore::default();
    for (name, val) in &store.tokens {
        let enc = encrypt_value(cipher, val)?;
        encrypted.tokens.insert(name.clone(), enc);
    }

    let json = serde_json::to_string_pretty(&encrypted)?;
    let tmp = path.with_extension("enc.tmp");
    fs::write(&tmp, &json)
        .with_context(|| format!("Failed to write tmp: {:?}", &tmp))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("Failed to rename: {:?} -> {:?}", &tmp, &path))?;
    Ok(())
}

fn mask_secret(value: &str) -> String {
    if value.len() <= 12 {
        if value.is_empty() {
            "(not set)".to_string()
        } else {
            "*".repeat(value.len())
        }
    } else {
        format!(
            "{}...{}",
            &value[..value.floor_char_boundary(4)],
            &value[value.floor_char_boundary(value.len() - 4)..]
        )
    }
}

pub fn contains_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    let patterns = [
        "ghp_", "gho_", "ghs_", "ghr_",
        "sk-", "sk_live_", "sk_test_",
        "xoxb-", "xoxp-", "xapp-",
        "nvapi-",
        "Bearer ", "bearer ",
        "api_key=", "apikey=", "token=",
        "pat-",
        "AKIA",
        "eyJ",
        "whsec_",
    ];
    for pat in &patterns {
        if lower.contains(&pat.to_lowercase()) {
            return true;
        }
    }
    for word in text.split_whitespace() {
        let clean: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if clean.len() > 40 && clean.chars().filter(|c| c.is_ascii_alphanumeric()).count() > 35 {
            return true;
        }
    }
    false
}

/// Public API: list all stored token names (no values).
pub fn list_token_names() -> Vec<String> {
    let cipher = match derive_cipher() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let store = match load_store(&cipher) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut names: Vec<String> = store.tokens.keys().cloned().collect();
    names.sort();
    names
}

/// Public API: retrieve a decrypted token value by name.
/// Used internally by tools and MCP servers. Never exposed to the LLM.
pub fn get_token_value(name: &str) -> Option<String> {
    let cipher = derive_cipher().ok()?;
    let store = load_store(&cipher).ok()?;
    store.tokens.get(name).cloned()
}

/// Handle the /token command.
pub fn handle_token(args: &str) -> Result<String> {
    let parts: Vec<&str> = args.trim().split_whitespace().collect();

    if parts.is_empty() || parts[0] == "list" {
        return list_tokens();
    }

    match parts[0] {
        "help" | "h" => Ok(help_text()),
        "set" => {
            if parts.len() < 3 {
                return Err(anyhow!(
                    "Usage: /token set <name> <value>\nExample: /token set github_token ghp_xxxx"
                ));
            }
            let name = parts[1];
            let value = parts[2..].join(" ");
            set_token(name, &value)
        }
        "remove" | "rm" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /token remove <name>"));
            }
            remove_token(parts[1])
        }
        "get" => {
            if parts.len() < 2 {
                return Err(anyhow!("Usage: /token get <name>"));
            }
            match get_token_value(parts[1]) {
                Some(val) => Ok(val),
                None => Err(anyhow!("No token named '{}'", parts[1])),
            }
        }
        _ => Err(anyhow!(
            "Unknown subcommand '{}'. Use /token help for usage.",
            parts[0]
        )),
    }
}

fn help_text() -> String {
    "\
Token Management
================

Securely store API keys and tokens. Values are encrypted at rest.

Commands:
  /token                  List all token names (values never shown)
  /token set <name> <val> Store an encrypted token
  /token remove <name>    Remove a token
  /token help             This message

Security:
  - Tokens are encrypted with AES-256-GCM on disk
  - Token values are never shown to the AI agent
  - Messages containing secrets are auto-deleted from chat
  - Only token names appear in the agent context

Examples:
  /token set github_token ghp_xxxxxxxxxxxx
  /token set linear_api_token lin_xxxxxxxxxxxx
  /token remove github_token"
        .into()
}

fn list_tokens() -> Result<String> {
    let cipher = derive_cipher()?;
    let store = load_store(&cipher)?;

    let mut lines = vec!["Configured Tokens".to_string(), String::new()];

    if store.tokens.is_empty() {
        lines.push("No tokens stored.".into());
        lines.push("Add one: /token set <name> <value>".into());
        return Ok(lines.join("\n"));
    }

    let mut names: Vec<&String> = store.tokens.keys().collect();
    names.sort();

    for name in &names {
        let val = &store.tokens[*name];
        lines.push(format!("  {} = {}", name, mask_secret(val)));
    }

    lines.push(String::new());
    lines.push(format!("{} token(s) stored.", store.tokens.len()));

    Ok(lines.join("\n"))
}

fn set_token(name: &str, value: &str) -> Result<String> {
    let cipher = derive_cipher()?;
    let mut store = load_store(&cipher)?;

    let is_update = store.tokens.contains_key(name);
    store.tokens.insert(name.to_string(), value.to_string());
    save_store(&cipher, &store)?;

    let action = if is_update { "Updated" } else { "Stored" };
    Ok(format!(
        "{} token '{}'. Value is encrypted and will not be shown to the AI agent.",
        action, name
    ))
}

fn remove_token(name: &str) -> Result<String> {
    let cipher = derive_cipher()?;
    let mut store = load_store(&cipher)?;

    if store.tokens.remove(name).is_some() {
        save_store(&cipher, &store)?;
        Ok(format!("Removed token '{}'.", name))
    } else {
        Err(anyhow!("No token named '{}'", name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_secret() {
        assert_eq!(mask_secret(""), "(not set)");
        assert_eq!(mask_secret("short"), "*****");
        assert_eq!(mask_secret("ghp_1234567890abcdef"), "ghp_...cdef");
    }

    #[test]
    fn test_contains_secret() {
        assert!(contains_secret("my token is ghp_abc123def456"));
        assert!(contains_secret("sk-live-1234567890"));
        assert!(contains_secret("use Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"));
        assert!(contains_secret("GITHUB_PERSONAL_ACCESS_TOKEN=gho_abc123"));
        assert!(!contains_secret("hello world"));
        assert!(!contains_secret("just a normal message"));
    }

    #[test]
    fn test_help() {
        let resp = help_text();
        assert!(resp.contains("Token Management"));
        assert!(resp.contains("/token set"));
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let cipher = derive_cipher().unwrap();
        let val = "ghp_test_token_value_12345";
        let encrypted = encrypt_value(&cipher, val).unwrap();
        assert_ne!(encrypted, val);
        let decrypted = decrypt_value(&cipher, &encrypted).unwrap();
        assert_eq!(decrypted, val);
    }

    #[test]
    fn test_set_and_list_tokens() {
        let cipher = derive_cipher().unwrap();
        let mut store = TokenStore::default();
        store.tokens.insert("test_key".into(), "test_value".into());
        save_store(&cipher, &store).unwrap();

        let loaded = load_store(&cipher).unwrap();
        assert_eq!(loaded.tokens.get("test_key").unwrap(), "test_value");

        // Clean up
        store.tokens.clear();
        save_store(&cipher, &store).unwrap();
    }
}
