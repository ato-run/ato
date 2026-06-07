//! Runtime observation v1 (#490): build the observed launch envelope from the
//! realized launch facts after a session's workload has spawned and reached
//! readiness.
//!
//! This is the ato-cli side of the capsule-core observation model
//! ([`capsule_core::execution_identity::ObservedLaunchEnvelope`]). It collects
//! only host-independent, leak-free facts (see the capsule-core module docs):
//! logical runtime kind/identity, the post-profile logical entrypoint, a
//! workspace-relative working directory, observed env **keys**, and in-guest
//! mount **targets**. The bound port / local URL are carried as diagnostic-only
//! evidence and never feed `observed_execution_id`.
//!
//! The `resolved_execution_id` anchor is intentionally left `None` here — it is
//! filled in by `execution_receipts::mark_v2_receipt_observed` from the receipt
//! being stamped, so the anchor always matches that receipt's resolved id.

use std::path::Path;

use capsule_core::execution_identity::{ObservedLaunchEnvelope, ObservedRuntimeEvidence};
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;

use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;

/// Build the observed runtime evidence for a freshly-spawned, ready session.
///
/// Call only after the workload has actually spawned and passed its readiness
/// gate — a failed/aborted launch must not produce evidence (the caller skips
/// stamping in that case).
pub(crate) fn build_observed_runtime_evidence(
    plan: &ManifestData,
    launch: &LaunchSpec,
    launch_ctx: &RuntimeLaunchContext,
    execution_cwd: Option<&Path>,
    bound_port: Option<u16>,
) -> ObservedRuntimeEvidence {
    let envelope = ObservedLaunchEnvelope {
        // Anchored by mark_v2_receipt_observed from the receipt itself.
        resolved_execution_id: None,
        runtime_kind: observed_runtime_kind(plan),
        runtime_identity: observed_runtime_identity(plan),
        entrypoint: observed_entrypoint(launch),
        // Derived ONLY from the executor's *realized* cwd
        // (`CapsuleProcess.execution_cwd`): the host source executor reports
        // `resolve_host_execution_cwd(...)` and the shell executor reports its
        // `working_dir`. Executors that do not report one (node/deno/static, which
        // run in an internal `runtime_dir`) yield `None` here — the cwd is omitted
        // from the identity rather than estimated from the caller's ambient cwd,
        // so `observed_execution_id` never commits to a cwd we did not observe.
        working_directory: observed_working_directory(
            execution_cwd,
            launch_ctx.workspace_root().map(|p| p.as_path()),
        ),
        env_keys: launch_ctx.env_permission_keys(),
        mount_targets: observed_mount_targets(launch_ctx),
        provider_projection_digest: None,
    };
    let local_url = bound_port.map(|port| format!("http://127.0.0.1:{port}/"));
    ObservedRuntimeEvidence::new(envelope)
        .with_bound_port(bound_port)
        .with_local_url(local_url)
}

/// Logical runtime kind/provider, e.g. `"source/node"` (never a host path).
fn observed_runtime_kind(plan: &ManifestData) -> String {
    let runtime = plan.execution_runtime().unwrap_or_default();
    match plan.execution_driver() {
        Some(driver) if !driver.is_empty() && !runtime.is_empty() => {
            format!("{runtime}/{driver}")
        }
        Some(driver) if !driver.is_empty() => driver,
        _ => runtime,
    }
}

/// Declared/version-based logical runtime identity, e.g. `"node 22.14.0"` —
/// derived from the plan's driver + resolved runtime version, never a host path.
/// (v1 does not hash the runtime binary; this is a logical identity, not a
/// content-addressed runtime fingerprint.)
fn observed_runtime_identity(plan: &ManifestData) -> Option<String> {
    let version = plan.execution_runtime_version()?;
    let name = plan
        .execution_driver()
        .filter(|d| !d.is_empty())
        .or_else(|| plan.execution_runtime())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "runtime".to_string());
    Some(format!("{name} {version}"))
}

/// Post-profile logical entrypoint (`command` followed by `args`). This is the
/// manifest/profile command, not the executor's host-path-laden wrapper argv.
fn observed_entrypoint(launch: &LaunchSpec) -> Vec<String> {
    let mut argv = Vec::with_capacity(1 + launch.args.len());
    if !launch.command.is_empty() {
        argv.push(launch.command.clone());
    }
    argv.extend(launch.args.iter().cloned());
    argv
}

/// In-guest filesystem mount targets (never host source paths).
fn observed_mount_targets(launch_ctx: &RuntimeLaunchContext) -> Vec<String> {
    launch_ctx
        .injected_mounts()
        .iter()
        .map(|mount| mount.target.clone())
        .collect()
}

/// Working directory relative to the workspace root, normalized to forward
/// slashes. Returns `None` when there is no workspace root to relativize
/// against (so a raw host path is never recorded), and a redaction marker when
/// the cwd lies outside the workspace.
fn observed_working_directory(cwd: Option<&Path>, workspace_root: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let root = workspace_root?;
    match cwd.strip_prefix(root) {
        Ok(rel) => {
            let rel = rel.to_string_lossy().replace('\\', "/");
            Some(if rel.is_empty() { ".".to_string() } else { rel })
        }
        Err(_) => Some("<outside-workspace>".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn working_directory_is_workspace_relative_forward_slashed() {
        let root = PathBuf::from("/tmp/ato-abc/source");
        assert_eq!(
            observed_working_directory(Some(&root), Some(&root)),
            Some(".".to_string())
        );
        assert_eq!(
            observed_working_directory(Some(&root.join("api/v1")), Some(&root)),
            Some("api/v1".to_string())
        );
    }

    #[test]
    fn working_directory_redacts_outside_workspace_and_omits_without_root() {
        let root = PathBuf::from("/tmp/ato-abc/source");
        // A cwd outside the workspace must not leak the host path.
        assert_eq!(
            observed_working_directory(Some(Path::new("/etc")), Some(&root)),
            Some("<outside-workspace>".to_string())
        );
        // No workspace root → omit rather than record a raw host path.
        assert_eq!(
            observed_working_directory(Some(Path::new("/tmp/whatever")), None),
            None
        );
        assert_eq!(observed_working_directory(None, Some(&root)), None);
    }
}
