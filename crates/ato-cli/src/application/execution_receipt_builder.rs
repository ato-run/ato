use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    ExecutionIdentityInput, ExecutionIdentityInputV2, ExecutionReceipt, ExecutionReceiptDocument,
    ExecutionReceiptV2, LaunchIdentity, PolicyIdentity, Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;
use serde::Serialize;

use crate::application::build_materialization::BuildObservation;
use crate::application::execution_observers_v2::{
    build_local_locator, build_policy_identity_v2, observe_dependencies_v2, observe_environment_v2,
    observe_filesystem_v2, observe_launch_v2, observe_runtime_v2, observe_source_provenance,
    observe_source_v2, ObserverContextV2,
};
use crate::executors::launch_context::RuntimeLaunchContext;

/// Receipt schema selector. Stable default remains v1 until step 17 of the
/// portability v2 implementation sequence completes (all v2 acceptance tests
/// pass and the default is flipped). Selectable via `ATO_RECEIPT_SCHEMA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptSchemaSelector {
    V1,
    V2Experimental,
}

impl ReceiptSchemaSelector {
    pub(crate) fn from_env() -> Self {
        match std::env::var("ATO_RECEIPT_SCHEMA").as_deref() {
            Ok("v2") | Ok("v2-experimental") => Self::V2Experimental,
            _ => Self::V1,
        }
    }
}

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

    let source = crate::application::execution_observers::observe_source(plan, &launch_spec)?;
    let dependencies = crate::application::execution_observers::observe_dependencies(
        &launch_spec,
        launch_ctx,
        build_observation,
    )?;
    let runtime =
        crate::application::execution_observers::observe_runtime(execution_plan, &launch_spec)?;
    let environment =
        crate::application::execution_observers::observe_environment(plan, launch_ctx)?;
    let filesystem = crate::application::execution_observers::observe_filesystem(
        plan,
        launch_ctx,
        &launch_spec,
    )?;
    let policy = PolicyIdentity {
        network_policy_hash: Tracked::known(
            execution_plan.consent.provisioning_policy_hash.clone(),
        ),
        capability_policy_hash: Tracked::known(execution_plan.consent.policy_segment_hash.clone()),
        sandbox_policy_hash: Tracked::known(sandbox_policy_hash(execution_plan)?),
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
    let reproducibility = crate::application::execution_reproducibility::classify_execution(
        execution_plan,
        &dependencies,
        &runtime,
        &environment,
        &filesystem,
    );

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

// TODO(v2-policy): expand to cover sandbox backend ID (landlock+bwrap / seatbelt /
// none), strength tier, platform-specific enforcement mode, and known gaps per
// RFC §3.6 / plan §"Policy identity v2". Current inputs only describe consent
// algo and launch target, not the actual sandbox backend. Tracked for step 12
// of the portability v2 implementation sequence.
#[derive(Serialize)]
struct SandboxPolicyHashInput<'a> {
    target_runtime: &'a str,
    target_driver: &'a str,
    fail_closed: bool,
    mount_set_algo_id: &'a str,
    mount_set_algo_version: u32,
}

fn sandbox_policy_hash(execution_plan: &ExecutionPlan) -> Result<String> {
    let input = SandboxPolicyHashInput {
        target_runtime: execution_plan.target.runtime.as_str(),
        target_driver: execution_plan.target.driver.as_str(),
        fail_closed: execution_plan.runtime.fail_closed,
        mount_set_algo_id: execution_plan.consent.mount_set_algo_id.as_str(),
        mount_set_algo_version: execution_plan.consent.mount_set_algo_version,
    };
    let canonical =
        serde_jcs::to_vec(&input).context("failed to canonicalize sandbox policy identity")?;
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

/// Build a v2 (experimental) execution receipt. Wraps the v2 observer pipeline
/// so the receipt builder is the single composition site.
pub(crate) fn build_prelaunch_receipt_v2(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceiptV2> {
    let launch_spec = derive_launch_spec(plan).with_context(|| {
        format!(
            "failed to derive launch spec for v2 execution receipt: {}",
            plan.manifest_path.display()
        )
    })?;

    let ctx = ObserverContextV2::for_plan(plan);
    let source = observe_source_v2(plan, &ctx)?;
    let provenance = observe_source_provenance(plan);
    let runtime = observe_runtime_v2(execution_plan, &launch_spec, &ctx)?;
    let dependencies =
        observe_dependencies_v2(plan, &launch_spec, launch_ctx, build_observation, &runtime)?;
    let environment = observe_environment_v2(plan, launch_ctx, &ctx)?;
    let filesystem = observe_filesystem_v2(plan, launch_ctx, &launch_spec, &ctx)?;
    let policy = build_policy_identity_v2(execution_plan);
    let launch = observe_launch_v2(&launch_spec, launch_ctx, &runtime, &ctx)?;
    let local = build_local_locator(plan, &launch_spec, launch_ctx, &runtime);

    // For classification, derive v1-compatible Tracked fields from the v2
    // observations and reuse the existing classifier so v1 and v2 receipts
    // share the same reproducibility verdict for the same launch envelope.
    let class_inputs =
        classification_inputs_from_v2(&dependencies, &runtime, &environment, &filesystem);
    let reproducibility = crate::application::execution_reproducibility::classify_execution(
        execution_plan,
        &class_inputs.dependencies,
        &class_inputs.runtime,
        &class_inputs.environment,
        &class_inputs.filesystem,
    );

    Ok(ExecutionReceiptV2::from_input(
        ExecutionIdentityInputV2::new(
            source,
            provenance,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            local,
            reproducibility,
        ),
        chrono::Utc::now().to_rfc3339(),
    )?)
}

pub(crate) fn build_prelaunch_receipt_document(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceiptDocument> {
    match ReceiptSchemaSelector::from_env() {
        ReceiptSchemaSelector::V1 => {
            let receipt =
                build_prelaunch_receipt(plan, execution_plan, launch_ctx, build_observation)?;
            Ok(ExecutionReceiptDocument::V1(receipt))
        }
        ReceiptSchemaSelector::V2Experimental => {
            let receipt =
                build_prelaunch_receipt_v2(plan, execution_plan, launch_ctx, build_observation)?;
            Ok(ExecutionReceiptDocument::V2(receipt))
        }
    }
}

struct ClassificationInputsV2 {
    dependencies: capsule_core::execution_identity::DependencyIdentity,
    runtime: capsule_core::execution_identity::RuntimeIdentity,
    environment: capsule_core::execution_identity::EnvironmentIdentity,
    filesystem: capsule_core::execution_identity::FilesystemIdentity,
}

fn classification_inputs_from_v2(
    dependencies: &capsule_core::execution_identity::DependencyIdentityV2,
    runtime: &capsule_core::execution_identity::RuntimeIdentityV2,
    environment: &capsule_core::execution_identity::EnvironmentIdentityV2,
    filesystem: &capsule_core::execution_identity::FilesystemIdentityV2,
) -> ClassificationInputsV2 {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, FilesystemIdentity, RuntimeIdentity,
        TrackingStatus,
    };

    let env_closure_status = if environment.entries.iter().all(|entry| {
        matches!(
            entry.normalization,
            capsule_core::execution_identity::ValueNormalizationStatus::Normalized
                | capsule_core::execution_identity::ValueNormalizationStatus::NoHostPath
        )
    }) && !environment.entries.is_empty()
    {
        TrackingStatus::Known
    } else {
        TrackingStatus::Untracked
    };

    let mut tracked_keys: Vec<String> = environment
        .entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    tracked_keys.sort();
    let mut unknown_keys = environment.ambient_untracked_keys.clone();
    if matches!(
        environment.fd_layout.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("fd-layout".to_string());
    }
    if matches!(
        environment.umask.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("umask".to_string());
    }
    if matches!(
        environment.ulimits.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("ulimits".to_string());
    }
    if !environment.entries.iter().any(|entry| entry.key == "TZ") {
        unknown_keys.push("timezone".to_string());
    }
    unknown_keys.sort();
    unknown_keys.dedup();

    let env_closure_value = format!(
        "blake3:{}",
        blake3::hash(
            serde_jcs::to_vec(&environment.entries)
                .unwrap_or_default()
                .as_slice()
        )
        .to_hex()
    );

    let env_v1 = EnvironmentIdentity {
        closure_hash: match env_closure_status {
            TrackingStatus::Known => Tracked::known(env_closure_value),
            _ => Tracked::untracked(
                "v2 environment closure has unnormalized or untracked identity-relevant entries",
            ),
        },
        mode: environment.mode,
        tracked_keys,
        redacted_keys: Vec::new(),
        unknown_keys,
    };

    let persistent_state_v1: Vec<String> = filesystem
        .persistent_state
        .iter()
        .map(|binding| {
            format!(
                "{}={}",
                binding.name,
                binding.identity.value.as_deref().unwrap_or("")
            )
        })
        .collect();

    let writable_dirs_v1: Vec<String> = filesystem
        .writable_dirs
        .iter()
        .map(|writable| writable.role.clone())
        .collect();

    let readonly_layers_v1: Vec<String> = filesystem
        .readonly_layers
        .iter()
        .map(|layer| layer.role.clone())
        .collect();

    let fs_v1 = FilesystemIdentity {
        view_hash: filesystem.view_hash.clone(),
        projection_strategy: "v2-canonical".to_string(),
        writable_dirs: writable_dirs_v1,
        persistent_state: persistent_state_v1,
        known_readonly_layers: readonly_layers_v1,
    };

    let runtime_v1 = RuntimeIdentity {
        declared: runtime.declared.clone(),
        resolved: runtime.resolved_ref.value.clone(),
        binary_hash: runtime.binary_hash.clone(),
        dynamic_linkage: runtime.dynamic_linkage.clone(),
        platform: runtime.platform.clone(),
    };

    let deps_v1 = DependencyIdentity {
        derivation_hash: dependencies.derivation_hash.clone(),
        output_hash: dependencies.output_hash.clone(),
    };

    ClassificationInputsV2 {
        dependencies: deps_v1,
        runtime: runtime_v1,
        environment: env_v1,
        filesystem: fs_v1,
    }
}
