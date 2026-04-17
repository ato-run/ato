use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

/// Persistent configuration for the ato-desktop application.
///
/// Stored at `~/.ato/desktop-config.json` and loaded on startup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// Light or Dark theme.
    pub theme: ThemeConfig,
    /// Default egress allow patterns for new sessions.
    #[serde(default)]
    pub default_egress_allow: Vec<String>,
    /// Terminal font size in pixels.
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: u16,
    /// Maximum number of concurrent terminal sessions.
    #[serde(default = "default_terminal_max_sessions")]
    pub terminal_max_sessions: usize,
    /// Automatically open devtools when a capsule is loaded.
    #[serde(default)]
    pub auto_open_devtools: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeConfig {
    Light,
    Dark,
}

fn default_terminal_font_size() -> u16 {
    14
}

fn default_terminal_max_sessions() -> usize {
    4
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            theme: ThemeConfig::Dark,
            default_egress_allow: Vec::new(),
            terminal_font_size: default_terminal_font_size(),
            terminal_max_sessions: default_terminal_max_sessions(),
            auto_open_devtools: false,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ato").join("desktop-config.json"))
}

/// Load configuration from `~/.ato/desktop-config.json`.
/// Returns `Default` if the file does not exist or is invalid.
pub fn load_config() -> DesktopConfig {
    let Some(path) = config_path() else {
        return DesktopConfig::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(config) => {
                info!(path = %path.display(), "Loaded desktop config");
                config
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse desktop config, using defaults");
                DesktopConfig::default()
            }
        },
        Err(_) => DesktopConfig::default(),
    }
}

/// Save configuration to `~/.ato/desktop-config.json`.
pub fn save_config(config: &DesktopConfig) {
    let Some(path) = config_path() else {
        warn!("Cannot determine home directory, config not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write desktop config");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize desktop config");
        }
    }
}

// ── Secret Store ──────────────────────────────────────────────────────────────

/// A single secret key-value pair.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecretEntry {
    pub key: String,
    /// Stored as plaintext in the JSON file (MVP).
    /// Phase 2: macOS Keychain integration.
    pub value: String,
}

/// Secret storage with per-capsule grant management.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SecretStore {
    /// Global secret entries.
    #[serde(default)]
    pub secrets: Vec<SecretEntry>,
    /// Per-capsule grants: capsule handle → list of secret keys allowed.
    #[serde(default)]
    pub grants: std::collections::HashMap<String, Vec<String>>,
}

impl SecretStore {
    pub fn add_secret(&mut self, key: String, value: String) {
        if let Some(existing) = self.secrets.iter_mut().find(|s| s.key == key) {
            existing.value = value;
        } else {
            self.secrets.push(SecretEntry { key, value });
        }
    }

    pub fn remove_secret(&mut self, key: &str) {
        self.secrets.retain(|s| s.key != key);
        for keys in self.grants.values_mut() {
            keys.retain(|k| k != key);
        }
    }

    pub fn secrets_for_capsule(&self, handle: &str) -> Vec<&SecretEntry> {
        let Some(allowed_keys) = self.grants.get(handle) else {
            return Vec::new();
        };
        self.secrets
            .iter()
            .filter(|s| allowed_keys.contains(&s.key))
            .collect()
    }

    pub fn grant_secret(&mut self, capsule_handle: &str, key: &str) {
        let keys = self
            .grants
            .entry(capsule_handle.to_string())
            .or_default();
        if !keys.contains(&key.to_string()) {
            keys.push(key.to_string());
        }
    }

    pub fn revoke_secret(&mut self, capsule_handle: &str, key: &str) {
        if let Some(keys) = self.grants.get_mut(capsule_handle) {
            keys.retain(|k| k != key);
        }
    }
}

fn secrets_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ato").join("secrets.json"))
}

pub fn load_secrets() -> SecretStore {
    let Some(path) = secrets_path() else {
        return SecretStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => {
                info!(path = %path.display(), "Loaded secret store");
                store
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse secret store, using empty");
                SecretStore::default()
            }
        },
        Err(_) => SecretStore::default(),
    }
}

pub fn save_secrets(store: &SecretStore) {
    let Some(path) = secrets_path() else {
        warn!("Cannot determine home directory, secrets not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write secret store");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize secret store");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips() {
        let config = DesktopConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.terminal_font_size, 14);
        assert_eq!(parsed.terminal_max_sessions, 4);
        assert!(!parsed.auto_open_devtools);
        assert_eq!(parsed.theme, ThemeConfig::Dark);
    }

    #[test]
    fn partial_json_uses_defaults() {
        let json = r#"{"theme": "light"}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.theme, ThemeConfig::Light);
        assert_eq!(parsed.terminal_font_size, 14);
        assert!(parsed.default_egress_allow.is_empty());
    }
}
