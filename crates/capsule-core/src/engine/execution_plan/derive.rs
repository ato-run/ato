#![allow(clippy::result_large_err)]

use std::path::Path;

use crate::ato_lock::AtoLock;
use crate::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash, normalize_unordered_set,
};
use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{
    CapsuleRef, Consent, ConsentKey, EXECUTION_PLAN_SCHEMA_VERSION, ExecutionDriver, ExecutionPlan,
    ExecutionRuntime, ExecutionTier, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION,
    NonInteractiveBehavior, OciPolicyEnvelope, OciPolicyMode, Platform, Provisioning,
    ProvisioningNetwork, Reproducibility, Runtime, RuntimeFilesystemPolicy, RuntimeNetworkPolicy,
    RuntimePolicy, RuntimeSecretsPolicy, SecretDelivery, TargetRef,
};
use crate::foundation::types::oci::OciImageResolution;
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

    let runtime_driver = runtime_driver_from_manifest(&named_target.runtime);
    let driver = resolve_driver(
        runtime,
        named_target.driver.as_deref().or(runtime_driver.as_deref()),
        named_target.language.as_deref(),
        &named_target.cmd,
    )?;
    let tier = derive_tier(runtime, driver)?;

    let scoped_id = loaded.model.name.clone();
    let version = loaded.model.version.clone();

    let egress_allow = loaded
        .model
        .network
        .as_ref()
        .map(|network| network.egress_allow.clone())
        .unwrap_or_default();

    let runtime_section = build_runtime_section(
        runtime,
        driver,
        egress_allow.clone(),
        named_target.entrypoint.clone(),
        named_target.cmd.clone(),
        decision.plan.execution_port(),
        &LockCompilerOverlay::default(),
    )?;
    let provisioning = build_provisioning(runtime, driver, &runtime_section.policy, tier);
    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    let oci_policy = if matches!(runtime, ExecutionRuntime::Oci) {
        Some(build_oci_policy_envelope(
            named_target.image.as_deref().unwrap_or(""),
            decision.plan.execution_port(),
            egress_allow,
            None,
        ))
    } else {
        None
    };

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
        oci: oci_policy,
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

    let driver = resolve_driver(
        runtime,
        selected.runtime.driver.as_deref(),
        None,
        &selected.runtime.cmd,
    )?;
    let tier = derive_tier(runtime, driver)?;
    let egress_allow = resolved
        .network
        .as_ref()
        .map(|network| network.egress_allow.clone())
        .unwrap_or_default();
    let runtime_section = build_runtime_section(
        runtime,
        driver,
        egress_allow.clone(),
        selected.runtime.entrypoint.clone(),
        selected.runtime.cmd.clone(),
        selected.runtime.port,
        overlay,
    )?;
    let provisioning = build_provisioning(runtime, driver, &runtime_section.policy, tier);
    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    let oci_policy = if matches!(runtime, ExecutionRuntime::Oci) {
        let declared_ref = selected.runtime.image.as_deref().unwrap_or("");
        Some(build_oci_policy_envelope(
            declared_ref,
            selected.runtime.port,
            egress_allow,
            selected.oci_image.clone(),
        ))
    } else {
        None
    };

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
        oci: oci_policy,
    })
}

/// Compile an OCI `ExecutionPlan` directly from an already-resolved image digest,
/// without consulting the lock runtime model.
///
/// Pure-OCI service capsules (`[targets.app] runtime="oci"`) have no source
/// `contract.process`, so `resolve_lock_runtime_model` (gated by
/// `ensure_execution_ready`) rejects them. They do not need a source runtime
/// model: the resolved image digest, target label, and optional port are
/// everything an OCI launch requires, and they are already available at run
/// time (digest from `resolution.oci_images`, metadata from the manifest).
///
/// This keeps the source-native path (`compile_execution_plan_from_lock`)
/// untouched while still producing a real launch receipt + execution_id, so
/// honest-readiness (ato#608/#609) can report `ready` for OCI targets.
///
/// NOTE: `egress_allow` is currently passed empty by the only caller because
/// PodmanProvider cannot enforce egress allowlists anyway (the strict gate
/// would only have refused to launch, never enforced). Sourcing manifest
/// `network.egress_allow` into this plan can be a follow-up.
#[allow(clippy::too_many_arguments)]
pub fn compile_oci_execution_plan_from_resolution(
    scoped_id: String,
    version: String,
    target_label: String,
    declared_image_ref: &str,
    port: Option<u16>,
    egress_allow: Vec<String>,
    resolved_image: Option<OciImageResolution>,
    platform: &PlatformSnapshot,
) -> Result<ExecutionPlan, AtoExecutionError> {
    let runtime = ExecutionRuntime::Oci;
    let driver = resolve_driver(runtime, None, None, &[])?;
    let tier = derive_tier(runtime, driver)?;

    let runtime_section = build_runtime_section(
        runtime,
        driver,
        egress_allow.clone(),
        String::new(),
        Vec::new(),
        port,
        &LockCompilerOverlay::default(),
    )?;
    let provisioning = build_provisioning(runtime, driver, &runtime_section.policy, tier);
    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    let oci_policy =
        build_oci_policy_envelope(declared_image_ref, port, egress_allow, resolved_image);

    Ok(ExecutionPlan {
        schema_version: EXECUTION_PLAN_SCHEMA_VERSION.to_string(),
        capsule: CapsuleRef {
            scoped_id: scoped_id.clone(),
            version: version.clone(),
        },
        target: TargetRef {
            label: target_label.clone(),
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
                target_label,
            },
            policy_segment_hash,
            provisioning_policy_hash,
            mount_set_algo_id: MOUNT_SET_ALGO_ID.to_string(),
            mount_set_algo_version: MOUNT_SET_ALGO_VERSION,
        },
        reproducibility: Reproducibility {
            platform: platform_from_snapshot(platform),
        },
        oci: Some(oci_policy),
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
    ) && let Some(port) = port
    {
        allow_hosts.push(format!("127.0.0.1:{port}"));
        allow_hosts.push(format!("localhost:{port}"));
        allow_hosts.push(format!("0.0.0.0:{port}"));
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
            // OCI runtime always uses the Oci driver; explicit driver override is not supported.
            return Ok(ExecutionDriver::Oci);
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

fn runtime_driver_from_manifest(runtime: &str) -> Option<String> {
    runtime
        .trim()
        .to_ascii_lowercase()
        .split_once('/')
        .and_then(|(_, driver)| (!driver.trim().is_empty()).then(|| driver.trim().to_string()))
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
        (ExecutionRuntime::Oci, ExecutionDriver::Oci) => Ok(ExecutionTier::Tier3),
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

fn build_oci_policy_envelope(
    declared_image_ref: &str,
    port_exposure: Option<u16>,
    egress_allow: Vec<String>,
    resolved_image: Option<OciImageResolution>,
) -> OciPolicyEnvelope {
    OciPolicyEnvelope {
        declared_image_ref: declared_image_ref.to_string(),
        resolved_image,
        port_exposure,
        egress_allow,
        policy_mode: OciPolicyMode::Strict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ato_lock::AtoLock;
    use crate::lock_runtime::{LockCompilerOverlay, resolve_lock_runtime_model};
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
    fn compile_accepts_oci_runtime_into_execution_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "oci-app"
version = "1.0.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080"#,
        )
        .expect("write manifest");

        let plan = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, Some("main"))
            .expect("should compile OCI plan");
        assert!(matches!(
            plan.execution_plan.target.runtime,
            ExecutionRuntime::Oci
        ));
        assert!(matches!(
            plan.execution_plan.target.driver,
            ExecutionDriver::Oci
        ));
        assert!(matches!(plan.tier, ExecutionTier::Tier3));
        let oci = plan
            .execution_plan
            .oci
            .expect("oci policy envelope must be present");
        assert_eq!(oci.declared_image_ref, "ghcr.io/example/app:latest");
        assert_eq!(oci.port_exposure, Some(8080));
        assert!(matches!(oci.policy_mode, OciPolicyMode::Strict));
        assert!(
            oci.resolved_image.is_none(),
            "no lock provided so resolved_image must be absent"
        );
    }

    #[test]
    fn oci_execution_plan_has_policy_envelope_with_declared_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "oci-app"
version = "1.0.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "oci"
image = "docker.io/library/nginx:1.25"
port = 80"#,
        )
        .expect("write manifest");

        let plan = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, Some("main"))
            .expect("should compile OCI plan");
        let oci = plan.execution_plan.oci.expect("policy envelope");
        assert_eq!(oci.declared_image_ref, "docker.io/library/nginx:1.25");
        assert_eq!(oci.port_exposure, Some(80));
    }

    #[test]
    fn oci_policy_mode_defaults_to_strict() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "oci-app"
version = "1.0.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "oci"
image = "docker.io/library/redis:7"
"#,
        )
        .expect("write manifest");

        let plan = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, Some("main"))
            .expect("OCI plan");
        let oci = plan.execution_plan.oci.expect("policy envelope");
        assert!(matches!(oci.policy_mode, OciPolicyMode::Strict));
    }

    #[test]
    fn derive_tier_oci_is_tier3() {
        let tier = derive_tier(ExecutionRuntime::Oci, ExecutionDriver::Oci).expect("tier");
        assert!(matches!(tier, ExecutionTier::Tier3));
    }

    #[test]
    fn compile_oci_from_resolution_yields_receipt_inputs() {
        use crate::foundation::types::oci::{OciImageResolution, OciPlatform};

        let resolved_image = OciImageResolution {
            declared_ref: "ghcr.io/go-gitea/gitea:latest".to_string(),
            resolved_digest: "sha256:7bae791181c2".to_string(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "arm64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        };

        let plan = compile_oci_execution_plan_from_resolution(
            "gitea".to_string(),
            "1.0.0".to_string(),
            "app".to_string(),
            "ghcr.io/go-gitea/gitea:latest",
            Some(3000),
            Vec::new(),
            Some(resolved_image),
            &PlatformSnapshot {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                libc: "unknown".to_string(),
            },
        )
        .expect("oci plan");

        let oci = plan.oci.as_ref().expect("oci envelope present");
        let resolved = oci.resolved_image.as_ref().expect("resolved image present");
        assert_eq!(resolved.resolved_digest, "sha256:7bae791181c2");
        assert_eq!(oci.port_exposure, Some(3000));
        assert_eq!(plan.target.label, "app");
        assert!(matches!(plan.target.runtime, ExecutionRuntime::Oci));
        assert!(matches!(plan.target.driver, ExecutionDriver::Oci));
        assert_eq!(plan.capsule.scoped_id, "gitea");
        assert_eq!(plan.consent.key.scoped_id, "gitea");
        assert_eq!(plan.consent.key.version, "1.0.0");
        assert_eq!(plan.consent.key.target_label, "app");
        assert!(!plan.consent.policy_segment_hash.is_empty());
    }

    #[test]
    fn non_oci_plan_has_no_oci_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-app"
version = "1.0.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "web/node"
run = "server.js"
port = 3000"#,
        )
        .expect("write manifest");

        let plan = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, Some("main"))
            .expect("node plan");
        assert!(
            plan.execution_plan.oci.is_none(),
            "non-OCI plan must have no OCI envelope"
        );
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
