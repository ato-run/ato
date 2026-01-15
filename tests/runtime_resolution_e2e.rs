//! Runtime Resolution E2E Tests
//!
//! Tests the UARC V1.1.0 Runtime Resolution implementation:
//! - Multi-target preference-based selection
//! - Legacy fallback mode
//! - Engine capability constraints
//!
//! To run:
//! ```bash
//! cargo test --test runtime_resolution_e2e
//! ```

use std::collections::{HashMap, HashSet};

use nacelle::capsule_types::capsule_v1::{
    CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleStorage,
    CapsuleType, OciTarget, RuntimeType, SourceTarget, TargetsConfig, WasmTarget,
};
use nacelle::runtime::resolver::{
    detect_current_platform, resolve_runtime, ResolveContext, ResolvedTarget,
};
use nacelle::runtime::RuntimeKind;

// ============================================================================
// Test Helpers
// ============================================================================

/// Create a minimal manifest with targets configuration
fn create_manifest_with_targets(name: &str, targets: TargetsConfig) -> CapsuleManifestV1 {
    CapsuleManifestV1 {
        schema_version: "1.1".to_string(),
        name: name.to_string(),
        version: "1.0.0".to_string(),
        capsule_type: CapsuleType::App,
        metadata: Default::default(),
        capabilities: None,
        requirements: CapsuleRequirements::default(),
        execution: CapsuleExecution {
            runtime: RuntimeType::Oci, // Legacy fallback
            entrypoint: "alpine:latest".to_string(),
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
        targets: Some(targets),
        services: None,
    }
}

/// Create a legacy manifest without targets
fn create_legacy_manifest(name: &str, runtime: RuntimeType) -> CapsuleManifestV1 {
    CapsuleManifestV1 {
        schema_version: "1.0".to_string(),
        name: name.to_string(),
        version: "1.0.0".to_string(),
        capsule_type: CapsuleType::App,
        metadata: Default::default(),
        capabilities: None,
        requirements: CapsuleRequirements::default(),
        execution: CapsuleExecution {
            runtime,
            entrypoint: "test-entry".to_string(),
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

/// Create a context with all runtimes enabled
fn context_all_runtimes() -> ResolveContext {
    let mut supported = HashSet::new();
    supported.insert(RuntimeKind::Wasm);
    supported.insert(RuntimeKind::Youki);
    supported.insert(RuntimeKind::Source);

    let mut toolchains = HashSet::new();
    toolchains.insert("python".to_string());
    toolchains.insert("node".to_string());

    let mut jit_toolchains = HashSet::new();
    jit_toolchains.insert("python".to_string());
    jit_toolchains.insert("node".to_string());
    jit_toolchains.insert("deno".to_string());
    jit_toolchains.insert("bun".to_string());

    ResolveContext {
        platform: detect_current_platform(),
        supported_runtimes: supported,
        wasm_available: true,
        docker_available: true,
        gpu_available: false,
        available_toolchains: toolchains,
        jit_toolchains,
    }
}

/// Create a context with only Wasm runtime
fn context_wasm_only() -> ResolveContext {
    let mut supported = HashSet::new();
    supported.insert(RuntimeKind::Wasm);

    ResolveContext {
        platform: detect_current_platform(),
        supported_runtimes: supported,
        wasm_available: true,
        docker_available: false,
        gpu_available: false,
        available_toolchains: HashSet::new(),
        jit_toolchains: HashSet::new(),
    }
}

/// Create a context with only Docker/OCI runtime
fn context_docker_only() -> ResolveContext {
    let mut supported = HashSet::new();
    supported.insert(RuntimeKind::Youki);

    ResolveContext {
        platform: detect_current_platform(),
        supported_runtimes: supported,
        wasm_available: false,
        docker_available: true,
        gpu_available: false,
        available_toolchains: HashSet::new(),
        jit_toolchains: HashSet::new(),
    }
}

// ============================================================================
// Test: Legacy Fallback Mode
// ============================================================================

#[test]
fn test_legacy_fallback_when_no_targets() {
    let manifest = create_legacy_manifest("legacy-app", RuntimeType::Oci);
    let context = context_all_runtimes();

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    match resolved {
        ResolvedTarget::Legacy {
            runtime_type,
            entrypoint,
        } => {
            assert_eq!(runtime_type, RuntimeType::Oci);
            assert_eq!(entrypoint, "test-entry");
        }
        _ => panic!("Expected Legacy target, got {:?}", resolved),
    }
}

#[test]
fn test_legacy_wasm_fallback() {
    let manifest = create_legacy_manifest("wasm-legacy", RuntimeType::Wasm);
    let context = context_all_runtimes();

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    match resolved {
        ResolvedTarget::Legacy { runtime_type, .. } => {
            assert_eq!(runtime_type, RuntimeType::Wasm);
        }
        _ => panic!("Expected Legacy Wasm target"),
    }
}

// ============================================================================
// Test: Multi-Target Resolution with Preference
// ============================================================================

#[test]
fn test_wasm_first_preference() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["wasm".to_string(), "oci".to_string()],
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:abc123".to_string(),
            world: "wasi:cli/run@0.2.0".to_string(),
            config: HashMap::new(),
        }),
        source: None,
        oci: Some(OciTarget {
            image: "alpine:latest".to_string(),
            digest: Some("sha256:xyz789".to_string()),
            cmd: vec!["echo".to_string(), "hello".to_string()],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("wasm-first", targets);
    let context = context_all_runtimes();

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    match resolved {
        ResolvedTarget::Wasm { digest, world, .. } => {
            assert_eq!(digest, "sha256:abc123");
            assert_eq!(world, "wasi:cli/run@0.2.0");
        }
        _ => panic!("Expected Wasm target, got {:?}", resolved),
    }
}

#[test]
fn test_oci_first_preference() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["oci".to_string(), "wasm".to_string()],
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:abc123".to_string(),
            world: "wasi:cli/run@0.2.0".to_string(),
            config: HashMap::new(),
        }),
        source: None,
        oci: Some(OciTarget {
            image: "alpine:latest".to_string(),
            digest: Some("sha256:xyz789".to_string()),
            cmd: vec!["echo".to_string(), "hello".to_string()],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("oci-first", targets);
    let context = context_all_runtimes();

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    match resolved {
        ResolvedTarget::Oci { image, digest, cmd } => {
            assert_eq!(image, "alpine:latest");
            assert_eq!(digest, Some("sha256:xyz789".to_string()));
            assert_eq!(cmd, vec!["echo".to_string(), "hello".to_string()]);
        }
        _ => panic!("Expected Oci target, got {:?}", resolved),
    }
}

// ============================================================================
// Test: Engine Constraint Filtering
// ============================================================================

#[test]
fn test_wasm_only_engine_selects_wasm() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["oci".to_string(), "wasm".to_string()],
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:abc123".to_string(),
            world: "wasi:cli/run@0.2.0".to_string(),
            config: HashMap::new(),
        }),
        source: None,
        oci: Some(OciTarget {
            image: "alpine:latest".to_string(),
            digest: None,
            cmd: vec![],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("wasm-only-engine", targets);
    let context = context_wasm_only(); // Only Wasm is supported

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    // Even though OCI is preferred, only Wasm is available
    match resolved {
        ResolvedTarget::Wasm { .. } => {}
        _ => panic!(
            "Expected Wasm target due to engine constraint, got {:?}",
            resolved
        ),
    }
}

#[test]
fn test_docker_only_engine_selects_oci() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["wasm".to_string(), "oci".to_string()],
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:abc123".to_string(),
            world: "wasi:cli/run@0.2.0".to_string(),
            config: HashMap::new(),
        }),
        source: None,
        oci: Some(OciTarget {
            image: "alpine:latest".to_string(),
            digest: None,
            cmd: vec![],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("docker-only-engine", targets);
    let context = context_docker_only(); // Only Docker/Youki is supported

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    // Even though Wasm is preferred, only OCI is available
    match resolved {
        ResolvedTarget::Oci { .. } => {}
        _ => panic!(
            "Expected Oci target due to engine constraint, got {:?}",
            resolved
        ),
    }
}

// ============================================================================
// Test: Source Target Resolution
// ============================================================================

#[test]
fn test_source_target_with_toolchain() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["source".to_string()],
        source_digest: Some(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        ),
        wasm: None,
        source: Some(SourceTarget {
            dev_mode: false,
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: Some("requirements.txt".to_string()),
            args: vec![],
        }),
        oci: None,
    };

    let manifest = create_manifest_with_targets("python-source", targets);
    let context = context_all_runtimes(); // Includes python toolchain

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    match resolved {
        ResolvedTarget::Source {
            language,
            entrypoint,
            ..
        } => {
            assert_eq!(language, "python");
            assert_eq!(entrypoint, "main.py");
        }
        _ => panic!("Expected Source target, got {:?}", resolved),
    }
}

#[test]
fn test_source_target_without_toolchain_falls_back() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["source".to_string(), "oci".to_string()],
        source_digest: None,
        wasm: None,
        source: Some(SourceTarget {
            dev_mode: false,
            language: "ruby".to_string(), // Not in available toolchains
            version: None,
            entrypoint: "main.rb".to_string(),
            dependencies: None,
            args: vec![],
        }),
        oci: Some(OciTarget {
            image: "ruby:latest".to_string(),
            digest: None,
            cmd: vec![],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("ruby-fallback", targets);
    let context = context_all_runtimes(); // Does NOT have ruby toolchain

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    // Ruby toolchain not available, should fall back to OCI
    match resolved {
        ResolvedTarget::Oci { image, .. } => {
            assert_eq!(image, "ruby:latest");
        }
        _ => panic!("Expected Oci fallback target, got {:?}", resolved),
    }
}

// ============================================================================
// Test: Default Preference Order
// ============================================================================

#[test]
fn test_default_preference_order_wasm_source_oci() {
    // No explicit preference - should use default: wasm -> source -> oci
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec![], // Empty = use default
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:wasm".to_string(),
            world: "test".to_string(),
            config: HashMap::new(),
        }),
        source: Some(SourceTarget {
            dev_mode: false,
            language: "python".to_string(),
            version: None,
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
        }),
        oci: Some(OciTarget {
            image: "test:latest".to_string(),
            digest: None,
            cmd: vec![],
            env: HashMap::new(),
        }),
    };

    let manifest = create_manifest_with_targets("default-order", targets);
    let context = context_all_runtimes();

    let resolved = resolve_runtime(&manifest, &context).expect("should resolve");

    // Default preference should select Wasm first
    match resolved {
        ResolvedTarget::Wasm { .. } => {}
        _ => panic!(
            "Expected Wasm target with default preference, got {:?}",
            resolved
        ),
    }
}

// ============================================================================
// Test: No Compatible Target Error
// ============================================================================

#[test]
fn test_no_compatible_target_error() {
    let targets = TargetsConfig {
        port: None,
        startup_timeout: 60,
        env: HashMap::new(),
        health_check: None,
        preference: vec!["wasm".to_string()],
        source_digest: None,
        wasm: Some(WasmTarget {
            digest: "sha256:abc".to_string(),
            world: "test".to_string(),
            config: HashMap::new(),
        }),
        source: None,
        oci: None,
    };

    let manifest = create_manifest_with_targets("incompatible", targets);
    let context = context_docker_only(); // No Wasm support

    let result = resolve_runtime(&manifest, &context);

    assert!(result.is_err(), "Should fail when no compatible target");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("No compatible target"),
        "Error should mention no compatible target: {}",
        err
    );
}

// ============================================================================
// Test: RuntimeKind Mapping
// ============================================================================

#[test]
fn test_resolved_target_runtime_kind() {
    let wasm_target = ResolvedTarget::Wasm {
        digest: "sha256:test".to_string(),
        world: "test".to_string(),
        component_path: None,
    };
    assert_eq!(wasm_target.runtime_kind(), RuntimeKind::Wasm);

    let oci_target = ResolvedTarget::Oci {
        image: "test".to_string(),
        digest: None,
        cmd: vec![],
    };
    assert_eq!(oci_target.runtime_kind(), RuntimeKind::Youki);

    let source_target = ResolvedTarget::Source {
        language: "python".to_string(),
        version: None,
        entrypoint: "main.py".to_string(),
        dependencies: None,
        args: vec![],
    };
    // Source targets use Native runtime (for python-uv, node, etc.)
    assert_eq!(source_target.runtime_kind(), RuntimeKind::Source);
}

// ============================================================================
// Test: Platform Detection
// ============================================================================

#[test]
fn test_platform_detection() {
    let platform = detect_current_platform();

    // Should return a valid platform string
    assert!(!platform.is_empty());

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    assert_eq!(platform, "darwin-arm64");

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    assert_eq!(platform, "darwin-x86_64");

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(platform, "linux-amd64");

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    assert_eq!(platform, "linux-arm64");
}
