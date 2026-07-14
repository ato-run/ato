use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::cli::ImportArgs;
use crate::runtime::process::{ImportPreviewSession, ImportPreviewWorkloadPid, ProcessManager};
use capsule::foundation::types::command_spec::contains_shell_operators;

const GITHUB_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "ato-cli-source-import";
const IMPORT_ROOT_DIR: &str = "tmp/import";
const IMPORT_LOG_DIR: &str = "tmp/import-logs";
const CAPSULE_TOML: &str = "capsule.toml";
const MAX_ERROR_EXCERPT_BYTES: usize = 4000;
const LOCAL_SOURCE_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_SOURCE_OVERRIDE";
const LOCAL_REVISION_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_REVISION_ID";
const LOCAL_TREE_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_TREE_HASH";
const KEEP_WORKSPACE_ENV: &str = "ATO_IMPORT_KEEP_WORKSPACE";
const IMPORT_PROBE_ID_ENV: &str = "ATO_IMPORT_PROBE_ID";
const IMPORT_SESSION_ID_ENV: &str = "ATO_IMPORT_SESSION_ID";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NormalizedGitHubInput {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) source_url_normalized: String,
}

#[derive(Debug, Serialize)]
struct ImportSource {
    source_url_normalized: String,
    source_host: String,
    repo_namespace: String,
    repo_name: String,
    revision_id: String,
    source_tree_hash: String,
    subdir: String,
}

#[derive(Debug, Serialize)]
struct ImportRecipe {
    origin: String,
    target_label: Option<String>,
    platform_os: String,
    platform_arch: String,
    recipe_toml: String,
    recipe_hash: String,
}

#[derive(Debug, Serialize)]
struct ImportRun {
    status: String,
    phase: Option<String>,
    error_class: Option<String>,
    error_excerpt: Option<String>,
    /// `"shell"` when the run command was executed through a shell
    /// interpreter (e.g., `sh -c`). Absent for direct argv execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    command_mode: Option<String>,
    /// `true` when the run command depends on a host shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_host_shell: Option<bool>,
    /// Shell kind used when `command_mode` is `"shell"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    shell_kind: Option<String>,
    /// Cleanup result for probe-mode shadow runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_status: Option<String>,
    /// Cleanup diagnostic when teardown was incomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_error: Option<String>,
    /// Runtime log path for probe-mode shadow runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<i32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    process_group_ids: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shadow_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readiness_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_policy: Option<String>,
}

#[derive(Debug, Serialize)]
struct ImportOutput {
    source: ImportSource,
    recipe: ImportRecipe,
    run: ImportRun,
    /// How the recipe was resolved. Present when remote lookup was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    recipe_resolution: Option<RecipeResolution>,
}

#[derive(Debug, Serialize)]
struct RecipeResolution {
    /// "remote_binding" | "remote_binding_failed" | "inference" | "skipped"
    source: String,
    /// Present when source is "remote_binding_failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<String>,
    /// Present when source is "remote_binding_failed" or "remote_binding"
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResolveBindingResponse {
    binding: Option<ResolveBindingItem>,
    recipe: Option<ResolveRecipeItem>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveBindingItem {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    binding_status: String,
    #[allow(dead_code)]
    smoke_status: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResolveRecipeItem {
    origin: String,
    trust_level: String,
    target_label: Option<String>,
    platform_os: String,
    platform_arch: String,
    recipe_toml: String,
    recipe_hash: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitResponse {
    sha: String,
    commit: GitHubCommitInner,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitInner {
    tree: GitHubTreeRef,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeRef {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct InferredManifestOutput {
    manifest_toml: String,
}

#[derive(Debug)]
struct MaterializedSource {
    source: ImportSource,
    checkout_dir: PathBuf,
    shadow_dir: PathBuf,
    _workspace: ImportWorkspace,
}

#[derive(Debug)]
struct ImportWorkspace {
    root: PathBuf,
    keep: bool,
}

impl Drop for ImportWorkspace {
    fn drop(&mut self) {
        if self.keep || std::env::var_os(KEEP_WORKSPACE_ENV).is_some() {
            return;
        }
        let _ = cleanup_import_workspace_root(&self.root);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportWorkspaceCleanupOutcome {
    Removed,
    SkippedActive { run_session_id: String },
    SkippedOpenProcess { pid_or_pgid: String, reason: String },
    SkippedUnknown { reason: String },
    AlreadyGone,
}

fn cleanup_import_workspace_root(root: &Path) -> Result<ImportWorkspaceCleanupOutcome> {
    cleanup_import_workspace_root_with(
        root,
        |workspace| {
            let process_manager = ProcessManager::new()?;
            Ok(process_manager
                .active_import_preview_session_for_workspace(workspace)?
                .map(|session| session.run_session_id))
        },
        workspace_open_process_guard,
    )
}

fn cleanup_import_workspace_root_with<SessionGuard, OpenGuard>(
    root: &Path,
    session_guard: SessionGuard,
    open_guard: OpenGuard,
) -> Result<ImportWorkspaceCleanupOutcome>
where
    SessionGuard: FnOnce(&Path) -> Result<Option<String>>,
    OpenGuard: FnOnce(&Path) -> Result<Option<WorkspaceOpenProcess>>,
{
    if !root.exists() {
        return Ok(ImportWorkspaceCleanupOutcome::AlreadyGone);
    }

    match session_guard(root) {
        Ok(Some(run_session_id)) => {
            return Ok(ImportWorkspaceCleanupOutcome::SkippedActive { run_session_id });
        }
        Ok(None) => {}
        Err(error) => {
            return Ok(ImportWorkspaceCleanupOutcome::SkippedUnknown {
                reason: error.to_string(),
            });
        }
    }

    match open_guard(root) {
        Ok(Some(open_process)) => {
            return Ok(ImportWorkspaceCleanupOutcome::SkippedOpenProcess {
                pid_or_pgid: open_process.pid_or_pgid,
                reason: open_process.reason,
            });
        }
        Ok(None) => {}
        Err(error) => {
            return Ok(ImportWorkspaceCleanupOutcome::SkippedUnknown {
                reason: error.to_string(),
            });
        }
    }

    fs::remove_dir_all(root).with_context(|| format!("failed to remove {}", root.display()))?;
    Ok(ImportWorkspaceCleanupOutcome::Removed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceOpenProcess {
    pid_or_pgid: String,
    reason: String,
}

#[cfg(unix)]
fn workspace_open_process_guard(root: &Path) -> Result<Option<WorkspaceOpenProcess>> {
    workspace_open_process_guard_with(root, &process_rows(), process_current_working_dir_with_lsof)
}

#[cfg(not(unix))]
fn workspace_open_process_guard(_root: &Path) -> Result<Option<WorkspaceOpenProcess>> {
    Ok(None)
}

#[cfg(unix)]
fn workspace_open_process_guard_with<F>(
    root: &Path,
    rows: &[ProcessRow],
    cwd_lookup: F,
) -> Result<Option<WorkspaceOpenProcess>>
where
    F: Fn(i32) -> Result<Option<PathBuf>>,
{
    let workspace = root.display().to_string();
    for row in rows {
        if row.command.contains(&workspace) {
            return Ok(Some(WorkspaceOpenProcess {
                pid_or_pgid: format!("pid:{}", row.pid),
                reason: format!("command references {}", root.display()),
            }));
        }

        if cwd_lookup(row.pid)?
            .as_ref()
            .is_some_and(|cwd| cwd.starts_with(root))
        {
            return Ok(Some(WorkspaceOpenProcess {
                pid_or_pgid: format!("pid:{}", row.pid),
                reason: format!("cwd is under {}", root.display()),
            }));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn process_current_working_dir_with_lsof(pid: i32) -> Result<Option<PathBuf>> {
    if pid <= 0 {
        return Ok(None);
    }

    let output = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
        .output()
        .with_context(|| format!("failed to execute lsof for pid {}", pid))?;

    if !output.status.success() {
        if output.stdout.is_empty() {
            return Ok(None);
        }
        return Err(anyhow::anyhow!(
            "lsof failed for pid {} with status {}",
            pid,
            output.status
        ));
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(path) = line.strip_prefix('n')
            && !path.is_empty()
        {
            return Ok(Some(PathBuf::from(path)));
        }
    }

    Ok(None)
}

pub(super) fn execute_import_command(args: ImportArgs) -> Result<()> {
    if args.keep_alive && !args.run {
        bail!("--keep-alive requires --run");
    }
    if args.keep_alive && !args.emit_json {
        bail!("--keep-alive requires --emit-json");
    }

    let input = normalize_github_import_input(&args.repo)?;
    let mut materialized = materialize_source(&input)?;

    // Try remote recipe binding resolution before falling back to local inference.
    let mut recipe_resolution: Option<RecipeResolution> = None;
    let (recipe_toml, origin) = if args.recipe.is_some() || args.no_remote_recipe {
        // User provided an explicit recipe, or disabled remote lookup — use local logic.
        load_or_infer_recipe(&args, &materialized.checkout_dir, &input.repo)?
    } else {
        // Attempt remote binding resolution.
        let remote = resolve_remote_recipe(&materialized.source);
        match remote {
            Ok(Some((toml, _hash))) => {
                recipe_resolution = Some(RecipeResolution {
                    source: "remote_binding".to_string(),
                    fallback: None,
                    error_class: None,
                });
                (toml, "registry".to_string())
            }
            Ok(None) => {
                // No remote binding found — fallback to local inference.
                let (toml, origin) =
                    load_or_infer_recipe(&args, &materialized.checkout_dir, &input.repo)?;
                recipe_resolution = Some(RecipeResolution {
                    source: "remote_binding_failed".to_string(),
                    fallback: Some(origin.clone()),
                    error_class: Some("no_verified_binding".to_string()),
                });
                (toml, origin)
            }
            Err(error) => {
                // API unavailable — fallback to local inference.
                let (toml, origin) =
                    load_or_infer_recipe(&args, &materialized.checkout_dir, &input.repo)?;
                recipe_resolution = Some(RecipeResolution {
                    source: "remote_binding_failed".to_string(),
                    fallback: Some(origin.clone()),
                    error_class: Some("api_unavailable".to_string()),
                });
                tracing::debug!(
                    ?error,
                    "remote recipe resolution failed; falling back to inference"
                );
                (toml, origin)
            }
        }
    };
    let final_shadow_tree_hash = materialize_shadow_recipe(&materialized.shadow_dir, &recipe_toml)?;
    materialized.source.source_tree_hash = final_shadow_tree_hash;
    let recipe_hash = blake3_label(recipe_toml.as_bytes());
    let target_label = infer_target_label(&recipe_toml);
    let mut run = if args.run && args.keep_alive {
        run_shadow_workspace_keep_alive(&mut materialized, &recipe_toml)?
    } else if args.run && (args.readiness_only || args.emit_json) {
        run_shadow_workspace_readiness_only(&materialized, &recipe_toml)?
    } else if args.run {
        run_shadow_workspace(&materialized)?
    } else {
        ImportRun {
            status: "not_run".to_string(),
            phase: None,
            error_class: None,
            error_excerpt: None,
            command_mode: None,
            requires_host_shell: None,
            shell_kind: None,
            cleanup_status: None,
            cleanup_error: None,
            log_path: None,
            run_session_id: None,
            pid: None,
            process_group_ids: Vec::new(),
            primary_port: None,
            primary_url: None,
            shadow_dir: None,
            readiness_state: None,
            cleanup_policy: None,
        }
    };
    apply_shell_info(&mut run, &recipe_toml);

    let output = ImportOutput {
        source: materialized.source,
        recipe: ImportRecipe {
            origin,
            target_label,
            platform_os: platform_os_label().to_string(),
            platform_arch: platform_arch_label().to_string(),
            recipe_toml,
            recipe_hash,
        },
        run,
        recipe_resolution,
    };

    if args.emit_json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_human_summary(&output);
    }
    Ok(())
}

fn print_human_summary(output: &ImportOutput) {
    println!(
        "Resolved {}\ncommit: {}\ntree: {}\nrecipe: {}\nrun: {}",
        output.source.source_url_normalized,
        output.source.revision_id,
        output.source.source_tree_hash,
        output.recipe.recipe_hash,
        output.run.status,
    );
}

pub(crate) fn normalize_github_import_input(input: &str) -> Result<NormalizedGitHubInput> {
    let trimmed = input.trim();
    if trimmed.starts_with("capsule://") {
        bail!("capsule:// imports are not supported yet; pass a GitHub repository");
    }

    if is_owner_repo(trimmed) {
        let (owner, repo) = split_owner_repo(trimmed)?;
        return Ok(normalized(owner, repo));
    }

    let as_url = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&as_url).context("invalid GitHub repository URL")?;
    if url.scheme() != "https" {
        bail!("only https://github.com repositories are supported");
    }
    match url.host_str().map(str::to_ascii_lowercase).as_deref() {
        Some("github.com") | Some("www.github.com") => {}
        _ => bail!("only github.com repositories are supported"),
    }
    let parts = url
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    if parts.len() != 2 {
        bail!("GitHub repository must be a repository root: owner/repo");
    }
    Ok(normalized(parts[0], parts[1]))
}

fn is_owner_repo(input: &str) -> bool {
    let parts = input.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(is_github_path_char))
}

fn is_github_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn split_owner_repo(input: &str) -> Result<(&str, &str)> {
    let mut parts = input.split('/');
    let owner = parts.next().context("missing GitHub owner")?;
    let repo = parts.next().context("missing GitHub repo")?;
    Ok((owner, repo))
}

fn normalized(owner: &str, repo_raw: &str) -> NormalizedGitHubInput {
    let repo = repo_raw.trim_end_matches(".git");
    let owner = owner.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    NormalizedGitHubInput {
        source_url_normalized: format!("https://github.com/{owner}/{repo}"),
        owner,
        repo,
    }
}

fn materialize_source(input: &NormalizedGitHubInput) -> Result<MaterializedSource> {
    let workspace = ImportWorkspace {
        root: import_workspace_root(input)?,
        keep: false,
    };
    let checkout_dir = workspace.root.join("source");
    let shadow_dir = workspace.root.join("shadow");

    let (revision_id, source_tree_hash) = if let Some(local_source) = local_source_override() {
        copy_source_tree(&local_source, &checkout_dir)?;
        let revision_id = std::env::var(LOCAL_REVISION_OVERRIDE_ENV)
            .unwrap_or_else(|_| "local-import-test-revision".to_string());
        let source_tree_hash = std::env::var(LOCAL_TREE_OVERRIDE_ENV)
            .unwrap_or_else(|_| source_tree_hash_from_files(&checkout_dir));
        (revision_id, source_tree_hash)
    } else {
        let resolved = resolve_github_source(input)?;
        clone_public_github_repo(input, &checkout_dir, &resolved.revision_id)?;
        checkout_resolved_revision(&checkout_dir, &resolved.revision_id)?;
        let git_metadata = checkout_dir.join(".git");
        if git_metadata.exists() {
            fs::remove_dir_all(&git_metadata).with_context(|| {
                format!(
                    "failed to remove source VCS metadata at {}",
                    git_metadata.display()
                )
            })?;
        }
        capsule::source_identity::verify_fully_materialized(&checkout_dir).with_context(|| {
            format!(
                "GitHub source at {} was not fully materialized",
                checkout_dir.display()
            )
        })?;
        (resolved.revision_id, resolved.source_tree_hash)
    };

    copy_source_tree(&checkout_dir, &shadow_dir)?;

    Ok(MaterializedSource {
        source: ImportSource {
            source_url_normalized: input.source_url_normalized.clone(),
            source_host: "github.com".to_string(),
            repo_namespace: input.owner.clone(),
            repo_name: input.repo.clone(),
            revision_id,
            source_tree_hash,
            subdir: ".".to_string(),
        },
        checkout_dir,
        shadow_dir,
        _workspace: workspace,
    })
}

fn local_source_override() -> Option<PathBuf> {
    std::env::var_os(LOCAL_SOURCE_OVERRIDE_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn import_workspace_root(input: &NormalizedGitHubInput) -> Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before UNIX_EPOCH")?
        .as_nanos();
    let root = capsule::common::paths::ato_path_or_workspace_tmp(IMPORT_ROOT_DIR).join(format!(
        "{}-{}-{}-{now}",
        input.owner,
        input.repo,
        std::process::id()
    ));
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn resolve_github_source(input: &NormalizedGitHubInput) -> Result<ImportSource> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("failed to create GitHub HTTP client")?;
    let repo_url = format!("{GITHUB_API_BASE}/repos/{}/{}", input.owner, input.repo);
    let repo = client
        .get(repo_url)
        .send()
        .context("failed to resolve GitHub repository")?
        .error_for_status()
        .context("GitHub repository lookup failed")?
        .json::<GitHubRepoResponse>()
        .context("failed to decode GitHub repository response")?;

    let commit_url = format!(
        "{GITHUB_API_BASE}/repos/{}/{}/commits/{}",
        input.owner, input.repo, repo.default_branch
    );
    let commit = client
        .get(commit_url)
        .send()
        .context("failed to resolve GitHub commit")?
        .error_for_status()
        .context("GitHub commit lookup failed")?
        .json::<GitHubCommitResponse>()
        .context("failed to decode GitHub commit response")?;

    Ok(ImportSource {
        source_url_normalized: input.source_url_normalized.clone(),
        source_host: "github.com".to_string(),
        repo_namespace: input.owner.clone(),
        repo_name: input.repo.clone(),
        revision_id: commit.sha,
        source_tree_hash: commit.commit.tree.sha,
        subdir: ".".to_string(),
    })
}

fn immutable_git_fetch_args(revision_id: &str) -> Vec<String> {
    vec![
        "fetch".to_string(),
        "--depth".to_string(),
        "1".to_string(),
        "--no-tags".to_string(),
        "origin".to_string(),
        revision_id.to_string(),
    ]
}

fn clone_public_github_repo(
    input: &NormalizedGitHubInput,
    target_dir: &Path,
    revision_id: &str,
) -> Result<()> {
    let parent = target_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("checkout target has no parent"))?;
    fs::create_dir_all(parent)?;
    let clone_url = format!("{}.git", input.source_url_normalized);
    fs::create_dir_all(target_dir)?;
    let init = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(parent)
        .arg("init")
        .arg("--quiet")
        .arg(target_dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to initialize checkout for {clone_url}"))?;
    if !init.status.success() {
        bail!(
            "failed to initialize checkout for {}: {}",
            clone_url,
            String::from_utf8_lossy(&init.stderr).trim()
        );
    }

    let remote = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(target_dir)
        .args(["remote", "add", "origin", clone_url.as_str()])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to configure checkout for {clone_url}"))?;
    if !remote.status.success() {
        bail!(
            "failed to configure checkout for {}: {}",
            clone_url,
            String::from_utf8_lossy(&remote.stderr).trim()
        );
    }

    let fetch = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(target_dir)
        .args(immutable_git_fetch_args(revision_id))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to fetch resolved revision {revision_id}"))?;
    if !fetch.status.success() {
        bail!(
            "failed to fetch resolved revision {}: {}",
            revision_id,
            String::from_utf8_lossy(&fetch.stderr).trim()
        );
    }
    Ok(())
}

fn checkout_resolved_revision(checkout_dir: &Path, revision_id: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(checkout_dir)
        .arg("checkout")
        .arg("--detach")
        .arg(revision_id)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to checkout resolved revision {revision_id}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "failed to checkout {}: {}",
        revision_id,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn copy_source_tree(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        fs::remove_dir_all(destination)
            .with_context(|| format!("failed to clear {}", destination.display()))?;
    }
    fs::create_dir_all(destination)?;
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        if path == source {
            continue;
        }
        let relative = path.strip_prefix(source)?;
        if relative
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        if entry.file_type().is_symlink() {
            bail!(
                "source tree contains an unsupported symbolic link at {}",
                path.display()
            );
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, &target).with_context(|| {
                format!("failed to copy {} to {}", path.display(), target.display())
            })?;
        } else {
            bail!(
                "source tree contains an unsupported filesystem entry at {}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Materialize the exact manifest consumed by `ato run`, then bind the import
/// identity to the same source-tree observer used by execution receipts and
/// the strict-realization prelaunch recheck.
fn materialize_shadow_recipe(shadow_dir: &Path, recipe_toml: &str) -> Result<String> {
    let shadow_manifest = shadow_dir.join(CAPSULE_TOML);
    fs::write(&shadow_manifest, recipe_toml)
        .with_context(|| format!("failed to write {}", shadow_manifest.display()))?;
    capsule::source_identity::verify_fully_materialized(shadow_dir).with_context(|| {
        format!(
            "shadow source at {} was not fully materialized",
            shadow_dir.display()
        )
    })?;
    crate::application::execution_observers::hash_source_tree(shadow_dir)
}

fn load_or_infer_recipe(
    args: &ImportArgs,
    checkout_dir: &Path,
    repo_name: &str,
) -> Result<(String, String)> {
    if let Some(path) = args.recipe.as_ref() {
        return Ok((
            fs::read_to_string(path)
                .with_context(|| format!("failed to read recipe {}", path.display()))?,
            "manual".to_string(),
        ));
    }

    let in_repo = checkout_dir.join(CAPSULE_TOML);
    if in_repo.is_file() {
        return Ok((
            fs::read_to_string(&in_repo)
                .with_context(|| format!("failed to read {}", in_repo.display()))?,
            "in_repo".to_string(),
        ));
    }

    match infer_recipe_with_existing_engine(checkout_dir) {
        Ok(toml) => Ok((toml, "inference".to_string())),
        Err(_) => Ok((infer_minimal_recipe(repo_name), "inference".to_string())),
    }
}

fn infer_recipe_with_existing_engine(checkout_dir: &Path) -> Result<String> {
    let output = Command::new(std::env::current_exe()?)
        .arg("project")
        .arg("infer-manifest")
        .arg(checkout_dir)
        .arg("--json")
        .current_dir(checkout_dir)
        .stdin(Stdio::null())
        .output()
        .context("failed to run ato project infer-manifest")?;
    if !output.status.success() {
        bail!(
            "ato project infer-manifest failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let parsed: InferredManifestOutput =
        serde_json::from_slice(&output.stdout).context("invalid infer-manifest JSON")?;
    if parsed.manifest_toml.trim().is_empty() {
        bail!("infer-manifest returned an empty recipe");
    }
    Ok(parsed.manifest_toml)
}

fn infer_minimal_recipe(repo_name: &str) -> String {
    let name = if repo_name.trim().is_empty() {
        "github-import"
    } else {
        repo_name
    };
    format!(
        "schema_version = \"0.3\"\nname = \"{name}\"\nversion = \"0.1.0\"\ntype = \"app\"\nruntime = \"source\"\nworking_dir = \".\"\n"
    )
}

fn run_shadow_workspace(materialized: &MaterializedSource) -> Result<ImportRun> {
    let import_probe_id = new_import_run_id("probe", &materialized.source)?;
    let output = run_ato_shadow(&materialized.shadow_dir, &import_probe_id)?;
    Ok(import_run_from_output(&output))
}

/// Run the shadow workspace as a readiness probe and tear it down before
/// returning.
fn run_shadow_workspace_readiness_only(
    materialized: &MaterializedSource,
    recipe_toml: &str,
) -> Result<ImportRun> {
    // Extract port from recipe for readiness waiting.
    // If the declared port is already in use, remap to a free port and inform
    // the child subprocess via ATO_UI_OVERRIDE_PORT so it binds the same port.
    let declared_port_from_recipe = infer_port(recipe_toml);
    let declared_port = declared_port_from_recipe.unwrap_or(1111);
    let actual_port = if crate::runtime::port_manager::is_port_available(declared_port) {
        declared_port
    } else {
        // Find a free port by scanning upward from a base offset.
        (declared_port.saturating_add(1)..=u16::MAX)
            .find(|&p| crate::runtime::port_manager::is_port_available(p))
            .unwrap_or(declared_port)
    };
    let ready_url = format!("http://127.0.0.1:{}/", actual_port);
    let import_probe_id = new_import_run_id("probe", &materialized.source)?;
    let log_path = import_run_log_path(&import_probe_id)?;

    // Spawn `ato run` as a probe. It must not outlive this command unless a
    // durable session handle owns it; readiness-only import has no such handle.
    let mut command = Command::new(std::env::current_exe()?);
    apply_probe_stdio(&mut command, &log_path)?;
    command
        .arg("run")
        .arg(&materialized.shadow_dir)
        .arg("--yes")
        .current_dir(&materialized.shadow_dir)
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    if actual_port != declared_port {
        command.env("ATO_UI_OVERRIDE_PORT", actual_port.to_string());
    }
    command.env(IMPORT_PROBE_ID_ENV, &import_probe_id);
    if std::env::var("CAPSULE_ALLOW_UNSAFE").ok().as_deref() == Some("1") {
        command.arg("--dangerously-skip-permissions");
    }
    let child = command
        .spawn()
        .context("failed to spawn shadow workspace")?;
    let mut cleanup = ProbeRunGuard::new(child, materialized.shadow_dir.clone());
    cleanup.observe();

    // Poll readiness for up to 300s.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build HTTP client")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        cleanup.observe();
        if let Ok(resp) = client.get(&ready_url).send()
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
        if let Ok(Some(status)) = cleanup.child_mut().try_wait() {
            // A process that exits with success() is not necessarily a
            // failure: the "detached shadow" pattern (a launcher that spawns
            // a backgrounded workload via start_new_session/setsid and then
            // exits on its own) is intentional and supported — the actual
            // workload lives on in the detached child, not this pid. The
            // single readiness probe just above can race the child's listen
            // socket becoming reachable (observed under contended CI
            // runners), so before declaring `exited_before_readiness`, give
            // the workload a few quick extra chances to answer rather than
            // failing on the very first miss that happens to coincide with
            // the launcher's own exit.
            if status.success() {
                let mut became_ready = false;
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Ok(resp) = client.get(&ready_url).send()
                        && resp.status().is_success()
                    {
                        became_ready = true;
                        break;
                    }
                }
                if became_ready {
                    ready = true;
                    break;
                }
            }
            // Short-lived commands can complete before readiness. Treat a
            // successful exit as a valid probe completion; failures still
            // flow through normal classification.
            let combined = read_error_excerpt_from_log(&log_path);
            let cleanup_outcome = cleanup.cleanup();
            if status.success() {
                if declared_port_from_recipe.is_some() {
                    return Ok(import_run_with_cleanup(
                        ImportRun {
                            status: "failed".to_string(),
                            phase: Some("readiness".to_string()),
                            error_class: Some("exited_before_readiness".to_string()),
                            error_excerpt: Some(redact_error_excerpt(&format!(
                                "process exited successfully before readiness probe {ready_url} passed\n{combined}",
                            ))),
                            command_mode: None,
                            requires_host_shell: None,
                            shell_kind: None,
                            cleanup_status: None,
                            cleanup_error: None,
                            log_path: None,
                            run_session_id: None,
                            pid: None,
                            process_group_ids: Vec::new(),
                            primary_port: None,
                            primary_url: None,
                            shadow_dir: None,
                            readiness_state: None,
                            cleanup_policy: None,
                        },
                        cleanup_outcome,
                        &log_path,
                    ));
                }
                return Ok(import_run_with_cleanup(
                    ImportRun {
                        status: "passed".to_string(),
                        phase: Some("completed".to_string()),
                        error_class: None,
                        error_excerpt: None,
                        command_mode: None,
                        requires_host_shell: None,
                        shell_kind: None,
                        cleanup_status: None,
                        cleanup_error: None,
                        log_path: None,
                        run_session_id: None,
                        pid: None,
                        process_group_ids: Vec::new(),
                        primary_port: None,
                        primary_url: None,
                        shadow_dir: None,
                        readiness_state: None,
                        cleanup_policy: None,
                    },
                    cleanup_outcome,
                    &log_path,
                ));
            }
            let (phase, error_class) = classify_run_failure(&combined);
            return Ok(import_run_with_cleanup(
                ImportRun {
                    status: "failed".to_string(),
                    phase: Some(phase.to_string()),
                    error_class: Some(error_class.to_string()),
                    error_excerpt: Some(redact_error_excerpt(&combined)),
                    command_mode: None,
                    requires_host_shell: None,
                    shell_kind: None,
                    cleanup_status: None,
                    cleanup_error: None,
                    log_path: None,
                    run_session_id: None,
                    pid: None,
                    process_group_ids: Vec::new(),
                    primary_port: None,
                    primary_url: None,
                    shadow_dir: None,
                    readiness_state: None,
                    cleanup_policy: None,
                },
                cleanup_outcome,
                &log_path,
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }

    if !ready {
        let cleanup_outcome = cleanup.cleanup();
        let excerpt = read_error_excerpt_from_log(&log_path);
        return Ok(import_run_with_cleanup(
            ImportRun {
                status: "failed".to_string(),
                phase: Some("readiness".to_string()),
                error_class: Some("readiness_timeout".to_string()),
                error_excerpt: Some(redact_error_excerpt(&format!(
                    "readiness probe {ready_url} did not pass within 300s\n{excerpt}",
                ))),
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                cleanup_status: None,
                cleanup_error: None,
                log_path: None,
                run_session_id: None,
                pid: None,
                process_group_ids: Vec::new(),
                primary_port: None,
                primary_url: None,
                shadow_dir: None,
                readiness_state: None,
                cleanup_policy: None,
            },
            cleanup_outcome,
            &log_path,
        ));
    }

    let cleanup_outcome = cleanup.cleanup();
    let mut run = ImportRun {
        status: "passed".to_string(),
        phase: Some("readiness".to_string()),
        error_class: None,
        error_excerpt: None,
        command_mode: None,
        requires_host_shell: None,
        shell_kind: None,
        cleanup_status: None,
        cleanup_error: None,
        log_path: None,
        run_session_id: None,
        pid: None,
        process_group_ids: Vec::new(),
        primary_port: None,
        primary_url: None,
        shadow_dir: None,
        readiness_state: None,
        cleanup_policy: None,
    };
    if cleanup_outcome.error.is_some() {
        run.status = "failed".to_string();
        run.phase = Some("cleanup".to_string());
        run.error_class = Some("cleanup_failed".to_string());
        run.error_excerpt = cleanup_outcome.error.clone();
    }
    Ok(import_run_with_cleanup(run, cleanup_outcome, &log_path))
}

fn run_shadow_workspace_keep_alive(
    materialized: &mut MaterializedSource,
    recipe_toml: &str,
) -> Result<ImportRun> {
    let declared_port = infer_port(recipe_toml).unwrap_or(1111);
    let actual_port = if crate::runtime::port_manager::is_port_available(declared_port) {
        declared_port
    } else {
        (declared_port.saturating_add(1)..=u16::MAX)
            .find(|&p| crate::runtime::port_manager::is_port_available(p))
            .unwrap_or(declared_port)
    };
    let primary_url = format!("http://127.0.0.1:{actual_port}/");
    let run_session_id = new_import_run_id("preview", &materialized.source)?;
    let log_path = import_run_log_path(&run_session_id)?;

    let mut command = Command::new(std::env::current_exe()?);
    apply_probe_stdio(&mut command, &log_path)?;
    command
        .arg("run")
        .arg(&materialized.shadow_dir)
        .arg("--yes")
        .current_dir(&materialized.shadow_dir)
        .stdin(Stdio::null())
        .env(IMPORT_SESSION_ID_ENV, &run_session_id);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    if actual_port != declared_port {
        command.env("ATO_UI_OVERRIDE_PORT", actual_port.to_string());
    }
    if std::env::var("CAPSULE_ALLOW_UNSAFE").ok().as_deref() == Some("1") {
        command.arg("--dangerously-skip-permissions");
    }

    let mut child = command
        .spawn()
        .context("failed to spawn keep-alive shadow workspace")?;
    let pid = child.id() as i32;
    let mut observed_pgids = std::collections::BTreeSet::new();
    observed_pgids.extend(probe_pgids(pid, &materialized.shadow_dir));
    observed_pgids.insert(pid);
    let mut workload_pids: Vec<ImportPreviewWorkloadPid> = Vec::new();
    merge_workload_pids(
        &mut workload_pids,
        probe_workload_pids(pid, &materialized.shadow_dir),
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build HTTP client")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let mut readiness_state = "pending".to_string();
    while std::time::Instant::now() < deadline {
        observed_pgids.extend(probe_pgids(pid, &materialized.shadow_dir));
        merge_workload_pids(
            &mut workload_pids,
            probe_workload_pids(pid, &materialized.shadow_dir),
        );
        if let Ok(resp) = client.get(&primary_url).send()
            && resp.status().is_success()
        {
            readiness_state = "ready".to_string();
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            // A process that exits with success() is not necessarily a
            // failure: the "detached shadow" pattern (a launcher that spawns
            // a backgrounded workload via start_new_session/setsid and then
            // exits on its own) is intentional and supported — the actual
            // workload lives on in the detached child, not this pid. The
            // single readiness probe just above can race the child's listen
            // socket becoming reachable (observed under contended CI
            // runners), so before declaring `exited_before_readiness`, give
            // the workload a few quick extra chances to answer rather than
            // failing on the very first miss that happens to coincide with
            // the launcher's own exit.
            if status.success() {
                let mut became_ready = false;
                for _ in 0..10 {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    if let Ok(resp) = client.get(&primary_url).send()
                        && resp.status().is_success()
                    {
                        became_ready = true;
                        break;
                    }
                }
                if became_ready {
                    readiness_state = "ready".to_string();
                    break;
                }
            }
            let combined = read_error_excerpt_from_log(&log_path);
            let (phase, error_class) = if status.success() {
                ("readiness", "exited_before_readiness")
            } else {
                classify_run_failure(&combined)
            };
            return Ok(ImportRun {
                status: "failed".to_string(),
                phase: Some(phase.to_string()),
                error_class: Some(error_class.to_string()),
                error_excerpt: Some(redact_error_excerpt(&combined)),
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                cleanup_status: None,
                cleanup_error: None,
                log_path: Some(log_path.display().to_string()),
                run_session_id: Some(run_session_id),
                pid: Some(pid),
                process_group_ids: observed_pgids.into_iter().collect(),
                primary_port: Some(actual_port),
                primary_url: Some(primary_url),
                shadow_dir: Some(materialized.shadow_dir.display().to_string()),
                readiness_state: Some("exited".to_string()),
                cleanup_policy: Some("not_started".to_string()),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }

    if readiness_state != "ready" {
        let mut cleanup = ProbeRunGuard::new(child, materialized.shadow_dir.clone());
        cleanup.observed_pgids = observed_pgids;
        let cleanup_outcome = cleanup.cleanup();
        let excerpt = read_error_excerpt_from_log(&log_path);
        return Ok(import_run_with_cleanup(
            ImportRun {
                status: "failed".to_string(),
                phase: Some("readiness".to_string()),
                error_class: Some("readiness_timeout".to_string()),
                error_excerpt: Some(redact_error_excerpt(&format!(
                    "readiness probe {primary_url} did not pass within 300s\n{excerpt}",
                ))),
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                cleanup_status: None,
                cleanup_error: None,
                log_path: None,
                run_session_id: Some(run_session_id),
                pid: Some(pid),
                process_group_ids: cleanup.observed_pgids.iter().copied().collect(),
                primary_port: Some(actual_port),
                primary_url: Some(primary_url),
                shadow_dir: Some(materialized.shadow_dir.display().to_string()),
                readiness_state: Some("timeout".to_string()),
                cleanup_policy: Some("failed_probe_teardown".to_string()),
            },
            cleanup_outcome,
            &log_path,
        ));
    }

    observed_pgids.extend(probe_pgids(pid, &materialized.shadow_dir));
    merge_workload_pids(
        &mut workload_pids,
        probe_workload_pids(pid, &materialized.shadow_dir),
    );
    let now = now_unix_ms()?;
    let (owner_kind, owner_pid) = import_preview_owner();
    let session = ImportPreviewSession {
        run_session_id,
        owner_kind,
        owner_pid,
        owner_process_start_time_unix_ms: owner_pid
            .try_into()
            .ok()
            .and_then(capsule::state::session::process::process_start_time_unix_ms),
        ato_run_pid: pid,
        ato_run_process_start_time_unix_ms:
            capsule::state::session::process::process_start_time_unix_ms(child.id()),
        process_group_ids: observed_pgids.into_iter().collect(),
        workload_pids,
        primary_port: Some(actual_port),
        primary_url: Some(primary_url),
        shadow_dir: materialized.shadow_dir.clone(),
        log_path: log_path.clone(),
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        expires_at_unix_ms: None,
        readiness_state,
        cleanup_policy: "keep_until_explicit_stop".to_string(),
        last_sweep_status: None,
        last_sweep_error: None,
    };
    let process_manager = ProcessManager::new()?;
    if let Err(error) = process_manager.write_import_preview_session(&session) {
        let mut cleanup = ProbeRunGuard::new(child, materialized.shadow_dir.clone());
        cleanup.observed_pgids = session.process_group_ids.iter().copied().collect();
        let cleanup_outcome = cleanup.cleanup();
        return Ok(import_run_with_cleanup(
            ImportRun {
                status: "failed".to_string(),
                phase: Some("session_store".to_string()),
                error_class: Some("session_store_write_failed".to_string()),
                error_excerpt: Some(redact_error_excerpt(&error.to_string())),
                command_mode: None,
                requires_host_shell: None,
                shell_kind: None,
                cleanup_status: None,
                cleanup_error: None,
                log_path: None,
                run_session_id: Some(session.run_session_id),
                pid: Some(session.ato_run_pid),
                process_group_ids: session.process_group_ids,
                primary_port: session.primary_port,
                primary_url: session.primary_url,
                shadow_dir: Some(session.shadow_dir.display().to_string()),
                readiness_state: Some(session.readiness_state),
                cleanup_policy: Some("failed_session_store_teardown".to_string()),
            },
            cleanup_outcome,
            &log_path,
        ));
    }
    let stored_session = match process_manager.read_import_preview_session(&session.run_session_id)
    {
        Ok(Some(stored_session)) => stored_session,
        Ok(None) => {
            let mut cleanup = ProbeRunGuard::new(child, materialized.shadow_dir.clone());
            cleanup.observed_pgids = session.process_group_ids.iter().copied().collect();
            let _ = cleanup.cleanup();
            anyhow::bail!("import preview session missing after store write");
        }
        Err(error) => {
            let mut cleanup = ProbeRunGuard::new(child, materialized.shadow_dir.clone());
            cleanup.observed_pgids = session.process_group_ids.iter().copied().collect();
            let _ = cleanup.cleanup();
            return Err(error);
        }
    };
    materialized._workspace.keep = true;
    Ok(import_run_from_import_preview_session(&stored_session))
}

fn import_run_from_import_preview_session(session: &ImportPreviewSession) -> ImportRun {
    ImportRun {
        status: "running".to_string(),
        phase: Some("readiness".to_string()),
        error_class: None,
        error_excerpt: None,
        command_mode: None,
        requires_host_shell: None,
        shell_kind: None,
        cleanup_status: None,
        cleanup_error: None,
        log_path: Some(session.log_path.display().to_string()),
        run_session_id: Some(session.run_session_id.clone()),
        pid: Some(session.ato_run_pid),
        process_group_ids: session.process_group_ids.clone(),
        primary_port: session.primary_port,
        primary_url: session.primary_url.clone(),
        shadow_dir: Some(session.shadow_dir.display().to_string()),
        readiness_state: Some(session.readiness_state.clone()),
        cleanup_policy: Some(session.cleanup_policy.clone()),
    }
}

fn import_preview_owner() -> (String, i32) {
    if let Some(owner_pid) = std::env::var("ATO_DESKTOP_PARENT_PID")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    {
        return ("desktop".to_string(), owner_pid);
    }
    if std::env::var_os("ATO_DESKTOP_SESSION_ROOT").is_some() {
        return ("desktop".to_string(), std::process::id() as i32);
    }
    ("cli".to_string(), cli_owner_pid())
}

#[cfg(unix)]
fn cli_owner_pid() -> i32 {
    unsafe { libc::getppid() }
}

#[cfg(not(unix))]
fn cli_owner_pid() -> i32 {
    std::process::id() as i32
}

fn now_unix_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before UNIX_EPOCH")?
        .as_millis() as u64)
}

#[derive(Debug, Clone)]
struct ProbeCleanupOutcome {
    status: String,
    error: Option<String>,
}

struct ProbeRunGuard {
    child: Option<Child>,
    shadow_dir: PathBuf,
    observed_pgids: std::collections::BTreeSet<i32>,
    cleaned: bool,
}

impl ProbeRunGuard {
    fn new(child: Child, shadow_dir: PathBuf) -> Self {
        Self {
            child: Some(child),
            shadow_dir,
            observed_pgids: std::collections::BTreeSet::new(),
            cleaned: false,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("probe child accessed after cleanup")
    }

    fn cleanup(&mut self) -> ProbeCleanupOutcome {
        if self.cleaned {
            return ProbeCleanupOutcome {
                status: "already_cleaned".to_string(),
                error: None,
            };
        }
        self.cleaned = true;
        self.observe();
        let Some(mut child) = self.child.take() else {
            return ProbeCleanupOutcome {
                status: "already_cleaned".to_string(),
                error: None,
            };
        };

        let outcome =
            terminate_probe_process_tree(&mut child, &self.shadow_dir, &self.observed_pgids);
        let _ = child.wait();
        outcome
    }

    fn observe(&mut self) {
        #[cfg(unix)]
        if let Some(child) = self.child.as_ref() {
            self.observed_pgids
                .extend(probe_pgids(child.id() as i32, &self.shadow_dir));
        }
    }
}

impl Drop for ProbeRunGuard {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

fn import_run_with_cleanup(
    mut run: ImportRun,
    cleanup: ProbeCleanupOutcome,
    log_path: &Path,
) -> ImportRun {
    run.cleanup_status = Some(cleanup.status);
    run.cleanup_error = cleanup.error;
    run.log_path = Some(log_path.display().to_string());
    run
}

fn apply_probe_stdio(command: &mut Command, log_path: &Path) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_path.display()))?;
    command.stdout(Stdio::from(stdout));
    command.stderr(Stdio::from(stderr));
    Ok(())
}

fn new_import_run_id(prefix: &str, source: &ImportSource) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before UNIX_EPOCH")?
        .as_nanos();
    Ok(format!(
        "{}-{}-{}-{}-{now}",
        prefix,
        source.repo_namespace,
        source.repo_name,
        std::process::id()
    ))
}

fn import_run_log_path(run_id: &str) -> Result<PathBuf> {
    Ok(
        capsule::common::paths::ato_path_or_workspace_tmp(IMPORT_LOG_DIR)
            .join(format!("{run_id}.log")),
    )
}

fn read_error_excerpt_from_log(log_path: &Path) -> String {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
fn terminate_probe_process_tree(
    child: &mut Child,
    shadow_dir: &Path,
    observed_pgids: &std::collections::BTreeSet<i32>,
) -> ProbeCleanupOutcome {
    let root_pid = child.id() as i32;
    let mut pgids = observed_pgids.clone();
    pgids.extend(probe_pgids(root_pid, shadow_dir));
    pgids.insert(root_pid);
    let had_live_processes = any_pgid_alive(&pgids);

    if child.try_wait().ok().flatten().is_some() && !had_live_processes {
        return ProbeCleanupOutcome {
            status: "already_exited".to_string(),
            error: None,
        };
    }

    for pgid in &pgids {
        signal_process_group(*pgid, libc::SIGTERM);
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() && !any_pgid_alive(&pgids) {
            return ProbeCleanupOutcome {
                status: "terminated".to_string(),
                error: None,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    for pgid in probe_pgids(root_pid, shadow_dir) {
        pgids.insert(pgid);
    }
    for pgid in &pgids {
        signal_process_group(*pgid, libc::SIGKILL);
    }

    std::thread::sleep(Duration::from_millis(100));
    if any_pgid_alive(&pgids) {
        ProbeCleanupOutcome {
            status: "failed".to_string(),
            error: Some(format!(
                "failed to terminate import probe process groups {:?} for {}",
                pgids,
                shadow_dir.display()
            )),
        }
    } else {
        ProbeCleanupOutcome {
            status: "killed".to_string(),
            error: None,
        }
    }
}

#[cfg(not(unix))]
fn terminate_probe_process_tree(
    child: &mut Child,
    _shadow_dir: &Path,
    _observed_pgids: &std::collections::BTreeSet<i32>,
) -> ProbeCleanupOutcome {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return ProbeCleanupOutcome {
            status: "already_exited".to_string(),
            error: None,
        };
    }
    // `Child::kill` only reaches the immediate process, but on Windows the
    // probe command is routinely wrapped (cmd /C → python, venv launcher →
    // python, …); killing just the wrapper leaves the server grandchild
    // running, holding the probe port and the inherited stdio pipes. Take
    // down the whole tree.
    let tree_kill = std::process::Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(&tree_kill, Ok(status) if status.success()) {
        let _ = child.wait();
        return ProbeCleanupOutcome {
            status: "killed".to_string(),
            error: None,
        };
    }
    // taskkill can race a child that exited in the meantime; re-check before
    // falling back to the direct kill.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return ProbeCleanupOutcome {
            status: "already_exited".to_string(),
            error: None,
        };
    }
    match child.kill() {
        Ok(()) => {
            let _ = child.wait();
            ProbeCleanupOutcome {
                status: "killed".to_string(),
                error: None,
            }
        }
        Err(error) => ProbeCleanupOutcome {
            status: "failed".to_string(),
            error: Some(error.to_string()),
        },
    }
}

#[cfg(unix)]
fn probe_pgids(root_pid: i32, shadow_dir: &Path) -> std::collections::BTreeSet<i32> {
    let rows = process_rows();
    let mut pending = vec![root_pid];
    let mut descendants = std::collections::BTreeSet::new();
    while let Some(parent) = pending.pop() {
        for row in rows.iter().filter(|row| row.ppid == parent) {
            if descendants.insert(row.pid) {
                pending.push(row.pid);
            }
        }
    }

    let shadow_dir = shadow_dir.display().to_string();
    rows.into_iter()
        .filter(|row| {
            row.pid == root_pid
                || descendants.contains(&row.pid)
                || (!shadow_dir.is_empty() && row.command.contains(&shadow_dir))
        })
        .filter_map(|row| (row.pgid > 0).then_some(row.pgid))
        .collect()
}

#[cfg(not(unix))]
fn probe_pgids(_root_pid: i32, _shadow_dir: &Path) -> std::collections::BTreeSet<i32> {
    std::collections::BTreeSet::new()
}

/// Durable host pids of the workload subtree spawned under `root_pid`
/// (the keep-alive `ato run` supervisor): on Linux this is the `bwrap`
/// wrapper plus its namespaced descendants (the sandboxed server). Each
/// pid is paired with its OS start time so `ato stop` can SIGTERM/SIGKILL
/// it directly later without being fooled by pid reuse. The supervisor
/// itself is excluded — it is recorded separately as `ato_run_pid`. Rows
/// whose command references `shadow_dir` are also included to catch a
/// workload that has been reparented away from the supervisor between the
/// spawn and this probe.
#[cfg(unix)]
fn probe_workload_pids(root_pid: i32, shadow_dir: &Path) -> Vec<ImportPreviewWorkloadPid> {
    let rows = process_rows();
    let mut pending = vec![root_pid];
    let mut descendants = std::collections::BTreeSet::new();
    while let Some(parent) = pending.pop() {
        for row in rows.iter().filter(|row| row.ppid == parent) {
            if row.pid != root_pid && descendants.insert(row.pid) {
                pending.push(row.pid);
            }
        }
    }

    let shadow_dir = shadow_dir.display().to_string();
    rows.into_iter()
        .filter(|row| {
            row.pid > 0
                && row.pid != root_pid
                && (descendants.contains(&row.pid)
                    || (!shadow_dir.is_empty() && row.command.contains(&shadow_dir)))
        })
        .map(|row| ImportPreviewWorkloadPid {
            pid: row.pid,
            start_time_unix_ms: u32::try_from(row.pid)
                .ok()
                .and_then(capsule::state::session::process::process_start_time_unix_ms),
        })
        .collect()
}

#[cfg(not(unix))]
fn probe_workload_pids(_root_pid: i32, _shadow_dir: &Path) -> Vec<ImportPreviewWorkloadPid> {
    Vec::new()
}

/// Merge newly observed workload pids into the running set, keyed by pid so
/// repeated probes accumulate the full subtree (a child that appears only
/// after readiness, e.g. a forked worker, is still captured).
fn merge_workload_pids(
    into: &mut Vec<ImportPreviewWorkloadPid>,
    observed: Vec<ImportPreviewWorkloadPid>,
) {
    for candidate in observed {
        if let Some(existing) = into
            .iter_mut()
            .find(|existing| existing.pid == candidate.pid)
        {
            // Keep the first capture's identity, but backfill a missing start
            // time from a later probe so the pid-reuse guard (which matches on
            // start time) stays strong. An already-recorded `Some` is never
            // overwritten.
            if existing.start_time_unix_ms.is_none() {
                existing.start_time_unix_ms = candidate.start_time_unix_ms;
            }
        } else {
            into.push(candidate);
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct ProcessRow {
    pid: i32,
    ppid: i32,
    pgid: i32,
    command: String,
}

#[cfg(unix)]
fn process_rows() -> Vec<ProcessRow> {
    let output = Command::new("ps")
        .args(["-axo", "pid,ppid,pgid,command"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some(ProcessRow {
                pid: parts.next()?.parse().ok()?,
                ppid: parts.next()?.parse().ok()?,
                pgid: parts.next()?.parse().ok()?,
                command: parts.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

#[cfg(unix)]
fn any_pgid_alive(pgids: &std::collections::BTreeSet<i32>) -> bool {
    pgids
        .iter()
        .any(|pgid| unsafe { libc::kill(-*pgid, 0) == 0 })
}

#[cfg(unix)]
fn signal_process_group(pgid: i32, signal: libc::c_int) {
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

fn infer_port(recipe_toml: &str) -> Option<u16> {
    let parsed = recipe_toml.parse::<toml::Value>().ok()?;
    // Check top-level port
    if let Some(p) = parsed.get("port").and_then(|v| v.as_integer())
        && p > 0
        && p <= u16::MAX as i64
    {
        return Some(p as u16);
    }
    // Check target-level port
    let targets = parsed.get("targets").and_then(|v| v.as_table())?;
    for (_label, target) in targets {
        if let Some(p) = target.get("port").and_then(|v| v.as_integer())
            && p > 0
            && p <= u16::MAX as i64
        {
            return Some(p as u16);
        }
    }
    None
}

fn run_ato_shadow(shadow_dir: &Path, import_probe_id: &str) -> Result<Output> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("run")
        .arg(shadow_dir)
        .arg("--yes")
        .current_dir(shadow_dir)
        .env(IMPORT_PROBE_ID_ENV, import_probe_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if std::env::var_os(LOCAL_SOURCE_OVERRIDE_ENV).is_some() {
        command.arg("--no-build");
    }
    if std::env::var("CAPSULE_ALLOW_UNSAFE").ok().as_deref() == Some("1") {
        command.arg("--dangerously-skip-permissions");
    }
    let mut child = command
        .spawn()
        .context("failed to spawn shadow workspace")?;
    // Wait up to 120s for the run to complete, then time out gracefully.
    // Foreground servers (e.g. Bun) will keep running; we collect their
    // exit status or timeout output.
    let timeout = std::time::Duration::from_secs(120);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = read_all(child.stdout.take());
                let stderr = read_all(child.stderr.take());
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None if start.elapsed() >= timeout => {
                // On Windows `Child::kill` stops only the direct `ato run`
                // process; its server children would survive, holding ports
                // and the piped stdio (read_all below would block forever).
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                let _ = child.kill();
                let stdout = read_all(child.stdout.take());
                let stderr = read_all(child.stderr.take());
                bail!(
                    "shadow workspace run timed out after {}s\n---stdout---\n{}\n---stderr---\n{}",
                    timeout.as_secs(),
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                        .lines()
                        .take(80)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }
}

/// Attempt to resolve a verified recipe binding from the ato-api.
fn resolve_remote_recipe(source: &ImportSource) -> Result<Option<(String, String)>> {
    let api_base =
        std::env::var("ATO_STORE_API_URL").unwrap_or_else(|_| "https://api.ato.run".to_string());
    let api_base = api_base.trim_end_matches('/');
    let mut url = reqwest::Url::parse(&format!("{}/v1/source-imports/bindings/resolve", api_base))
        .context("invalid API base URL")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("source_url_normalized", &source.source_url_normalized);
        q.append_pair("revision_id", &source.revision_id);
        q.append_pair("platform_os", platform_os_label());
        q.append_pair("platform_arch", platform_arch_label());
        q.append_pair("subdir", &source.subdir);
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .context("remote recipe API request failed")?;

    if !response.status().is_success() {
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        anyhow::bail!("remote recipe API returned {}", response.status());
    }

    let resolved: ResolveBindingResponse = response
        .json()
        .context("failed to parse remote recipe response")?;

    match resolved.recipe {
        Some(recipe) if !recipe.recipe_toml.is_empty() => {
            Ok(Some((recipe.recipe_toml, recipe.recipe_hash)))
        }
        Some(_) => Ok(None),
        None => Ok(None),
    }
}

fn read_all(reader: Option<impl std::io::Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut r) = reader {
        let _ = r.read_to_end(&mut buf);
    }
    buf
}

fn import_run_from_output(output: &Output) -> ImportRun {
    if output.status.success() {
        return ImportRun {
            status: "passed".to_string(),
            phase: None,
            error_class: None,
            error_excerpt: None,
            command_mode: None,
            requires_host_shell: None,
            shell_kind: None,
            cleanup_status: None,
            cleanup_error: None,
            log_path: None,
            run_session_id: None,
            pid: None,
            process_group_ids: Vec::new(),
            primary_port: None,
            primary_url: None,
            shadow_dir: None,
            readiness_state: None,
            cleanup_policy: None,
        };
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = if stderr.trim().is_empty() {
        stdout.to_string()
    } else {
        stderr.to_string()
    };
    let (phase, error_class) = classify_run_failure(&combined);
    ImportRun {
        status: "failed".to_string(),
        phase: Some(phase.to_string()),
        error_class: Some(error_class.to_string()),
        error_excerpt: Some(redact_error_excerpt(&combined)),
        command_mode: None,
        requires_host_shell: None,
        shell_kind: None,
        cleanup_status: None,
        cleanup_error: None,
        log_path: None,
        run_session_id: None,
        pid: None,
        process_group_ids: Vec::new(),
        primary_port: None,
        primary_url: None,
        shadow_dir: None,
        readiness_state: None,
        cleanup_policy: None,
    }
}

fn classify_run_failure(text: &str) -> (&'static str, &'static str) {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("could not create shared memory segment")
        || (lowered.contains("shmget") && lowered.contains("no space left on device"))
    {
        return ("install", "postgres_shared_memory_exhausted");
    }
    if lowered.contains("distutils") {
        return ("install", "node_gyp_missing_distutils");
    }
    if lowered.contains("gyp err") || lowered.contains("gypERR") || lowered.contains("gyp:") {
        return ("install", "native_dependency_build_failed");
    }
    if lowered.contains("no loader") && lowered.contains(".node") {
        return ("build", "esbuild_native_loader");
    }
    if lowered.contains("missing_required_env")
        || lowered.contains("missing required env")
        || lowered.contains("required environment")
        || lowered.contains("missing:")
    {
        return ("run", "missing_required_env");
    }
    if lowered.contains("database_url")
        && (lowered.contains("not set")
            || lowered.contains("missing")
            || lowered.contains("undefined"))
    {
        return ("run", "database_url_missing");
    }
    if lowered.contains("module not found") || lowered.contains("cannot find module") {
        return ("run", "module_not_found");
    }
    if lowered.contains("prisma") {
        // Finer-grained Prisma error classification, ordered most-specific-first.
        if lowered.contains("environment variable not found: database_url") {
            return ("prestart", "prisma_database_url_missing");
        }
        if lowered.contains("can't reach database server") || lowered.contains("p1001") {
            return ("prestart", "prisma_database_connection_failed");
        }
        if lowered.contains("failed migrations") || lowered.contains("p3009") {
            return ("prestart", "prisma_failed_migration_state");
        }
        if lowered.contains("query engine")
            && (lowered.contains("could not locate") || lowered.contains("not found"))
        {
            return ("run", "prisma_query_engine_not_found");
        }
        if lowered.contains("schema.prisma")
            && (lowered.contains("not found") || lowered.contains("does not exist"))
        {
            return ("prestart", "prisma_schema_not_found");
        }
        if (lowered.contains("command not found") || lowered.contains("no such file"))
            && !lowered.contains("query engine")
        {
            return ("prestart", "prisma_cli_not_found");
        }
        if (lowered.contains("migration") || lowered.contains("migrate deploy"))
            && (lowered.contains("fail") || lowered.contains("error"))
            && !lowered.contains("build successful")
        {
            return ("prestart", "prisma_migration_sql_failed");
        }
    }
    if lowered.contains("missing_required_env")
        || lowered.contains("missing required env")
        || lowered.contains("required environment")
        || lowered.contains("missing:")
    {
        return ("run", "missing_required_env");
    }
    if lowered.contains("missing provider")
        || lowered.contains("provider not found")
        || lowered.contains("no provider")
    {
        return ("install", "missing_provider");
    }
    if lowered.contains("readiness") && lowered.contains("timeout") {
        return ("readiness", "readiness_timeout");
    }
    if lowered.contains("port") && lowered.contains("detect") {
        return ("readiness", "port_not_detected");
    }
    if lowered.contains("build") && !lowered.contains("build successful") {
        return ("build", "build_failed");
    }
    if lowered.contains("install")
        || lowered.contains("provision")
        || lowered.contains("lockdraft")
        || lowered.contains("lock incomplete")
    {
        return ("install", "install_failed");
    }
    if lowered.contains("run") || lowered.contains("exit status") || lowered.contains("failed") {
        return ("run", "run_failed");
    }
    ("run", "unknown")
}

fn redact_error_excerpt(text: &str) -> String {
    let mut output = text.to_string();
    for pattern in ["sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "AKIA"] {
        output = redact_token_prefix(&output, pattern);
    }
    output
        .lines()
        .take(100)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(MAX_ERROR_EXCERPT_BYTES)
        .collect()
}

fn redact_token_prefix(input: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(prefix) {
        out.push_str(&rest[..index]);
        out.push_str("[REDACTED]");
        let token_start = index + prefix.len();
        let token_tail = rest[token_start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
            .map(|end| token_start + end)
            .unwrap_or(rest.len());
        rest = &rest[token_tail..];
    }
    out.push_str(rest);
    out
}

fn infer_target_label(recipe_toml: &str) -> Option<String> {
    let parsed = recipe_toml.parse::<toml::Value>().ok()?;
    parsed
        .get("default_target")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            parsed
                .get("targets")
                .and_then(toml::Value::as_table)
                .and_then(|targets| targets.keys().next().cloned())
        })
}

/// Detect whether the recipe's run/build commands require shell execution.
/// Sets `command_mode`, `requires_host_shell`, and `shell_kind` on `ImportRun`
/// if any of the commands contain shell operators.
fn apply_shell_info(run: &mut ImportRun, recipe_toml: &str) {
    let commands = extract_command_strings(recipe_toml);
    let has_shell = commands.iter().any(|c| contains_shell_operators(c));
    if has_shell {
        run.command_mode = Some("shell".to_string());
        run.requires_host_shell = Some(true);
        run.shell_kind = Some("posix-sh".to_string());
    }
}

/// Extract `run`, `build`, `install`, and `prestart` string values from a
/// recipe TOML document. Handles both top-level and `[targets.<name>]` forms.
fn extract_command_strings(recipe_toml: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let parsed = match recipe_toml.parse::<toml::Value>() {
        Ok(v) => v,
        Err(_) => return commands,
    };

    // Top-level commands
    for key in &["run", "build", "install", "prestart"] {
        if let Some(v) = parsed.get(key).and_then(toml::Value::as_str) {
            commands.push(v.to_string());
        }
    }

    // Target-level commands
    if let Some(targets) = parsed.get("targets").and_then(toml::Value::as_table) {
        for (_label, target) in targets {
            for key in &["run", "build", "install", "prestart"] {
                if let Some(v) = target.get(key).and_then(toml::Value::as_str) {
                    commands.push(v.to_string());
                }
            }
        }
    }

    commands
}

fn platform_os_label() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    }
}

fn platform_arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => other,
    }
}

fn blake3_label(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn source_tree_hash_from_files(root: &Path) -> String {
    let mut entries = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .strip_prefix(root)
            .ok()
            .map(|relative| {
                relative
                    .components()
                    .any(|component| component.as_os_str() == ".git")
            })
            .unwrap_or(false)
        {
            continue;
        }
        entries.push(path.to_path_buf());
    }
    entries.sort();

    let mut hasher = blake3::Hasher::new();
    for path in entries {
        if let Ok(relative) = path.strip_prefix(root) {
            hasher.update(relative.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            if let Ok(bytes) = fs::read(&path) {
                hasher.update(&bytes);
            }
            hasher.update(b"\0");
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn source_identity_test_dir(prefix: &str) -> tempfile::TempDir {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&target).expect("create target test directory");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(target)
            .expect("create source identity test directory")
    }

    #[test]
    fn normalizes_supported_github_inputs() {
        for input in [
            "blinkospace/blinko",
            "github.com/blinkospace/blinko",
            "https://github.com/blinkospace/blinko",
        ] {
            let normalized = normalize_github_import_input(input).expect("normalized");
            assert_eq!(normalized.owner, "blinkospace");
            assert_eq!(normalized.repo, "blinko");
            assert_eq!(
                normalized.source_url_normalized,
                "https://github.com/blinkospace/blinko"
            );
        }
    }

    #[test]
    fn rejects_capsule_scheme_for_now() {
        let error = normalize_github_import_input("capsule://store/foo/bar")
            .expect_err("capsule scheme rejected");
        assert!(error.to_string().contains("capsule:// imports"));
    }

    #[test]
    fn github_fetch_is_pinned_to_resolved_commit() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let args = immutable_git_fetch_args(sha);
        assert_eq!(args.last().map(String::as_str), Some(sha));
        assert!(!args.iter().any(|arg| arg == "main"));
        assert!(args.windows(2).any(|pair| pair == ["--depth", "1"]));
    }

    #[cfg(unix)]
    #[test]
    fn copy_source_tree_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = source_identity_test_dir("import-symlink-");
        let source = root.path().join("source");
        let shadow = root.path().join("shadow");
        fs::create_dir_all(&source).expect("create source");
        fs::write(source.join("app.txt"), b"source bytes").expect("write source");
        symlink("app.txt", source.join("app-link.txt")).expect("create source symlink");

        let error = copy_source_tree(&source, &shadow).expect_err("symlink must fail closed");
        assert!(error.to_string().contains("unsupported symbolic link"));
    }

    #[test]
    fn shadow_identity_is_hashed_after_recipe_materialization() {
        let root = source_identity_test_dir("import-final-shadow-");
        let shadow = root.path().join("shadow");
        fs::create_dir_all(&shadow).expect("create shadow");
        fs::write(shadow.join("app.txt"), b"source bytes").expect("write source");
        let before = crate::application::execution_observers::hash_source_tree(&shadow)
            .expect("hash pre-recipe tree");
        let recipe = "schema_version = \"0.3\"\nname = \"final-shadow\"\n";

        let final_hash =
            materialize_shadow_recipe(&shadow, recipe).expect("materialize final shadow");

        assert_ne!(final_hash, before);
        assert_eq!(
            final_hash,
            crate::application::execution_observers::hash_source_tree(&shadow)
                .expect("hash receipt source tree")
        );
        assert_eq!(
            fs::read_to_string(shadow.join(CAPSULE_TOML)).expect("read materialized recipe"),
            recipe
        );
    }

    #[test]
    fn cleanup_import_workspace_root_skips_active_session() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let outcome = cleanup_import_workspace_root_with(
            &workspace,
            |_| Ok(Some("preview-123".to_string())),
            |_| Ok(None),
        )
        .expect("cleanup outcome");

        assert_eq!(
            outcome,
            ImportWorkspaceCleanupOutcome::SkippedActive {
                run_session_id: "preview-123".to_string()
            }
        );
        assert!(workspace.exists());
    }

    #[test]
    fn cleanup_import_workspace_root_removes_when_no_guards_match() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("shadow")).expect("workspace");

        let outcome = cleanup_import_workspace_root_with(&workspace, |_| Ok(None), |_| Ok(None))
            .expect("cleanup outcome");

        assert_eq!(outcome, ImportWorkspaceCleanupOutcome::Removed);
        assert!(!workspace.exists());
    }

    #[test]
    fn cleanup_import_workspace_root_skips_when_session_guard_fails() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let outcome = cleanup_import_workspace_root_with(
            &workspace,
            |_| Err(anyhow::anyhow!("session store unavailable")),
            |_| Ok(None),
        )
        .expect("cleanup outcome");

        assert_eq!(
            outcome,
            ImportWorkspaceCleanupOutcome::SkippedUnknown {
                reason: "session store unavailable".to_string()
            }
        );
        assert!(workspace.exists());
    }

    #[test]
    #[cfg(unix)]
    fn workspace_open_process_guard_detects_command_reference() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let rows = vec![ProcessRow {
            pid: 4242,
            ppid: 1,
            pgid: 4242,
            command: format!("python3 {}", workspace.display()),
        }];

        let open_process = workspace_open_process_guard_with(&workspace, &rows, |_| Ok(None))
            .expect("guard result")
            .expect("open process");

        assert_eq!(open_process.pid_or_pgid, "pid:4242");
        assert!(open_process.reason.contains("command references"));
    }

    #[test]
    #[cfg(unix)]
    fn workspace_open_process_guard_detects_cwd_reference() {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let shadow = workspace.join("shadow");
        fs::create_dir_all(&shadow).expect("workspace");
        let rows = vec![ProcessRow {
            pid: 5151,
            ppid: 1,
            pgid: 5151,
            command: "python3 server.py".to_string(),
        }];

        let open_process =
            workspace_open_process_guard_with(&workspace, &rows, |_| Ok(Some(shadow.clone())))
                .expect("guard result")
                .expect("open process");

        assert_eq!(open_process.pid_or_pgid, "pid:5151");
        assert!(open_process.reason.contains("cwd is under"));
    }

    #[test]
    fn classifies_distutils_failure() {
        let (phase, class) =
            classify_run_failure("ModuleNotFoundError: No module named 'distutils'");
        assert_eq!(phase, "install");
        assert_eq!(class, "node_gyp_missing_distutils");
    }

    #[test]
    fn postgres_shmget_failure_classified_as_postgres_shared_memory_exhausted() {
        let (phase, class) = classify_run_failure(
            "FATAL:  could not create shared memory segment: No space left on device\nDETAIL:  Failed system call was shmget(key=411762501, size=56, 03600).\ninitdb: removing data directory",
        );
        assert_eq!(phase, "install");
        assert_eq!(class, "postgres_shared_memory_exhausted");
    }

    #[test]
    fn postgres_shmget_failure_lowercase_classified() {
        let (phase, class) = classify_run_failure(
            "fatal:  could not create shared memory segment: no space left on device",
        );
        assert_eq!(phase, "install");
        assert_eq!(class, "postgres_shared_memory_exhausted");
    }

    #[test]
    fn unknown_provider_failure_falls_through_to_unknown() {
        let (_phase, class) = classify_run_failure(
            "some completely unfamiliar error text that does not match any known pattern",
        );
        assert_eq!(class, "unknown");
    }

    #[test]
    fn prisma_query_engine_not_found_classified() {
        let (_phase, class) = classify_run_failure(
            "prisma:error Invalid `prisma.config.findFirst()` Prisma Client could not locate the Query Engine for runtime \"darwin-arm64\".",
        );
        assert_eq!(class, "prisma_query_engine_not_found");
    }

    #[test]
    fn prisma_database_url_missing_classified() {
        let (_phase, class) =
            classify_run_failure("prisma:error Environment variable not found: DATABASE_URL");
        assert_eq!(class, "prisma_database_url_missing");
    }

    #[test]
    fn prisma_database_connection_failed_classified() {
        let (_phase, class) =
            classify_run_failure("prisma:error Can't reach database server at `localhost:5432`");
        assert_eq!(class, "prisma_database_connection_failed");
    }

    #[test]
    fn prisma_failed_migration_state_classified() {
        let (_phase, class) = classify_run_failure(
            "prisma P3009: found failed migrations in the target database, new migrations cannot be applied",
        );
        assert_eq!(class, "prisma_failed_migration_state");
    }

    #[test]
    fn prisma_schema_not_found_classified() {
        let (_phase, class) =
            classify_run_failure("Prisma schema file prisma/schema.prisma not found");
        assert_eq!(class, "prisma_schema_not_found");
    }

    #[test]
    fn prisma_migration_sql_still_caught_as_fallback() {
        let (_phase, class) = classify_run_failure(
            "prisma error: migration 20251231140909_add_fonts_table failed to apply",
        );
        assert_eq!(class, "prisma_migration_sql_failed");
    }

    #[test]
    fn redacts_secret_like_excerpts() {
        let redacted =
            redact_error_excerpt("token ghp_abcdefghijklmnopqrstuvwxyz and sk-abcdefghi");
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
        assert!(!redacted.contains("sk-abcdefghi"));
    }

    #[test]
    fn merge_workload_pids_dedups_by_pid_and_preserves_order() {
        let mut acc = Vec::new();
        merge_workload_pids(
            &mut acc,
            vec![
                ImportPreviewWorkloadPid {
                    pid: 100,
                    start_time_unix_ms: Some(11),
                },
                ImportPreviewWorkloadPid {
                    pid: 200,
                    start_time_unix_ms: Some(22),
                },
            ],
        );
        // Second probe: 100 already present (a re-observed start time must NOT
        // overwrite the first capture), 300 is new.
        merge_workload_pids(
            &mut acc,
            vec![
                ImportPreviewWorkloadPid {
                    pid: 100,
                    start_time_unix_ms: Some(99),
                },
                ImportPreviewWorkloadPid {
                    pid: 300,
                    start_time_unix_ms: Some(33),
                },
            ],
        );
        assert_eq!(
            acc,
            vec![
                ImportPreviewWorkloadPid {
                    pid: 100,
                    start_time_unix_ms: Some(11),
                },
                ImportPreviewWorkloadPid {
                    pid: 200,
                    start_time_unix_ms: Some(22),
                },
                ImportPreviewWorkloadPid {
                    pid: 300,
                    start_time_unix_ms: Some(33),
                },
            ]
        );
    }

    #[test]
    fn merge_workload_pids_backfills_missing_start_time_but_keeps_existing() {
        let mut acc = vec![
            // First probe captured the pid before its start time was readable.
            ImportPreviewWorkloadPid {
                pid: 100,
                start_time_unix_ms: None,
            },
            // This one already has a start time; a later probe must not change it.
            ImportPreviewWorkloadPid {
                pid: 200,
                start_time_unix_ms: Some(22),
            },
        ];
        merge_workload_pids(
            &mut acc,
            vec![
                // Later probe now sees pid 100's start time -> backfill None -> Some.
                ImportPreviewWorkloadPid {
                    pid: 100,
                    start_time_unix_ms: Some(55),
                },
                // Re-observed start time for pid 200 must NOT overwrite Some(22).
                ImportPreviewWorkloadPid {
                    pid: 200,
                    start_time_unix_ms: Some(99),
                },
            ],
        );
        assert_eq!(
            acc,
            vec![
                ImportPreviewWorkloadPid {
                    pid: 100,
                    start_time_unix_ms: Some(55),
                },
                ImportPreviewWorkloadPid {
                    pid: 200,
                    start_time_unix_ms: Some(22),
                },
            ]
        );
    }
}
