//! Dispatch for `ato internal *` plumbing commands.

use anyhow::Result;

use crate::application::auth::consent_store::approve_execution_plan_consent;
use crate::cli::{ConsentInternalCommands, InternalCommands};

pub(crate) fn execute_internal_command(command: InternalCommands) -> Result<()> {
    match command {
        InternalCommands::Consent { command } => execute_consent_command(command),
    }
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
