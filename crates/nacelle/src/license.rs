//! License verification module
//!
//! Implements Proof of License (PoL) verification for nacelle runtime.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use blake3::Hasher;
use chrono::{Duration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::io::Read;
use std::path::Path;

/// License content type
pub const LICENSE_CONTENT_TYPE: &str = "application/vnd.capsule.license";

/// Grace period for subscriptions (7 days)
pub const GRACE_PERIOD_DAYS: i64 = 7;

/// License types
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenseType {
    Perpetual,
    Subscription,
    Trial,
}

/// License manifest structure (minimal fields for verification)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LicenseManifest {
    pub sync: LicenseSync,
    pub meta: LicenseMeta,
    pub license: LicenseInfo,
    pub signature: Option<LicenseSignature>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LicenseSync {
    pub version: String,
    pub content_type: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LicenseMeta {
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LicenseInfo {
    pub grantee: String,
    pub target: String,
    #[serde(rename = "type")]
    pub license_type: LicenseType,
    pub expiry: Option<String>,
    #[serde(default)]
    pub entitlements: Vec<String>,
    pub license_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LicenseSignature {
    pub algo: String,
    pub manifest_hash: String,
    pub payload_hash: Option<String>,
    pub timestamp: String,
    pub value: String,
}

/// License verification result
#[derive(Debug, Clone)]
pub enum LicenseVerificationResult {
    /// License is valid
    Valid {
        entitlements: Vec<String>,
        expiry: Option<chrono::DateTime<Utc>>,
    },
    /// License is expired
    Expired {
        expired_at: chrono::DateTime<Utc>,
        grace_remaining: Option<Duration>,
    },
    /// Signature verification failed
    InvalidSignature(String),
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
            Self::Valid { .. } => true,
            Self::Expired {
                grace_remaining, ..
            } => grace_remaining.is_some(),
            _ => false,
        }
    }

    /// Get entitlements if valid or in grace period
    pub fn entitlements(&self) -> Vec<String> {
        match self {
            Self::Valid { entitlements, .. } => entitlements.clone(),
            _ => vec![],
        }
    }

    /// Format entitlements as environment variable value
    pub fn entitlements_env(&self) -> String {
        self.entitlements().join(",")
    }
}

/// Verify a license.sync archive
pub fn verify_license(
    license_path: &Path,
    app_did: &str,
    user_did: &str,
) -> Result<LicenseVerificationResult> {
    // Read and parse the license archive
    let file = std::fs::File::open(license_path)
        .with_context(|| format!("Failed to open license: {}", license_path.display()))?;

    let mut archive =
        zip::ZipArchive::new(file).with_context(|| "Failed to read license archive")?;

    // Read manifest.toml
    let manifest_content = {
        let mut manifest_file = archive
            .by_name("manifest.toml")
            .with_context(|| "manifest.toml not found in license")?;
        let mut content = String::new();
        manifest_file.read_to_string(&mut content)?;
        content
    };

    let manifest: LicenseManifest = toml::from_str(&manifest_content)
        .map_err(|e| LicenseVerificationResult::MalformedLicense(e.to_string()))
        .map_err(|_| anyhow::anyhow!("Failed to parse license manifest"))?;

    // Verify content type
    if manifest.sync.content_type != LICENSE_CONTENT_TYPE {
        return Ok(LicenseVerificationResult::MalformedLicense(format!(
            "Invalid content_type: {}",
            manifest.sync.content_type
        )));
    }

    // Verify signature
    if let Some(ref sig) = manifest.signature {
        match verify_signature(&manifest_content, sig) {
            Ok(false) => {
                return Ok(LicenseVerificationResult::InvalidSignature(
                    "Signature verification failed".to_string(),
                ));
            }
            Err(e) => {
                return Ok(LicenseVerificationResult::InvalidSignature(e.to_string()));
            }
            _ => {}
        }
    } else {
        return Ok(LicenseVerificationResult::InvalidSignature(
            "License is not signed".to_string(),
        ));
    }

    // Verify target
    if manifest.license.target != app_did {
        return Ok(LicenseVerificationResult::TargetMismatch {
            expected: app_did.to_string(),
            actual: manifest.license.target.clone(),
        });
    }

    // Verify grantee
    if manifest.license.grantee != user_did {
        return Ok(LicenseVerificationResult::GranteeMismatch {
            expected: user_did.to_string(),
            actual: manifest.license.grantee.clone(),
        });
    }

    // Verify expiry
    if let Some(ref expiry_str) = manifest.license.expiry {
        if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expiry_str) {
            let expiry_utc = expiry.with_timezone(&Utc);
            let now = Utc::now();

            if now > expiry_utc {
                let grace_end = expiry_utc + Duration::days(GRACE_PERIOD_DAYS);
                let grace_remaining = if manifest.license.license_type == LicenseType::Subscription
                    && now < grace_end
                {
                    Some(grace_end - now)
                } else {
                    None
                };

                return Ok(LicenseVerificationResult::Expired {
                    expired_at: expiry_utc,
                    grace_remaining,
                });
            }

            return Ok(LicenseVerificationResult::Valid {
                entitlements: manifest.license.entitlements.clone(),
                expiry: Some(expiry_utc),
            });
        }
    }

    // Perpetual license (no expiry)
    Ok(LicenseVerificationResult::Valid {
        entitlements: manifest.license.entitlements.clone(),
        expiry: None,
    })
}

/// Verify the manifest signature
fn verify_signature(manifest_content: &str, sig: &LicenseSignature) -> Result<bool> {
    if sig.algo != "Ed25519" {
        bail!("Unsupported signature algorithm: {}", sig.algo);
    }

    // Parse the manifest to remove [signature] section for hash verification
    let manifest_without_sig = remove_signature_section(manifest_content)?;

    // Compute manifest hash
    let mut hasher = Hasher::new();
    hasher.update(manifest_without_sig.as_bytes());
    let computed_hash = format!("blake3:{}", hex::encode(hasher.finalize().as_bytes()));

    if computed_hash != sig.manifest_hash {
        return Ok(false);
    }

    // Extract public key from created_by DID
    let manifest: LicenseManifest = toml::from_str(manifest_content)?;
    let public_key = did_to_public_key(&manifest.meta.created_by)?;

    // Verify signature
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).map_err(|_| anyhow::anyhow!("Invalid public key"))?;

    let sig_bytes = BASE64
        .decode(&sig.value)
        .with_context(|| "Failed to decode signature")?;

    if sig_bytes.len() != 64 {
        bail!("Invalid signature length");
    }

    let mut sig_array = [0u8; 64];
    sig_array.copy_from_slice(&sig_bytes);
    let signature = Signature::from_bytes(&sig_array);

    // Build signing payload
    let signing_payload = if let Some(ref ph) = sig.payload_hash {
        format!("{}|{}", sig.manifest_hash, ph)
    } else {
        sig.manifest_hash.clone()
    };

    Ok(verifying_key
        .verify(signing_payload.as_bytes(), &signature)
        .is_ok())
}

/// Remove [signature] section from TOML for hash verification
fn remove_signature_section(content: &str) -> Result<String> {
    let mut result = String::new();
    let mut in_signature = false;

    for line in content.lines() {
        if line.trim() == "[signature]" {
            in_signature = true;
            continue;
        }
        if in_signature && line.starts_with('[') {
            in_signature = false;
        }
        if !in_signature {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result.trim_end().to_string())
}

/// Extract public key from did:key
fn did_to_public_key(did: &str) -> Result<[u8; 32]> {
    use unsigned_varint::decode as varint_decode;

    if !did.starts_with("did:key:z") {
        bail!("Invalid did:key format");
    }

    let encoded = &did[9..]; // Remove "did:key:z"
    let decoded = bs58::decode(encoded)
        .into_vec()
        .map_err(|e| anyhow::anyhow!("Failed to decode base58: {}", e))?;

    // Parse multicodec prefix (0xed01 for ed25519-pub)
    let (codec, rest) = varint_decode::u64(&decoded)
        .map_err(|e| anyhow::anyhow!("Failed to decode multicodec: {}", e))?;

    if codec != 0xed01 {
        bail!("Unsupported key type: 0x{:x}", codec);
    }

    if rest.len() != 32 {
        bail!("Invalid public key length: {}", rest.len());
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(rest);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn test_entitlements_env() {
        let result = LicenseVerificationResult::Valid {
            entitlements: vec!["pro".to_string(), "cloud".to_string()],
            expiry: None,
        };
        assert_eq!(result.entitlements_env(), "pro,cloud");
    }
}
