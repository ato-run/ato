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
