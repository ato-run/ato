use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvOrigin {
    Host,
    ManifestStatic,
    ManifestRequiredEnv,
    DepIdentityExport(String),
    DepRuntimeExport(String),
    DepCredential(String, String),
}

impl EnvOrigin {
    pub fn is_identity_trackable(&self) -> bool {
        matches!(
            self,
            EnvOrigin::Host
                | EnvOrigin::ManifestStatic
                | EnvOrigin::ManifestRequiredEnv
                | EnvOrigin::DepIdentityExport(_)
        )
    }
}

pub fn default_env_origin() -> EnvOrigin {
    EnvOrigin::Host
}
