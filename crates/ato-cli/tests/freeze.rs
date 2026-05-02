//! Tests for the A1 freeze pipeline: tree hash → atomic store write → ref +
//! meta records → idempotent re-freeze and concurrent flock contention.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use ato_cli::dependency_materializer::freeze::{freeze_dep_tree, DerivationLock};
use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheLookupResult, CacheStrategy, DepDerivationKeyV1,
    DependencyMaterializationRequest, DependencyMaterializer, InstallPolicies, ManifestInputs,
    PlatformTriple, RuntimeSelection, SessionDependencyMaterializer, StoreRefRecord,
};
use capsule_core::blob::BlobManifest;
use capsule_core::common::store::{ato_store_dep_ref_path, BlobAddress};
use serial_test::serial;
use tempfile::TempDir;

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn sample_request() -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        session_id: "freeze-test".to_string(),
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
        attestation_strategy: AttestationStrategy::None,
    }
}

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

#[test]
#[serial]
fn freeze_writes_payload_manifest_ref_and_meta() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let deps = tmp.path().join("session-deps");
    write_file(&deps, "node_modules/foo/index.js", b"console.log('foo');\n");
    write_file(&deps, "node_modules/foo/package.json", b"{\"name\":\"foo\"}");

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();
    let outcome = freeze_dep_tree(&deps, &derivation_hash, "npm").unwrap();

    assert!(outcome.did_freeze, "first freeze must move bytes");
    assert!(outcome.blob_hash.starts_with("sha256:"));
    let address = BlobAddress::parse(&outcome.blob_hash).unwrap();

    // Payload + manifest + ref + meta all live where we expect them.
    assert!(address.payload_dir().is_dir(), "payload dir missing");
    assert!(
        address
            .payload_dir()
            .join("node_modules/foo/index.js")
            .is_file(),
        "expected payload contents to be copied"
    );
    let manifest = BlobManifest::read_from(&address.manifest_path()).unwrap();
    assert!(manifest.matches_blob_hash(&outcome.blob_hash));
    assert_eq!(manifest.derivation_hash, derivation_hash);
    assert!(manifest.file_count >= 2);

    let ref_path = ato_store_dep_ref_path("npm", &derivation_hash);
    let record: StoreRefRecord =
        serde_json::from_slice(&fs::read(ref_path).unwrap()).unwrap();
    assert_eq!(record.blob_hash.as_deref(), Some(outcome.blob_hash.as_str()));
    assert_eq!(record.cache_status, "frozen");

    let meta_path = address.meta_path();
    assert!(meta_path.is_file(), "meta record missing");
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(meta_path).unwrap()).unwrap();
    assert_eq!(meta["last_event"], "freeze");
}

#[test]
#[serial]
fn second_freeze_observes_existing_blob_without_rewriting() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let deps = tmp.path().join("session-deps");
    write_file(&deps, "node_modules/foo/index.js", b"const a = 1;\n");

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();

    let first = freeze_dep_tree(&deps, &derivation_hash, "npm").unwrap();
    assert!(first.did_freeze);
    let second = freeze_dep_tree(&deps, &derivation_hash, "npm").unwrap();
    assert!(
        !second.did_freeze,
        "second freeze must observe the existing blob"
    );
    assert_eq!(first.blob_hash, second.blob_hash);

    let address = BlobAddress::parse(&first.blob_hash).unwrap();
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(address.meta_path()).unwrap()).unwrap();
    assert_eq!(meta["last_event"], "observe");
}

#[test]
#[serial]
fn freeze_makes_plan_report_a_hit() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let deps = tmp.path().join("session-deps");
    write_file(&deps, "lib.js", b"// shared\n");
    write_file(&deps, "package.json", b"{\"name\":\"shared\"}");

    let req = sample_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req).derivation_hash().unwrap();
    let outcome = freeze_dep_tree(&deps, &derivation_hash, &req.ecosystem).unwrap();

    let plan = SessionDependencyMaterializer::new().plan(&req).unwrap();
    match plan.cache_lookup {
        CacheLookupResult::Hit { blob_hash } => assert_eq!(blob_hash, outcome.blob_hash),
        other => panic!("expected hit after freeze, got {other:?}"),
    }
}

#[test]
#[serial]
fn freeze_atomic_write_leaves_no_tmp_files_in_target_root() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let deps = tmp.path().join("session-deps");
    write_file(&deps, "data.txt", b"payload");

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();
    let outcome = freeze_dep_tree(&deps, &derivation_hash, "npm").unwrap();

    // Walk the blob shard parent and assert no `*.tmp-*` siblings remain.
    let address = BlobAddress::parse(&outcome.blob_hash).unwrap();
    let shard_parent = address.dir().parent().unwrap().to_path_buf();
    let mut leftovers = Vec::new();
    for entry in fs::read_dir(&shard_parent).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.contains(".tmp-") {
            leftovers.push(name);
        }
    }
    assert!(
        leftovers.is_empty(),
        "found leftover staging dirs after freeze: {leftovers:?}"
    );
}

#[test]
#[serial]
fn derivation_lock_serializes_concurrent_acquisitions() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());

    let derivation_hash = "sha256:concurrent";
    let lock = DerivationLock::acquire(derivation_hash).unwrap();
    let lock_path = lock.path().to_path_buf();
    assert!(lock_path.is_file());

    let barrier = Arc::new(Barrier::new(2));
    let handle = {
        let barrier = barrier.clone();
        let derivation_hash = derivation_hash.to_string();
        thread::spawn(move || {
            barrier.wait();
            // This must block until the main thread drops `lock` because the
            // file lock is exclusive.
            let other = DerivationLock::acquire(&derivation_hash).unwrap();
            // After acquiring, drop is fine; just confirm we got past the
            // call after the main thread released.
            drop(other);
        })
    };
    barrier.wait();
    // Give the worker a moment to attempt acquisition; flock blocks.
    thread::sleep(std::time::Duration::from_millis(50));
    drop(lock);
    handle.join().unwrap();
}

#[test]
#[serial]
fn freeze_preserves_executable_bit_and_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());

    let deps = tmp.path().join("session-deps");
    write_file(&deps, "bin/run", b"#!/bin/sh\necho hi\n");
    fs::set_permissions(deps.join("bin/run"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("bin/run", deps.join("entrypoint")).unwrap();

    let derivation_hash = DepDerivationKeyV1::from_request(&sample_request())
        .derivation_hash()
        .unwrap();
    let outcome = freeze_dep_tree(&deps, &derivation_hash, "npm").unwrap();
    let address = BlobAddress::parse(&outcome.blob_hash).unwrap();

    let frozen_bin = address.payload_dir().join("bin/run");
    let mode = fs::metadata(&frozen_bin).unwrap().permissions().mode() & 0o100;
    assert_ne!(mode, 0, "executable bit must survive freeze");

    let frozen_link = address.payload_dir().join("entrypoint");
    let metadata = fs::symlink_metadata(&frozen_link).unwrap();
    assert!(metadata.file_type().is_symlink());
    let target = fs::read_link(&frozen_link).unwrap();
    assert_eq!(target, std::path::PathBuf::from("bin/run"));
}
