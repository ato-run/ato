use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::install::{GitHubCheckout, GitHubInstallDraftResolvedRef, GitHubInstallDraftResponse};

const DEFAULT_PREVIEW_DIR: &str = ".ato/previews";
const ENV_PREVIEW_ROOT: &str = "ATO_PREVIEW_ROOT";
const PREVIEW_METADATA_FILE_NAME: &str = "metadata.json";
const PREVIEW_MANIFEST_FILE_NAME: &str = "capsule.toml";

#[derive(Debug, Default)]
struct PreviewTomlSummary {
    driver: Option<String>,
    pack_include: Vec<String>,
    port: Option<u16>,
    runtime: Option<String>,
    runtime_version: Option<String>,
}

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

    pub fn set_inference_attempt_id(&mut self, attempt_id: Option<&str>) {
        self.inference_attempt_id = attempt_id.map(str::to_string);
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

fn preview_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_PREVIEW_ROOT) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(DEFAULT_PREVIEW_DIR))
}

fn generate_preview_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("preview-{}", hex::encode(bytes))
}

fn derived_plan_from_github_draft(
    install_draft: Option<&GitHubInstallDraftResponse>,
) -> DerivedExecutionPlan {
    let mut plan = DerivedExecutionPlan::default();
    let Some(draft) = install_draft else {
        plan.warnings.push(
            "No ato store install draft was available; preview will rely on local zero-config inference."
                .to_string(),
        );
        plan.promotion_eligibility = PreviewPromotionEligibility::RequiresManualReview;
        return plan;
    };

    if let Some(preview_toml) = draft.preview_toml.as_deref() {
        let summary = summarize_preview_toml(preview_toml);
        plan.runtime = summary.runtime;
        plan.driver = summary.driver;
        plan.resolved_runtime_version = summary.runtime_version;
        plan.resolved_port = summary.port;
        plan.resolved_pack_include = summary.pack_include;
    }

    if let Some(hint) = draft.capsule_hint.as_ref() {
        plan.warnings.extend(hint.warnings.clone());
    }
    let required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .unwrap_or_default();
    if !required_env.is_empty() {
        plan.deferred_constraints.push(format!(
            "Required environment variables must be provided before promotion: {}",
            required_env.join(", ")
        ));
    }
    plan.promotion_eligibility = if draft_requires_manual_review(draft) {
        PreviewPromotionEligibility::RequiresManualReview
    } else {
        PreviewPromotionEligibility::Eligible
    };

    plan
}

fn draft_requires_manual_review(draft: &GitHubInstallDraftResponse) -> bool {
    let launchability_requires_manual_review = draft
        .capsule_hint
        .as_ref()
        .and_then(|hint| hint.launchability.as_deref())
        == Some("manual_review");
    if draft.retryable {
        return false;
    }

    let has_required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .map(|values| {
            // Skip env vars already satisfied by the process environment.
            values
                .into_iter()
                .filter(|k| std::env::var(k).is_err())
                .collect::<Vec<_>>()
        })
        .map(|unsatisfied| !unsatisfied.is_empty())
        .unwrap_or(false);
    let (has_manual_review_warning, has_soft_preview_warning) = draft
        .capsule_hint
        .as_ref()
        .map(|hint| {
            (
                hint.warnings
                    .iter()
                    .any(|warning| warning_requires_manual_review(warning)),
                hint.warnings
                    .iter()
                    .any(|warning| warning_is_soft_preview_advisory(warning)),
            )
        })
        .unwrap_or((false, false));

    has_required_env
        || has_manual_review_warning
        || (launchability_requires_manual_review && !has_soft_preview_warning)
}

fn warning_requires_manual_review(warning: &str) -> bool {
    if warning_is_soft_preview_advisory(warning) {
        return false;
    }

    let lowered = warning.to_ascii_lowercase();

    lowered.contains("frozen-lockfile")
        || lowered.contains("uv.lock")
        || lowered.contains("package-lock.json")
        || lowered.contains("yarn.lock")
        || lowered.contains("pnpm-lock.yaml")
        || lowered.contains("bun.lock")
        || lowered.contains("multiple node lockfiles")
        || lowered.contains("database")
        || lowered.contains("redis")
        || lowered.contains("credential")
        || lowered.contains("secret")
        || lowered.contains("token")
        || lowered.contains("requires manual intervention")
        || lowered.contains("manual intervention required")
        || lowered.contains("required environment variable")
        || lowered.contains("required environment variables")
        || warning.contains("必須環境変数")
        || warning.contains("環境変数が必要")
        || warning.contains("環境変数を設定")
        || warning.contains("外部DB")
        || warning.contains("認証")
}

fn warning_is_soft_preview_advisory(warning: &str) -> bool {
    let lowered = warning.to_ascii_lowercase();
    lowered.contains("could not be normalized to a direct node entrypoint")
        || lowered.contains("a development server command was inferred from package.json")
        // ato run uses plain install (not --frozen-lockfile), so lockfile platform-
        // compatibility warnings from the store draft are not actionable for preview runs.
        || lowered.contains("frozen-lockfile")
}

fn required_env_from_preview_toml(manifest_text: &str) -> Vec<String> {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return Vec::new();
    };

    let root_required_env = parsed
        .get("required_env")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !root_required_env.is_empty() {
        return root_required_env;
    }

    parsed
        .get("env")
        .and_then(|env| env.get("required"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn summarize_preview_toml(manifest_text: &str) -> PreviewTomlSummary {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return PreviewTomlSummary::default();
    };

    let runtime = parsed
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let driver = runtime
        .as_deref()
        .and_then(|value| value.split('/').nth(1))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let runtime_version = parsed
        .get("runtime_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let port = parsed
        .get("port")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u16::try_from(value).ok());
    let pack_include = parsed
        .get("pack")
        .and_then(|pack| pack.get("include"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    PreviewTomlSummary {
        driver,
        pack_include,
        port,
        runtime,
        runtime_version,
    }
}
