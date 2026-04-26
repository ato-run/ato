//! License Capsule types
//!
//! Types for license.sync manifest and verification.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// License manifest content type
pub const LICENSE_CONTENT_TYPE: &str = "application/vnd.capsule.license";

/// Grace period for subscription licenses (7 days)
pub const GRACE_PERIOD_DAYS: i64 = 7;

/// License types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenseType {
    /// One-time purchase, never expires
    Perpetual,
    /// Recurring payment, expires if not renewed
    Subscription,
    /// Time-limited trial
    Trial,
}

impl LicenseType {
    /// Check if this license type has a grace period
    pub fn has_grace_period(&self) -> bool {
        matches!(self, LicenseType::Subscription)
    }
}

/// License sync section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseSync {
    pub version: String,
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_ext: Option<String>,
}

impl Default for LicenseSync {
    fn default() -> Self {
        Self {
            version: "1.0".to_string(),
            content_type: LICENSE_CONTENT_TYPE.to_string(),
            display_ext: Some("license".to_string()),
        }
    }
}

/// License metadata section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseMeta {
    /// DID of the issuer (Registry or Developer)
    pub created_by: String,
    /// When the license was issued
    pub created_at: String,
}

/// Core license information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseInfo {
    /// DID of the license holder (purchaser)
    pub grantee: String,
    /// DID of the target app/content
    pub target: String,
    /// License type
    #[serde(rename = "type")]
    pub license_type: LicenseType,
    /// Expiration date (required for subscription/trial)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    /// Feature flags granted by this license
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entitlements: Vec<String>,
    /// Name of the issuing entity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer_name: Option<String>,
    /// URL of the issuing entity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer_url: Option<String>,
    /// Unique license identifier
    pub license_id: String,
}

impl LicenseInfo {
    /// Parse expiry as DateTime
    pub fn expiry_datetime(&self) -> Option<DateTime<Utc>> {
        self.expiry
            .as_ref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// Check if the license is expired (without grace period)
    pub fn is_expired(&self) -> bool {
        if let Some(expiry) = self.expiry_datetime() {
            Utc::now() > expiry
        } else {
            false // Perpetual licenses don't expire
        }
    }

    /// Check if the license is in grace period
    pub fn is_in_grace_period(&self) -> bool {
        if self.license_type != LicenseType::Subscription {
            return false;
        }
        if let Some(expiry) = self.expiry_datetime() {
            let now = Utc::now();
            let grace_end = expiry + Duration::days(GRACE_PERIOD_DAYS);
            now > expiry && now <= grace_end
        } else {
            false
        }
    }

    /// Get remaining grace period duration
    pub fn grace_remaining(&self) -> Option<Duration> {
        if !self.license_type.has_grace_period() {
            return None;
        }
        if let Some(expiry) = self.expiry_datetime() {
            let now = Utc::now();
            if now <= expiry {
                return None; // Not expired yet
            }
            let grace_end = expiry + Duration::days(GRACE_PERIOD_DAYS);
            if now <= grace_end {
                Some(grace_end - now)
            } else {
                None // Grace period ended
            }
        } else {
            None
        }
    }
}

/// License policy section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicensePolicy {
    #[serde(default = "default_ttl")]
    pub ttl: u64,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_ttl() -> u64 {
    86400 // 24 hours
}

fn default_timeout() -> u64 {
    30
}

impl Default for LicensePolicy {
    fn default() -> Self {
        Self {
            ttl: default_ttl(),
            timeout: default_timeout(),
        }
    }
}

/// License permissions section
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LicensePermissions {
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub allow_env: Vec<String>,
}

/// License signature section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseSignature {
    pub algo: String,
    pub manifest_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    pub timestamp: String,
    pub value: String,
}

/// Complete license manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseManifest {
    pub sync: LicenseSync,
    pub meta: LicenseMeta,
    pub license: LicenseInfo,
    #[serde(default)]
    pub policy: LicensePolicy,
    #[serde(default)]
    pub permissions: LicensePermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<LicenseSignature>,
}

impl LicenseManifest {
    /// Get the license ID
    pub fn license_id(&self) -> &str {
        &self.license.license_id
    }

    /// Get the grantee DID
    pub fn grantee(&self) -> &str {
        &self.license.grantee
    }

    /// Get the target app DID
    pub fn target(&self) -> &str {
        &self.license.target
    }

    /// Check if the license is signed
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

/// License verification result
#[derive(Debug, Clone)]
pub enum LicenseVerificationResult {
    /// License is valid
    Valid {
        entitlements: Vec<String>,
        expiry: Option<DateTime<Utc>>,
    },
    /// License is expired
    Expired {
        expired_at: DateTime<Utc>,
        grace_remaining: Option<Duration>,
    },
    /// Signature verification failed
    InvalidSignature,
    /// License target doesn't match the app
    TargetMismatch { expected: String, actual: String },
    /// License grantee doesn't match the user
    GranteeMismatch { expected: String, actual: String },
    /// License manifest is malformed
    MalformedLicense(String),
}

impl LicenseVerificationResult {
    /// Check if the license allows execution
    pub fn allows_execution(&self) -> bool {
        match self {
            LicenseVerificationResult::Valid { .. } => true,
            LicenseVerificationResult::Expired {
                grace_remaining, ..
            } => grace_remaining.is_some(),
            _ => false,
        }
    }

    /// Get entitlements if valid
    pub fn entitlements(&self) -> Option<&[String]> {
        match self {
            LicenseVerificationResult::Valid { entitlements, .. } => Some(entitlements),
            _ => None,
        }
    }
}

/// License JSON payload (embedded in license.sync/payload/license.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicensePayload {
    pub grantee: String,
    pub target: String,
    #[serde(rename = "type")]
    pub license_type: LicenseType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entitlements: Vec<String>,
    pub license_id: String,
    pub issued_at: String,
}

impl From<&LicenseInfo> for LicensePayload {
    fn from(info: &LicenseInfo) -> Self {
        Self {
            grantee: info.grantee.clone(),
            target: info.target.clone(),
            license_type: info.license_type,
            expiry: info.expiry.clone(),
            entitlements: info.entitlements.clone(),
            license_id: info.license_id.clone(),
            issued_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{
        LicenseInfo, LicenseManifest, LicenseMeta, LicensePermissions, LicensePolicy, LicenseSync,
        LicenseType, LicenseVerificationResult,
    };

    #[test]
    fn test_license_type_grace_period() {
        assert!(!LicenseType::Perpetual.has_grace_period());
        assert!(LicenseType::Subscription.has_grace_period());
        assert!(!LicenseType::Trial.has_grace_period());
    }

    #[test]
    fn test_perpetual_license_never_expires() {
        let info = LicenseInfo {
            grantee: "did:key:z6MkUser".to_string(),
            target: "did:key:z6MkApp".to_string(),
            license_type: LicenseType::Perpetual,
            expiry: None,
            entitlements: vec![],
            issuer_name: None,
            issuer_url: None,
            license_id: "lic_test123".to_string(),
        };
        assert!(!info.is_expired());
        assert!(!info.is_in_grace_period());
    }

    #[test]
    fn test_expired_subscription() {
        let past = (Utc::now() - Duration::days(3)).to_rfc3339();
        let info = LicenseInfo {
            grantee: "did:key:z6MkUser".to_string(),
            target: "did:key:z6MkApp".to_string(),
            license_type: LicenseType::Subscription,
            expiry: Some(past),
            entitlements: vec!["pro".to_string()],
            issuer_name: None,
            issuer_url: None,
            license_id: "lic_test456".to_string(),
        };
        assert!(info.is_expired());
        assert!(info.is_in_grace_period()); // Within 7-day grace
        assert!(info.grace_remaining().is_some());
    }

    #[test]
    fn test_expired_trial_no_grace() {
        let past = (Utc::now() - Duration::days(1)).to_rfc3339();
        let info = LicenseInfo {
            grantee: "did:key:z6MkUser".to_string(),
            target: "did:key:z6MkApp".to_string(),
            license_type: LicenseType::Trial,
            expiry: Some(past),
            entitlements: vec![],
            issuer_name: None,
            issuer_url: None,
            license_id: "lic_trial789".to_string(),
        };
        assert!(info.is_expired());
        assert!(!info.is_in_grace_period()); // Trials have no grace
        assert!(info.grace_remaining().is_none());
    }

    #[test]
    fn test_verification_result_allows_execution() {
        let valid = LicenseVerificationResult::Valid {
            entitlements: vec!["pro".to_string()],
            expiry: None,
        };
        assert!(valid.allows_execution());

        let expired_with_grace = LicenseVerificationResult::Expired {
            expired_at: Utc::now() - Duration::days(1),
            grace_remaining: Some(Duration::days(6)),
        };
        assert!(expired_with_grace.allows_execution());

        let expired_no_grace = LicenseVerificationResult::Expired {
            expired_at: Utc::now() - Duration::days(10),
            grace_remaining: None,
        };
        assert!(!expired_no_grace.allows_execution());

        let invalid = LicenseVerificationResult::InvalidSignature;
        assert!(!invalid.allows_execution());
    }

    #[test]
    fn test_license_manifest_toml_roundtrip() {
        let manifest = LicenseManifest {
            sync: LicenseSync::default(),
            meta: LicenseMeta {
                created_by: "did:key:z6MkIssuer".to_string(),
                created_at: "2026-02-02T00:00:00Z".to_string(),
            },
            license: LicenseInfo {
                grantee: "did:key:z6MkUser".to_string(),
                target: "did:key:z6MkApp".to_string(),
                license_type: LicenseType::Subscription,
                expiry: Some("2027-02-02T00:00:00Z".to_string()),
                entitlements: vec!["pro".to_string(), "cloud_sync".to_string()],
                issuer_name: Some("Test Registry".to_string()),
                issuer_url: Some("https://test.example.com".to_string()),
                license_id: "lic_abc123".to_string(),
            },
            policy: LicensePolicy::default(),
            permissions: LicensePermissions {
                allow_hosts: vec!["api.test.example.com".to_string()],
                allow_env: vec![],
            },
            signature: None,
        };

        let toml_str = manifest.to_toml().unwrap();
        let parsed = LicenseManifest::from_toml(&toml_str).unwrap();

        assert_eq!(parsed.license_id(), "lic_abc123");
        assert_eq!(parsed.grantee(), "did:key:z6MkUser");
        assert_eq!(parsed.license.entitlements.len(), 2);
    }
}
