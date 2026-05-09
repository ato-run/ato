//! Dispatch for `ato internal *` plumbing commands.

use anyhow::{anyhow, Result};

use capsule_core::router::ExecutionProfile;

use crate::application::auth::consent_store::approve_execution_plan_consent;
use crate::application::preflight::collect_aggregate_requirements;
use crate::cli::{ConsentInternalCommands, InternalCommands};

pub(crate) fn execute_internal_command(command: InternalCommands) -> Result<()> {
    match command {
        InternalCommands::Consent { command } => execute_consent_command(command),
        InternalCommands::Preflight {
            target,
            registry: _,
            json,
        } => execute_preflight_command(target, json),
    }
}

/// `ato internal preflight <target> [--json]` handler. Delegates to
/// the side-effect-free preflight collector and serializes the result
/// for stdout.
///
/// Stdout policy:
/// - `--json`: single-line aggregate envelope. The desktop launch
///   worker scrapes this exact shape — see
///   `crate::application::preflight::AggregatePreflightResult` for the
///   field set.
/// - default: brief human-readable summary, one line per pending
///   requirement.
///
/// Exit policy: returns `Ok(())` regardless of whether requirements
/// are pending (the caller decides what to do based on the
/// `requirements` array). Non-zero exits are reserved for genuine
/// failures (manifest missing, derivation failed, consent store
/// unreadable). This matches `ato inspect requirements`'s convention.
fn execute_preflight_command(target: String, json: bool) -> Result<()> {
    let result = collect_aggregate_requirements(&target, ExecutionProfile::Dev)
        .map_err(|err| anyhow!("preflight collection failed: {err}"))?;

    if json {
        let payload = serde_json::json!({
            "schema_version": "1",
            "ok": result.is_empty(),
            "capsule_id": result.capsule_id,
            "capsule_version": result.capsule_version,
            "visited_targets": result.visited_targets,
            "requirements": result.requirements,
        });
        println!("{payload}");
    } else if result.is_empty() {
        println!(
            "preflight: {}@{} — no pending requirements; launch can proceed.",
            result.capsule_id, result.capsule_version
        );
    } else {
        println!(
            "preflight: {}@{} — {} requirement(s) across {} target(s):",
            result.capsule_id,
            result.capsule_version,
            result.requirements.len(),
            result.visited_targets.len()
        );
        for envelope in &result.requirements {
            println!("  - {}", envelope.display.message);
        }
    }
    Ok(())
}

fn execute_consent_command(command: ConsentInternalCommands) -> Result<()> {
    match command {
        ConsentInternalCommands::ApproveExecutionPlan {
            scoped_id,
            version,
            target_label,
            policy_segment_hash,
            provisioning_policy_hash,
            json,
        } => {
            approve_execution_plan_consent(
                &scoped_id,
                &version,
                &target_label,
                &policy_segment_hash,
                &provisioning_policy_hash,
            )?;

            if json {
                // Single-line JSON envelope, parse-friendly for the
                // desktop's CLI envelope reader.
                let payload = serde_json::json!({
                    "ok": true,
                    "consent": {
                        "scoped_id": scoped_id,
                        "version": version,
                        "target_label": target_label,
                        "policy_segment_hash": policy_segment_hash,
                        "provisioning_policy_hash": provisioning_policy_hash,
                    }
                });
                println!("{payload}");
            }
            Ok(())
        }
    }
}
