//! Application-level glue for the A2 trust layer.
//!
//! Wraps `capsule_core::attestation` with the bits the CLI side needs:
//! key loading from the `ATO_ATTESTATION_KEY` env var, builder identity
//! defaulting, and a one-call `issue_freeze_attestation` that the
//! freeze-on-miss path can invoke without rebuilding the predicate.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use capsule_core::attestation::{
    sign_envelope, store_envelope, AttestationEnvelope, AttestationKey, AttestationPredicate,
    AttestationStatement, AttestationSubject, FreezeMetadata, PolicySnapshot, StoredAttestationKey,
};
use capsule_core::blob::TreeHash;

const ENV_ATTESTATION_KEY: &str = "ATO_ATTESTATION_KEY";
const ENV_BUILDER_ID: &str = "ATO_ATTESTATION_BUILDER_ID";

/// Lazy holder for an attestation key + builder identity loaded from env.
///
/// `None` means the operator has not opted in; callers should treat that
/// as "do not emit attestations" rather than as an error.
pub struct AttestationContext {
    pub key: AttestationKey,
    pub builder_id: String,
}

impl AttestationContext {
    /// Loads `ATO_ATTESTATION_KEY` (path to a StoredAttestationKey JSON)
    /// and `ATO_ATTESTATION_BUILDER_ID` (free-form string).
    ///
    /// Returns `Ok(None)` when no key is configured. Returns an error if
    /// the env var is set but the key file cannot be loaded.
    pub fn from_env() -> Result<Option<Self>> {
        let Some(path) = std::env::var_os(ENV_ATTESTATION_KEY) else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if path.as_os_str().is_empty() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read attestation key at {}", path.display()))?;
        let stored: StoredAttestationKey = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "failed to parse attestation key at {} (expected StoredAttestationKey JSON)",
                path.display()
            )
        })?;
        let key = AttestationKey::from_stored(&stored).with_context(|| {
            format!(
                "failed to materialize attestation key from {}",
                path.display()
            )
        })?;
        let builder_id = std::env::var(ENV_BUILDER_ID).unwrap_or_else(|_| default_builder_id());
        Ok(Some(Self { key, builder_id }))
    }
}

/// Builds, signs, and stores a blob attestation.
///
/// The envelope ends up at
/// `~/.ato/store/attestations/blobs/<sanitized-blob-hash>/<sanitized-key-id>.json`.
pub fn issue_freeze_attestation(
    context: &AttestationContext,
    blob_hash: &str,
    derivation_hash: &str,
    tree: &TreeHash,
    policy: &PolicySnapshot,
) -> Result<(AttestationEnvelope, PathBuf)> {
    let predicate = AttestationPredicate {
        builder_id: context.builder_id.clone(),
        source: None,
        source_tree_hash: None,
        derivation_hash: Some(derivation_hash.to_string()),
        policy: policy.clone(),
        freeze: Some(FreezeMetadata {
            file_count: tree.file_count,
            symlink_count: tree.symlink_count,
            dir_count: tree.dir_count,
            total_bytes: tree.total_bytes,
        }),
    };
    let statement = AttestationStatement::new(
        AttestationSubject::for_blob(blob_hash),
        predicate,
        chrono::Utc::now().to_rfc3339(),
    );
    let envelope =
        sign_envelope(statement, &context.key).context("failed to sign attestation envelope")?;
    let path = store_envelope(&envelope).context("failed to persist attestation envelope")?;
    Ok((envelope, path))
}

fn default_builder_id() -> String {
    format!("ato-cli@{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::attestation::{generate_keypair, verify_envelope, TrustRoot};
    use std::ffi::OsStr;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set<V: AsRef<OsStr>>(key: &'static str, value: V) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn sample_tree() -> TreeHash {
        TreeHash {
            blob_hash: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_string(),
            file_count: 5,
            symlink_count: 1,
            dir_count: 2,
            total_bytes: 1024,
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_returns_none_when_key_unset() {
        let _guard = EnvGuard::unset(ENV_ATTESTATION_KEY);
        let context = AttestationContext::from_env().unwrap();
        assert!(context.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn from_env_loads_key_when_set() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.json");
        let generated = generate_keypair();
        let stored = generated.to_stored();
        fs::write(&key_path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();

        let _guard = EnvGuard::set(ENV_ATTESTATION_KEY, &key_path);
        let _builder = EnvGuard::set(ENV_BUILDER_ID, "test-builder");

        let context = AttestationContext::from_env().unwrap().expect("present");
        assert_eq!(context.builder_id, "test-builder");
        assert_eq!(context.key.key_id(), generated.key_id());
    }

    #[test]
    #[serial_test::serial]
    fn issue_then_verify_round_trip() {
        let tmp = TempDir::new().unwrap();
        let _home = EnvGuard::set("ATO_HOME", tmp.path());

        let key = generate_keypair();
        let context = AttestationContext {
            key,
            builder_id: "ato-cli@test".to_string(),
        };
        let policy = PolicySnapshot::default();
        let (envelope, path) = issue_freeze_attestation(
            &context,
            &sample_tree().blob_hash,
            "sha256:abc",
            &sample_tree(),
            &policy,
        )
        .unwrap();

        assert!(path.is_file());
        assert_eq!(envelope.signature.key_id, context.key.key_id());

        let trust_root = TrustRoot::new(&context.key.public_key_bytes(), Some("self".into()));
        verify_envelope(&envelope, &trust_root).unwrap();
    }
}
