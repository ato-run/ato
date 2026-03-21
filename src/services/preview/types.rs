use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::inference_feedback::InferenceAttemptHandle;
use crate::install::{GitHubCheckout, GitHubInstallDraftResolvedRef, GitHubInstallDraftResponse};

use super::draft::derived_plan_from_github_draft;
use super::storage::{generate_preview_id, preview_root};
use super::{PREVIEW_MANIFEST_FILE_NAME, PREVIEW_METADATA_FILE_NAME};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewTargetKind {
    GitHubRepository,
    LocalPath,
    ScopedCapsule,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PreviewPromotionEligibility {
    Eligible,
    #[default]
    RequiresManualReview,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DerivedExecutionPlan {
    pub runtime: Option<String>,
    pub driver: Option<String>,
    pub resolved_runtime_version: Option<String>,
    pub resolved_port: Option<u16>,
    #[serde(default)]
    pub resolved_lock_files: Vec<PathBuf>,
    #[serde(default)]
    pub resolved_pack_include: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub deferred_constraints: Vec<String>,
    pub promotion_eligibility: PreviewPromotionEligibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSession {
    pub preview_id: String,
    pub target_reference: String,
    pub target_kind: PreviewTargetKind,
    pub invocation_dir: PathBuf,
    pub session_root: PathBuf,
    pub manifest_path: PathBuf,
    pub metadata_path: PathBuf,
    pub checkout_dir: Option<PathBuf>,
    pub repository: Option<String>,
    pub resolved_ref: Option<GitHubInstallDraftResolvedRef>,
    pub manifest_source: Option<String>,
    pub preview_toml: Option<String>,
    pub inference_mode: Option<String>,
    pub inference_attempt_id: Option<String>,
    pub retry_count: u8,
    pub last_failure_reason: Option<String>,
    pub last_smoke_error_class: Option<String>,
    pub last_smoke_error_excerpt: Option<String>,
    pub manual_fix_applied: bool,
    pub checkout_preserved: bool,
    pub derived_plan: DerivedExecutionPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewStorageLayout {
    pub root: PathBuf,
    pub session_root: PathBuf,
    pub metadata_path: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug)]
pub struct GitHubPreviewPreparation {
    pub checkout: GitHubCheckout,
    pub draft_fetch_warning: Option<String>,
    pub install_draft: Option<GitHubInstallDraftResponse>,
    pub preview_session: PreviewSession,
    pub session_persist_warning: Option<String>,
}

impl PreviewStorageLayout {
    pub fn for_preview_id(preview_id: &str) -> Result<Self> {
        let root = preview_root()?;
        Ok(Self::for_preview_id_at(&root, preview_id))
    }

    pub(super) fn for_preview_id_at(root: &Path, preview_id: &str) -> Self {
        let session_root = root.join(preview_id);
        Self {
            root: root.to_path_buf(),
            metadata_path: session_root.join(PREVIEW_METADATA_FILE_NAME),
            manifest_path: session_root.join(PREVIEW_MANIFEST_FILE_NAME),
            session_root,
        }
    }

    pub fn ensure_exists(&self) -> Result<()> {
        fs::create_dir_all(&self.session_root).with_context(|| {
            format!(
                "Failed to create preview session directory: {}",
                self.session_root.display()
            )
        })
    }
}

impl PreviewSession {
    pub fn new(
        target_reference: impl Into<String>,
        target_kind: PreviewTargetKind,
        invocation_dir: PathBuf,
        derived_plan: DerivedExecutionPlan,
    ) -> Result<Self> {
        let preview_id = generate_preview_id();
        let layout = PreviewStorageLayout::for_preview_id(&preview_id)?;
        Ok(Self {
            preview_id,
            target_reference: target_reference.into(),
            target_kind,
            invocation_dir,
            session_root: layout.session_root.clone(),
            manifest_path: layout.manifest_path.clone(),
            metadata_path: layout.metadata_path.clone(),
            checkout_dir: None,
            repository: None,
            resolved_ref: None,
            manifest_source: None,
            preview_toml: None,
            inference_mode: None,
            inference_attempt_id: None,
            retry_count: 0,
            last_failure_reason: None,
            last_smoke_error_class: None,
            last_smoke_error_excerpt: None,
            manual_fix_applied: false,
            checkout_preserved: false,
            derived_plan,
        })
    }

    pub fn persist(&self) -> Result<()> {
        let layout = PreviewStorageLayout::for_preview_id_at(
            self.session_root.parent().unwrap_or_else(|| Path::new(".")),
            &self.preview_id,
        );
        layout.ensure_exists()?;

        let serialized = serde_json::to_vec_pretty(self)
            .context("Failed to serialize preview session metadata")?;
        fs::write(&self.metadata_path, serialized).with_context(|| {
            format!(
                "Failed to write preview session metadata: {}",
                self.metadata_path.display()
            )
        })?;

        if let Some(preview_toml) = &self.preview_toml {
            fs::write(&self.manifest_path, preview_toml).with_context(|| {
                format!(
                    "Failed to write preview manifest: {}",
                    self.manifest_path.display()
                )
            })?;
        }

        Ok(())
    }

    pub fn load(metadata_path: &Path) -> Result<Self> {
        let raw = fs::read(metadata_path).with_context(|| {
            format!(
                "Failed to read preview session metadata: {}",
                metadata_path.display()
            )
        })?;
        serde_json::from_slice(&raw).context("Failed to deserialize preview session metadata")
    }

    pub fn update_from_install_draft(&mut self, draft: &GitHubInstallDraftResponse) {
        self.preview_toml = draft.preview_toml.clone();
        self.resolved_ref = Some(draft.resolved_ref.clone());
        self.manifest_source = Some(draft.manifest_source.clone());
        self.inference_mode = draft.inference_mode.clone();
        self.derived_plan = derived_plan_from_github_draft(Some(draft));
    }

    pub fn set_inference_attempt(&mut self, attempt: Option<&InferenceAttemptHandle>) {
        self.inference_attempt_id = attempt.map(|value| value.attempt_id.clone());
    }

    pub fn record_retry_draft(&mut self, draft: &GitHubInstallDraftResponse, retry_ordinal: u8) {
        self.retry_count = retry_ordinal;
        self.manual_fix_applied = false;
        self.update_from_install_draft(draft);
    }

    pub fn record_smoke_failure(&mut self, report: &capsule_core::smoke::SmokeFailureReport) {
        self.last_failure_reason = Some(report.message.clone());
        self.last_smoke_error_class = Some(report.class.as_str().to_string());
        self.last_smoke_error_excerpt = Some(report.stderr_tail.clone());
    }

    pub fn record_manual_intervention_required(&mut self, reason: &str) {
        self.last_failure_reason = Some(reason.to_string());
    }

    pub fn record_manual_fix(&mut self, manifest_text: &str) {
        self.preview_toml = Some(manifest_text.to_string());
        self.manual_fix_applied = true;
        self.last_failure_reason = None;
        self.last_smoke_error_class = None;
        self.last_smoke_error_excerpt = None;
    }
}
