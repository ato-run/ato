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
        let keys = self.grants.entry(capsule_handle.to_string()).or_default();
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

// ── Capsule Config Store (non-secret) ─────────────────────────────────────────

/// Per-capsule plaintext configuration (model name, port, etc.).
///
/// Mirrors `SecretStore` for non-secret kinds — `String`, `Number`,
/// `Enum` from `ConfigField`. Two reasons we keep this separate from
/// the secret store rather than overloading `SecretStore`:
///
/// 1. **Threat model.** Secrets are write-only in the UI (masked
///    input, never re-displayed); non-secret values are read-write
///    and intentionally rendered back into the modal so the user can
///    see what they previously chose. Mixing them invites a bug
///    where a secret leaks into the read-back path.
/// 2. **Grant model.** Secrets require an explicit per-capsule grant
///    (`SecretStore.grants`) so a capsule can only read keys the
///    user has approved for it. Non-secret config has no such
///    isolation requirement — it lives next to the capsule that
///    asked for it. The shared map shape would force an unused
///    grant table on the non-secret path.
///
/// Persisted at `~/.ato/capsule-configs.json` as a flat JSON object.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapsuleConfigStore {
    /// `handle` → (`name` → `value`). Empty maps are kept to make
    /// "this capsule has been configured before, just not for these
    /// keys" distinguishable from "never configured" — Day 6's UX
    /// may want to surface that distinction in the modal.
    #[serde(default)]
    pub configs: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl CapsuleConfigStore {
    /// Set (or overwrite) a single config value for a capsule.
    pub fn set_config(&mut self, capsule_handle: &str, key: String, value: String) {
        self.configs
            .entry(capsule_handle.to_string())
            .or_default()
            .insert(key, value);
    }

    /// Snapshot of all `KEY = value` pairs configured for `handle`.
    /// Returns an empty vec when the capsule has no recorded
    /// configuration yet — callers should treat the empty case as
    /// "let preflight tell us what's missing" rather than as an
    /// error.
    pub fn configs_for_capsule(&self, handle: &str) -> Vec<(String, String)> {
        match self.configs.get(handle) {
            Some(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => Vec::new(),
        }
    }

    /// Remove a single config entry. Used by future Day 7+ "Reset
    /// configuration" affordances; not wired into the modal yet.
    #[allow(dead_code)]
    pub fn clear_config(&mut self, capsule_handle: &str, key: &str) {
        if let Some(map) = self.configs.get_mut(capsule_handle) {
            map.remove(key);
        }
    }
}

fn capsule_configs_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ato").join("capsule-configs.json"))
}

pub fn load_capsule_configs() -> CapsuleConfigStore {
    let Some(path) = capsule_configs_path() else {
        return CapsuleConfigStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => {
                info!(path = %path.display(), "Loaded capsule config store");
                store
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse capsule config store, using empty");
                CapsuleConfigStore::default()
            }
        },
        Err(_) => CapsuleConfigStore::default(),
    }
}

pub fn save_capsule_configs(store: &CapsuleConfigStore) {
    let Some(path) = capsule_configs_path() else {
        warn!("Cannot determine home directory, capsule configs not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write capsule config store");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize capsule config store");
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

    #[test]
    fn capsule_config_store_set_and_query_roundtrip() {
        let mut store = CapsuleConfigStore::default();
        store.set_config("capsule.byok-ai-chat", "MODEL".into(), "gpt-4".into());
        store.set_config("capsule.byok-ai-chat", "PORT".into(), "8080".into());
        store.set_config("capsule.other", "MODEL".into(), "claude".into());

        let mut byok = store.configs_for_capsule("capsule.byok-ai-chat");
        byok.sort();
        assert_eq!(
            byok,
            vec![
                ("MODEL".to_string(), "gpt-4".to_string()),
                ("PORT".to_string(), "8080".to_string()),
            ],
            "configs_for_capsule must isolate per-handle entries",
        );
        // Missing handle returns empty — never an error.
        assert!(store.configs_for_capsule("capsule.unknown").is_empty());

        // JSON round-trip preserves the nested shape.
        let json = serde_json::to_string(&store).unwrap();
        let parsed: CapsuleConfigStore = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.configs.len(), 2);
        assert_eq!(
            parsed
                .configs
                .get("capsule.byok-ai-chat")
                .unwrap()
                .get("MODEL"),
            Some(&"gpt-4".to_string())
        );
    }

    #[test]
    fn capsule_config_store_overwrites_same_key() {
        let mut store = CapsuleConfigStore::default();
        store.set_config("capsule.x", "MODEL".into(), "gpt-4".into());
        store.set_config("capsule.x", "MODEL".into(), "gpt-5".into());
        let configs = store.configs_for_capsule("capsule.x");
        assert_eq!(configs, vec![("MODEL".to_string(), "gpt-5".to_string())]);
    }
}
