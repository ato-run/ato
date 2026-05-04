//! `ato reconstruct <execution_id>` — diagnose whether a v2 execution
//! receipt has the inputs needed for cross-host reconstruction.
//!
//! Phase 1 (this commit): pure diagnosis. The command reads the receipt,
//! evaluates each portability prerequisite (source_provenance, dependency
//! identity, runtime resolved_ref, policy hashes, environment closure),
//! and emits either a green check or a precise gap list. It NEVER
//! actually fetches source / deps / runtimes — `--execute` is reserved
//! and currently rejects with `not-implemented`.
//!
//! Phase 2 (deferred): wire the diagnostic outcome into a real fetch
//! pipeline (registry source download, derivation re-materialization,
//! runtime install) so a portable v2 receipt can be reconstructed end-
//! to-end on a foreign host. That work depends on the registry exposing
//! source-tree blobs by hash, which is not yet a stable API.

use anyhow::{bail, Result};
use capsule_core::execution_identity::{
    ExecutionReceiptDocument, ExecutionReceiptV2, TrackingStatus,
};
use serde::Serialize;

use crate::application::execution_receipts;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconstructDiagnosticView {
    execution_id: String,
    schema_version: u32,
    portable: bool,
    blockers: Vec<Blocker>,
    warnings: Vec<String>,
    summary: ReconstructSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconstructSummary {
    source_ref: Option<String>,
    runtime_resolved_ref: Option<String>,
    dependency_derivation_hash: Option<String>,
    dependency_output_hash: Option<String>,
    reproducibility_class: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Blocker {
    field: &'static str,
    reason: &'static str,
}

pub(super) fn execute_reconstruct_command(id: String, json: bool, execute: bool) -> Result<()> {
    if execute {
        bail!(
            "ato reconstruct --execute is not yet implemented; cross-host fetch + relaunch is \
             Phase 2 follow-up work. Run without --execute to print a portability diagnostic."
        );
    }

    let document = execution_receipts::read_receipt_document(&id)?;
    let receipt = match document {
        ExecutionReceiptDocument::V2(r) => r,
        ExecutionReceiptDocument::V1(_) => {
            bail!(
                "execution receipt {id} is schema_version=1; cross-host reconstruction requires v2 \
                 (the portable launch envelope identity). Re-run with `ATO_RECEIPT_SCHEMA=v2-experimental` \
                 to capture a v2 receipt, or use `ato replay` for same-host replay of v1 receipts."
            );
        }
    };

    let view = analyze_v2_receipt(&receipt);
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_human_view(&view);
    }
    Ok(())
}

fn analyze_v2_receipt(receipt: &ExecutionReceiptV2) -> ReconstructDiagnosticView {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    // Portable source identity: cross-host reconstruction needs either a
    // registry_ref (so we can fetch the source tree by name) or a
    // source_tree_hash that the operator can resolve to a blob.
    if receipt.source.source_tree_hash.status != TrackingStatus::Known {
        blockers.push(Blocker {
            field: "source.source_tree_hash",
            reason: "not Known; cannot identify which source bytes to fetch",
        });
    }
    if receipt.source_provenance.registry_ref.is_none()
        && receipt.source_provenance.git_remote.is_none()
    {
        blockers.push(Blocker {
            field: "source_provenance",
            reason: "no registry_ref nor git_remote; nothing to fetch from",
        });
    }

    // Runtime resolved_ref: cross-host needs a portable runtime
    // identifier (e.g. node@20.11.0). A resolved local path is
    // host-bound and cannot reproduce on a foreign host.
    if receipt.runtime.resolved_ref.status != TrackingStatus::Known {
        blockers.push(Blocker {
            field: "runtime.resolved_ref",
            reason: "not Known; foreign host has no canonical name to install",
        });
    }
    if receipt.runtime.binary_hash.status != TrackingStatus::Known {
        warnings.push(
            "runtime.binary_hash is not Known; foreign-host binary may not match".to_string(),
        );
    }

    // Dependencies: NotApplicable is fine (script with no deps).
    // Unknown / Untracked block reconstruction because the foreign host
    // cannot tell what to materialize.
    let derivation_status = receipt.dependencies.derivation_hash.status;
    if matches!(
        derivation_status,
        TrackingStatus::Unknown | TrackingStatus::Untracked
    ) {
        blockers.push(Blocker {
            field: "dependencies.derivation_hash",
            reason: "Unknown/Untracked; no canonical input identity to re-resolve deps",
        });
    }

    // Policy: cross-host inherits policy from the receipt; warn (not
    // block) if any policy hash is Untracked because the foreign host
    // would have to make up sensible defaults.
    if receipt.policy.network_policy_hash.status != TrackingStatus::Known {
        warnings.push(
            "policy.network_policy_hash is not Known; foreign host will substitute deny-all"
                .to_string(),
        );
    }

    // Environment: Closed mode is the green light. Partial / Untracked
    // mean the foreign host will not see the same env entries the
    // original host launched with.
    if !matches!(
        receipt.environment.mode,
        capsule_core::execution_identity::EnvironmentMode::Closed
    ) {
        warnings.push(format!(
            "environment.mode is {:?}; foreign-host launch will not match the original env closure",
            receipt.environment.mode
        ));
    }

    let portable = blockers.is_empty();
    let summary = ReconstructSummary {
        source_ref: receipt.source_provenance.registry_ref.clone(),
        runtime_resolved_ref: receipt.runtime.resolved_ref.value.clone(),
        dependency_derivation_hash: receipt.dependencies.derivation_hash.value.clone(),
        dependency_output_hash: receipt.dependencies.output_hash.value.clone(),
        reproducibility_class: format!("{:?}", receipt.reproducibility.class),
    };

    ReconstructDiagnosticView {
        execution_id: receipt.execution_id.clone(),
        schema_version: receipt.schema_version,
        portable,
        blockers,
        warnings,
        summary,
    }
}

fn print_human_view(view: &ReconstructDiagnosticView) {
    println!("Execution: {}", view.execution_id);
    println!("  Schema: v{}", view.schema_version);
    println!("  Reproducibility: {}", view.summary.reproducibility_class);
    if let Some(source_ref) = view.summary.source_ref.as_deref() {
        println!("  Source ref: {source_ref}");
    }
    if let Some(runtime) = view.summary.runtime_resolved_ref.as_deref() {
        println!("  Runtime: {runtime}");
    }
    if let Some(deriv) = view.summary.dependency_derivation_hash.as_deref() {
        println!("  Dep derivation: {deriv}");
    }
    if let Some(out) = view.summary.dependency_output_hash.as_deref() {
        println!("  Dep output: {out}");
    }
    println!();
    if view.portable {
        println!("✅ Portable: cross-host reconstruction prerequisites are satisfied.");
    } else {
        println!("❌ Not portable: {} blocker(s).", view.blockers.len());
        for blocker in &view.blockers {
            println!("  - {}: {}", blocker.field, blocker.reason);
        }
    }
    if !view.warnings.is_empty() {
        println!();
        println!("Warnings ({}):", view.warnings.len());
        for warning in &view.warnings {
            println!("  - {warning}");
        }
    }
    println!();
    println!(
        "Note: Phase 1 reconstruct is diagnosis-only. `--execute` to actually fetch and \
         re-launch is reserved for Phase 2 (registry blob API)."
    );
}
