use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    ExecutionIdentityInput, ExecutionReceipt, LaunchIdentity, PolicyIdentity, SourceIdentity,
    Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;

use crate::application::build_materialization::BuildObservation;
use crate::executors::launch_context::RuntimeLaunchContext;

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
