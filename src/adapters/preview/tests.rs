use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use capsule_core::smoke::{SmokeFailureClass, SmokeFailureReport};

use super::{
    draft_requires_manual_review, github_draft_manual_review_reason, preview_root,
    required_env_from_preview_toml, DerivedExecutionPlan, PreviewPromotionEligibility,
    PreviewSession, PreviewStorageLayout, PreviewTargetKind, ENV_PREVIEW_ROOT,
};
use crate::install::{
    GitHubInstallDraftCapsuleToml, GitHubInstallDraftHint, GitHubInstallDraftRepo,
    GitHubInstallDraftResolvedRef, GitHubInstallDraftResponse,
};

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
        proposed_install_command: "ato run github.com/jellydn/hono-minimal-deno-app".to_string(),
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
            warnings: vec!["pnpm-lock.yaml is required for fail-closed provisioning".to_string()],
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
