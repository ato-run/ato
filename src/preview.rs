use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::inference_feedback::InferenceAttemptHandle;
use crate::install::{
    self, GitHubCheckout, GitHubInstallDraftResolvedRef, GitHubInstallDraftResponse,
};

const DEFAULT_PREVIEW_DIR: &str = ".ato/previews";
const ENV_PREVIEW_ROOT: &str = "ATO_PREVIEW_ROOT";
const PREVIEW_METADATA_FILE_NAME: &str = "metadata.json";
const PREVIEW_MANIFEST_FILE_NAME: &str = "capsule.toml";

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

    fn for_preview_id_at(root: &Path, preview_id: &str) -> Self {
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

pub fn persist_session_with_warning(session: &PreviewSession) -> Option<String> {
    session
        .persist()
        .err()
        .map(|error| format!("Failed to persist preview session metadata: {error}"))
}

pub fn preview_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_PREVIEW_ROOT) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(DEFAULT_PREVIEW_DIR))
}

pub fn load_preview_session_for_manifest(manifest_path: &Path) -> Result<Option<PreviewSession>> {
    if manifest_path.file_name().and_then(|value| value.to_str())
        != Some(PREVIEW_MANIFEST_FILE_NAME)
    {
        return Ok(None);
    }

    let root = preview_root()?;
    let session_root = match manifest_path.parent() {
        Some(path) => path,
        None => return Ok(None),
    };
    if !session_root.starts_with(&root) {
        return Ok(None);
    }

    let metadata_path = session_root.join(PREVIEW_METADATA_FILE_NAME);
    if !metadata_path.exists() {
        return Ok(None);
    }

    PreviewSession::load(&metadata_path).map(Some)
}

pub async fn prepare_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
) -> Result<GitHubPreviewPreparation> {
    let (install_draft, draft_fetch_warning) = match install::fetch_github_install_draft(repository).await {
        Ok(draft) => (Some(draft), None),
        Err(error) => (
            None,
            Some(format!(
                "Failed to fetch ato store install draft: {error}. Falling back to local zero-config inference."
            )),
        ),
    };

    let checkout = install::download_github_repository_at_ref(
        repository,
        install_draft
            .as_ref()
            .map(|draft| draft.resolved_ref.sha.as_str()),
    )
    .await?;
    let install_draft = install_draft
        .as_ref()
        .map(|draft| draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir))
        .transpose()?;

    let preview_session = build_github_preview_session(
        repository,
        invocation_dir,
        &checkout,
        install_draft.as_ref(),
    )?;
    let session_persist_warning = persist_session_with_warning(&preview_session);

    Ok(GitHubPreviewPreparation {
        checkout,
        draft_fetch_warning,
        install_draft,
        preview_session,
        session_persist_warning,
    })
}

pub fn draft_requires_manual_review(draft: &GitHubInstallDraftResponse) -> bool {
    if draft
        .capsule_hint
        .as_ref()
        .and_then(|hint| hint.launchability.as_deref())
        == Some("manual_review")
    {
        return true;
    }
    if draft.retryable {
        return false;
    }

    let has_required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .map(|values| !values.is_empty())
        .unwrap_or(false);
    let has_manual_review_warning = draft
        .capsule_hint
        .as_ref()
        .map(|hint| {
            hint.warnings
                .iter()
                .any(|warning| warning_requires_manual_review(warning))
        })
        .unwrap_or(false);

    has_required_env || has_manual_review_warning
}

pub fn github_draft_manual_review_reason(draft: &GitHubInstallDraftResponse) -> String {
    if let Some(warning) = draft.capsule_hint.as_ref().and_then(|hint| {
        hint.warnings
            .iter()
            .find(|warning| warning_requires_manual_review(warning))
    }) {
        return warning.clone();
    }

    "Generated draft requires manual review before fail-closed provisioning can continue."
        .to_string()
}

fn warning_requires_manual_review(warning: &str) -> bool {
    let lowered = warning.to_ascii_lowercase();

    lowered.contains("frozen-lockfile")
        || lowered.contains("uv.lock")
        || lowered.contains("package-lock.json")
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

pub fn required_env_from_preview_toml(manifest_text: &str) -> Vec<String> {
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

fn generate_preview_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("preview-{}", hex::encode(bytes))
}

fn build_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
    checkout: &GitHubCheckout,
    install_draft: Option<&GitHubInstallDraftResponse>,
) -> Result<PreviewSession> {
    let preview_toml = install_draft.and_then(|draft| draft.preview_toml.clone());
    let mut session = PreviewSession::new(
        repository,
        PreviewTargetKind::GitHubRepository,
        invocation_dir.to_path_buf(),
        derived_plan_from_github_draft(install_draft),
    )?;
    session.checkout_dir = Some(checkout.checkout_dir.clone());
    session.checkout_preserved = false;
    session.repository = Some(checkout.repository.clone());
    session.preview_toml = preview_toml;
    if let Some(draft) = install_draft {
        session.resolved_ref = Some(draft.resolved_ref.clone());
        session.manifest_source = Some(draft.manifest_source.clone());
        session.inference_mode = draft.inference_mode.clone();
    }
    Ok(session)
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

#[derive(Debug, Default)]
struct PreviewTomlSummary {
    driver: Option<String>,
    pack_include: Vec<String>,
    port: Option<u16>,
    runtime: Option<String>,
    runtime_version: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::{
        draft_requires_manual_review, github_draft_manual_review_reason, preview_root,
        required_env_from_preview_toml, DerivedExecutionPlan, PreviewPromotionEligibility,
        PreviewSession, PreviewStorageLayout, PreviewTargetKind, ENV_PREVIEW_ROOT,
    };
    use crate::install::{
        GitHubInstallDraftCapsuleToml, GitHubInstallDraftHint, GitHubInstallDraftRepo,
        GitHubInstallDraftResolvedRef, GitHubInstallDraftResponse,
    };
    use capsule_core::smoke::{SmokeFailureClass, SmokeFailureReport};
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn preview_root_prefers_env_override() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set(ENV_PREVIEW_ROOT, temp.path());
        assert_eq!(preview_root().expect("preview root"), temp.path());
    }

    #[test]
    fn preview_session_layout_uses_expected_files() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set(ENV_PREVIEW_ROOT, temp.path());

        let layout = PreviewStorageLayout::for_preview_id("preview-test").expect("layout");
        assert_eq!(layout.session_root, temp.path().join("preview-test"));
        assert_eq!(
            layout.metadata_path,
            temp.path().join("preview-test").join("metadata.json")
        );
        assert_eq!(
            layout.manifest_path,
            temp.path().join("preview-test").join("capsule.toml")
        );
    }

    #[test]
    fn preview_session_persists_metadata_and_manifest() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set(ENV_PREVIEW_ROOT, temp.path());

        let mut session = PreviewSession::new(
            "github.com/example/repo",
            PreviewTargetKind::GitHubRepository,
            PathBuf::from("/workspace"),
            DerivedExecutionPlan {
                runtime: Some("source".to_string()),
                driver: Some("python".to_string()),
                resolved_runtime_version: Some("3.11.10".to_string()),
                resolved_port: Some(8000),
                resolved_lock_files: vec![PathBuf::from("uv.lock")],
                resolved_pack_include: vec!["src/**".to_string()],
                warnings: vec!["generated lockfile".to_string()],
                deferred_constraints: vec!["author must commit uv.lock".to_string()],
                promotion_eligibility: PreviewPromotionEligibility::Eligible,
            },
        )
        .expect("session");
        session.preview_toml = Some("schema_version = \"0.3\"\nname = \"demo\"\n".to_string());

        session.persist().expect("persist");

        assert!(session.metadata_path.exists());
        assert!(session.manifest_path.exists());

        let loaded = PreviewSession::load(&session.metadata_path).expect("load");
        assert_eq!(loaded.preview_id, session.preview_id);
        assert_eq!(loaded.target_reference, "github.com/example/repo");
        assert_eq!(loaded.derived_plan.resolved_port, Some(8000));
        assert_eq!(loaded.derived_plan.driver.as_deref(), Some("python"));
    }

    #[test]
    fn required_env_prefers_root_field_over_legacy_env_section() {
        let required = required_env_from_preview_toml(
            "required_env = [\"DATABASE_URL\"]\n\n[env]\nrequired = [\"SHOULD_NOT_WIN\"]\n",
        );
        assert_eq!(required, vec!["DATABASE_URL".to_string()]);
    }

    #[test]
    fn draft_requires_manual_review_when_required_env_exists() {
        let draft = GitHubInstallDraftResponse {
            repo: GitHubInstallDraftRepo {
                owner: "example".to_string(),
                repo: "repo".to_string(),
                full_name: "example/repo".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "example/repo".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/example/repo".to_string(),
            resolved_ref: GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "abc123".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some(
                "schema_version = \"0.3\"\nrequired_env = [\"DATABASE_URL\"]\n".to_string(),
            ),
            capsule_hint: Some(GitHubInstallDraftHint {
                confidence: "high".to_string(),
                warnings: Vec::new(),
                launchability: Some("runnable".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: false,
        };

        assert!(draft_requires_manual_review(&draft));
    }

    #[test]
    fn draft_allows_deno_advisory_warnings_when_launchable() {
        let draft = GitHubInstallDraftResponse {
            repo: GitHubInstallDraftRepo {
                owner: "jellydn".to_string(),
                repo: "hono-minimal-deno-app".to_string(),
                full_name: "jellydn/hono-minimal-deno-app".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "jellydn/hono-minimal-deno-app".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/jellydn/hono-minimal-deno-app"
                .to_string(),
            resolved_ref: GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "deadbeef".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some(
                "schema_version = \"0.3\"\nname = \"hono-minimal-deno-app\"\nruntime = \"source/deno\"\nrun = \"deno task start\"\n"
                    .to_string(),
            ),
            capsule_hint: Some(GitHubInstallDraftHint {
                confidence: "high".to_string(),
                warnings: vec![
                    "Deno runtime detected but runtime field set to source/node per ExtractedFacts; actual runtime is Deno which requires 'deno run' commands, not Node.js package managers. The capsule.toml v0.3 schema does not have a dedicated source/deno runtime; consider manual review if Deno-specific provisioning is required.".to_string(),
                    "deno.lock exists but Deno lockfile provisioning is not covered by standard Node.js package manager inference; Deno's native lockfile handling may require custom provision logic.".to_string(),
                ],
                launchability: Some("runnable".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: false,
        };

        assert!(!draft_requires_manual_review(&draft));
    }

    #[test]
    fn draft_requires_manual_review_for_explicit_lockfile_blocker_warning() {
        let draft = GitHubInstallDraftResponse {
            repo: GitHubInstallDraftRepo {
                owner: "example".to_string(),
                repo: "repo".to_string(),
                full_name: "example/repo".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "example/repo".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/example/repo".to_string(),
            resolved_ref: GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "abc123".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some("schema_version = \"0.3\"\nname = \"demo\"\n".to_string()),
            capsule_hint: Some(GitHubInstallDraftHint {
                confidence: "high".to_string(),
                warnings: vec![
                    "pnpm-lock.yaml is required for fail-closed provisioning".to_string()
                ],
                launchability: Some("runnable".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: false,
        };

        assert!(draft_requires_manual_review(&draft));
        assert_eq!(
            github_draft_manual_review_reason(&draft),
            "pnpm-lock.yaml is required for fail-closed provisioning"
        );
    }

    #[test]
    fn draft_allows_optional_port_env_advisory_warning() {
        let draft = GitHubInstallDraftResponse {
            repo: GitHubInstallDraftRepo {
                owner: "typicode".to_string(),
                repo: "json-server".to_string(),
                full_name: "typicode/json-server".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "typicode/json-server".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/typicode/json-server".to_string(),
            resolved_ref: GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "deadbeef".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some(
                "schema_version = \"0.3\"\nname = \"json-server\"\nruntime = \"source/node\"\nrun = \"node src/bin.ts fixtures/db.json\"\n"
                    .to_string(),
            ),
            capsule_hint: Some(GitHubInstallDraftHint {
                confidence: "high".to_string(),
                warnings: vec![
                    "db.jsonファイルが必須です。実行時に第1引数として指定する必要があります。"
                        .to_string(),
                    "PORT環境変数またはコマンドラインオプション-pで3000以外のポートを指定できます。"
                        .to_string(),
                ],
                launchability: Some("runnable".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: false,
        };

        assert!(!draft_requires_manual_review(&draft));
    }

    #[test]
    fn preview_session_tracks_retry_failure_and_manual_fix() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set(ENV_PREVIEW_ROOT, temp.path());

        let mut session = PreviewSession::new(
            "github.com/example/repo",
            PreviewTargetKind::GitHubRepository,
            PathBuf::from("/workspace"),
            DerivedExecutionPlan::default(),
        )
        .expect("session");

        let draft = GitHubInstallDraftResponse {
            repo: GitHubInstallDraftRepo {
                owner: "example".to_string(),
                repo: "repo".to_string(),
                full_name: "example/repo".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "example/repo".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/example/repo".to_string(),
            resolved_ref: GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "abc123".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some("schema_version = \"0.3\"\nname = \"demo\"\n".to_string()),
            capsule_hint: Some(GitHubInstallDraftHint {
                confidence: "high".to_string(),
                warnings: vec!["needs lockfile".to_string()],
                launchability: Some("runnable".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: true,
        };

        session.record_retry_draft(&draft, 2);
        assert_eq!(session.retry_count, 2);

        let report = SmokeFailureReport {
            class: SmokeFailureClass::StartupTimeout,
            message: "timed out".to_string(),
            stderr_tail: "tail".to_string(),
            exit_status: None,
        };
        session.record_smoke_failure(&report);
        assert_eq!(session.last_failure_reason.as_deref(), Some("timed out"));
        assert_eq!(
            session.last_smoke_error_class.as_deref(),
            Some("startup_timeout")
        );

        session.record_manual_fix("schema_version = \"0.3\"\nname = \"fixed\"\n");
        assert!(session.manual_fix_applied);
        assert_eq!(
            session.preview_toml.as_deref(),
            Some("schema_version = \"0.3\"\nname = \"fixed\"\n")
        );
        assert!(session.last_failure_reason.is_none());
        assert!(session.last_smoke_error_class.is_none());
    }
}
