use std::fmt;
use std::sync::Once;

use tracing::warn;

use crate::capsule_v3::cas_store::CasStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasDisableReason {
    EnvironmentVariableOff,
    InitializationFailed(String),
    NotSupportedByPlatform,
    NoV3ManifestInArtifact,
}

impl fmt::Display for CasDisableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CasDisableReason::EnvironmentVariableOff => {
                write!(f, "CAS was disabled by environment configuration")
            }
            CasDisableReason::InitializationFailed(message) => {
                write!(f, "CAS initialization failed: {message}")
            }
            CasDisableReason::NotSupportedByPlatform => {
                write!(f, "CAS is not supported on this platform")
            }
            CasDisableReason::NoV3ManifestInArtifact => {
                write!(f, "artifact does not contain payload.v3.manifest.json")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum CasProvider {
    Enabled(CasStore),
    Disabled(CasDisableReason),
}

impl CasProvider {
    pub fn from_env() -> Self {
        match CasStore::from_env() {
            Ok(store) => Self::Enabled(store),
            Err(err) => Self::Disabled(CasDisableReason::InitializationFailed(err.to_string())),
        }
    }

    pub fn as_store(&self) -> Option<&CasStore> {
        match self {
            Self::Enabled(store) => Some(store),
            Self::Disabled(_) => None,
        }
    }

    pub fn into_store(self) -> Option<CasStore> {
        match self {
            Self::Enabled(store) => Some(store),
            Self::Disabled(_) => None,
        }
    }

    pub fn disable_reason(&self) -> Option<&CasDisableReason> {
        match self {
            Self::Enabled(_) => None,
            Self::Disabled(reason) => Some(reason),
        }
    }

    pub fn log_disabled_once(context: &'static str, reason: &CasDisableReason) {
        static WARN_ONCE: Once = Once::new();
        WARN_ONCE.call_once(|| {
            warn!(
                context = context,
                reason = %reason,
                "CAS is disabled; falling back to legacy payload path"
            );
        });
    }
}
