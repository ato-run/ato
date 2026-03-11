#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey};
use rand::rngs::OsRng;

/// Capsule signer for audit and legacy compatibility.
#[derive(Debug, Clone)]
pub struct CapsuleSigner {
    signer_id: String,
    signing_key: SigningKey,
    fingerprint: String,
}

impl CapsuleSigner {
    pub fn new<S: Into<String>>(signer_id: S) -> Self {
        let signer_id = signer_id.into();
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let fingerprint = format!("ed25519:{}", BASE64.encode(verifying_key.as_bytes()));
        Self {
            signer_id,
            signing_key,
            fingerprint,
        }
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn signer_id(&self) -> &str {
        &self.signer_id
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }
}
