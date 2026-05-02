use std::collections::BTreeMap;

use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    DependencyIdentity, EnvironmentIdentity, EnvironmentMode, ExecutionIdentityInput,
    ExecutionReceipt, FilesystemIdentity, LaunchIdentity, PlatformIdentity, PolicyIdentity,
    ReproducibilityCause, ReproducibilityClass, ReproducibilityIdentity, RuntimeIdentity,
    SourceIdentity, Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;
use serde::Serialize;

use crate::application::build_materialization::BuildObservation;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::runtime::overrides as runtime_overrides;

pub(crate) fn build_prelaunch_receipt(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceipt> {
    let launch_spec = derive_launch_spec(plan).with_context(|| {
        format!(
            "failed to derive launch spec for execution receipt: {}",
            plan.manifest_path.display()
        )
    })?;

    let source = SourceIdentity {
        source_ref: Tracked::known(format!("local:{}", plan.manifest_path.display())),
        source_tree_hash: Tracked::unknown(
            "source tree observer not enabled; build input digest is tracked separately",
        ),
    };
    let dependencies = DependencyIdentity {
        derivation_hash: build_observation
            .map(|observation| Tracked::known(observation.input_digest.clone()))
            .unwrap_or_else(|| Tracked::unknown("build materialization observation unavailable")),
        output_hash: Tracked::unknown("dependency output observer not enabled"),
    };
    let runtime = RuntimeIdentity {
        declared: launch_spec
            .runtime
            .clone()
            .or_else(|| launch_spec.driver.clone())
            .or_else(|| launch_spec.language.clone()),
        resolved: launch_spec.runtime.clone(),
        binary_hash: Tracked::unknown("runtime binary observer not enabled"),
        dynamic_linkage: Tracked::untracked("dynamic linkage observer not implemented"),
        platform: PlatformIdentity {
            os: execution_plan.reproducibility.platform.os.clone(),
            arch: execution_plan.reproducibility.platform.arch.clone(),
            libc: execution_plan.reproducibility.platform.libc.clone(),
        },
    };
    let environment = environment_identity(plan, launch_ctx)?;
    let filesystem = filesystem_identity(plan, launch_ctx, &launch_spec)?;
    let policy = PolicyIdentity {
        network_policy_hash: Tracked::known(
            execution_plan.consent.provisioning_policy_hash.clone(),
        ),
        capability_policy_hash: Tracked::known(execution_plan.consent.policy_segment_hash.clone()),
    };
    let launch = LaunchIdentity {
        entry_point: launch_spec.command,
        argv: {
            let mut argv = launch_spec.args;
            argv.extend(launch_ctx.command_args().iter().cloned());
            argv
        },
        working_directory: launch_spec.working_dir.display().to_string(),
    };
    let reproducibility = classify_reproducibility(execution_plan, &dependencies, &runtime);

    Ok(ExecutionReceipt::from_input(
        ExecutionIdentityInput::new(
            source,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            reproducibility,
        ),
        chrono::Utc::now().to_rfc3339(),
    )?)
}

fn environment_identity(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<EnvironmentIdentity> {
    let mut env = BTreeMap::new();
    env.extend(plan.execution_env());
    env.extend(launch_ctx.merged_env());
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        env.insert("PORT".to_string(), port.to_string());
    }

    let mut tracked_keys = Vec::new();
    let mut redacted_keys = Vec::new();
    let mut hashed_values = BTreeMap::new();
    for (key, value) in env {
        if is_sensitive_env_key(&key) {
            redacted_keys.push(key.clone());
        } else {
            tracked_keys.push(key.clone());
        }
        hashed_values.insert(
            key,
            format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex()),
        );
    }
    tracked_keys.sort();
    redacted_keys.sort();

    Ok(EnvironmentIdentity {
        closure_hash: Tracked::known(canonical_hash(&EnvironmentHashInput {
            values: hashed_values,
        })?),
        mode: EnvironmentMode::Closed,
        tracked_keys,
        redacted_keys,
        unknown_keys: Vec::new(),
    })
}

fn filesystem_identity(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    launch_spec: &capsule_core::launch_spec::LaunchSpec,
) -> Result<FilesystemIdentity> {
    let mut writable_dirs = launch_ctx
        .injected_mounts()
        .iter()
        .filter(|mount| !mount.readonly)
        .map(|mount| mount.target.clone())
        .collect::<Vec<_>>();
    writable_dirs.sort();
    writable_dirs.dedup();

    let known_readonly_layers = launch_ctx
        .injected_mounts()
        .iter()
        .filter(|mount| mount.readonly)
        .map(|mount| mount.target.clone())
        .collect::<Vec<_>>();

    let projection_strategy = if launch_ctx.effective_cwd().is_some() {
        "projected-cwd"
    } else {
        "direct"
    }
    .to_string();
    let source_root = plan.workspace_root.display().to_string();
    let working_directory = launch_spec.working_dir.display().to_string();
    let persistent_state = Vec::<String>::new();

    Ok(FilesystemIdentity {
        view_hash: Tracked::known(canonical_hash(&FilesystemHashInput {
            source_root: &source_root,
            working_directory: &working_directory,
            projection_strategy: projection_strategy.as_str(),
            writable_dirs: &writable_dirs,
            persistent_state: &persistent_state,
            known_readonly_layers: &known_readonly_layers,
        })?),
        projection_strategy,
        writable_dirs,
        persistent_state,
        known_readonly_layers,
    })
}

fn classify_reproducibility(
    execution_plan: &ExecutionPlan,
    dependencies: &DependencyIdentity,
    runtime: &RuntimeIdentity,
) -> ReproducibilityIdentity {
    let mut causes = Vec::new();
    if !execution_plan.runtime.policy.network.allow_hosts.is_empty() {
        causes.push(ReproducibilityCause::NetworkBound);
    }
    if dependencies.output_hash.status != capsule_core::execution_identity::TrackingStatus::Known {
        causes.push(ReproducibilityCause::UnknownDependencyOutput);
    }
    if runtime.binary_hash.status != capsule_core::execution_identity::TrackingStatus::Known {
        causes.push(ReproducibilityCause::UnknownRuntimeIdentity);
    }
    if runtime.dynamic_linkage.status == capsule_core::execution_identity::TrackingStatus::Untracked
    {
        causes.push(ReproducibilityCause::HostBound);
    }
    causes.sort();
    causes.dedup();

    let class = if causes.is_empty() {
        ReproducibilityClass::Pure
    } else if causes.iter().any(|cause| {
        matches!(
            cause,
            ReproducibilityCause::UnknownDependencyOutput
                | ReproducibilityCause::UnknownRuntimeIdentity
                | ReproducibilityCause::UntrackedEnvironment
                | ReproducibilityCause::UntrackedFilesystemView
                | ReproducibilityCause::LifecycleUnknown
        )
    }) {
        ReproducibilityClass::BestEffort
    } else {
        ReproducibilityClass::Bounded
    };

    ReproducibilityIdentity { class, causes }
}

#[derive(Serialize)]
struct EnvironmentHashInput {
    values: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct FilesystemHashInput<'a> {
    source_root: &'a str,
    working_directory: &'a str,
    projection_strategy: &'a str,
    writable_dirs: &'a [String],
    persistent_state: &'a [String],
    known_readonly_layers: &'a [String],
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String> {
    let canonical =
        serde_jcs::to_vec(value).context("failed to canonicalize execution receipt observation")?;
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.contains("PRIVATE_KEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_env_keys_are_redacted_by_name() {
        assert!(is_sensitive_env_key("OPENAI_API_KEY"));
        assert!(is_sensitive_env_key("github_token"));
        assert!(!is_sensitive_env_key("PATH"));
    }

    #[test]
    fn canonical_hash_is_stable_for_sorted_maps() {
        let mut left = BTreeMap::new();
        left.insert("B".to_string(), "2".to_string());
        left.insert("A".to_string(), "1".to_string());
        let mut right = BTreeMap::new();
        right.insert("A".to_string(), "1".to_string());
        right.insert("B".to_string(), "2".to_string());

        let left_hash = canonical_hash(&EnvironmentHashInput { values: left }).expect("left hash");
        let right_hash =
            canonical_hash(&EnvironmentHashInput { values: right }).expect("right hash");
        assert_eq!(left_hash, right_hash);
    }
}
