mod diagnose;
mod shadow;
mod types;

use std::sync::Arc;

use anyhow::Result;
use capsule_core::router::ManifestData;
use capsule_core::CapsuleReporter;

use crate::executors::launch_context::RuntimeLaunchContext;
use crate::reporters::CliReporter;

use self::types::{
    ProvisioningAction, ProvisioningAudit, ProvisioningMaterializationStatus, ProvisioningOutcome,
    ProvisioningPlan,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoProvisioningOptions {
    pub preview_mode: bool,
    pub background: bool,
}

pub struct AuditReporter<'a> {
    audit: &'a ProvisioningAudit,
}

impl<'a> AuditReporter<'a> {
    pub fn from_outcome(outcome: &'a ProvisioningOutcome) -> Option<Self> {
        outcome.audit.as_ref().map(|audit| Self { audit })
    }

    pub fn title(&self) -> &'static str {
        "Auto-Provisioning Audit"
    }

    pub fn body(&self) -> String {
        let mut lines = Vec::new();

        if self.audit.materialization_records.iter().any(|record| {
            record.stage == "shadow_lockfile"
                && record.status == ProvisioningMaterializationStatus::Applied
        }) {
            lines.push("Automatically generated a shadow lockfile.".to_string());
        }

        if self.audit.materialization_records.iter().any(|record| {
            record.stage == "synthetic_env"
                && record.status == ProvisioningMaterializationStatus::Applied
        }) {
            let injected_database_env = self.audit.actions.iter().any(|action| {
                matches!(
                    action,
                    ProvisioningAction::InjectSyntheticEnv { missing_keys, .. }
                        if missing_keys.iter().any(|key| {
                            let normalized = key.trim().to_ascii_uppercase();
                            normalized.contains("DATABASE") || normalized.ends_with("_DB")
                        })
                )
            });
            lines.push(if injected_database_env {
                "Injected placeholder database environment variables via a synthetic .env file."
                    .to_string()
            } else {
                "Injected placeholder environment variables via a synthetic .env file.".to_string()
            });
        }

        if self.audit.shadow_manifest_path.is_some() {
            lines.push(
                "Re-routed execution through the auto-provisioned shadow workspace.".to_string(),
            );
        }

        lines.join("\n")
    }
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
    let additional_env =
        shadow::materialize_synthetic_env(plan, &plan_summary, &shadow_workspace, &mut audit)?;
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
        audit: Some(audit),
        shadow_workspace: Some(shadow_workspace),
        additional_env,
        additional_mounts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use capsule_core::router::{ExecutionProfile, ManifestData};

    use super::{
        types::{
            ProvisioningAction, ProvisioningAudit, ProvisioningMaterializationStatus,
            ProvisioningPlan, ProvisioningSafetyClass,
        },
        AuditReporter, AutoProvisioningOptions,
    };

    fn manifest_data() -> ManifestData {
        ManifestData {
            manifest: toml::from_str(
                r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
            )
            .expect("manifest"),
            manifest_path: PathBuf::from("/workspace/capsule.toml"),
            manifest_dir: PathBuf::from("/workspace"),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: HashMap::new(),
        }
    }

    #[test]
    fn audit_reporter_highlights_applied_lockfile_and_env_remediation() {
        let plan = manifest_data();
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![
                ProvisioningAction::GenerateShadowLockfile {
                    target: "app".to_string(),
                    driver: "node".to_string(),
                    safety: ProvisioningSafetyClass::SafeDefault,
                },
                ProvisioningAction::InjectSyntheticEnv {
                    target: "app".to_string(),
                    missing_keys: vec!["DATABASE_URL".to_string()],
                    safety: ProvisioningSafetyClass::SafeDefault,
                },
            ],
        };
        let mut audit = ProvisioningAudit::new(
            &plan,
            &AutoProvisioningOptions {
                preview_mode: false,
                background: false,
            },
            &summary,
        );
        audit.shadow_manifest_path =
            Some("/workspace/.tmp/ato-auto-provision/run-1/capsule.toml".to_string());
        audit.record_materialization(
            "shadow_lockfile",
            "app",
            Some("node"),
            ProvisioningMaterializationStatus::Applied,
            "generated package-lock.json",
        );
        audit.record_materialization(
            "synthetic_env",
            "app",
            Some("node"),
            ProvisioningMaterializationStatus::Applied,
            "wrote synthetic .env",
        );

        let body = AuditReporter { audit: &audit }.body();

        assert!(body.contains("Automatically generated a shadow lockfile."));
        assert!(body.contains(
            "Injected placeholder database environment variables via a synthetic .env file."
        ));
        assert!(body.contains("Re-routed execution through the auto-provisioned shadow workspace."));
    }

    #[test]
    fn audit_reporter_avoids_claiming_skipped_lockfile_generation() {
        let plan = manifest_data();
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::GenerateShadowLockfile {
                target: "app".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = ProvisioningAudit::new(
            &plan,
            &AutoProvisioningOptions {
                preview_mode: false,
                background: false,
            },
            &summary,
        );
        audit.record_materialization(
            "shadow_lockfile",
            "app",
            Some("node"),
            ProvisioningMaterializationStatus::Skipped,
            "package-lock.json already exists",
        );

        let body = AuditReporter { audit: &audit }.body();

        assert!(!body.contains("Automatically generated a shadow lockfile."));
    }
}
