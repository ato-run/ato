use std::collections::HashMap;
use std::path::PathBuf;

use capsule_core::router::ManifestData;
use serde::{Deserialize, Serialize};

use crate::executors::launch_context::InjectedMount;

use super::AutoProvisioningOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningSafetyClass {
    SafeDefault,
    InteractiveOptIn,
    ExplicitOptIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProvisioningIssue {
    MissingLockfile {
        target: String,
        driver: String,
        working_dir: PathBuf,
        candidates: Vec<String>,
    },
    MissingRequiredEnv {
        target: String,
        missing_keys: Vec<String>,
    },
    RuntimeSelectionRequired {
        target: String,
        runtime: String,
        driver: String,
    },
}

impl ProvisioningIssue {
    pub fn summary(&self) -> String {
        match self {
            Self::MissingLockfile {
                target,
                driver,
                working_dir,
                candidates,
            } => format!(
                "Auto-provisioning issue [{}]: {} target in {} is missing a lockfile ({})",
                target,
                driver,
                working_dir.display(),
                candidates.join(", ")
            ),
            Self::MissingRequiredEnv {
                target,
                missing_keys,
            } => format!(
                "Auto-provisioning issue [{}]: synthetic env may be needed for {}",
                target,
                missing_keys.join(", ")
            ),
            Self::RuntimeSelectionRequired {
                target,
                runtime,
                driver,
            } => format!(
                "Auto-provisioning issue [{}]: {} / {} needs an explicit runtime selection",
                target, runtime, driver
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProvisioningAction {
    GenerateShadowLockfile {
        target: String,
        driver: String,
        safety: ProvisioningSafetyClass,
    },
    InjectSyntheticEnv {
        target: String,
        missing_keys: Vec<String>,
        safety: ProvisioningSafetyClass,
    },
    SelectRuntime {
        target: String,
        runtime: String,
        driver: String,
        safety: ProvisioningSafetyClass,
    },
}

impl ProvisioningAction {
    pub fn summary(&self) -> String {
        match self {
            Self::GenerateShadowLockfile { target, driver, .. } => format!(
                "Auto-provisioning plan [{}]: generate shadow lockfile for {}",
                target, driver
            ),
            Self::InjectSyntheticEnv {
                target,
                missing_keys,
                ..
            } => format!(
                "Auto-provisioning plan [{}]: prepare synthetic env for {}",
                target,
                missing_keys.join(", ")
            ),
            Self::SelectRuntime {
                target,
                runtime,
                driver,
                ..
            } => format!(
                "Auto-provisioning plan [{}]: select compatible runtime for {} / {}",
                target, runtime, driver
            ),
        }
    }

    pub fn safety(&self) -> ProvisioningSafetyClass {
        match self {
            Self::GenerateShadowLockfile { safety, .. }
            | Self::InjectSyntheticEnv { safety, .. }
            | Self::SelectRuntime { safety, .. } => *safety,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProvisioningPlan {
    pub issues: Vec<ProvisioningIssue>,
    pub actions: Vec<ProvisioningAction>,
}

impl ProvisioningPlan {
    pub fn from_issues(issues: Vec<ProvisioningIssue>) -> Self {
        let mut actions = Vec::new();
        for issue in &issues {
            match issue {
                ProvisioningIssue::MissingLockfile { target, driver, .. } => {
                    actions.push(ProvisioningAction::GenerateShadowLockfile {
                        target: target.clone(),
                        driver: driver.clone(),
                        safety: ProvisioningSafetyClass::SafeDefault,
                    });
                }
                ProvisioningIssue::MissingRequiredEnv {
                    target,
                    missing_keys,
                } => {
                    actions.push(ProvisioningAction::InjectSyntheticEnv {
                        target: target.clone(),
                        missing_keys: missing_keys.clone(),
                        safety: ProvisioningSafetyClass::SafeDefault,
                    });
                }
                ProvisioningIssue::RuntimeSelectionRequired {
                    target,
                    runtime,
                    driver,
                } => {
                    actions.push(ProvisioningAction::SelectRuntime {
                        target: target.clone(),
                        runtime: runtime.clone(),
                        driver: driver.clone(),
                        safety: ProvisioningSafetyClass::SafeDefault,
                    });
                }
            }
        }

        Self { issues, actions }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowWorkspaceRef {
    pub root_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub audit_path: PathBuf,
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningMaterializationStatus {
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisioningMaterializationRecord {
    pub stage: String,
    pub target: String,
    pub driver: Option<String>,
    pub status: ProvisioningMaterializationStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningAudit {
    pub manifest_path: String,
    pub target: String,
    pub preview_mode: bool,
    pub background: bool,
    pub issues: Vec<ProvisioningIssue>,
    pub actions: Vec<ProvisioningAction>,
    pub shadow_root: Option<String>,
    pub shadow_manifest_path: Option<String>,
    pub materialization_records: Vec<ProvisioningMaterializationRecord>,
}

impl ProvisioningAudit {
    pub fn new(
        plan: &ManifestData,
        options: &AutoProvisioningOptions,
        summary: &ProvisioningPlan,
    ) -> Self {
        Self {
            manifest_path: plan.manifest_path.display().to_string(),
            target: plan.selected_target_label().to_string(),
            preview_mode: options.preview_mode,
            background: options.background,
            issues: summary.issues.clone(),
            actions: summary.actions.clone(),
            shadow_root: None,
            shadow_manifest_path: None,
            materialization_records: Vec::new(),
        }
    }

    pub fn record_materialization(
        &mut self,
        stage: impl Into<String>,
        target: impl Into<String>,
        driver: Option<&str>,
        status: ProvisioningMaterializationStatus,
        detail: impl Into<String>,
    ) {
        self.materialization_records
            .push(ProvisioningMaterializationRecord {
                stage: stage.into(),
                target: target.into(),
                driver: driver.map(ToString::to_string),
                status,
                detail: detail.into(),
            });
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProvisioningOutcome {
    pub plan: ProvisioningPlan,
    pub shadow_workspace: Option<ShadowWorkspaceRef>,
    pub additional_env: HashMap<String, String>,
    pub additional_mounts: Vec<InjectedMount>,
}
