//! Profile Capsule types
//!
//! Types for profile.sync manifest and payload.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Profile manifest content type
pub const PROFILE_CONTENT_TYPE: &str = "application/vnd.capsule.profile";

/// Profile sync section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSync {
    pub version: String,
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_ext: Option<String>,
}

impl Default for ProfileSync {
    fn default() -> Self {
        Self {
            version: "1.0".to_string(),
            content_type: PROFILE_CONTENT_TYPE.to_string(),
            display_ext: Some("profile".to_string()),
        }
    }
}

/// Profile metadata section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub created_by: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Profile information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<HashMap<String, String>>,
}

/// Profile policy section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilePolicy {
    #[serde(default = "default_ttl")]
    pub ttl: u64,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_ttl() -> u64 {
    86400 // 24 hours
}

fn default_timeout() -> u64 {
    10
}

impl Default for ProfilePolicy {
    fn default() -> Self {
        Self {
            ttl: default_ttl(),
            timeout: default_timeout(),
        }
    }
}

/// Profile permissions (minimal for read-only profile)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilePermissions {
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub allow_env: Vec<String>,
}

/// Profile signature section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSignature {
    pub algo: String,
    pub manifest_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    pub timestamp: String,
    pub value: String,
}

/// Complete profile manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManifest {
    pub sync: ProfileSync,
    pub meta: ProfileMeta,
    pub profile: ProfileInfo,
    #[serde(default)]
    pub policy: ProfilePolicy,
    #[serde(default)]
    pub permissions: ProfilePermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<ProfileSignature>,
}

impl ProfileManifest {
    /// Create a new unsigned profile manifest
    pub fn new(did: String, display_name: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            sync: ProfileSync::default(),
            meta: ProfileMeta {
                created_by: did,
                created_at: now,
                updated_at: None,
            },
            profile: ProfileInfo {
                display_name,
                bio: None,
                avatar_hash: None,
                links: None,
            },
            policy: ProfilePolicy::default(),
            permissions: ProfilePermissions::default(),
            signature: None,
        }
    }

    /// Get the DID of the profile owner
    pub fn did(&self) -> &str {
        &self.meta.created_by
    }

    /// Get the display name
    pub fn display_name(&self) -> &str {
        &self.profile.display_name
    }

    /// Check if profile has a valid signature
    pub fn is_signed(&self) -> bool {
        self.signature.is_some()
    }

    /// Serialize to TOML
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Parse from TOML
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_manifest_creation() {
        let did = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string();
        let manifest = ProfileManifest::new(did.clone(), "Alice".to_string());

        assert_eq!(manifest.did(), did);
        assert_eq!(manifest.display_name(), "Alice");
        assert!(!manifest.is_signed());
        assert_eq!(manifest.sync.content_type, PROFILE_CONTENT_TYPE);
    }

    #[test]
    fn test_profile_manifest_toml_roundtrip() {
        let did = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string();
        let mut manifest = ProfileManifest::new(did, "Bob".to_string());
        manifest.profile.bio = Some("Test bio".to_string());
        manifest.profile.links = Some(HashMap::from([(
            "website".to_string(),
            "https://example.com".to_string(),
        )]));

        let toml_str = manifest.to_toml().unwrap();
        let parsed = ProfileManifest::from_toml(&toml_str).unwrap();

        assert_eq!(parsed.display_name(), "Bob");
        assert_eq!(parsed.profile.bio, Some("Test bio".to_string()));
    }
}
