use std::collections::BTreeMap;
use std::convert::TryInto;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

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
    /// DID of the previous signing key (for key rotation; see §4.4 of the spec).
    pub previous_key: Option<String>,
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
    if let Some(prev_key) = &metadata.previous_key {
        meta_map.insert("previous_key".to_string(), Value::String(prev_key.clone()));
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

/// JSON signature embedded in `signature.json` inside a `.capsule` archive.
///
/// This is the spec-compliant format (Part I §3.2). Consumers can verify authenticity
/// without any out-of-band binary protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleArtifactSignature {
    /// Format version. Currently "1".
    pub version: String,
    /// Signature algorithm. Currently "ed25519".
    pub alg: String,
    /// Public key identifier in `did:key:` or `ed25519:<base64>` format.
    pub key_id: String,
    /// ISO-8601 UTC timestamp of signing.
    pub signed_at: String,
    /// `sha256:<hex>` of `capsule.toml` bytes as serialised in the archive.
    pub manifest_hash: String,
    /// `sha256:<hex>` of `payload.tar.zst` bytes.
    pub payload_hash: String,
    /// Base64-encoded Ed25519 signature over the JCS-canonicalised pre-image object.
    pub signature: String,
    /// Optional previous key id for key rotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_key: Option<String>,
}

/// Unsigned placeholder written when no signing key is provided at pack time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleArtifactSignaturePlaceholder {
    pub signed: bool,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sbom: Option<Value>,
}

impl CapsuleArtifactSignaturePlaceholder {
    pub fn new(sbom: Option<Value>) -> Self {
        CapsuleArtifactSignaturePlaceholder {
            signed: false,
            note: "To be signed".to_string(),
            sbom,
        }
    }
}

/// Union type that can be either a real signature or a placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SignatureJsonContent {
    Signed(CapsuleArtifactSignature),
    Unsigned(CapsuleArtifactSignaturePlaceholder),
}

/// Compute `sha256:<hex>` hash of arbitrary bytes.
pub fn sha256_prefixed(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Create a signed `CapsuleArtifactSignature` from manifest and payload bytes.
///
/// The message that is signed is the JCS-canonicalised JSON object containing
/// `version`, `alg`, `key_id`, `signed_at`, `manifest_hash`, and `payload_hash`.
/// The `signature` field is then appended to form the final object.
pub fn sign_capsule_artifact(
    manifest_bytes: &[u8],
    payload_bytes: &[u8],
    stored_key: &StoredKey,
    previous_key: Option<String>,
) -> Result<CapsuleArtifactSignature> {
    let signing_key = stored_key.to_signing_key()?;
    let key_id = stored_key.did()?;
    let manifest_hash = sha256_prefixed(manifest_bytes);
    let payload_hash = sha256_prefixed(payload_bytes);
    let signed_at = Utc::now().to_rfc3339();

    let pre_image = serde_json::json!({
        "version": "1",
        "alg": "ed25519",
        "key_id": key_id,
        "signed_at": signed_at,
        "manifest_hash": manifest_hash,
        "payload_hash": payload_hash,
    });
    let canonical =
        serde_jcs::to_vec(&pre_image).map_err(|e| anyhow!("JCS canonicalization failed: {e}"))?;

    let sig: Signature = signing_key.sign(&canonical);

    Ok(CapsuleArtifactSignature {
        version: "1".to_string(),
        alg: "ed25519".to_string(),
        key_id,
        signed_at,
        manifest_hash,
        payload_hash,
        signature: BASE64.encode(sig.to_bytes()),
        previous_key,
    })
}

/// Verify a `CapsuleArtifactSignature` against manifest and payload bytes.
pub fn verify_capsule_artifact_signature(
    sig: &CapsuleArtifactSignature,
    manifest_bytes: &[u8],
    payload_bytes: &[u8],
) -> Result<()> {
    if sig.version != "1" {
        bail!("unsupported signature version {}", sig.version);
    }
    if sig.alg != "ed25519" {
        bail!("unsupported signature algorithm {}", sig.alg);
    }

    let expected_manifest_hash = sha256_prefixed(manifest_bytes);
    if sig.manifest_hash != expected_manifest_hash {
        bail!(
            "manifest_hash mismatch: expected {}, got {}",
            expected_manifest_hash,
            sig.manifest_hash
        );
    }
    let expected_payload_hash = sha256_prefixed(payload_bytes);
    if sig.payload_hash != expected_payload_hash {
        bail!(
            "payload_hash mismatch: expected {}, got {}",
            expected_payload_hash,
            sig.payload_hash
        );
    }

    let key_bytes = parse_developer_key(&sig.key_id)?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| anyhow!("failed to parse key_id as Ed25519 public key"))?;

    let pre_image = serde_json::json!({
        "version": sig.version,
        "alg": sig.alg,
        "key_id": sig.key_id,
        "signed_at": sig.signed_at,
        "manifest_hash": sig.manifest_hash,
        "payload_hash": sig.payload_hash,
    });
    let canonical =
        serde_jcs::to_vec(&pre_image).map_err(|e| anyhow!("JCS canonicalization failed: {e}"))?;

    let sig_bytes = BASE64
        .decode(&sig.signature)
        .map_err(|e| anyhow!("failed to decode signature base64: {e}"))?;
    if sig_bytes.len() != 64 {
        bail!("signature must be 64 bytes, got {}", sig_bytes.len());
    }
    let sig_fixed: [u8; 64] = sig_bytes.as_slice().try_into().expect("length checked");
    let ed_sig = Signature::from_bytes(&sig_fixed);
    verifying_key
        .verify(&canonical, &ed_sig)
        .map_err(|_| anyhow!("signature verification failed"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_signature_matches_manifest, parse_developer_key, read_signature_file,
        sign_capsule_artifact, verify_capsule_artifact_signature, verify_signature_file,
        write_signature_file, CapsuleArtifactSignature, CapsuleArtifactSignaturePlaceholder,
        SignatureMetadata, StoredKey,
    };

    use chrono::Utc;
    use ed25519_dalek::Signer;
    use std::collections::BTreeMap;
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
            previous_key: None,
            extra: BTreeMap::new(),
        };
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sig");
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
            previous_key: None,
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

    #[test]
    fn capsule_artifact_signature_roundtrip() {
        let stored = StoredKey::generate();
        let manifest_bytes = b"[package]\nname = \"test\"\nversion = \"0.1.0\"";
        let payload_bytes = b"fake-payload-zst-content";

        let sig = sign_capsule_artifact(manifest_bytes, payload_bytes, &stored, None)
            .expect("sign_capsule_artifact");
        assert_eq!(sig.version, "1");
        assert_eq!(sig.alg, "ed25519");
        assert!(sig.key_id.starts_with("did:key:"));
        assert!(sig.manifest_hash.starts_with("sha256:"));
        assert!(sig.payload_hash.starts_with("sha256:"));
        assert!(sig.previous_key.is_none());

        verify_capsule_artifact_signature(&sig, manifest_bytes, payload_bytes)
            .expect("verify_capsule_artifact_signature");
    }

    #[test]
    fn capsule_artifact_signature_tamper_manifest() {
        let stored = StoredKey::generate();
        let manifest_bytes = b"[package]\nname = \"test\"";
        let payload_bytes = b"payload";

        let sig = sign_capsule_artifact(manifest_bytes, payload_bytes, &stored, None).unwrap();
        let tampered = b"[package]\nname = \"evil\"";
        let err = verify_capsule_artifact_signature(&sig, tampered, payload_bytes).unwrap_err();
        assert!(
            err.to_string().contains("manifest_hash mismatch"),
            "expected manifest_hash mismatch, got: {err}"
        );
    }

    #[test]
    fn capsule_artifact_signature_json_roundtrip() {
        let stored = StoredKey::generate();
        let sig = sign_capsule_artifact(b"manifest", b"payload", &stored, None).unwrap();
        let json = serde_json::to_string(&sig).unwrap();
        let parsed: CapsuleArtifactSignature = serde_json::from_str(&json).unwrap();
        assert_eq!(sig.key_id, parsed.key_id);
        assert_eq!(sig.signature, parsed.signature);
    }

    #[test]
    fn capsule_artifact_placeholder_json() {
        let placeholder = CapsuleArtifactSignaturePlaceholder::new(None);
        let json = serde_jcs::to_string(&placeholder).unwrap();
        assert!(json.contains("\"signed\":false"));
        assert!(json.contains("\"note\":\"To be signed\""));
    }
}
