//! Read / write A2 attestations and trust roots under `~/.ato/`.
//!
//! Attestations land at
//! `~/.ato/store/attestations/<kind>/<sanitized-hash>/<key_id>.json`.
//! Trust roots live at `~/.ato/trust/roots/<key_id>.json`.
//!
//! `<kind>` is `"blobs"` or `"payloads"`. The hash component is sanitized
//! with the same rules as `ato_store_dep_ref_path` so colons survive on
//! Windows.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::common::paths::{ato_store_attestations_dir, ato_trust_roots_dir};

use super::types::AttestationEnvelope;
use super::verify::TrustRoot;

/// Returns `~/.ato/store/attestations/blobs/<sanitized-hash>/`.
pub fn blob_attestations_dir(blob_hash: &str) -> PathBuf {
    ato_store_attestations_dir()
        .join("blobs")
        .join(sanitize(blob_hash))
}

/// Returns `~/.ato/store/attestations/payloads/<sanitized-hash>/`.
pub fn payload_attestations_dir(payload_hash: &str) -> PathBuf {
    ato_store_attestations_dir()
        .join("payloads")
        .join(sanitize(payload_hash))
}

/// Persists `envelope` under the directory keyed by its subject. Returns
/// the on-disk path so callers can record an attestation_ref.
pub fn store_envelope(envelope: &AttestationEnvelope) -> Result<PathBuf> {
    let dir = match envelope.statement.subject.kind.as_str() {
        "blob" => blob_attestations_dir(&envelope.statement.subject.hash),
        "payload" => payload_attestations_dir(&envelope.statement.subject.hash),
        other => anyhow::bail!("unknown attestation subject kind: {other}"),
    };
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let file_name = format!("{}.json", sanitize(&envelope.signature.key_id));
    let path = dir.join(file_name);
    let bytes = serde_json::to_vec_pretty(envelope).context("failed to serialize envelope")?;
    fs::write(&path, [bytes, b"\n".to_vec()].concat())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Reads and parses an envelope from `path`.
pub fn read_envelope(path: &Path) -> Result<AttestationEnvelope> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read envelope at {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse envelope at {}", path.display()))
}

/// Returns `~/.ato/trust/roots/<key_id>.json`.
pub fn trust_root_path(key_id: &str) -> PathBuf {
    ato_trust_roots_dir().join(format!("{}.json", sanitize(key_id)))
}

/// Writes a trust root file with just a public key + label.
pub fn write_trust_root_pubkey(
    public_key_bytes: &[u8; 32],
    label: Option<&str>,
) -> Result<PathBuf> {
    let trust_root = TrustRoot::new(public_key_bytes, label.map(str::to_string));
    fs::create_dir_all(ato_trust_roots_dir())
        .with_context(|| format!("failed to create {}", ato_trust_roots_dir().display()))?;
    let path = trust_root_path(&trust_root.key_id);
    let bytes = serde_json::to_vec_pretty(&trust_root).context("failed to serialize trust root")?;
    fs::write(&path, [bytes, b"\n".to_vec()].concat())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
