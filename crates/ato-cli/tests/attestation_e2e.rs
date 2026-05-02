//! End-to-end coverage for the A2 freeze → attest → verify loop.
//!
//! Stays at the library boundary (no CLI subprocess) so the test runs in
//! the same hermetic ATO_HOME the rest of the cache tests use.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use ato_cli::dependency_materializer::freeze::freeze_dep_tree;
use ato_cli::dependency_materializer::DepDerivationKeyV1;
use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheStrategy, DependencyMaterializationRequest, InstallPolicies,
    ManifestInputs, PlatformTriple, RuntimeSelection,
};
use capsule_core::attestation::{
    blob_attestations_dir, generate_keypair, read_envelope, verify_envelope,
    write_trust_root_pubkey, TrustRoot,
};
use capsule_core::common::paths::ato_trust_roots_dir;
use serial_test::serial;
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
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn sample_request() -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        session_id: "attestation-e2e".to_string(),
        capsule_id: "capsule".to_string(),
        source_root: "/repo".into(),
        workspace_root: "/repo".into(),
        ecosystem: "npm".to_string(),
        package_manager: Some("pnpm".to_string()),
        package_manager_version: Some("9.0.0".to_string()),
        runtime: RuntimeSelection {
            name: "node".to_string(),
            version: Some("20.10.0".to_string()),
        },
        manifests: ManifestInputs {
            lockfile_digest: Some("sha256:lock".to_string()),
            package_manifest_digest: Some("sha256:manifest".to_string()),
            workspace_manifest_digest: None,
            path_dependency_digest: None,
        },
        policies: InstallPolicies {
            lifecycle_script_policy: "sandbox".to_string(),
            registry_policy: "default".to_string(),
            network_policy: "default".to_string(),
            env_allowlist_digest: None,
        },
        platform: PlatformTriple {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: Some("glibc".to_string()),
            abi: None,
        },
        cache_strategy: CacheStrategy::DerivationCache,
        attestation_strategy: AttestationStrategy::LocalSign,
    }
}

#[test]
#[serial]
fn freeze_emits_attestation_when_attestation_key_env_is_set() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());

    // Generate a key, persist it as the attestation key.
    let key = generate_keypair();
    let key_path = tmp.path().join("attestation-key.json");
    fs::write(
        &key_path,
        serde_json::to_vec_pretty(&key.to_stored()).unwrap(),
    )
    .unwrap();
    let _key_env = EnvGuard::set("ATO_ATTESTATION_KEY", &key_path);
    let _builder = EnvGuard::set("ATO_ATTESTATION_BUILDER_ID", "ato-cli@e2e-test");

    // Register the public half as a trust root so verification can proceed.
    write_trust_root_pubkey(&key.public_key_bytes(), Some("e2e")).unwrap();

    // Freeze a fake install tree.
    let deps = tmp.path().join("install-output");
    write_file(&deps, "node_modules/foo/index.js", b"console.log('ok');\n");
    write_file(
        &deps,
        "node_modules/foo/package.json",
        b"{\"name\":\"foo\"}",
    );

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();

    // Use provider_cache::freeze_after_install so the attestation hook
    // fires (freeze_dep_tree alone does not emit attestations).
    let outcome =
        ato_cli::provider_cache::freeze_after_install(&deps, &derivation_hash, "npm").unwrap();
    assert!(outcome.did_freeze);

    // Attestation should land under the canonical blob attestation dir.
    let attestation_dir = blob_attestations_dir(&outcome.blob_hash);
    assert!(
        attestation_dir.is_dir(),
        "expected attestation dir at {}",
        attestation_dir.display()
    );

    // Find the issued envelope, verify it against the trust root.
    let mut envelopes = Vec::new();
    for entry in fs::read_dir(&attestation_dir).unwrap() {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .map(|e| e == "json")
            .unwrap_or(false)
        {
            envelopes.push(entry.path());
        }
    }
    assert_eq!(envelopes.len(), 1);
    let envelope = read_envelope(&envelopes[0]).unwrap();
    assert_eq!(envelope.statement.subject.hash, outcome.blob_hash);
    assert_eq!(envelope.statement.subject.kind, "blob");
    assert_eq!(
        envelope.statement.predicate.derivation_hash.as_deref(),
        Some(derivation_hash.as_str())
    );

    // Load the trust root we wrote and verify.
    let trust_root_path = ato_trust_roots_dir().join(format!(
        "{}.json",
        envelope.signature.key_id.replace(':', "-")
    ));
    let trust_root: TrustRoot =
        serde_json::from_slice(&fs::read(trust_root_path).unwrap()).unwrap();
    verify_envelope(&envelope, &trust_root).unwrap();
}

#[test]
#[serial]
fn freeze_skips_attestation_when_key_env_is_unset() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    std::env::remove_var("ATO_ATTESTATION_KEY");

    let deps = tmp.path().join("install");
    write_file(&deps, "lib.js", b"// shared\n");

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();
    let outcome =
        ato_cli::provider_cache::freeze_after_install(&deps, &derivation_hash, "npm").unwrap();
    let attestation_dir = blob_attestations_dir(&outcome.blob_hash);
    assert!(
        !attestation_dir.exists(),
        "no attestation dir should appear without ATO_ATTESTATION_KEY"
    );
    let _ = freeze_dep_tree;
}
