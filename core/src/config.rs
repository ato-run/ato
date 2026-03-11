use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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
        .context("Failed to determine home directory")?
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

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;

    let cfg = toml::from_str::<CapsuleConfig>(&raw)
        .with_context(|| format!("Failed to parse TOML config: {}", path.display()))?;
    Ok(cfg)
}

pub fn save_config(cfg: &CapsuleConfig) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir: {}", dir.display()))?;

    let path = config_path()?;
    let toml = toml::to_string_pretty(cfg).context("Failed to serialize config")?;
    write_atomic(&path, toml.as_bytes())
        .with_context(|| format!("Failed to write config: {}", path.display()))?;
    Ok(())
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, content)
        .with_context(|| format!("Failed to write temp file: {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename temp file into place: {}", path.display()))?;
    Ok(())
}
