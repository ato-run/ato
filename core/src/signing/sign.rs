//! Ed25519 Signature Creation
//!
//! Migrated from nacelle/src/verification/signing.rs

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CapsuleError, Result};
/// Capsule signature metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleSignature {
    /// Signature algorithm (ed25519)
    pub algorithm: String,
    /// Base64-encoded signature
    pub signature: String,
    /// SHA-256 hash of the signed content
    pub content_hash: String,
    /// Public key used for signing (base64)
    pub public_key: String,
    /// Signer identity
    pub signer: String,
    /// When the signature was created (Unix timestamp)
    pub signed_at: u64,
    /// Optional: Sigstore transparency log entry URL
    pub transparency_log_url: Option<String>,
}

/// Sign a capsule bundle
///
/// # Arguments
/// * `bundle_path` - Path to the bundle directory
/// * `key_path` - Path to the private key file
/// * `signer_name` - Identity of the signer
///
/// # Returns
/// Signature metadata written to `.signature` file
///
/// # Deprecated
/// This function uses the legacy `.signature` directory-bundle format (content_hash over
/// `capsule.toml` only). New capsule artifacts use `signature.json` inside the TAR with
/// `manifest_hash` + `payload_hash`. Use `capsule_core::types::signing::sign_capsule_artifact`
/// for packing, or `signing::sign_artifact` for sidecar `.sig` files on release artifacts.
#[allow(dead_code)]
pub fn sign_bundle(
    bundle_path: &Path,
    key_path: &Path,
    signer_name: &str,
) -> Result<CapsuleSignature> {
    // Read the private key
    let key_bytes = read_key_bytes(key_path)?;
    let signing_key =
        SigningKey::from_bytes(&key_bytes.try_into().map_err(|_| {
            CapsuleError::Crypto("Invalid key length (expected 32 bytes)".to_string())
        })?);

    // Read manifest to sign
    let manifest_path = bundle_path.join("capsule.toml");
    let manifest_bytes = std::fs::read(&manifest_path)?;

    // Hash the content
    let mut hasher = Sha256::new();
    hasher.update(&manifest_bytes);
    let hash = hasher.finalize();
    let content_hash = hex::encode(hash);

    // Sign the manifest
    let signature = signing_key.sign(&manifest_bytes);

    // Get public key
    let verifying_key = signing_key.verifying_key();
    let public_key = BASE64.encode(verifying_key.as_bytes());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let sig_data = CapsuleSignature {
        algorithm: "ed25519".to_string(),
        signature: BASE64.encode(signature.to_bytes()),
        content_hash: content_hash.clone(),
        public_key: public_key.clone(),
        signer: signer_name.to_string(),
        signed_at: now,
        transparency_log_url: None,
    };

    // Write signature file
    let sig_path = bundle_path.join(".signature");
    let sig_json =
        serde_json::to_string_pretty(&sig_data).map_err(|e| CapsuleError::Crypto(e.to_string()))?;
    std::fs::write(&sig_path, sig_json)?;

    tracing::info!("✅ Signed bundle: {}", bundle_path.display());
    tracing::info!("   Signer: {}", signer_name);
    tracing::info!("   Public key: {}...", &public_key[..20]);
    tracing::info!("   Content hash: {}...", &content_hash[..16]);

    Ok(sig_data)
}

/// Sign a single artifact (e.g., .wasm) and write a sidecar .sig file.
///
/// Returns the signature file path.
pub fn sign_artifact(
    artifact_path: &Path,
    key_path: &Path,
    signer_name: &str,
    signature_path: Option<std::path::PathBuf>,
) -> Result<std::path::PathBuf> {
    let key_bytes = read_key_bytes(key_path)?;
    let signing_key =
        SigningKey::from_bytes(&key_bytes.try_into().map_err(|_| {
            CapsuleError::Crypto("Invalid key length (expected 32 bytes)".to_string())
        })?);

    let artifact_bytes = std::fs::read(artifact_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&artifact_bytes);
    let hash = hasher.finalize();
    let content_hash = hex::encode(hash);

    let signature = signing_key.sign(&artifact_bytes);
    let verifying_key = signing_key.verifying_key();
    let public_key = BASE64.encode(verifying_key.as_bytes());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let sig_data = CapsuleSignature {
        algorithm: "ed25519".to_string(),
        signature: BASE64.encode(signature.to_bytes()),
        content_hash: content_hash.clone(),
        public_key: public_key.clone(),
        signer: signer_name.to_string(),
        signed_at: now,
        transparency_log_url: None,
    };

    let sig_path = signature_path.unwrap_or_else(|| default_sig_path(artifact_path));
    let sig_json =
        serde_json::to_string_pretty(&sig_data).map_err(|e| CapsuleError::Crypto(e.to_string()))?;
    std::fs::write(&sig_path, sig_json)?;

    tracing::info!("✅ Signed artifact: {}", artifact_path.display());
    tracing::info!("   Signature: {}", sig_path.display());
    tracing::info!("   Signer: {}", signer_name);
    tracing::info!("   Public key: {}...", &public_key[..20]);
    tracing::info!("   Content hash: {}...", &content_hash[..16]);

    Ok(sig_path)
}

fn default_sig_path(artifact_path: &Path) -> std::path::PathBuf {
    if let Some(ext) = artifact_path.extension().and_then(|s| s.to_str()) {
        let new_ext = format!("{}.sig", ext);
        return artifact_path.with_extension(new_ext);
    }
    artifact_path.with_extension("sig")
}

/// Generate a new Ed25519 key pair
///
/// # Arguments
/// * `output_path` - Path to write the private key (32 bytes)
///
/// # Returns
/// Base64-encoded public key string
#[allow(dead_code)]
pub fn generate_keypair(output_path: &Path) -> Result<String> {
    let mut rng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut rng);

    // Write private key
    std::fs::write(output_path, signing_key.to_bytes())?;

    // Set restrictive permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(output_path)?.permissions();
        perms.set_mode(0o600); // Owner read/write only
        std::fs::set_permissions(output_path, perms)?;
    }

    let verifying_key = signing_key.verifying_key();
    let public_key = BASE64.encode(verifying_key.as_bytes());

    // Write public key
    let pub_path = output_path.with_extension("pub");
    std::fs::write(&pub_path, &public_key)?;

    tracing::info!("✅ Generated key pair:");
    tracing::info!("   Private key: {}", output_path.display());
    tracing::info!("   Public key: {}", pub_path.display());
    tracing::info!("   Public key (base64): {}", public_key);

    Ok(public_key)
}

fn read_key_bytes(path: &Path) -> Result<Vec<u8>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CapsuleError::AuthRequired(format!(
                "Signing key not found: {}",
                path.display()
            )));
        }
        Err(err) => return Err(CapsuleError::Io(err)),
    };

    // Try to parse as JSON (StoredKey format)
    if let Ok(text) = std::str::from_utf8(&bytes) {
        if let Ok(stored) = serde_json::from_str::<StoredKeyRef>(text) {
            if stored.key_type == "ed25519" {
                if let Ok(secret_bytes) = BASE64.decode(&stored.secret_key) {
                    return Ok(secret_bytes);
                }
            }
        }
    }

    // Otherwise, return raw bytes
    Ok(bytes)
}

/// Reference to StoredKey for parsing JSON key files
#[derive(Deserialize)]
struct StoredKeyRef {
    key_type: String,
    #[allow(dead_code)]
    public_key: String,
    secret_key: String,
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{generate_keypair, sign_bundle};

    #[test]
    fn test_generate_keypair() {
        let temp = tempdir().unwrap();
        let key_path = temp.path().join("test.key");

        let public_key = generate_keypair(&key_path).unwrap();

        // Check files exist
        assert!(key_path.exists());
        assert!(key_path.with_extension("pub").exists());

        // Check key format
        assert_eq!(public_key.len(), 44); // Base64 encoded 32 bytes

        // Check private key length
        let key_bytes = std::fs::read(&key_path).unwrap();
        assert_eq!(key_bytes.len(), 32);
    }

    #[test]
    fn test_sign_bundle() {
        let temp = tempdir().unwrap();
        let bundle_path = temp.path().join("bundle");
        std::fs::create_dir(&bundle_path).unwrap();

        // Create dummy manifest
        let manifest = r#"
[capsule]
name = "test"
version = "1.0.0"
"#;
        std::fs::write(bundle_path.join("capsule.toml"), manifest).unwrap();

        // Generate key
        let key_path = temp.path().join("test.key");
        generate_keypair(&key_path).unwrap();

        // Sign bundle
        let sig = sign_bundle(&bundle_path, &key_path, "test-signer").unwrap();

        assert_eq!(sig.algorithm, "ed25519");
        assert_eq!(sig.signer, "test-signer");
        assert!(!sig.signature.is_empty());
        assert!(!sig.content_hash.is_empty());

        // Check signature file exists
        assert!(bundle_path.join(".signature").exists());
    }
}
