use ato_cli::dependency_materializer::{
    AttestationStrategy, CacheStrategy, DepDerivationKeyV1, DependencyMaterializationRequest,
    InstallPolicies, ManifestInputs, PlatformTriple, RuntimeSelection, SourceResolutionRecord,
};

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
