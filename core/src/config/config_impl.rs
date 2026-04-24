use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CapsuleError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapsuleConfig {
    /// Default engine name (key into `engines`).
    #[serde(default)]
    pub default_engine: Option<String>,

    /// Registered engines by name.
    #[serde(default)]
    pub engines: HashMap<String, EngineRegistration>,

    /// Registry-related user settings.
    #[serde(default)]
    pub registry: RegistryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineRegistration {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    /// Default registry URL used by supported commands.
    #[serde(default)]
    pub url: Option<String>,
}

pub fn config_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| CapsuleError::Config("Failed to determine home directory".to_string()))?
        .join(".ato"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load_config() -> Result<CapsuleConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(CapsuleConfig::default());
    }

    let raw = fs::read_to_string(&path).map_err(|e| {
        CapsuleError::Config(format!("Failed to read config '{}': {}", path.display(), e))
    })?;

    let cfg = toml::from_str::<CapsuleConfig>(&raw).map_err(|e| {
        CapsuleError::Config(format!(
            "Failed to parse TOML config '{}': {}",
            path.display(),
            e
        ))
    })?;
    Ok(cfg)
}

pub fn save_config(cfg: &CapsuleConfig) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).map_err(|e| {
        CapsuleError::Config(format!(
            "Failed to create config dir '{}': {}",
            dir.display(),
            e
        ))
    })?;

    let path = config_path()?;
    let toml = toml::to_string_pretty(cfg)
        .map_err(|e| CapsuleError::Config(format!("Failed to serialize config: {}", e)))?;
    write_atomic(&path, toml.as_bytes())
        .map_err(|e| CapsuleError::Config(format!("Failed to write config: {}", e)))?;
    Ok(())
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, content).map_err(|e| CapsuleError::Io(e))?;
    fs::rename(&tmp, path).map_err(|e| CapsuleError::Io(e))?;
    Ok(())
}
