//! Runtime Resolution E2E Tests (Source-only)
//!
//! v0.2.0: nacelle supports Source targets only. OCI/Wasm targets are rejected
//! and must be handled by capsule-cli.
//!
//! To run:
//! ```bash
//! cargo test --test runtime_resolution_e2e
//! ```

use std::collections::HashMap;

use nacelle::capsule_types::capsule_v1::{
    CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleStorage,
    CapsuleType, RuntimeType, SourceTarget, TargetsConfig, WasmTarget,
};
use nacelle::launcher::resolver::{resolve_runtime, ResolveContext, ResolveError, ResolvedTarget};
use nacelle::launcher::RuntimeKind;

fn base_manifest(name: &str, runtime: RuntimeType) -> CapsuleManifestV1 {
    CapsuleManifestV1 {
        schema_version: "1.1".to_string(),
        name: name.to_string(),
        version: "1.0.0".to_string(),
        capsule_type: CapsuleType::App,
        metadata: Default::default(),
        capabilities: None,
        requirements: CapsuleRequirements::default(),
        execution: CapsuleExecution {
            runtime,
            entrypoint: "main.py".to_string(),
            port: None,
            health_check: None,
            startup_timeout: 60,
            env: HashMap::new(),
            signals: Default::default(),
        },
        storage: CapsuleStorage::default(),
        routing: CapsuleRouting::default(),
        network: None,
        model: None,
        transparency: None,
        pool: None,
        build: None,
        isolation: None,
        targets: None,
        services: None,
    }
}

fn manifest_with_targets(name: &str, targets: TargetsConfig) -> CapsuleManifestV1 {
    let mut manifest = base_manifest(name, RuntimeType::Source);
    manifest.targets = Some(targets);
    manifest
}

#[test]
fn resolves_source_target() {
    let targets = TargetsConfig {
        preference: vec!["source".to_string()],
        source: Some(SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: Some("requirements.txt".to_string()),
            args: vec!["-u".to_string()],
            dev_mode: false,
        }),
        ..Default::default()
    };

    let manifest = manifest_with_targets("source-app", targets);
    let context = ResolveContext::source_only();

    let resolved = resolve_runtime(&manifest, &context).expect("resolve source target");

    match resolved {
        ResolvedTarget::Source {
            language,
            entrypoint,
            ..
        } => {
            assert_eq!(language, "python");
            assert_eq!(entrypoint, "main.py");
        }
    }
}

#[test]
fn legacy_source_without_targets_is_invalid() {
    let manifest = base_manifest("legacy-source", RuntimeType::Source);
    let context = ResolveContext::source_only();

    let result = resolve_runtime(&manifest, &context);

    assert!(matches!(
        result,
        Err(ResolveError::InvalidConfiguration { .. })
    ));
}

#[test]
fn legacy_oci_is_unsupported() {
    let manifest = base_manifest("legacy-oci", RuntimeType::Oci);
    let context = ResolveContext::source_only();

    let result = resolve_runtime(&manifest, &context);

    assert!(matches!(
        result,
        Err(ResolveError::UnsupportedTarget { .. })
    ));
}

#[test]
fn only_wasm_target_is_incompatible() {
    let targets = TargetsConfig {
        preference: vec!["wasm".to_string()],
        wasm: Some(WasmTarget {
            digest: "sha256:abc123".to_string(),
            world: "wasi:cli/command".to_string(),
            config: HashMap::new(),
        }),
        ..Default::default()
    };

    let manifest = manifest_with_targets("wasm-only", targets);
    let context = ResolveContext::source_only();

    let result = resolve_runtime(&manifest, &context);

    assert!(matches!(
        result,
        Err(ResolveError::NoCompatibleTarget { .. })
    ));
}

#[test]
fn runtime_kind_for_source_target() {
    let target = ResolvedTarget::Source {
        language: "python".to_string(),
        version: None,
        entrypoint: "main.py".to_string(),
        dependencies: None,
        args: vec![],
        dev_mode: false,
    };

    assert_eq!(target.runtime_kind(), RuntimeKind::Source);
}
