use std::collections::BTreeMap;
use std::convert::TryInto;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::identity;

const SIGNATURE_VERSION: u8 = 0x01;
const KEY_TYPE_ED25519: u8 = 0x01;

/// Parse a developer_key string (ed25519:base64 or did:key) into raw bytes
pub fn parse_developer_key(value: &str) -> Result<[u8; 32]> {
    // Support did:key format
    if value.starts_with("did:key:") {
        return identity::did_to_public_key(value)
            .map_err(|e| anyhow!("failed to parse did:key: {}", e));
    }

    // Legacy ed25519:base64 format
    let value = value
        .strip_prefix("ed25519:")
        .ok_or_else(|| anyhow!("developer_key must start with ed25519: or did:key:"))?;
    let decoded = BASE64
        .decode(value)
        .map_err(|err| anyhow!("failed to decode developer_key: {err}"))?;
    if decoded.len() != 32 {
        bail!(
            "developer_key must decode to 32 bytes, got {}",
            decoded.len()
        );
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredKey {
    pub key_type: String,
    pub public_key: String,
    pub secret_key: String,
}

impl StoredKey {
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        StoredKey {
            key_type: "ed25519".to_string(),
            public_key: BASE64.encode(verifying_key.as_bytes()),
            secret_key: BASE64.encode(signing_key.to_bytes()),
        }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create key directory {}", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(self)?;
        fs::write(path, format!("{}\n", payload))
            .with_context(|| format!("failed to write key file {}", path.display()))?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read key file {}", path.display()))?;
        let stored: StoredKey = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse key file {}", path.display()))?;
        Ok(stored)
    }

    pub fn to_signing_key(&self) -> Result<SigningKey> {
        if self.key_type.as_str() != "ed25519" {
            bail!("unsupported key_type {}; expected ed25519", self.key_type);
        }
        let secret_bytes = BASE64
            .decode(&self.secret_key)
            .map_err(|err| anyhow!("failed to decode secret key: {err}"))?;
        if secret_bytes.len() != 32 {
            bail!("secret key must be 32 bytes, got {}", secret_bytes.len());
        }
        let secret_fixed: [u8; 32] = secret_bytes.as_slice().try_into().expect("length checked");
        let signing_key = SigningKey::from_bytes(&secret_fixed);
        let verifying_key = signing_key.verifying_key();

        let public_encoded = BASE64.encode(verifying_key.as_bytes());
        if public_encoded != self.public_key {
            bail!("public key mismatch between stored public and derived secret");
        }
        Ok(signing_key)
    }

    pub fn developer_key_fingerprint(&self) -> String {
        format!("ed25519:{}", self.public_key)
    }

    /// Get the DID (did:key format) for this key
    pub fn did(&self) -> Result<String> {
        let public_bytes = BASE64
            .decode(&self.public_key)
            .map_err(|err| anyhow!("failed to decode public key: {err}"))?;
        if public_bytes.len() != 32 {
            bail!("public key must be 32 bytes, got {}", public_bytes.len());
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&public_bytes);
        Ok(identity::public_key_to_did(&key_bytes))
    }
}

pub struct SignatureMetadata {
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub signer: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub extra: BTreeMap<String, Value>,
}

pub fn write_signature_file(
    path: &Path,
    verifying_key: &VerifyingKey,
    signature: &Signature,
    metadata: &SignatureMetadata,
) -> Result<()> {
    let mut meta_map = Map::new();
    meta_map.insert(
        "package_sha256".to_string(),
        Value::String(metadata.package_sha256.clone()),
    );
    meta_map.insert(
        "manifest_sha256".to_string(),
        Value::String(metadata.manifest_sha256.clone()),
    );
    meta_map.insert(
        "timestamp".to_string(),
        Value::String(metadata.timestamp.to_rfc3339()),
    );
    meta_map.insert("tool".to_string(), Value::String("ato-cli".to_string()));
    meta_map.insert(
        "tool_version".to_string(),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    if let Some(signer) = &metadata.signer {
        meta_map.insert("signer".to_string(), Value::String(signer.clone()));
    }
    for (key, value) in &metadata.extra {
        meta_map.insert(key.clone(), value.clone());
    }
    let metadata_json = Value::Object(meta_map);
    let metadata_bytes = serde_json::to_vec(&metadata_json)?;
    if metadata_bytes.len() > u16::MAX as usize {
        bail!(
            "signature metadata too large ({} bytes)",
            metadata_bytes.len()
        );
    }

    let mut buffer = Vec::with_capacity(1 + 1 + 32 + 64 + 2 + metadata_bytes.len());
    buffer.push(SIGNATURE_VERSION);
    buffer.push(KEY_TYPE_ED25519);
    buffer.extend_from_slice(verifying_key.as_bytes());
    buffer.extend_from_slice(&signature.to_bytes());
    buffer.extend_from_slice(&(metadata_bytes.len() as u16).to_be_bytes());
    buffer.extend_from_slice(&metadata_bytes);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create signature directory {}", parent.display())
        })?;
    }

    fs::write(path, buffer)
        .with_context(|| format!("failed to write signature {}", path.display()))?;

    Ok(())
}

pub struct SignatureFile {
    pub version: u8,
    pub key_type: u8,
    pub public_key: [u8; 32],
    pub signature: Signature,
    pub metadata: Value,
}

impl SignatureFile {
    pub fn package_sha256(&self) -> Option<String> {
        self.metadata
            .get("package_sha256")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

pub fn read_signature_file(path: &Path) -> Result<SignatureFile> {
    let data =
        fs::read(path).with_context(|| format!("failed to read signature {}", path.display()))?;
    if data.len() < 1 + 1 + 32 + 64 + 2 {
        bail!("signature file too short");
    }
    let version = data[0];
    let key_type = data[1];
    let mut offset = 2;
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&data[offset..offset + 32]);
    offset += 32;

    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(&data[offset..offset + 64]);
    offset += 64;
    let signature = Signature::from_bytes(&sig_bytes);

    let metadata_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    offset += 2;
    if data.len() < offset + metadata_len {
        bail!("signature metadata length out of bounds");
    }
    let metadata_bytes = &data[offset..offset + metadata_len];
    let metadata: Value = serde_json::from_slice(metadata_bytes)
        .context("failed to parse signature metadata JSON")?;

    Ok(SignatureFile {
        version,
        key_type,
        public_key,
        signature,
        metadata,
    })
}

pub fn ensure_signature_matches_manifest(sig: &SignatureFile, developer_key: &str) -> Result<()> {
    if sig.version != SIGNATURE_VERSION {
        bail!("unsupported signature version {}", sig.version);
    }
    if sig.key_type != KEY_TYPE_ED25519 {
        bail!("unsupported key_type {}", sig.key_type);
    }
    let manifest_key = parse_developer_key(developer_key)?;
    if sig.public_key != manifest_key {
        bail!("signature public key does not match manifest developer_key");
    }
    Ok(())
}

pub fn verify_signature_file(sig: &SignatureFile, message: &[u8]) -> Result<()> {
    if sig.version != SIGNATURE_VERSION {
        bail!("unsupported signature version {}", sig.version);
    }
    if sig.key_type != KEY_TYPE_ED25519 {
        bail!("unsupported key_type {}", sig.key_type);
    }
    let verifying = VerifyingKey::from_bytes(&sig.public_key)
        .map_err(|_| anyhow!("failed to parse signature public key"))?;
    verifying
        .verify(message, &sig.signature)
        .map_err(|_| anyhow!("signature verification failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use tempfile::tempdir;

    #[test]
    fn stored_key_roundtrip_signature() {
        let stored = StoredKey::generate();
        let signing_key = stored.to_signing_key().expect("signing key");
        let message = b"sign-test";
        let signature = signing_key.sign(message);
        let metadata = SignatureMetadata {
            package_sha256: "abc".to_string(),
            manifest_sha256: "def".to_string(),
            signer: None,
            timestamp: Utc::now(),
            extra: BTreeMap::new(),
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("developer.sig");
        write_signature_file(&path, &signing_key.verifying_key(), &signature, &metadata).unwrap();
        let sig = read_signature_file(&path).unwrap();
        ensure_signature_matches_manifest(&sig, &stored.developer_key_fingerprint()).unwrap();
        verify_signature_file(&sig, message).unwrap();
    }

    #[test]
    fn stored_key_did_conversion() {
        let stored = StoredKey::generate();
        let did = stored.did().expect("did");
        assert!(
            did.starts_with("did:key:z6Mk"),
            "DID should start with did:key:z6Mk"
        );

        // Verify we can parse it back
        let public_key = parse_developer_key(&did).expect("parse did");
        let original_key =
            parse_developer_key(&stored.developer_key_fingerprint()).expect("parse fingerprint");
        assert_eq!(
            public_key, original_key,
            "DID and fingerprint should produce same public key"
        );
    }

    #[test]
    fn parse_developer_key_did_format() {
        // Generate a test key
        let stored = StoredKey::generate();
        let did = stored.did().expect("did");

        // Parse it
        let public_key = parse_developer_key(&did).expect("parse did");

        // Verify it matches
        let signing_key = stored.to_signing_key().expect("signing key");
        assert_eq!(public_key, *signing_key.verifying_key().as_bytes());
    }

    #[test]
    fn signature_with_did_key() {
        let stored = StoredKey::generate();
        let did = stored.did().expect("did");
        let signing_key = stored.to_signing_key().expect("signing key");
        let message = b"test-did-signature";
        let signature = signing_key.sign(message);
        let metadata = SignatureMetadata {
            package_sha256: "abc".to_string(),
            manifest_sha256: "def".to_string(),
            signer: Some(did.clone()),
            timestamp: Utc::now(),
            extra: BTreeMap::new(),
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("developer.sig");
        write_signature_file(&path, &signing_key.verifying_key(), &signature, &metadata).unwrap();
        let sig = read_signature_file(&path).unwrap();

        // Verify using DID format
        ensure_signature_matches_manifest(&sig, &did).unwrap();
        verify_signature_file(&sig, message).unwrap();
    }
}
