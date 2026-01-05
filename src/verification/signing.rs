//! Capsule Signing and Verification
//!
//! Uses Ed25519 signatures for Capsule manifest verification.
//! Future: Integrate with Sigstore for transparency log.
//!
//! Security notes:
//! - Private keys should be handled with care
//! - Use zeroize for sensitive data cleanup (future improvement)
//! - Never log or expose private key material

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Capsule signer for creating signed manifests
pub struct CapsuleSigner {
    signing_key: SigningKey,
    signer_name: String,
}

impl CapsuleSigner {
    /// Create a new signer with a random key
    pub fn new(signer_name: &str) -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);

        Self {
            signing_key,
            signer_name: signer_name.to_string(),
        }
    }

    /// Create a signer from an existing key (for key management)
    pub fn from_key(signing_key: SigningKey, signer_name: &str) -> Self {
        Self {
            signing_key,
            signer_name: signer_name.to_string(),
        }
    }

    /// Sign capsule manifest content
    pub fn sign(&self, content: &[u8]) -> Result<CapsuleSignature> {
        // Hash the content
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hasher.finalize();
        let content_hash = hex::encode(hash);

        // Sign the hash
        let signature = self.signing_key.sign(content);

        // Get public key
        let verifying_key = self.signing_key.verifying_key();
        let public_key = BASE64.encode(verifying_key.as_bytes());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(CapsuleSignature {
            algorithm: "ed25519".to_string(),
            signature: BASE64.encode(signature.to_bytes()),
            content_hash,
            public_key,
            signer: self.signer_name.clone(),
            signed_at: now,
            transparency_log_url: None,
        })
    }

    /// Export public key for distribution
    pub fn public_key(&self) -> String {
        let verifying_key = self.signing_key.verifying_key();
        BASE64.encode(verifying_key.as_bytes())
    }

    /// Export private key bytes for secure storage
    ///
    /// # Security Warning
    /// This method returns raw key bytes. The caller is responsible for:
    /// - Securely storing the key (encrypted at rest)
    /// - Zeroizing memory after use
    /// - Never logging or exposing the key
    ///
    /// Consider using a key management system (KMS) instead.
    #[deprecated(
        since = "0.3.0",
        note = "Use secure key storage instead. This method may expose key material in memory."
    )]
    pub fn export_private_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }
}

/// Capsule verifier for checking signed manifests
pub struct CapsuleVerifier {
    trusted_keys: Vec<VerifyingKey>,
}

impl CapsuleVerifier {
    /// Create a verifier with a list of trusted public keys
    pub fn new(trusted_public_keys: Vec<String>) -> Result<Self> {
        let mut trusted_keys = Vec::new();

        for key_b64 in trusted_public_keys {
            let key_bytes = BASE64
                .decode(&key_b64)
                .map_err(|e| anyhow!("Invalid base64 public key: {}", e))?;

            let key_array: [u8; 32] = key_bytes
                .try_into()
                .map_err(|_| anyhow!("Public key must be 32 bytes"))?;

            let verifying_key = VerifyingKey::from_bytes(&key_array)
                .map_err(|e| anyhow!("Invalid Ed25519 public key: {}", e))?;

            trusted_keys.push(verifying_key);
        }

        Ok(Self { trusted_keys })
    }

    /// Verify a capsule signature
    pub fn verify(&self, content: &[u8], sig: &CapsuleSignature) -> Result<()> {
        // 1. Check algorithm
        if sig.algorithm != "ed25519" {
            return Err(anyhow!(
                "Unsupported signature algorithm: {}",
                sig.algorithm
            ));
        }

        // 2. Verify content hash
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hasher.finalize();
        let computed_hash = hex::encode(hash);

        if computed_hash != sig.content_hash {
            return Err(anyhow!("Content hash mismatch"));
        }

        // 3. Decode public key
        let key_bytes = BASE64
            .decode(&sig.public_key)
            .map_err(|e| anyhow!("Invalid base64 public key: {}", e))?;

        let key_array: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow!("Public key must be 32 bytes"))?;

        let verifying_key = VerifyingKey::from_bytes(&key_array)
            .map_err(|e| anyhow!("Invalid Ed25519 public key: {}", e))?;

        // 4. Check if key is trusted
        if !self.trusted_keys.contains(&verifying_key) {
            return Err(anyhow!("Public key not in trusted set"));
        }

        // 5. Decode and verify signature
        let sig_bytes = BASE64
            .decode(&sig.signature)
            .map_err(|e| anyhow!("Invalid base64 signature: {}", e))?;

        let signature = Signature::from_slice(&sig_bytes)
            .map_err(|e| anyhow!("Invalid signature format: {}", e))?;

        verifying_key
            .verify(content, &signature)
            .map_err(|e| anyhow!("Signature verification failed: {}", e))?;

        Ok(())
    }

    /// Verify with any trusted key (for discovery)
    pub fn verify_with_discovery(&self, content: &[u8], sig: &CapsuleSignature) -> Result<()> {
        // Same as verify() but could fetch trusted keys from remote registry
        self.verify(content, sig)
    }
}

/// Trusted key store (in production, would use KV or database)
pub struct TrustedKeyStore {
    keys: std::collections::HashMap<String, String>,
}

impl TrustedKeyStore {
    pub fn new() -> Self {
        Self {
            keys: std::collections::HashMap::new(),
        }
    }

    /// Add a trusted publisher
    pub fn add_publisher(&mut self, name: &str, public_key: &str) {
        self.keys.insert(name.to_string(), public_key.to_string());
    }

    /// Get all trusted keys
    pub fn get_all_keys(&self) -> Vec<String> {
        self.keys.values().cloned().collect()
    }

    /// Check if a key is trusted
    pub fn is_trusted(&self, public_key: &str) -> bool {
        self.keys.values().any(|k| k == public_key)
    }
}

impl Default for TrustedKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify() {
        let signer = CapsuleSigner::new("test-publisher");
        let content = b"capsule manifest content";

        let signature = signer.sign(content).unwrap();

        assert_eq!(signature.algorithm, "ed25519");
        assert_eq!(signature.signer, "test-publisher");
        assert!(!signature.signature.is_empty());
        assert!(!signature.content_hash.is_empty());

        // Verify
        let verifier = CapsuleVerifier::new(vec![signature.public_key.clone()]).unwrap();
        let result = verifier.verify(content, &signature);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_tampered_content() {
        let signer = CapsuleSigner::new("test-publisher");
        let content = b"original content";
        let signature = signer.sign(content).unwrap();

        let verifier = CapsuleVerifier::new(vec![signature.public_key.clone()]).unwrap();

        // Try to verify with different content
        let tampered = b"tampered content";
        let result = verifier.verify(tampered, &signature);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("hash mismatch"));
    }

    #[test]
    fn test_verify_untrusted_key() {
        let signer = CapsuleSigner::new("untrusted-publisher");
        let content = b"content";
        let signature = signer.sign(content).unwrap();

        // Create verifier with different trusted key
        let other_signer = CapsuleSigner::new("trusted-publisher");
        let verifier = CapsuleVerifier::new(vec![other_signer.public_key()]).unwrap();

        let result = verifier.verify(content, &signature);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not in trusted set"));
    }

    #[test]
    fn test_trusted_key_store() {
        let mut store = TrustedKeyStore::new();

        let signer1 = CapsuleSigner::new("publisher1");
        let signer2 = CapsuleSigner::new("publisher2");

        store.add_publisher("publisher1", &signer1.public_key());
        store.add_publisher("publisher2", &signer2.public_key());

        assert!(store.is_trusted(&signer1.public_key()));
        assert!(store.is_trusted(&signer2.public_key()));

        let unknown_key = "unknown_key_base64";
        assert!(!store.is_trusted(unknown_key));

        let all_keys = store.get_all_keys();
        assert_eq!(all_keys.len(), 2);
    }

    #[test]
    fn test_signature_serialization() {
        let signer = CapsuleSigner::new("test");
        let content = b"test content";
        let signature = signer.sign(content).unwrap();

        // Serialize to JSON
        let json = serde_json::to_string(&signature).unwrap();
        assert!(json.contains("ed25519"));

        // Deserialize
        let deserialized: CapsuleSignature = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.algorithm, signature.algorithm);
        assert_eq!(deserialized.signature, signature.signature);
    }

    #[test]
    fn test_multiple_trusted_keys() {
        let signer1 = CapsuleSigner::new("publisher1");
        let signer2 = CapsuleSigner::new("publisher2");

        let content = b"content";
        let sig1 = signer1.sign(content).unwrap();
        let sig2 = signer2.sign(content).unwrap();

        // Verifier trusts both keys
        let verifier =
            CapsuleVerifier::new(vec![signer1.public_key(), signer2.public_key()]).unwrap();

        // Both signatures should verify
        assert!(verifier.verify(content, &sig1).is_ok());
        assert!(verifier.verify(content, &sig2).is_ok());
    }
}
