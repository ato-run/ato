use crate::config;
use crate::error::{CapsuleError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustStore {
    pub fingerprints: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    Verified,
    Untrusted,
    Unknown,
}

impl TrustStore {
    pub fn load() -> Result<Self> {
        let path = trust_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| CapsuleError::Config(format!("Failed to read trust store: {}", e)))?;
        serde_json::from_str(&raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to parse trust store: {}", e)))
    }

    pub fn save(&self) -> Result<()> {
        let path = trust_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CapsuleError::Config(format!("Failed to create trust dir: {}", e)))?;
        }
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| CapsuleError::Config(format!("Failed to serialize trust store: {}", e)))?;
        fs::write(&path, raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to write trust store: {}", e)))?;
        Ok(())
    }

    pub fn verify_or_record(&mut self, did: &str, fingerprint: &str) -> TrustState {
        match self.fingerprints.get(did) {
            Some(existing) if existing == fingerprint => TrustState::Verified,
            Some(_) => TrustState::Untrusted,
            None => {
                self.fingerprints
                    .insert(did.to_string(), fingerprint.to_string());
                TrustState::Unknown
            }
        }
    }
}

fn trust_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("trust_store.json"))
}
