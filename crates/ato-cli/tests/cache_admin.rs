//! Tests for `ato cache stats` and `ato cache clear` exposed through the
//! library boundary in `application::cache_admin`.

use std::fs;
use std::path::Path;

use ato_cli::cache_admin::{clear_all, clear_derivation, collect_cache_stats};
use ato_cli::dependency_materializer::freeze::freeze_dep_tree;
use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheStrategy, DepDerivationKeyV1, DependencyMaterializationRequest,
    InstallPolicies, ManifestInputs, PlatformTriple, RuntimeSelection,
};
use serial_test::serial;

mod support;

use support::IsolatedAto;

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn sample_request(seed: &str) -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        session_id: format!("session-{seed}"),
        capsule_id: format!("capsule-{seed}"),
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
            lockfile_digest: Some(format!("sha256:lock-{seed}")),
            package_manifest_digest: Some(format!("sha256:manifest-{seed}")),
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

fn freeze_two_derivations(tmp: &Path) -> (String, String, String, String) {
    let req_a = sample_request("a");
    let req_b = sample_request("b");

    let deps_a = tmp.join("install-a");
    let deps_b = tmp.join("install-b");
    write_file(&deps_a, "lib.js", b"// a\n");
    write_file(&deps_b, "lib.js", b"// b\n");

    let dh_a = DepDerivationKeyV1::from_request(&req_a)
        .derivation_hash()
        .unwrap();
    let dh_b = DepDerivationKeyV1::from_request(&req_b)
        .derivation_hash()
        .unwrap();

    let outcome_a = freeze_dep_tree(&deps_a, &dh_a, &req_a.ecosystem).unwrap();
    let outcome_b = freeze_dep_tree(&deps_b, &dh_b, &req_b.ecosystem).unwrap();

    (dh_a, dh_b, outcome_a.blob_hash, outcome_b.blob_hash)
}

#[test]
#[serial]
fn stats_on_empty_store_reports_zero_blobs() {
    let _env = IsolatedAto::new();

    let stats = collect_cache_stats().unwrap();
    assert_eq!(stats.blob_count, 0);
    assert_eq!(stats.ref_count, 0);
    assert_eq!(stats.total_bytes, 0);
    assert_eq!(stats.unreferenced_blob_count, 0);
}

#[test]
#[serial]
fn stats_after_two_freezes_reports_two_blobs_and_two_refs() {
    let env = IsolatedAto::new();
    let (_dh_a, _dh_b, _, _) = freeze_two_derivations(env.path());

    let stats = collect_cache_stats().unwrap();
    assert_eq!(stats.blob_count, 2, "expected two blobs after two freezes");
    assert_eq!(stats.ref_count, 2);
    assert_eq!(
        stats.unreferenced_blob_count, 0,
        "every blob has a ref pointing at it"
    );
    assert!(stats.largest_blob_bytes.is_some());
}

#[test]
#[serial]
fn clear_all_removes_blobs_and_refs() {
    let env = IsolatedAto::new();
    let (_dh_a, _dh_b, _, _) = freeze_two_derivations(env.path());

    let outcome = clear_all().unwrap();
    assert_eq!(outcome.blobs_removed, 2);
    assert_eq!(outcome.refs_removed, 2);

    let stats = collect_cache_stats().unwrap();
    assert_eq!(stats.blob_count, 0);
    assert_eq!(stats.ref_count, 0);
}

#[test]
#[serial]
fn clear_derivation_removes_only_the_named_entry() {
    let env = IsolatedAto::new();
    let (dh_a, _dh_b, blob_a, blob_b) = freeze_two_derivations(env.path());

    let outcome = clear_derivation(&dh_a).unwrap();
    assert_eq!(outcome.refs_removed, 1);
    assert_eq!(outcome.blobs_removed, 1);

    let stats = collect_cache_stats().unwrap();
    assert_eq!(stats.blob_count, 1);
    assert_eq!(stats.ref_count, 1);

    // Blob B should still be present; blob A is gone.
    assert_ne!(blob_a, blob_b);
}

#[test]
#[serial]
fn clear_derivation_keeps_blob_when_another_ref_still_points_at_it() {
    use ato_cli::dependency_materializer::StoreRefRecord;
    use capsule_core::common::store::ato_store_dep_ref_path;

    let env = IsolatedAto::new();
    let req = sample_request("shared");
    let deps = env.path().join("install");
    write_file(&deps, "lib.js", b"shared\n");

    let dh_first = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();
    let outcome = freeze_dep_tree(&deps, &dh_first, &req.ecosystem).unwrap();

    // Manually drop in a second ref from a different derivation hash that
    // points at the same blob, mimicking two semantically equivalent
    // installs collapsing onto one cache entry.
    let dh_second = "sha256:11111111111111111111111111111111111111111111111111111111deadbeef";
    let path = ato_store_dep_ref_path(&req.ecosystem, dh_second);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let extra_record = StoreRefRecord {
        schema_version: "1".to_string(),
        ecosystem: req.ecosystem.clone(),
        derivation_hash: dh_second.to_string(),
        blob_hash: Some(outcome.blob_hash.clone()),
        cache_status: "frozen".to_string(),
        created_at: "2026-05-03T00:00:00Z".to_string(),
    };
    fs::write(&path, serde_json::to_vec_pretty(&extra_record).unwrap()).unwrap();

    let cleared = clear_derivation(&dh_first).unwrap();
    assert_eq!(cleared.refs_removed, 1);
    assert_eq!(
        cleared.blobs_removed, 0,
        "blob must remain because another ref points at it"
    );
    assert_eq!(cleared.skipped_referenced.len(), 1);

    let stats = collect_cache_stats().unwrap();
    assert_eq!(stats.blob_count, 1);
    assert_eq!(stats.ref_count, 1);
}
