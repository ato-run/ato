//! Ed25519 Signature Verification
//!
//! Migrated from nacelle/src/verification/signing.rs

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::sign::CapsuleSignature;
use crate::error::{CapsuleError, Result};

/// Verify a signed bundle
///
/// # Arguments
/// * `bundle_path` - Path to the bundle directory
/// * `trusted_public_keys` - List of trusted public keys (base64 encoded)
///
/// # Returns
/// Ok if signature is valid, Err otherwise
#[allow(dead_code)]
pub fn verify_bundle(bundle_path: &Path, trusted_public_keys: &[String]) -> Result<()> {
    // Read signature file
    let sig_path = bundle_path.join(".signature");
    if !sig_path.exists() {
        return Err(CapsuleError::NotFound(
            "No signature file found in bundle".to_string(),
        ));
    }

    let sig_json = std::fs::read_to_string(&sig_path)?;
    let sig_data: CapsuleSignature =
        serde_json::from_str(&sig_json).map_err(|e| CapsuleError::Crypto(e.to_string()))?;

    // Read manifest
    let manifest_path = bundle_path.join("capsule.toml");
    let manifest_bytes = std::fs::read(&manifest_path)?;

    // Verify content hash
    let mut hasher = Sha256::new();
    hasher.update(&manifest_bytes);
    let hash = hasher.finalize();
    let content_hash = hex::encode(hash);

    if content_hash != sig_data.content_hash {
        return Err(CapsuleError::HashMismatch(
            sig_data.content_hash,
            content_hash,
        ));
    }

    // Check if public key is trusted
    if !trusted_public_keys.contains(&sig_data.public_key) {
        return Err(CapsuleError::AuthRequired(format!(
            "Public key {}... is not in trusted key list",
            &sig_data.public_key[..20]
        )));
    }

    // Decode public key
    let pub_key_bytes = BASE64
        .decode(&sig_data.public_key)
        .map_err(|e| CapsuleError::Crypto(e.to_string()))?;
    let verifying_key = VerifyingKey::from_bytes(
        &pub_key_bytes
            .try_into()
            .map_err(|_| CapsuleError::Crypto("Invalid public key length".to_string()))?,
    )
    .map_err(|e| CapsuleError::Crypto(e.to_string()))?;

    // Decode signature
    let sig_bytes = BASE64
        .decode(&sig_data.signature)
        .map_err(|e| CapsuleError::Crypto(e.to_string()))?;
    let signature = Signature::from_bytes(
        &sig_bytes
            .try_into()
            .map_err(|_| CapsuleError::Crypto("Invalid signature length".to_string()))?,
    );

    // Verify signature
    verifying_key
        .verify(&manifest_bytes, &signature)
        .map_err(|e| CapsuleError::Crypto(format!("Signature verification failed: {}", e)))?;

    tracing::info!("✅ Signature verified:");
    tracing::info!("   Signer: {}", sig_data.signer);
    tracing::info!("   Public key: {}...", &sig_data.public_key[..20]);
    tracing::info!("   Signed at: {} (Unix timestamp)", sig_data.signed_at);

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::verify_bundle;
    use crate::signing::sign::{generate_keypair, sign_bundle};

    #[test]
    fn test_verify_valid_signature() {
        let temp = tempdir().unwrap();
        let bundle_path = temp.path().join("bundle");
        std::fs::create_dir(&bundle_path).unwrap();

        // Create manifest
        let manifest = r#"
[capsule]
name = "test"
version = "1.0.0"
"#;
        std::fs::write(bundle_path.join("capsule.toml"), manifest).unwrap();

        // Generate key and sign
        let key_path = temp.path().join("test.key");
        let public_key = generate_keypair(&key_path).unwrap();
        sign_bundle(&bundle_path, &key_path, "test-signer").unwrap();

        // Verify
        let result = verify_bundle(&bundle_path, &[public_key]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_tampered_manifest() {
        let temp = tempdir().unwrap();
        let bundle_path = temp.path().join("bundle");
        std::fs::create_dir(&bundle_path).unwrap();

        // Create and sign manifest
        let manifest = r#"
[capsule]
name = "test"
version = "1.0.0"
"#;
        std::fs::write(bundle_path.join("capsule.toml"), manifest).unwrap();

        let key_path = temp.path().join("test.key");
        let public_key = generate_keypair(&key_path).unwrap();
        sign_bundle(&bundle_path, &key_path, "test-signer").unwrap();

        // Tamper with manifest
        let tampered = manifest.replace("1.0.0", "2.0.0");
        std::fs::write(bundle_path.join("capsule.toml"), tampered).unwrap();

        // Verify should fail
        let result = verify_bundle(&bundle_path, &[public_key]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Hash mismatch"));
    }

    #[test]
    fn test_verify_untrusted_key() {
        let temp = tempdir().unwrap();
        let bundle_path = temp.path().join("bundle");
        std::fs::create_dir(&bundle_path).unwrap();

        // Create and sign manifest
        let manifest = r#"
[capsule]
name = "test"
version = "1.0.0"
"#;
        std::fs::write(bundle_path.join("capsule.toml"), manifest).unwrap();

        let key_path = temp.path().join("test.key");
        generate_keypair(&key_path).unwrap();
        sign_bundle(&bundle_path, &key_path, "test-signer").unwrap();

        // Try to verify with different trusted key
        let other_key_path = temp.path().join("other.key");
        let other_public_key = generate_keypair(&other_key_path).unwrap();

        let result = verify_bundle(&bundle_path, &[other_public_key]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not in trusted key list"));
    }

    #[test]
    fn test_verify_missing_signature() {
        let temp = tempdir().unwrap();
        let bundle_path = temp.path().join("bundle");
        std::fs::create_dir(&bundle_path).unwrap();

        // Create manifest without signing
        let manifest = r#"
[capsule]
name = "test"
version = "1.0.0"
"#;
        std::fs::write(bundle_path.join("capsule.toml"), manifest).unwrap();

        let result = verify_bundle(&bundle_path, &["dummy_key".to_string()]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No signature file found"));
    }
}
