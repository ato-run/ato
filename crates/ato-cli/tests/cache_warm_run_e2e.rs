//! End-to-end coverage for the A1 freeze → plan(hit) → project flow that
//! a warm `ato run` will exercise once the CLI flag wires --cache=derivation
//! through. The test stays at the library boundary so it can run without
//! invoking pnpm/uv/etc.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use ato_cli::dependency_materializer::freeze::freeze_dep_tree;
use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheLookupResult, CacheStrategy, DepDerivationKeyV1,
    DependencyMaterializationRequest, DependencyMaterializer, InstallPolicies, ManifestInputs,
    PlatformTriple, RuntimeSelection, SessionDependencyMaterializer,
};
use ato_cli::projection::project_payload;
use capsule_core::blob::hash_tree;
use capsule_core::common::store::BlobAddress;
use serial_test::serial;
use tempfile::TempDir;

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn cached_request() -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        session_id: "warm-test".to_string(),
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
fn freeze_then_project_round_trip_preserves_blob_hash() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    // Cold path: install simulation produced this dependency tree.
    let cold_deps = tmp.path().join("cold-run/deps");
    write_file(
        &cold_deps,
        "node_modules/foo/index.js",
        b"console.log('foo');\n",
    );
    write_file(
        &cold_deps,
        "node_modules/foo/package.json",
        b"{\"name\":\"foo\",\"version\":\"1.0.0\"}\n",
    );
    write_file(
        &cold_deps,
        "node_modules/.bin/foo",
        b"#!/usr/bin/env node\n",
    );

    let req = cached_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();
    let original = hash_tree(&cold_deps).unwrap();

    let outcome = freeze_dep_tree(&cold_deps, &derivation_hash, &req.ecosystem).unwrap();
    assert!(outcome.did_freeze);
    assert_eq!(outcome.blob_hash, original.blob_hash);

    // Warm path: plan() must report the freeze as a hit and projection
    // must reproduce the tree with an identical blob hash.
    let plan = SessionDependencyMaterializer::new().plan(&req).unwrap();
    let blob_hash = match plan.cache_lookup {
        CacheLookupResult::Hit { blob_hash } => blob_hash,
        other => panic!("expected hit, got {other:?}"),
    };
    assert_eq!(blob_hash, original.blob_hash);

    let address = BlobAddress::parse(&blob_hash).unwrap();
    let warm_deps = tmp.path().join("warm-run/deps");
    project_payload(&address.payload_dir(), &warm_deps).unwrap();

    let warm_hash = hash_tree(&warm_deps).unwrap();
    assert_eq!(
        warm_hash.blob_hash, original.blob_hash,
        "projecting the frozen blob must reproduce the original tree's hash"
    );
    assert_eq!(warm_hash.file_count, original.file_count);
    assert_eq!(warm_hash.total_bytes, original.total_bytes);
}

#[test]
#[serial]
fn warm_run_observes_existing_blob_without_rewriting() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let deps = tmp.path().join("install-output");
    write_file(&deps, "lib.js", b"// shared\n");

    let req = cached_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();

    let cold = freeze_dep_tree(&deps, &derivation_hash, &req.ecosystem).unwrap();
    assert!(cold.did_freeze);

    // A second run with the same inputs should reuse the existing blob.
    let warm = freeze_dep_tree(&deps, &derivation_hash, &req.ecosystem).unwrap();
    assert!(!warm.did_freeze);
    assert_eq!(warm.blob_hash, cold.blob_hash);
}
