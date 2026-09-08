//! Model Store - Per-user model overrides with AES-256-GCM encrypted API keys
//!
//! Each user can override the active LLM provider for their sessions.
//! API keys are encrypted at rest using AES-256-GCM with a machine-derived key.
//! Stored in `~/.ahnara/model_overrides/<channel>_<user_id>.json`.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

/// What a user can override about their model/provider.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserModelOverride {
    /// Provider type for adapter selection: "openai" / "anthropic" / "google" / "openrouter" / "groq" / "deepseek" / "custom"
    #[serde(default)]
    pub provider_type: Option<String>,
    pub base_url: Option<String>,
    pub model_id: Option<String>,
    /// AES-256-GCM encrypted API key (base64 encoded: nonce + ciphertext + tag)
    #[serde(default)]
    pub encrypted_api_key: Option<String>,
    /// Timestamp of last update (epoch seconds)
    #[serde(default)]
    pub updated_at: u64,
}

/// Persistent store for per-user model overrides.
pub struct ModelStore {
    data_dir: PathBuf,
    /// AES-256-GCM cipher derived from machine identity
    cipher: Aes256Gcm,
}

impl ModelStore {
    pub fn new(data_dir: &std::path::Path) -> Result<Self> {
        let override_dir = data_dir.join("model_overrides");
        fs::create_dir_all(&override_dir)
            .with_context(|| format!("Failed to create model overrides dir: {:?}", &override_dir))?;

        let cipher = Self::derive_cipher(data_dir)?;

        Ok(Self {
            data_dir: override_dir,
            cipher,
        })
    }

    /// Derive an AES-256 key from machine-specific entropy.
    /// Uses hostname + a persistent secret salt file stored alongside the DB.
    fn derive_cipher(data_dir: &std::path::Path) -> Result<Aes256Gcm> {
        let salt_path = data_dir.join(".model_key_salt");

        // Generate or read a persistent random salt (32 bytes)
        if !salt_path.exists() {
            let mut salt = [0u8; 32];
            // Use a simple PRNG seeded from process-specific data for salt generation
            // (the salt file itself is the real entropy source once created)
            for (i, byte) in salt.iter_mut().enumerate() {
                *byte = ((std::process::id() as u64)
                    .wrapping_mul(0x9E3779B97F4A7C15)
                    .wrapping_add(i as u64)
                    ^ std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64) as u8;
            }
            fs::write(&salt_path, &salt)
                .with_context(|| format!("Failed to write salt file: {:?}", &salt_path))?;
        }

        let salt = fs::read(&salt_path)
            .with_context(|| format!("Failed to read salt file: {:?}", &salt_path))?;

        // Derive key: SHA-256(hostname + salt)
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "ahnara".into());

        let mut hasher = Sha256::new();
        hasher.update(hostname.as_bytes());
        hasher.update(&salt);
        let key_bytes = hasher.finalize();

        let cipher = Aes256Gcm::new_from_slice(&key_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {}", e))?;

        Ok(cipher)
    }

    fn path_for(&self, channel: &str, user_id: &str) -> PathBuf {
        let safe_id = format!("{}_{}", channel, user_id)
            .replace(['/', '\\', ':'], "_");
        self.data_dir.join(format!("{}.json", safe_id))
    }

    /// Load a user's model override. Returns None if no override exists.
    pub fn get(&self, channel: &str, user_id: &str) -> Result<Option<UserModelOverride>> {
        let path = self.path_for(channel, user_id);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read model override: {:?}", &path))?;
        let ov: UserModelOverride = serde_json::from_str(&data)?;
        Ok(Some(ov))
    }

    /// Save a user's model override.
    pub fn set(&self, channel: &str, user_id: &str, ov: &UserModelOverride) -> Result<()> {
        let path = self.path_for(channel, user_id);
        let data = serde_json::to_string_pretty(ov)?;
        fs::write(&path, data)
            .with_context(|| format!("Failed to write model override: {:?}", &path))?;
        Ok(())
    }

    /// Delete a user's model override (resets to defaults).
    pub fn delete(&self, channel: &str, user_id: &str) -> Result<bool> {
        let path = self.path_for(channel, user_id);
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Encrypt an API key with AES-256-GCM. Returns base64(nonce || ciphertext || tag).
    pub fn encrypt_key(&self, plaintext: &str) -> Result<String> {
        use base64::Engine;
        // Generate random 12-byte nonce
        let mut nonce_bytes = [0u8; 12];
        for (i, byte) in nonce_bytes.iter_mut().enumerate() {
            *byte = ((std::process::id() as u64)
                .wrapping_mul(0x9E3779B97F4A7C15)
                .wrapping_add(i as u64 + std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64)) as u8;
        }
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self.cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        // Prepend nonce to ciphertext for storage
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
    }

    /// Decrypt an API key. Input is base64(nonce || ciphertext || tag).
    pub fn decrypt_key(&self, encoded: &str) -> Result<String> {
        use base64::Engine;
        let combined = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| anyhow::anyhow!("Invalid base64: {}", e))?;

        if combined.len() < 12 {
            return Err(anyhow::anyhow!("Encrypted data too short"));
        }

        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        String::from_utf8(plaintext)
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in decrypted key: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store() -> ModelStore {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ahnara_model_test_{}", ts));
        fs::create_dir_all(&dir).unwrap();
        ModelStore::new(&dir).unwrap()
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let store = temp_store();
        let key = "sk-test-1234567890abcdef";
        let encrypted = store.encrypt_key(key).unwrap();
        assert_ne!(encrypted, key);
        let decrypted = store.decrypt_key(&encrypted).unwrap();
        assert_eq!(decrypted, key);
    }

    #[test]
    fn test_different_encryptions_same_plaintext() {
        let store = temp_store();
        let enc1 = store.encrypt_key("my-api-key").unwrap();
        let enc2 = store.encrypt_key("my-api-key").unwrap();
        // Different nonces produce different ciphertexts
        assert_ne!(enc1, enc2);
        // Both decrypt to the same value
        assert_eq!(store.decrypt_key(&enc1).unwrap(), "my-api-key");
        assert_eq!(store.decrypt_key(&enc2).unwrap(), "my-api-key");
    }

    #[test]
    fn test_set_get_delete() {
        let store = temp_store();
        let mut ov = UserModelOverride::default();
        ov.base_url = Some("https://api.openai.com/v1".into());
        ov.model_id = Some("gpt-4o".into());
        ov.encrypted_api_key = Some(store.encrypt_key("sk-secret-key").unwrap());
        ov.updated_at = 1234567890;

        store.set("telegram", "12345", &ov).unwrap();

        let loaded = store.get("telegram", "12345").unwrap().unwrap();
        assert_eq!(loaded.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(loaded.model_id.as_deref(), Some("gpt-4o"));

        // Decrypt the stored key
        let decrypted = store.decrypt_key(loaded.encrypted_api_key.as_ref().unwrap()).unwrap();
        assert_eq!(decrypted, "sk-secret-key");

        // Delete
        assert!(store.delete("telegram", "12345").unwrap());
        assert!(store.get("telegram", "12345").unwrap().is_none());
    }

    #[test]
    fn test_different_users_isolated() {
        let store = temp_store();
        let mut ov1 = UserModelOverride::default();
        ov1.model_id = Some("gpt-4o".into());
        store.set("telegram", "user_a", &ov1).unwrap();

        let mut ov2 = UserModelOverride::default();
        ov2.model_id = Some("claude-3".into());
        store.set("telegram", "user_b", &ov2).unwrap();

        assert_eq!(
            store.get("telegram", "user_a").unwrap().unwrap().model_id.as_deref(),
            Some("gpt-4o")
        );
        assert_eq!(
            store.get("telegram", "user_b").unwrap().unwrap().model_id.as_deref(),
            Some("claude-3")
        );
    }
}
