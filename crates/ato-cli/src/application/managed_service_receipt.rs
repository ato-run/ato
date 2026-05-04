//! Phase Z: synthesize a v2 execution receipt for `ato app bootstrap`-style
//! managed services so they participate in the same identity journal as
//! `ato run` / `ato app session start` user-launched capsules.
//!
//! Managed services have a different shape than capsule launches:
//!
//! - There is no `capsule.toml` — the entry point is a `run.sh` helper
//!   inside the materialized service directory.
//! - There is typically no Hourglass pipeline; ato-cli's
//!   `ManagedServiceRuntime::start_service` shells out directly via
//!   `Command::new(run.sh).spawn()`.
//! - The launch envelope is much smaller (just the helper, working dir,
//!   declared dependencies, and the inherited shell env).
//!
//! We still want each managed-service spawn to produce a receipt because:
//!
//! 1. Identity audit: "what helper bytes did the system actually run when
//!    ato bootstrapped my desktop?" should be answerable after the fact.
//! 2. Drift detection: a service-record hash change means the helper
//!    body changed, which is the natural signal for "re-orchestration
//!    required".
//! 3. Symmetry with `ato run`: same `~/.ato/executions/<id>/` journal
//!    location, same `inspect` UI, same v2 schema.
//!
//! The synthesis fills in only what the service record exposes and uses
//! `Untracked` / `NotApplicable` for everything else; the resulting
//! receipt classifies as `BestEffort` because dynamic linkage, env
//! closure, and filesystem semantics are not observable from a managed
//! service spawn site without re-implementing observation.

use std::path::Path;

use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    DependencyIdentityV2, EnvironmentIdentityV2, EnvironmentMode, ExecutionIdentityInputV2,
    ExecutionReceiptDocument, ExecutionReceiptV2, FilesystemIdentityV2, FilesystemSemantics,
    LaunchArg, LaunchEntryPoint, LaunchIdentityV2, PlatformIdentity, PolicyIdentityV2,
    ReproducibilityCause, ReproducibilityClass, ReproducibilityIdentity, RuntimeCompleteness,
    RuntimeIdentityV2, SourceIdentityV2, SourceProvenance, SourceProvenanceKind, Tracked,
};

use crate::application::execution_observers::hash_source_tree;

/// Minimum data required to synthesize a managed-service receipt. Field
/// names mirror `app_control::MaterializedServiceRecord` so the caller
/// passes through the record almost verbatim.
pub(crate) struct ManagedServiceReceiptInput<'a> {
    pub(crate) name: &'a str,
    pub(crate) service_dir: &'a Path,
    pub(crate) helper_path: &'a Path,
    /// Reserved for future inclusion in dependency identity (managed services
    /// today do not declare materialized dep blobs, but the dependency graph
    /// is still meaningful for inspect/replay).
    #[allow(dead_code)]
    pub(crate) depends_on: &'a [String],
    pub(crate) lifecycle: &'a str,
    pub(crate) source_label: &'a str,
}

/// Build an `ExecutionReceiptDocument::V2` describing a managed-service
/// spawn. Returns the document plus the resulting `execution_id` so the
/// caller can attach it to the service record without re-parsing.
pub(crate) fn synthesize_managed_service_receipt(
    input: &ManagedServiceReceiptInput<'_>,
) -> Result<(ExecutionReceiptDocument, String)> {
    let source = SourceIdentityV2 {
        source_tree_hash: source_tree_for_service(input)?,
        manifest_path_role: Tracked::not_applicable(),
    };
    let provenance = SourceProvenance {
        kind: SourceProvenanceKind::Local,
        git_remote: None,
        git_commit: None,
        registry_ref: Some(format!(
            "managed-service:{label}/{name}",
            label = input.source_label,
            name = input.name
        )),
    };

    let dependencies = DependencyIdentityV2 {
        derivation_hash: Tracked::not_applicable(),
        output_hash: Tracked::not_applicable(),
        derivation_inputs: None,
    };

    let runtime = RuntimeIdentityV2 {
        declared: Some("shell".to_string()),
        resolved_ref: Tracked::known("shell:run.sh".to_string()),
        binary_hash: Tracked::untracked("managed-service helper hash not separately tracked"),
        dynamic_linkage: Tracked::untracked(
            "dynamic linkage observer not implemented for managed services",
        ),
        completeness: RuntimeCompleteness::DeclaredOnly,
        platform: PlatformIdentity {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            libc: detect_libc().to_string(),
        },
    };

    let environment = EnvironmentIdentityV2 {
        entries: Vec::new(),
        fd_layout: Tracked::untracked("managed-service fd layout observer not implemented"),
        umask: Tracked::untracked("managed-service umask observer not implemented"),
        ulimits: Tracked::untracked("managed-service ulimits observer not implemented"),
        mode: EnvironmentMode::Untracked,
        ambient_untracked_keys: Vec::new(),
    };

    let filesystem = FilesystemIdentityV2 {
        view_hash: Tracked::untracked(
            "managed-service filesystem view is the materialized service dir; full mount semantics not yet observed",
        ),
        partial_view_hash: None,
        source_root: Tracked::known("workspace:.".to_string()),
        working_directory: Tracked::known("workspace:.".to_string()),
        readonly_layers: Vec::new(),
        writable_dirs: Vec::new(),
        persistent_state: Vec::new(),
        semantics: FilesystemSemantics {
            case_sensitivity: Tracked::untracked("case sensitivity observer not implemented"),
            symlink_policy: Tracked::untracked("symlink policy observer not implemented"),
            tmp_policy: Tracked::untracked("tmp policy observer not implemented"),
        },
    };

    let policy = PolicyIdentityV2 {
        network_policy_hash: Tracked::untracked(
            "managed-service network policy: inherits desktop allowlist",
        ),
        capability_policy_hash: Tracked::untracked(
            "managed-service capability policy: inherits desktop grants",
        ),
        sandbox_policy_hash: Tracked::known(format!(
            "blake3:managed-service-lifecycle-{}",
            input.lifecycle
        )),
    };

    let helper_role = input
        .helper_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run.sh");
    let launch = LaunchIdentityV2 {
        entry_point: LaunchEntryPoint::WorkspaceRelative {
            path: format!("workspace:{helper_role}"),
        },
        argv: Vec::<LaunchArg>::new(),
        working_directory: Tracked::known("workspace:.".to_string()),
    };

    let causes = vec![
        ReproducibilityCause::UnknownRuntimeIdentity,
        ReproducibilityCause::UntrackedEnvironment,
        ReproducibilityCause::UntrackedFilesystemView,
        ReproducibilityCause::UntrackedDynamicDependency,
    ];
    let reproducibility = ReproducibilityIdentity {
        class: ReproducibilityClass::BestEffort,
        causes,
    };

    let receipt = ExecutionReceiptV2::from_input(
        ExecutionIdentityInputV2::new(
            source,
            provenance,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            None, // local locator: not meaningful for managed services
            reproducibility,
        ),
        chrono::Utc::now().to_rfc3339(),
    )?;
    let execution_id = receipt.execution_id.clone();
    Ok((ExecutionReceiptDocument::V2(receipt), execution_id))
}

fn source_tree_for_service(input: &ManagedServiceReceiptInput<'_>) -> Result<Tracked<String>> {
    if input.service_dir.is_dir() {
        let hash = hash_source_tree(input.service_dir).with_context(|| {
            format!(
                "failed to hash managed-service dir {}",
                input.service_dir.display()
            )
        })?;
        Ok(Tracked::known(hash))
    } else {
        Ok(Tracked::unknown(format!(
            "managed-service dir not present at observation time: {}",
            input.service_dir.display()
        )))
    }
}

fn detect_libc() -> &'static str {
    if cfg!(target_os = "linux") {
        "glibc"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "msvcrt"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn mk_service<'a>(dir: &'a Path, helper_path: &'a Path) -> ManagedServiceReceiptInput<'a> {
        ManagedServiceReceiptInput {
            name: "test-service",
            service_dir: dir,
            helper_path,
            depends_on: &[],
            lifecycle: "long-running",
            source_label: "ato/desktop",
        }
    }

    fn write_helper(dir: &Path, body: &[u8]) -> std::path::PathBuf {
        fs::write(dir.join("run.sh"), body).unwrap();
        fs::write(dir.join("README"), b"a service").unwrap();
        dir.join("run.sh")
    }

    #[test]
    fn synthesized_receipt_is_v2_and_carries_execution_id() {
        let dir = tempdir().expect("dir");
        let helper = write_helper(dir.path(), b"#!/bin/sh\necho hello\n");
        let input = mk_service(dir.path(), &helper);

        let (doc, exec_id) = synthesize_managed_service_receipt(&input).expect("synth");
        assert!(exec_id.starts_with("blake3:"));
        match doc {
            ExecutionReceiptDocument::V2(r) => {
                assert_eq!(r.schema_version, 2);
                assert_eq!(r.execution_id, exec_id);
                assert_eq!(
                    r.source_provenance.registry_ref.as_deref(),
                    Some("managed-service:ato/desktop/test-service")
                );
                assert!(matches!(
                    r.runtime.completeness,
                    RuntimeCompleteness::DeclaredOnly
                ));
                assert!(matches!(
                    r.reproducibility.class,
                    ReproducibilityClass::BestEffort
                ));
            }
            ExecutionReceiptDocument::V1(_) => panic!("expected v2 receipt"),
        }
    }

    #[test]
    fn synthesized_receipt_is_stable_across_calls_with_identical_inputs() {
        let dir = tempdir().expect("dir");
        let helper = write_helper(dir.path(), b"#!/bin/sh\necho hello\n");
        let input = mk_service(dir.path(), &helper);

        let (_, id1) = synthesize_managed_service_receipt(&input).expect("synth1");
        let (_, id2) = synthesize_managed_service_receipt(&input).expect("synth2");
        // computed_at is excluded from execution_id projection (per
        // execution_identity::IdentityProjectionV2). The two ids must match
        // even though the receipts differ in computed_at.
        assert_eq!(id1, id2, "managed-service identity must be time-invariant");
    }

    #[test]
    fn changing_helper_body_changes_execution_id() {
        let dir = tempdir().expect("dir");
        let helper = write_helper(dir.path(), b"#!/bin/sh\necho hello\n");
        let input1 = mk_service(dir.path(), &helper);
        let (_, id1) = synthesize_managed_service_receipt(&input1).expect("synth1");

        // Modify the helper.
        fs::write(dir.path().join("run.sh"), b"#!/bin/sh\necho different\n").unwrap();
        let input2 = mk_service(dir.path(), &helper);
        let (_, id2) = synthesize_managed_service_receipt(&input2).expect("synth2");
        assert_ne!(id1, id2, "helper body change must move execution_id");
    }
}
