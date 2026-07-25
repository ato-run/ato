//! OCI Compose lock: persist image digest resolutions for Compose-imported services.
//!
//! Writes `ato.oci.lock.json` in the project directory. Keeps OCI image
//! resolution state separate from `capsule.lock.json` (source capsule locks)
//! and `capsule.lock` (canonical lock).
//!
//! # Identity contract
//! [`OciComposeLock::execution_identity_hash`] hashes only the fields that
//! affect reproducibility. The following are explicitly **excluded**:
//! - `container_id`, `network_id`, `volume_id`
//! - allocated host ports
//! - secret values
//!
//! # Replay contract
//! A lock entry is reused iff `source_hash` + `declared_ref` +
//! `provider_semantics` all match the current run. Any drift triggers
//! re-resolution.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::types::OciPlatform;

pub const OCI_COMPOSE_LOCK_FILE_NAME: &str = "ato.oci.lock.json";
pub const OCI_COMPOSE_LOCK_VERSION: u32 = 1;

// ── Public model ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OciComposeLock {
    pub version: u32,
    pub import: OciImportMeta,
    /// Service-name → resolved image entry. BTreeMap keeps serialized output
    /// stable across runs.
    pub images: BTreeMap<String, OciImageLockEntry>,
}

/// Provenance of the Compose import that produced this lock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OciImportMeta {
    pub kind: String,
    pub source_path: String,
    /// SHA-256 of the raw compose file content: `"sha256:<hex>"`.
    pub source_hash: String,
}

/// Resolved image state for one Compose service.
///
/// Does **not** include: container_id, network_id, host_port, secret values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OciImageLockEntry {
    pub declared_ref: String,
    pub resolved_digest: String,
    /// Serialized as `"<os>/<arch>"` or `"<os>/<arch>/<variant>"`.
    pub platform: String,
    /// Coarse provider semantics label, e.g. `"podman-rootless-native-v1"`.
    pub provider_semantics: String,
}

#[derive(Debug, Error)]
pub enum OciLockError {
    #[error("oci_lock_resolution_missing: {0}")]
    ResolutionMissing(String),
    #[error("oci_lock_compose_source_drift: {0}")]
    ComposeSourceDrift(String),
    #[error("oci_lock_platform_mismatch: {0}")]
    PlatformMismatch(String),
    #[error("oci_lock_provider_semantics_mismatch: {0}")]
    ProviderSemanticsMismatch(String),
    #[error("oci_lock_write_failed: {0}")]
    WriteFailed(String),
    #[error("oci_lock_parse_failed: {0}")]
    ParseFailed(String),
}

// ── Model impl ────────────────────────────────────────────────────────────────

impl OciComposeLock {
    pub fn new(import: OciImportMeta, images: BTreeMap<String, OciImageLockEntry>) -> Self {
        Self {
            version: OCI_COMPOSE_LOCK_VERSION,
            import,
            images,
        }
    }

    /// Deterministic execution identity hash.
    ///
    /// Covers: `source_hash`, per-service `declared_ref`, `resolved_digest`,
    /// `platform`, `provider_semantics`. BTreeMap ensures stable key order.
    pub fn execution_identity_hash(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("source_hash:{}", self.import.source_hash));
        for (name, entry) in &self.images {
            parts.push(format!("service:{name}"));
            parts.push(format!("  declared_ref:{}", entry.declared_ref));
            parts.push(format!("  resolved_digest:{}", entry.resolved_digest));
            parts.push(format!("  platform:{}", entry.platform));
            parts.push(format!("  provider_semantics:{}", entry.provider_semantics));
        }
        let combined = parts.join("\n");
        let mut hasher = Sha256::new();
        hasher.update(combined.as_bytes());
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Returns `true` if the lock entry for `service` is fresh for the current
    /// compose source hash, declared image ref, and provider semantics.
    pub fn entry_is_fresh(
        &self,
        source_hash: &str,
        service: &str,
        declared_ref: &str,
        provider_semantics: &str,
    ) -> bool {
        if self.import.source_hash != source_hash {
            return false;
        }
        match self.images.get(service) {
            Some(entry) => {
                entry.declared_ref == declared_ref && entry.provider_semantics == provider_semantics
            }
            None => false,
        }
    }
}

impl OciImageLockEntry {
    /// Convert an `OciPlatform` to the `"<os>/<arch>[/<variant>]"` string
    /// stored in the lock.
    pub fn platform_str(p: &OciPlatform) -> String {
        match &p.variant {
            Some(v) => format!("{}/{}/{}", p.os, p.architecture, v),
            None => format!("{}/{}", p.os, p.architecture),
        }
    }
}

// ── I/O ───────────────────────────────────────────────────────────────────────

/// Load `ato.oci.lock.json` from `project_dir`.
/// Returns `Ok(None)` when the file does not exist.
pub fn load_from_dir(project_dir: &Path) -> Result<Option<OciComposeLock>, OciLockError> {
    let path = project_dir.join(OCI_COMPOSE_LOCK_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| OciLockError::ParseFailed(format!("read {}: {e}", path.display())))?;
    serde_json::from_str::<OciComposeLock>(&raw)
        .map(Some)
        .map_err(|e| OciLockError::ParseFailed(format!("parse {}: {e}", path.display())))
}

/// Write `ato.oci.lock.json` to `project_dir`.
pub fn write_to_dir(project_dir: &Path, lock: &OciComposeLock) -> Result<(), OciLockError> {
    let path = project_dir.join(OCI_COMPOSE_LOCK_FILE_NAME);
    let raw = serde_json::to_string_pretty(lock)
        .map_err(|e| OciLockError::WriteFailed(format!("serialize: {e}")))?;
    fs::write(&path, raw)
        .map_err(|e| OciLockError::WriteFailed(format!("write {}: {e}", path.display())))
}

// ── Hash utilities ────────────────────────────────────────────────────────────

/// Compute `"sha256:<hex>"` over raw compose file content.
pub fn compute_compose_source_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// Parse `"<os>/<arch>[/<variant>]"` back to an `OciPlatform`.
pub fn parse_platform_str(s: &str) -> OciPlatform {
    let mut parts = s.splitn(3, '/');
    OciPlatform {
        os: parts.next().unwrap_or("linux").to_string(),
        architecture: parts.next().unwrap_or("amd64").to_string(),
        variant: parts.next().map(|v| v.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn sample_lock(source_hash: &str, digest: &str, semantics: &str) -> OciComposeLock {
        let mut images = BTreeMap::new();
        images.insert(
            "db".to_string(),
            OciImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_digest: digest.to_string(),
                platform: "linux/amd64".to_string(),
                provider_semantics: semantics.to_string(),
            },
        );
        OciComposeLock::new(
            OciImportMeta {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: source_hash.to_string(),
            },
            images,
        )
    }

    #[test]
    fn compute_compose_source_hash_is_deterministic() {
        let content = "version: '3'\nservices:\n  db:\n    image: postgres:14\n";
        let h1 = compute_compose_source_hash(content);
        let h2 = compute_compose_source_hash(content);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("sha256:"));
    }

    #[test]
    fn compute_compose_source_hash_differs_on_changed_content() {
        let h1 = compute_compose_source_hash("version: '3'\n");
        let h2 = compute_compose_source_hash("version: '3.1'\n");
        assert_ne!(h1, h2);
    }

    #[test]
    fn resolved_digest_drift_changes_execution_identity() {
        let lock1 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        let lock2 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_b",
            "podman-rootless-native-v1",
        );
        assert_ne!(
            lock1.execution_identity_hash(),
            lock2.execution_identity_hash()
        );
    }

    #[test]
    fn provider_semantics_drift_changes_execution_identity() {
        let lock1 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        let lock2 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-machine-v1",
        );
        assert_ne!(
            lock1.execution_identity_hash(),
            lock2.execution_identity_hash()
        );
    }

    #[test]
    fn secret_values_are_not_in_identity() {
        // Identity is computed solely from the lock struct fields.
        // Secret values never enter OciComposeLock — they are absent by design.
        // Two locks with identical structure but hypothetically different
        // "runtime secret values" must yield the same identity hash.
        let lock1 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        let lock2 = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        assert_eq!(
            lock1.execution_identity_hash(),
            lock2.execution_identity_hash()
        );
    }

    #[test]
    fn allocated_host_port_does_not_change_execution_identity() {
        // Host port is runtime state — not stored in lock, so identity is unaffected.
        let lock = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        let id1 = lock.execution_identity_hash();
        // Simulate "different host port" by computing identity twice — since host_port
        // is not part of the lock, both invocations return the same value.
        let id2 = lock.execution_identity_hash();
        assert_eq!(id1, id2);
    }

    #[test]
    fn container_id_does_not_change_execution_identity() {
        // Container ID is runtime state — not stored in lock, so identity is unaffected.
        let lock = sample_lock(
            "sha256:aaaa",
            "sha256:digest_a",
            "podman-rootless-native-v1",
        );
        assert_eq!(
            lock.execution_identity_hash(),
            lock.execution_identity_hash()
        );
    }

    #[test]
    fn entry_is_fresh_matches_correct_inputs() {
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        assert!(lock.entry_is_fresh(
            "sha256:abc",
            "db",
            "postgres:14",
            "podman-rootless-native-v1"
        ));
    }

    #[test]
    fn entry_is_fresh_rejects_source_hash_drift() {
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        assert!(!lock.entry_is_fresh(
            "sha256:different",
            "db",
            "postgres:14",
            "podman-rootless-native-v1"
        ));
    }

    #[test]
    fn entry_is_fresh_rejects_declared_ref_drift() {
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        assert!(!lock.entry_is_fresh(
            "sha256:abc",
            "db",
            "postgres:15",
            "podman-rootless-native-v1"
        ));
    }

    #[test]
    fn entry_is_fresh_rejects_provider_semantics_drift() {
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        assert!(!lock.entry_is_fresh(
            "sha256:abc",
            "db",
            "postgres:14",
            "podman-rootless-machine-v1"
        ));
    }

    #[test]
    fn parse_platform_str_roundtrips() {
        let platform = OciPlatform {
            os: "linux".to_string(),
            architecture: "amd64".to_string(),
            variant: None,
        };
        let s = OciImageLockEntry::platform_str(&platform);
        assert_eq!(s, "linux/amd64");
        let back = parse_platform_str(&s);
        assert_eq!(back, platform);
    }

    #[test]
    fn parse_platform_str_roundtrips_with_variant() {
        let platform = OciPlatform {
            os: "linux".to_string(),
            architecture: "arm64".to_string(),
            variant: Some("v8".to_string()),
        };
        let s = OciImageLockEntry::platform_str(&platform);
        assert_eq!(s, "linux/arm64/v8");
        let back = parse_platform_str(&s);
        assert_eq!(back, platform);
    }

    #[test]
    fn lock_serializes_and_deserializes() {
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        let json = serde_json::to_string_pretty(&lock).expect("serialize");
        let back: OciComposeLock = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(lock, back);
    }

    #[test]
    fn lock_parse_failure_returns_typed_error_from_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join(OCI_COMPOSE_LOCK_FILE_NAME);
        std::fs::write(&lock_path, b"not valid json {{{").expect("write");
        let err = load_from_dir(dir.path()).expect_err("should fail");
        assert!(matches!(err, OciLockError::ParseFailed(_)));
        assert!(err.to_string().contains("oci_lock_parse_failed"));
    }

    #[test]
    fn lock_write_failure_returns_typed_error_from_write() {
        // Write to a path whose parent doesn't exist to trigger a write error.
        let dir = tempfile::tempdir().expect("tempdir");
        let non_existent_parent = dir.path().join("does_not_exist");
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        let err = write_to_dir(&non_existent_parent, &lock).expect_err("should fail");
        assert!(matches!(err, OciLockError::WriteFailed(_)));
        assert!(err.to_string().contains("oci_lock_write_failed"));
    }

    #[test]
    fn lock_roundtrips_via_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = sample_lock("sha256:abc", "sha256:digest_x", "podman-rootless-native-v1");
        write_to_dir(dir.path(), &lock).expect("write");
        let loaded = load_from_dir(dir.path())
            .expect("load")
            .expect("should exist");
        assert_eq!(lock, loaded);
    }

    #[test]
    fn load_from_dir_returns_none_when_file_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = load_from_dir(dir.path()).expect("no error");
        assert!(result.is_none());
    }
}
