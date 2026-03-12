use std::path::Path;

use crate::execution_plan::canonical::{
    canonicalize_policy_paths, compute_policy_segment_hash, compute_provisioning_policy_hash,
    normalize_unordered_set,
};
use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{
    CapsuleRef, Consent, ConsentKey, ExecutionDriver, ExecutionPlan, ExecutionRuntime,
    ExecutionTier, NonInteractiveBehavior, Platform, Provisioning, ProvisioningNetwork,
    Reproducibility, Runtime, RuntimeFilesystemPolicy, RuntimeNetworkPolicy, RuntimePolicy,
    RuntimeSecretsPolicy, SecretDelivery, TargetRef, EXECUTION_PLAN_SCHEMA_VERSION,
    MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION,
};
use crate::manifest;
use crate::router::{self, ExecutionProfile, RuntimeDecision};

#[derive(Debug, Clone)]
pub struct CompiledExecutionPlan {
    pub execution_plan: ExecutionPlan,
    pub runtime_decision: RuntimeDecision,
    pub tier: ExecutionTier,
}

pub fn compile_execution_plan(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
) -> Result<CompiledExecutionPlan, AtoExecutionError> {
    let loaded = manifest::load_manifest(manifest_path).map_err(|err| {
        AtoExecutionError::policy_violation(format!("failed to load manifest: {err}"))
    })?;

    let decision = router::route_manifest(manifest_path, profile, target_label).map_err(|err| {
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

    let mut allow_hosts = loaded
        .model
        .network
        .as_ref()
        .map(|network| network.egress_allow.clone())
        .unwrap_or_default();

    if matches!(runtime, ExecutionRuntime::Web) {
        let port = decision.plan.execution_port().ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "targets.{}.port is required for runtime=web",
                selected_target_label
            ))
        })?;
        allow_hosts.push(format!("127.0.0.1:{port}"));
        allow_hosts.push(format!("localhost:{port}"));
        allow_hosts.push(format!("0.0.0.0:{port}"));
    } else if matches!(
        (runtime, driver),
        (ExecutionRuntime::Source, ExecutionDriver::Deno)
    ) {
        if let Some(port) = decision.plan.execution_port() {
            allow_hosts.push(format!("127.0.0.1:{port}"));
            allow_hosts.push(format!("localhost:{port}"));
            allow_hosts.push(format!("0.0.0.0:{port}"));
        }
    }
    allow_hosts = normalize_unordered_set(&allow_hosts);

    let read_only_raw = if matches!(
        (runtime, driver),
        (ExecutionRuntime::Web, ExecutionDriver::Static)
    ) {
        vec![named_target.entrypoint.clone()]
    } else {
        Vec::new()
    };
    let read_only = canonicalize_policy_paths(&decision.plan.manifest_dir, &read_only_raw)?;

    let runtime_policy = RuntimePolicy {
        network: RuntimeNetworkPolicy {
            allow_hosts: allow_hosts.clone(),
        },
        filesystem: RuntimeFilesystemPolicy {
            read_only,
            read_write: Vec::new(),
        },
        secrets: RuntimeSecretsPolicy {
            allow_secret_ids: Vec::new(),
            delivery: SecretDelivery::Fd,
        },
        args: named_target.cmd.clone(),
    };

    let runtime_section = Runtime {
        policy: runtime_policy,
        fail_closed: true,
        non_interactive_behavior: NonInteractiveBehavior::DenyIfUnconsented,
    };

    let provisioning = Provisioning {
        network: ProvisioningNetwork {
            allow_registry_hosts: allow_hosts.clone(),
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
        allowed_registries: allow_hosts,
    };

    let policy_segment_hash =
        compute_policy_segment_hash(&runtime_section, MOUNT_SET_ALGO_ID, MOUNT_SET_ALGO_VERSION)?;
    let provisioning_policy_hash = compute_provisioning_policy_hash(&provisioning)?;

    let scoped_id = loaded.model.name.clone();
    let version = loaded.model.version.clone();

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
            platform: Platform {
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                libc: detect_libc().to_string(),
            },
        },
    };

    Ok(CompiledExecutionPlan {
        execution_plan,
        runtime_decision: decision,
        tier,
    })
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
    use std::fs;

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
schema_version = "0.2"
name = "oci-app"
version = "1.0.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "oci"
entrypoint = "ghcr.io/example/app:latest"
"#,
        )
        .expect("write manifest");

        let err = compile_execution_plan(&manifest_path, ExecutionProfile::Dev, None).unwrap_err();
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }
}
