use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheLookupResult, CacheStrategy, DepDerivationKeyV1,
    DependencyMaterializationRequest, DependencyMaterializer, InstallPolicies, ManifestInputs,
    PlatformTriple, RuntimeSelection, SessionDependencyMaterializer, SourceResolutionRecord,
    StoreRefRecord,
};
use capsule_core::blob::{BlobManifest, BLOB_MANIFEST_SCHEMA_VERSION, BLOB_TREE_ALGORITHM};
use capsule_core::common::store::{ato_store_dep_ref_path, BlobAddress};
use serial_test::serial;
use tempfile::TempDir;

const SAMPLE_BLOB_HASH: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn sample_request() -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        session_id: "test".to_string(),
        capsule_id: "capsule".to_string(),
        source_root: "/repo".into(),
        workspace_root: "/repo".into(),
        ecosystem: "npm".to_string(),
        package_manager: Some("pnpm".to_string()),
        package_manager_version: Some("9.12.3".to_string()),
        runtime: RuntimeSelection {
            name: "node".to_string(),
            version: Some("20.12.2".to_string()),
        },
        manifests: ManifestInputs {
            lockfile_digest: Some("sha256:lock".to_string()),
            package_manifest_digest: Some("sha256:manifest".to_string()),
            workspace_manifest_digest: Some("sha256:workspace".to_string()),
            path_dependency_digest: Some("sha256:path".to_string()),
        },
        policies: InstallPolicies {
            lifecycle_script_policy: "sandbox".to_string(),
            registry_policy: "default".to_string(),
            network_policy: "strict".to_string(),
            env_allowlist_digest: Some("sha256:env".to_string()),
        },
        platform: PlatformTriple {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: Some("glibc".to_string()),
            abi: None,
        },
        cache_strategy: CacheStrategy::None,
        attestation_strategy: AttestationStrategy::None,
    }
}

fn hash(req: &DependencyMaterializationRequest) -> String {
    DepDerivationKeyV1::from_request(req)
        .derivation_hash()
        .expect("derivation hash")
}

#[test]
fn all_v1_derivation_keys_change_the_digest() {
    let base = sample_request();
    let base_hash = hash(&base);

    let mut schema = DepDerivationKeyV1::from_request(&base);
    schema.schema_version = 2;
    assert_ne!(
        base_hash,
        schema.derivation_hash().unwrap(),
        "schema_version should affect digest"
    );

    let mut cases: Vec<(&str, DependencyMaterializationRequest)> = Vec::new();

    let mut changed = base.clone();
    changed.ecosystem = "pypi".to_string();
    cases.push(("ecosystem", changed));

    let mut changed = base.clone();
    changed.package_manager = Some("npm".to_string());
    cases.push(("package_manager", changed));

    let mut changed = base.clone();
    changed.package_manager_version = Some("10.0.0".to_string());
    cases.push(("package_manager_compat_class", changed));

    let mut changed = base.clone();
    changed.runtime.version = Some("21.0.0".to_string());
    cases.push(("runtime_compat_class", changed));

    let mut changed = base.clone();
    changed.platform.arch = "aarch64".to_string();
    cases.push(("platform_triple", changed));

    let mut changed = base.clone();
    changed.manifests.lockfile_digest = Some("sha256:lock2".to_string());
    cases.push(("lockfile_digest", changed));

    let mut changed = base.clone();
    changed.manifests.package_manifest_digest = Some("sha256:manifest2".to_string());
    cases.push(("manifest_digest", changed));

    let mut changed = base.clone();
    changed.manifests.path_dependency_digest = Some("sha256:path2".to_string());
    cases.push(("path_dependency_digest", changed));

    let mut changed = base.clone();
    changed.policies.network_policy = "offline".to_string();
    cases.push(("install_policy_digest", changed));

    let mut changed = base.clone();
    changed.policies.env_allowlist_digest = Some("sha256:env2".to_string());
    cases.push(("env_allowlist_digest", changed));

    for (key, changed) in cases {
        assert_ne!(base_hash, hash(&changed), "{key} should affect digest");
    }
}

#[test]
fn non_v1_fields_do_not_change_the_digest() {
    let base = sample_request();
    let base_hash = hash(&base);

    let mut patch_package_manager = base.clone();
    patch_package_manager.package_manager_version = Some("9.99.99".to_string());
    assert_eq!(base_hash, hash(&patch_package_manager));

    let mut patch_runtime = base.clone();
    patch_runtime.runtime.version = Some("20.99.99".to_string());
    assert_eq!(base_hash, hash(&patch_runtime));

    let mut workspace_manifest = base.clone();
    workspace_manifest.manifests.workspace_manifest_digest = Some("sha256:workspace2".to_string());
    assert_eq!(base_hash, hash(&workspace_manifest));

    let mut strategy = base.clone();
    strategy.cache_strategy = CacheStrategy::DerivationCache;
    strategy.attestation_strategy = AttestationStrategy::LocalSign;
    assert_eq!(base_hash, hash(&strategy));
}

#[test]
fn mutable_requested_ref_is_not_part_of_source_identity() {
    let main = SourceResolutionRecord {
        authority: "github.com".to_string(),
        repository: Some("acme/app".to_string()),
        requested_ref: Some("main".to_string()),
        resolved_commit: "3f2e9c1".to_string(),
        resolved_at: "2026-05-02T00:00:00Z".to_string(),
        commit_signature_verdict: None,
    };
    let tag = SourceResolutionRecord {
        requested_ref: Some("v1.0.0".to_string()),
        ..main.clone()
    };

    assert_eq!(main.identity_hash().unwrap(), tag.identity_hash().unwrap());
}

/// Restores the previous value of an env var when dropped so concurrent tests
/// (run serially via `#[serial]`) do not bleed state into each other.
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

fn cached_request() -> DependencyMaterializationRequest {
    DependencyMaterializationRequest {
        cache_strategy: CacheStrategy::DerivationCache,
        ..sample_request()
    }
}

fn write_ref(ato_home: &Path, ecosystem: &str, derivation_hash: &str, blob_hash: Option<&str>) {
    std::env::set_var("ATO_HOME", ato_home);
    let path = ato_store_dep_ref_path(ecosystem, derivation_hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = StoreRefRecord {
        schema_version: "1".to_string(),
        ecosystem: ecosystem.to_string(),
        derivation_hash: derivation_hash.to_string(),
        blob_hash: blob_hash.map(str::to_string),
        cache_status: if blob_hash.is_some() {
            "hit".into()
        } else {
            "miss".into()
        },
        created_at: "2026-05-03T00:00:00Z".to_string(),
    };
    fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
}

fn write_blob(ato_home: &Path, blob_hash: &str, derivation_hash: &str) -> PathBuf {
    std::env::set_var("ATO_HOME", ato_home);
    let address = BlobAddress::parse(blob_hash).expect("valid hash");
    fs::create_dir_all(address.payload_dir()).unwrap();
    fs::write(address.payload_dir().join("placeholder"), b"").unwrap();

    let manifest = BlobManifest {
        schema_version: BLOB_MANIFEST_SCHEMA_VERSION,
        algorithm: BLOB_TREE_ALGORITHM.to_string(),
        blob_hash: blob_hash.to_string(),
        derivation_hash: derivation_hash.to_string(),
        created_at: "2026-05-03T00:00:00Z".to_string(),
        file_count: 1,
        symlink_count: 0,
        dir_count: 0,
        total_bytes: 0,
    };
    manifest.write_to(&address.manifest_path()).unwrap();
    address.dir()
}

fn snapshot_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if !root.exists() {
        return paths;
    }
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        paths.push(entry.path().to_path_buf());
    }
    paths.sort();
    paths
}

#[test]
#[serial]
fn plan_returns_disabled_when_strategy_is_none() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let plan = SessionDependencyMaterializer::new()
        .plan(&sample_request())
        .unwrap();
    assert!(matches!(plan.cache_lookup, CacheLookupResult::Disabled));
}

#[test]
#[serial]
fn plan_returns_disabled_when_safety_env_is_off() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    std::env::remove_var("ATO_DEP_CACHE");

    let plan = SessionDependencyMaterializer::new()
        .plan(&cached_request())
        .unwrap();
    assert!(matches!(plan.cache_lookup, CacheLookupResult::Disabled));
}

#[test]
#[serial]
fn plan_returns_miss_when_no_ref_exists() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let plan = SessionDependencyMaterializer::new()
        .plan(&cached_request())
        .unwrap();
    assert!(matches!(plan.cache_lookup, CacheLookupResult::Miss));
}

#[test]
#[serial]
fn plan_returns_hit_when_ref_and_manifest_match() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let req = cached_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();
    write_blob(tmp.path(), SAMPLE_BLOB_HASH, &derivation_hash);
    write_ref(
        tmp.path(),
        &req.ecosystem,
        &derivation_hash,
        Some(SAMPLE_BLOB_HASH),
    );

    let plan = SessionDependencyMaterializer::new().plan(&req).unwrap();
    match plan.cache_lookup {
        CacheLookupResult::Hit { blob_hash } => assert_eq!(blob_hash, SAMPLE_BLOB_HASH),
        other => panic!("expected hit, got {other:?}"),
    }
}

#[test]
#[serial]
fn plan_falls_back_to_miss_when_manifest_blob_hash_disagrees() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let req = cached_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();
    write_blob(tmp.path(), SAMPLE_BLOB_HASH, &derivation_hash);

    let liar = "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    write_ref(tmp.path(), &req.ecosystem, &derivation_hash, Some(liar));

    let plan = SessionDependencyMaterializer::new().plan(&req).unwrap();
    assert!(
        matches!(plan.cache_lookup, CacheLookupResult::Miss),
        "ref claiming a blob the manifest does not back must be a miss"
    );
}

#[test]
#[serial]
fn plan_returns_miss_when_blob_payload_dir_is_absent() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let req = cached_request();
    let derivation_hash = DepDerivationKeyV1::from_request(&req)
        .derivation_hash()
        .unwrap();
    write_ref(
        tmp.path(),
        &req.ecosystem,
        &derivation_hash,
        Some(SAMPLE_BLOB_HASH),
    );

    let plan = SessionDependencyMaterializer::new().plan(&req).unwrap();
    assert!(matches!(plan.cache_lookup, CacheLookupResult::Miss));
}

#[test]
#[serial]
fn plan_does_not_modify_the_filesystem() {
    let tmp = TempDir::new().unwrap();
    let _home = EnvGuard::set("ATO_HOME", tmp.path());
    let _cache = EnvGuard::set("ATO_DEP_CACHE", "1");

    let before = snapshot_paths(tmp.path());

    let _ = SessionDependencyMaterializer::new()
        .plan(&cached_request())
        .unwrap();

    let after = snapshot_paths(tmp.path());
    assert_eq!(
        before, after,
        "plan() must be read-only; new fs entries appeared after the call"
    );
}
