mod http;
mod install;
mod locks;
mod policy;
mod release;

use anyhow::{Context, Result};
use std::path::PathBuf;

#[allow(unused_imports)]
pub(crate) use install::{auto_bootstrap_nacelle, install_engine_release};
#[allow(unused_imports)]
pub(crate) use policy::resolve_auto_bootstrap_policy_from_env;
#[allow(unused_imports)]
pub(crate) use release::{
    extract_first_sha256_hex, fetch_release_sha256, parse_sha256_for_artifact,
};

pub(super) const ENGINES_DIR: &str = ".ato/engines";
pub(super) const ENGINE_LOCK_DIR: &str = ".locks";
pub(super) const DEFAULT_NACELLE_RELEASE_BASE_URL: &str = "https://releases.capsule.dev/nacelle";
pub(super) const AUTO_BOOTSTRAP_ENV: &str = "ATO_NACELLE_AUTO_BOOTSTRAP";
pub(super) const OFFLINE_ENV: &str = "ATO_OFFLINE";
pub(super) const DISABLE_NETWORK_BOOTSTRAP_ENV: &str = "ATO_DISABLE_NETWORK_BOOTSTRAP";
pub(super) const NACELLE_VERSION_ENV: &str = "ATO_NACELLE_VERSION";
pub(super) const NACELLE_RELEASE_BASE_URL_ENV: &str = "ATO_NACELLE_RELEASE_BASE_URL";

pub const PINNED_NACELLE_VERSION: &str = "v0.2.1";

#[cfg(test)]
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub arch: String,
    pub os: String,
}

#[allow(dead_code)]
pub(crate) struct EngineInstallResult {
    pub version: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NacelleBootstrapPolicy {
    pub version: String,
    pub release_base_url: String,
    pub network_allowed: bool,
    pub disabled_reason: Option<String>,
}

pub struct EngineManager {
    engines_dir: PathBuf,
}

impl EngineManager {
    pub fn new() -> Result<Self> {
        let engines_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
            .join(ENGINES_DIR);

        if !engines_dir.exists() {
            std::fs::create_dir_all(&engines_dir).with_context(|| {
                format!(
                    "Failed to create engines directory: {}",
                    engines_dir.display()
                )
            })?;
        }

        Ok(Self { engines_dir })
    }

    pub fn engine_path(&self, name: &str, version: &str) -> PathBuf {
        self.engines_dir.join(format!("{}-{}", name, version))
    }

    #[cfg(test)]
    fn parse_engine_filename(&self, filename: &str) -> Option<EngineInfo> {
        let parts: Vec<&str> = filename.split('-').collect();
        if parts.len() < 3 {
            return None;
        }

        let name = parts[0];
        let version = parts[1];
        let os_arch = parts[2..].join("-");

        let (os, arch) = if os_arch.contains('-') {
            let os_arch_parts: Vec<&str> = os_arch.splitn(2, '-').collect();
            (os_arch_parts[0], os_arch_parts[1])
        } else {
            ("unknown", os_arch.as_str())
        };

        Some(EngineInfo {
            name: name.to_string(),
            version: version.to_string(),
            url: String::new(),
            sha256: String::new(),
            arch: arch.to_string(),
            os: os.to_string(),
        })
    }
}

#[cfg(test)]
mod tests;
