use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::cli::ImportArgs;

const GITHUB_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "ato-cli-source-import";
const IMPORT_ROOT_DIR: &str = ".tmp/ato-import";
const CAPSULE_TOML: &str = "capsule.toml";
const MAX_ERROR_EXCERPT_BYTES: usize = 1200;
const LOCAL_SOURCE_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_SOURCE_OVERRIDE";
const LOCAL_REVISION_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_REVISION_ID";
const LOCAL_TREE_OVERRIDE_ENV: &str = "ATO_IMPORT_LOCAL_TREE_HASH";
const KEEP_WORKSPACE_ENV: &str = "ATO_IMPORT_KEEP_WORKSPACE";

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
}

#[derive(Debug, Serialize)]
struct ImportOutput {
    source: ImportSource,
    recipe: ImportRecipe,
    run: ImportRun,
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
}

impl Drop for ImportWorkspace {
    fn drop(&mut self) {
        if std::env::var_os(KEEP_WORKSPACE_ENV).is_some() {
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn execute_import_command(args: ImportArgs) -> Result<()> {
    let input = normalize_github_import_input(&args.repo)?;
    let materialized = materialize_source(&input)?;
    let (recipe_toml, origin) =
        load_or_infer_recipe(&args, &materialized.checkout_dir, &input.repo)?;
    let recipe_hash = blake3_label(recipe_toml.as_bytes());
    let target_label = infer_target_label(&recipe_toml);
    let run = if args.run {
        run_shadow_workspace(&materialized, &recipe_toml)?
    } else {
        ImportRun {
            status: "not_run".to_string(),
            phase: None,
            error_class: None,
            error_excerpt: None,
        }
    };

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
        clone_public_github_repo(input, &checkout_dir)?;
        checkout_resolved_revision(&checkout_dir, &resolved.revision_id)?;
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
    let root = std::env::current_dir()?.join(IMPORT_ROOT_DIR).join(format!(
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

fn clone_public_github_repo(input: &NormalizedGitHubInput, target_dir: &Path) -> Result<()> {
    let parent = target_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("checkout target has no parent"))?;
    fs::create_dir_all(parent)?;
    let clone_url = format!("{}.git", input.source_url_normalized);
    let output = Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(parent)
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(&clone_url)
        .arg(target_dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run git clone for {clone_url}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "failed to clone {}: {}",
        clone_url,
        String::from_utf8_lossy(&output.stderr).trim()
    )
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
        }
    }
    Ok(())
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

fn run_shadow_workspace(materialized: &MaterializedSource, recipe_toml: &str) -> Result<ImportRun> {
    let shadow_manifest = materialized.shadow_dir.join(CAPSULE_TOML);
    fs::write(&shadow_manifest, recipe_toml)
        .with_context(|| format!("failed to write {}", shadow_manifest.display()))?;

    let output = run_ato_shadow(&materialized.shadow_dir)?;
    Ok(import_run_from_output(&output))
}

fn run_ato_shadow(shadow_dir: &Path) -> Result<Output> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("run")
        .arg(shadow_dir)
        .arg("--yes")
        .current_dir(shadow_dir)
        .stdin(Stdio::null());
    if std::env::var_os(LOCAL_SOURCE_OVERRIDE_ENV).is_some() {
        command.arg("--no-build");
    }
    if std::env::var("CAPSULE_ALLOW_UNSAFE").ok().as_deref() == Some("1") {
        command.arg("--dangerously-skip-permissions");
    }
    Ok(command.output().context("failed to run shadow workspace")?)
}

fn import_run_from_output(output: &Output) -> ImportRun {
    if output.status.success() {
        return ImportRun {
            status: "passed".to_string(),
            phase: None,
            error_class: None,
            error_excerpt: None,
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
    }
}

fn classify_run_failure(text: &str) -> (&'static str, &'static str) {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("distutils") {
        return ("install", "node_gyp_missing_distutils");
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
    if lowered.contains("build") {
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
        .take(40)
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
    fn classifies_distutils_failure() {
        let (phase, class) =
            classify_run_failure("ModuleNotFoundError: No module named 'distutils'");
        assert_eq!(phase, "install");
        assert_eq!(class, "node_gyp_missing_distutils");
    }

    #[test]
    fn redacts_secret_like_excerpts() {
        let redacted =
            redact_error_excerpt("token ghp_abcdefghijklmnopqrstuvwxyz and sk-abcdefghi");
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
        assert!(!redacted.contains("sk-abcdefghi"));
    }
}
