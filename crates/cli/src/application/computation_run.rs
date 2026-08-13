//! CLI composition root for the addressable-computation repository path.

use std::sync::Arc;

use anyhow::{Context, Result};
use ato_adapter_repository::{RepositoryOptions, compile_repository};
use ato_kernel::{Action, Kernel, Run};
use ato_objects::{MemoryObjectStore, ObjectResolver};
use ato_semantics_workspace::{WorkspaceSemantics, decode_workspace_residual, observe_exit};
use nacelle::workspace_provider::NacelleWorkspaceProvider;

use crate::cli::commands::run::RunArgs;

pub(crate) fn supports(args: &RunArgs) -> bool {
    args.target.is_dir()
        && !args.watch
        && !args.background
        && !args.preview_mode
        && !args.plan_only
        && args.registry.is_none()
        && args.state_bindings.is_empty()
        && args.inject_bindings.is_empty()
        && args.managed_state_root.is_none()
        && args.install_lifecycle_context.is_none()
        && args.capsule_launch_inputs.is_empty()
        && args.pinned_revision_output_dir.is_none()
        && args.export_request.is_none()
        && args.read_grants.is_empty()
        && args.write_grants.is_empty()
        && args.read_write_grants.is_empty()
}

pub(crate) fn execute(args: &RunArgs) -> Result<()> {
    let objects = Arc::new(MemoryObjectStore::default());
    let options = RepositoryOptions {
        arguments: args.args.clone(),
        sandbox_required: args.sandbox_mode && !args.dangerously_skip_permissions,
        ..RepositoryOptions::default()
    };
    let compiled = compile_repository(&args.target, objects.as_ref(), options)
        .context("failed to compile repository into ato.workspace@1")?;
    let provider = Arc::new(NacelleWorkspaceProvider::default());
    provider
        .bind_source(compiled.source, &compiled.repository_root)
        .context("failed to bind repository source to Nacelle provider")?;

    let mut kernel = Kernel::<()>::new(objects.clone());
    kernel
        .register(Arc::new(WorkspaceSemantics::new(provider)))
        .context("failed to register workspace semantics")?;
    let computation = kernel
        .seal(&compiled.computation)
        .context("failed to seal repository computation")?;
    let mut run = Run { head: computation };
    kernel
        .step(&mut run, &Action::Tau)
        .context("workspace computation failed")?;

    let resolved = kernel.resolve(&run.head)?;
    let metadata = objects.metadata(&resolved.object().residual)?;
    let bytes = ato_objects::read_exact_object(
        objects.as_ref(),
        &resolved.object().residual,
        metadata.size,
        ato_semantics_workspace::MAX_WORKSPACE_RESIDUAL_BYTES,
    )?;
    let residual = decode_workspace_residual(&bytes)?;
    let exit = observe_exit(&residual).context("workspace did not reach an observable exit")?;
    if exit != 0 {
        anyhow::bail!("workspace exited with status {exit}");
    }
    Ok(())
}
