mod diagnose;
mod shadow;
mod types;

use std::sync::Arc;

use anyhow::Result;
use capsule_core::router::ManifestData;
use capsule_core::CapsuleReporter;

use crate::executors::launch_context::RuntimeLaunchContext;
use crate::reporters::CliReporter;

use self::types::{ProvisioningAudit, ProvisioningOutcome, ProvisioningPlan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoProvisioningOptions {
    pub preview_mode: bool,
    pub background: bool,
}

pub async fn run_auto_provisioning_phase(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    reporter: Arc<CliReporter>,
    options: &AutoProvisioningOptions,
) -> Result<ProvisioningOutcome> {
    let issues = diagnose::collect_issues(plan, launch_ctx)?;
    let plan_summary = ProvisioningPlan::from_issues(issues);
    if plan_summary.actions.is_empty() {
        return Ok(ProvisioningOutcome::default());
    }

    let mut audit = ProvisioningAudit::new(plan, options, &plan_summary);
    let mut shadow_workspace = shadow::prepare_shadow_workspace(plan, &audit)?;
    audit.shadow_root = Some(shadow_workspace.root_dir.display().to_string());
    let additional_env = shadow::materialize_synthetic_env(plan, &plan_summary, &shadow_workspace)?;
    if let Err(error) =
        shadow::materialize_shadow_lockfiles(plan, &plan_summary, &shadow_workspace, &mut audit)
    {
        let _ = shadow::write_audit(&shadow_workspace.audit_path, &audit);
        return Err(error);
    }
    shadow_workspace.manifest_path =
        shadow::materialize_shadow_manifest(plan, &plan_summary, &shadow_workspace)?;
    audit.shadow_manifest_path = shadow_workspace
        .manifest_path
        .as_ref()
        .map(|path| path.display().to_string());
    shadow::write_audit(&shadow_workspace.audit_path, &audit)?;

    reporter
        .notify(format!(
            "Auto-provisioning analysis found {} issue(s) and prepared {} action(s)",
            plan_summary.issues.len(),
            plan_summary.actions.len()
        ))
        .await?;
    for issue in &plan_summary.issues {
        reporter.warn(issue.summary()).await?;
    }
    for action in &plan_summary.actions {
        reporter
            .notify(format!(
                "{} [{}]",
                action.summary(),
                match action.safety() {
                    types::ProvisioningSafetyClass::SafeDefault => "safe-default",
                    types::ProvisioningSafetyClass::InteractiveOptIn => "interactive-opt-in",
                    types::ProvisioningSafetyClass::ExplicitOptIn => "explicit-opt-in",
                }
            ))
            .await?;
    }

    Ok(ProvisioningOutcome {
        plan: plan_summary,
        shadow_workspace: Some(shadow_workspace),
        additional_env,
        additional_mounts: Vec::new(),
    })
}
