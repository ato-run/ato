#![allow(clippy::result_large_err)]

use std::path::Path;

use crate::ato_lock::AtoLock;
use crate::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash, normalize_unordered_set,
};
use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{
    CapsuleRef, Consent, ConsentKey, ExecutionDriver, ExecutionPlan, ExecutionRuntime,
    ExecutionTier, NonInteractiveBehavior, Platform, Provisioning, ProvisioningNetwork,
    Reproducibility, Runtime, RuntimeFilesystemPolicy, RuntimeNetworkPolicy, RuntimePolicy,
    RuntimeSecretsPolicy, SecretDelivery, TargetRef, EXECUTION_PLAN_SCHEMA_VERSION,
    MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION,
};
use crate::lock_runtime::{LockCompilerOverlay, ResolvedLockRuntimeModel};
use crate::manifest;
use crate::router::{self, ExecutionProfile, RuntimeDecision};
use crate::types::ValidationMode;

#[derive(Debug, Clone)]
pub struct CompiledExecutionPlan {
    pub execution_plan: ExecutionPlan,
    pub runtime_decision: RuntimeDecision,
    pub tier: ExecutionTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSnapshot {
    pub os: String,
    pub arch: String,
    pub libc: String,
}

impl PlatformSnapshot {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            libc: detect_libc().to_string(),
        }
    }
}

pub fn compile_execution_plan(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
) -> Result<CompiledExecutionPlan, AtoExecutionError> {
    compile_execution_plan_with_validation_mode(
        manifest_path,
        profile,
        target_label,
        ValidationMode::Strict,
    )
}

pub fn compile_execution_plan_with_validation_mode(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    validation_mode: ValidationMode,
) -> Result<CompiledExecutionPlan, AtoExecutionError> {
    let loaded = manifest::load_manifest_with_validation_mode(manifest_path, validation_mode)
        .map_err(|err| {
            AtoExecutionError::policy_violation(format!("failed to load manifest: {err}"))
        })?;

    let decision = router::route_manifest_with_validation_mode(
        manifest_path,
        profile,
        target_label,
        validation_mode,
    )
    .map_err(|err| {
        AtoExecutionError::policy_violation(format!("failed to route manifest: {err}"))
    })?;

    let selected_target_label = decision.plan.selected_target_label().to_string();
    let named_target = loaded
        .model
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target(&selected_target_label))
        .ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "target '{}' is missing in [targets]",
                selected_target_label
            ))
        })?;

    let runtime = ExecutionRuntime::from_manifest(&named_target.runtime).ok_or_else(|| {
        AtoExecutionError::policy_violation(format!(
            "unsupported runtime '{}' in targets.{}",
            named_target.runtime, selected_target_label
        ))
    })?;

    if matches!(runtime, ExecutionRuntime::Oci) {
        return Err(AtoExecutionError::policy_violation(
            "runtime=oci is not supported by the ExecutionPlan isolation model",
        ));
    }

    let driver = resolve_driver(
        runtime,
        named_target.driver.as_deref(),
        named_target.language.as_deref(),
        &named_target.cmd,
    )?;
    let tier = derive_tier(runtime, driver)?;

    let scoped_id = loaded.model.name.clone();
    let version = loaded.model.version.clone();

    let runtime_section = build_runtime_section(
        runtime,
        driver,
        loaded
            .model
            .network
            .as_ref()
            .map(|network| network.egress_allow.clone())
            .unwrap_or_default(),
        named_target.entrypoint.clone(),
        named_target.cmd.clone(),
        decision.plan.execution_port(),
        &LockCompilerOverlay::default(),
    )?;
    let provisioning = build_provisioning(runtime, driver, &runtime_section.policy, tier);
    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    let execution_plan = ExecutionPlan {
        schema_version: EXECUTION_PLAN_SCHEMA_VERSION.to_string(),
        capsule: CapsuleRef {
            scoped_id: scoped_id.clone(),
            version: version.clone(),
        },
        target: TargetRef {
            label: selected_target_label.clone(),
            runtime,
            driver,
            language: named_target.language.clone(),
        },
        provisioning,
        runtime: runtime_section,
        consent: Consent {
            key: ConsentKey {
                scoped_id,
                version,
                target_label: selected_target_label,
            },
            policy_segment_hash,
            provisioning_policy_hash,
            mount_set_algo_id: MOUNT_SET_ALGO_ID.to_string(),
            mount_set_algo_version: MOUNT_SET_ALGO_VERSION,
        },
        reproducibility: Reproducibility {
            platform: platform_from_snapshot(&PlatformSnapshot::current()),
        },
    };

    Ok(CompiledExecutionPlan {
        execution_plan,
        runtime_decision: decision,
        tier,
    })
}

pub fn compile_execution_plan_from_lock(
    _lock: &AtoLock,
    resolved: &ResolvedLockRuntimeModel,
    overlay: &LockCompilerOverlay,
    platform: &PlatformSnapshot,
) -> Result<ExecutionPlan, AtoExecutionError> {
    let scoped_id = resolved.metadata.name.clone().ok_or_else(|| {
        AtoExecutionError::execution_contract_invalid(
            "lock-derived execution requires contract.metadata.name so consent identity does not fall back to placeholders",
            Some("contract.metadata.name"),
            None,
        )
    })?;
    let version = resolved.metadata.version.clone().ok_or_else(|| {
        AtoExecutionError::execution_contract_invalid(
            "lock-derived execution requires contract.metadata.version so consent identity does not fall back to placeholders",
            Some("contract.metadata.version"),
            None,
        )
    })?;
    let selected = &resolved.selected;
    let runtime = ExecutionRuntime::from_manifest(&selected.runtime.runtime).ok_or_else(|| {
        AtoExecutionError::policy_violation(format!(
            "unsupported runtime '{}' in lock-derived target '{}'",
            selected.runtime.runtime, selected.target_label
        ))
    })?;

    if matches!(runtime, ExecutionRuntime::Oci) {
        return Err(AtoExecutionError::policy_violation(
            "runtime=oci is not supported by the ExecutionPlan isolation model",
        ));
    }

    let driver = resolve_driver(
        runtime,
        selected.runtime.driver.as_deref(),
        None,
        &selected.runtime.cmd,
    )?;
    let tier = derive_tier(runtime, driver)?;
    let runtime_section = build_runtime_section(
        runtime,
        driver,
        resolved
            .network
            .as_ref()
            .map(|network| network.egress_allow.clone())
            .unwrap_or_default(),
        selected.runtime.entrypoint.clone(),
        selected.runtime.cmd.clone(),
        selected.runtime.port,
        overlay,
    )?;
    let provisioning = build_provisioning(runtime, driver, &runtime_section.policy, tier);
    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    Ok(ExecutionPlan {
        schema_version: EXECUTION_PLAN_SCHEMA_VERSION.to_string(),
        capsule: CapsuleRef {
            scoped_id: scoped_id.clone(),
            version: version.clone(),
        },
        target: TargetRef {
            label: selected.target_label.clone(),
            runtime,
            driver,
            language: None,
        },
        provisioning,
        runtime: runtime_section,
        consent: Consent {
            key: ConsentKey {
                scoped_id,
                version,
                target_label: selected.target_label.clone(),
            },
            policy_segment_hash,
            provisioning_policy_hash,
            mount_set_algo_id: MOUNT_SET_ALGO_ID.to_string(),
            mount_set_algo_version: MOUNT_SET_ALGO_VERSION,
        },
        reproducibility: Reproducibility {
            platform: platform_from_snapshot(platform),
        },
    })
}

fn build_runtime_section(
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
    network_allow: Vec<String>,
    entrypoint: String,
    args: Vec<String>,
    port: Option<u16>,
    overlay: &LockCompilerOverlay,
) -> Result<Runtime, AtoExecutionError> {
    let mut allow_hosts = overlay.network_allow_hosts.clone().unwrap_or(network_allow);

    if matches!(runtime, ExecutionRuntime::Web) {
        let port = port.ok_or_else(|| {
            AtoExecutionError::policy_violation("runtime=web requires an execution port")
        })?;
        allow_hosts.push(format!("127.0.0.1:{port}"));
        allow_hosts.push(format!("localhost:{port}"));
        allow_hosts.push(format!("0.0.0.0:{port}"));
    } else if matches!(
        (runtime, driver),
        (ExecutionRuntime::Source, ExecutionDriver::Deno)
            | (ExecutionRuntime::Source, ExecutionDriver::Node)
    ) {
        if let Some(port) = port {
            allow_hosts.push(format!("127.0.0.1:{port}"));
            allow_hosts.push(format!("localhost:{port}"));
            allow_hosts.push(format!("0.0.0.0:{port}"));
        }
    }

    let read_only = overlay.filesystem_read_only.clone().unwrap_or_else(|| {
        if matches!(
            (runtime, driver),
            (ExecutionRuntime::Web, ExecutionDriver::Static)
        ) {
            vec![entrypoint]
        } else {
            Vec::new()
        }
    });

    Ok(Runtime {
        policy: RuntimePolicy {
            network: RuntimeNetworkPolicy {
                allow_hosts: normalize_unordered_set(&allow_hosts),
            },
            filesystem: RuntimeFilesystemPolicy {
                read_only: normalize_unordered_set(&read_only),
                read_write: normalize_unordered_set(
                    &overlay.filesystem_read_write.clone().unwrap_or_default(),
                ),
            },
            secrets: RuntimeSecretsPolicy {
                allow_secret_ids: normalize_unordered_set(
                    &overlay.secret_ids.clone().unwrap_or_default(),
                ),
                delivery: SecretDelivery::Fd,
            },
            args,
        },
        fail_closed: true,
        non_interactive_behavior: NonInteractiveBehavior::DenyIfUnconsented,
    })
}

fn build_provisioning(
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
    policy: &RuntimePolicy,
    tier: ExecutionTier,
) -> Provisioning {
    Provisioning {
        network: ProvisioningNetwork {
            allow_registry_hosts: policy.network.allow_hosts.clone(),
        },
        lock_required: matches!(
            (runtime, driver),
            (ExecutionRuntime::Source, ExecutionDriver::Deno)
                | (ExecutionRuntime::Source, ExecutionDriver::Node)
                | (ExecutionRuntime::Source, ExecutionDriver::Python)
                | (ExecutionRuntime::Web, ExecutionDriver::Deno)
                | (ExecutionRuntime::Web, ExecutionDriver::Node)
                | (ExecutionRuntime::Web, ExecutionDriver::Python)
        ),
        integrity_required: matches!(tier, ExecutionTier::Tier1),
        allowed_registries: policy.network.allow_hosts.clone(),
    }
}

fn platform_from_snapshot(snapshot: &PlatformSnapshot) -> Platform {
    Platform {
        os: snapshot.os.clone(),
        arch: snapshot.arch.clone(),
        libc: snapshot.libc.clone(),
    }
}

fn resolve_driver(
    runtime: ExecutionRuntime,
    explicit_driver: Option<&str>,
    language: Option<&str>,
    cmd: &[String],
) -> Result<ExecutionDriver, AtoExecutionError> {
    let parsed = explicit_driver.map(|value| {
        ExecutionDriver::from_manifest(value).ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "unsupported driver '{}' (allowed: static|deno|node|python|wasmtime|native)",
                value
            ))
        })
    });

    let parsed = match parsed {
        Some(v) => Some(v?),
        None => None,
    };

    if matches!(runtime, ExecutionRuntime::Web) && parsed.is_none() {
        return Err(AtoExecutionError::policy_violation(
            "runtime=web requires explicit driver (static|node|deno|python)",
        ));
    }

    let inferred = match runtime {
        ExecutionRuntime::Web => ExecutionDriver::Static,
        ExecutionRuntime::Wasm => ExecutionDriver::Wasmtime,
        ExecutionRuntime::Source => {
            if let Some(program) = cmd.first() {
                match program.trim().to_ascii_lowercase().as_str() {
                    "deno" => return Ok(parsed.unwrap_or(ExecutionDriver::Deno)),
                    "node" | "nodejs" => return Ok(parsed.unwrap_or(ExecutionDriver::Node)),
                    "python" | "python3" | "py" => {
                        return Ok(parsed.unwrap_or(ExecutionDriver::Python));
                    }
                    _ => {}
                }
            }

            match language
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "deno" => ExecutionDriver::Deno,
                "node" | "nodejs" | "javascript" | "typescript" | "js" | "ts" => {
                    ExecutionDriver::Node
                }
                "python" | "python3" | "py" => ExecutionDriver::Python,
                _ => ExecutionDriver::Native,
            }
        }
        ExecutionRuntime::Oci => {
            return Err(AtoExecutionError::policy_violation(
                "runtime=oci is not supported",
            ));
        }
    };

    let chosen = parsed.unwrap_or(inferred);

    match (runtime, chosen) {
        (ExecutionRuntime::Web, ExecutionDriver::Static)
        | (ExecutionRuntime::Web, ExecutionDriver::Deno)
        | (ExecutionRuntime::Web, ExecutionDriver::Node)
        | (ExecutionRuntime::Web, ExecutionDriver::Python)
        | (ExecutionRuntime::Wasm, ExecutionDriver::Wasmtime)
        | (ExecutionRuntime::Source, ExecutionDriver::Deno)
        | (ExecutionRuntime::Source, ExecutionDriver::Node)
        | (ExecutionRuntime::Source, ExecutionDriver::Python)
        | (ExecutionRuntime::Source, ExecutionDriver::Native) => Ok(chosen),
        _ => Err(AtoExecutionError::policy_violation(format!(
            "driver '{}' is incompatible with runtime '{}'",
            chosen.as_str(),
            runtime.as_str()
        ))),
    }
}

pub fn derive_tier(
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
) -> Result<ExecutionTier, AtoExecutionError> {
    match (runtime, driver) {
        (ExecutionRuntime::Web, ExecutionDriver::Static)
        | (ExecutionRuntime::Web, ExecutionDriver::Deno)
        | (ExecutionRuntime::Web, ExecutionDriver::Node)
        | (ExecutionRuntime::Source, ExecutionDriver::Deno)
        | (ExecutionRuntime::Source, ExecutionDriver::Node)
        | (ExecutionRuntime::Wasm, ExecutionDriver::Wasmtime) => Ok(ExecutionTier::Tier1),
        (ExecutionRuntime::Web, ExecutionDriver::Python)
        | (ExecutionRuntime::Source, ExecutionDriver::Python)
        | (ExecutionRuntime::Source, ExecutionDriver::Native) => Ok(ExecutionTier::Tier2),
        _ => Err(AtoExecutionError::policy_violation(format!(
            "unable to derive tier from runtime='{}' driver='{}'",
            runtime.as_str(),
            driver.as_str()
        ))),
    }
}

fn detect_libc() -> &'static str {
    #[cfg(target_env = "gnu")]
    {
        "glibc"
    }
    #[cfg(target_env = "musl")]
    {
        "musl"
    }
    #[cfg(target_env = "msvc")]
    {
        "msvc"
    }
    #[cfg(not(any(target_env = "gnu", target_env = "musl", target_env = "msvc")))]
    {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ato_lock::AtoLock;
    use crate::lock_runtime::{resolve_lock_runtime_model, LockCompilerOverlay};
    use serde_json::json;
    use std::fs;

    fn sample_lock() -> AtoLock {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "demo", "version": "0.1.0", "default_target": "main"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": ["deno", "run", "main.ts"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([
                {
                    "name": "main",
                    "target": "main",
                    "process": {"entrypoint": "main.ts", "cmd": ["deno", "run", "main.ts"]}
                }
            ]),
        );
        lock.contract.entries.insert(
            "network".to_string(),
            json!({"egress_allow": ["registry.npmjs.org"]}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "selected_target": "main"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {
                    "label": "main",
                    "runtime": "source",
                    "driver": "deno",
                    "entrypoint": "main.ts",
                    "cmd": ["deno", "run", "main.ts"],
                    "port": 3000
                }
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete"}),
        );
        lock
    }

    #[test]
    fn tier_derivation_accepts_supported_pairs() {
        assert!(matches!(
            derive_tier(ExecutionRuntime::Web, ExecutionDriver::Static).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Web, ExecutionDriver::Node).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Web, ExecutionDriver::Deno).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Web, ExecutionDriver::Python).unwrap(),
            ExecutionTier::Tier2
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Source, ExecutionDriver::Deno).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Wasm, ExecutionDriver::Wasmtime).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Source, ExecutionDriver::Native).unwrap(),
            ExecutionTier::Tier2
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Source, ExecutionDriver::Node).unwrap(),
            ExecutionTier::Tier1
        ));
        assert!(matches!(
            derive_tier(ExecutionRuntime::Source, ExecutionDriver::Python).unwrap(),
            ExecutionTier::Tier2
        ));
    }

    #[test]
    fn tier_derivation_rejects_unsupported_pairs() {
        let err = derive_tier(ExecutionRuntime::Wasm, ExecutionDriver::Native).unwrap_err();
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }

    #[test]
    fn driver_resolution_infers_from_language() {
        let driver =
            resolve_driver(ExecutionRuntime::Source, None, Some("deno"), &[]).expect("driver");
        assert!(matches!(driver, ExecutionDriver::Deno));
    }

    #[test]
    fn driver_resolution_infers_node_from_language() {
        let driver = resolve_driver(ExecutionRuntime::Source, None, Some("typescript"), &[])
            .expect("driver");
        assert!(matches!(driver, ExecutionDriver::Node));
    }

    #[test]
    fn driver_resolution_infers_python_from_language() {
        let driver =
            resolve_driver(ExecutionRuntime::Source, None, Some("python"), &[]).expect("driver");
        assert!(matches!(driver, ExecutionDriver::Python));
    }

    #[test]
    fn driver_resolution_infers_deno_from_cmd_program() {
        let driver = resolve_driver(
            ExecutionRuntime::Source,
            None,
            None,
            &["deno".to_string(), "run".to_string(), "main.ts".to_string()],
        )
        .expect("driver");
        assert!(matches!(driver, ExecutionDriver::Deno));
    }

    #[test]
    fn driver_resolution_rejects_mismatch() {
        let err = resolve_driver(ExecutionRuntime::Web, Some("native"), None, &[]).unwrap_err();
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }

    #[test]
    fn driver_resolution_requires_explicit_driver_for_web() {
        let err = resolve_driver(ExecutionRuntime::Web, None, None, &[]).unwrap_err();
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
        assert!(err.message.contains("requires explicit driver"));
    }

    #[test]
    fn compile_rejects_oci_runtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "oci-app"
version = "1.0.0"
type = "app"

runtime = "oci"
run = "ghcr.io/example/app:latest""#,
        )
        .expect("write manifest");

        let err = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, None).unwrap_err();
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }

    #[test]
    fn compile_from_lock_preserves_selected_target_and_hash_inputs() {
        let lock = sample_lock();
        let resolved = resolve_lock_runtime_model(&lock, Some("main")).expect("resolved");
        let plan = compile_execution_plan_from_lock(
            &lock,
            &resolved,
            &LockCompilerOverlay::default(),
            &PlatformSnapshot {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                libc: "unknown".to_string(),
            },
        )
        .expect("plan");

        assert_eq!(plan.target.label, "main");
        assert_eq!(plan.capsule.scoped_id, "demo");
        assert_eq!(plan.consent.key.target_label, "main");
        assert!(!plan.consent.policy_segment_hash.is_empty());
    }

    #[test]
    fn compile_from_lock_rejects_missing_metadata_identity() {
        let mut lock = sample_lock();
        let metadata = lock
            .contract
            .entries
            .get_mut("metadata")
            .and_then(|value| value.as_object_mut())
            .expect("metadata");
        metadata.remove("version");

        let resolved = resolve_lock_runtime_model(&lock, Some("main")).expect("resolved");
        let error = compile_execution_plan_from_lock(
            &lock,
            &resolved,
            &LockCompilerOverlay::default(),
            &PlatformSnapshot {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                libc: "unknown".to_string(),
            },
        )
        .expect_err("missing metadata version must fail");

        assert_eq!(error.code, "ATO_ERR_EXECUTION_CONTRACT_INVALID");
        assert!(error.to_string().contains("contract.metadata.version"));
    }

    #[test]
    fn compile_from_lock_rejects_incomplete_draft_without_closure() {
        let mut lock = sample_lock();
        lock.resolution.entries.remove("closure");

        let error = resolve_lock_runtime_model(&lock, Some("main")).expect_err("must fail");
        assert_eq!(error.code, "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE");
    }
}
