use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::proc_util::CommandNoWindowExt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use capsule::capsule::manifest::blake3_digest;
use capsule::common::paths::ato_path;
use capsule::contract::manifest::load_manifest;
use capsule::router::{ExecutionProfile, route_manifest};
use capsule::types::{CapsuleManifest, ValidationMode};
use serde::Serialize;

use crate::state::{GuestRoute, LocalManifestRoute, ManifestSource};

#[derive(Debug, Clone)]
pub(crate) struct GithubDraftRequest {
    pub(crate) repo: String,
    pub(crate) title: String,
    pub(crate) manifest_toml: String,
    pub(crate) manifest_source: ManifestSource,
    pub(crate) requested_ref: String,
}

#[derive(Debug, Serialize)]
struct DraftMetadata {
    source_handle: String,
    requested_ref: String,
    resolved_commit: String,
    repo_url: String,
    manifest_source: String,
    manifest_hash: String,
    draft_id: String,
    created_at: String,
}

pub(crate) fn prepare_github_manifest_draft(request: GithubDraftRequest) -> Result<GuestRoute> {
    let repo = crate::source_import_session::normalize_github_import_input(&request.repo)
        .with_context(|| format!("invalid GitHub repository {}", request.repo))?;
    let source_handle = format!("github.com/{}/{}", repo.owner, repo.repo);
    let requested_ref = if request.requested_ref.trim().is_empty() {
        "HEAD".to_string()
    } else {
        request.requested_ref.trim().to_string()
    };

    let resolved_commit = resolve_git_ref(&repo.clone_url, &requested_ref)?;
    if !is_full_sha(&resolved_commit) {
        bail!("resolved_commit must be a 40-character SHA, got {resolved_commit}");
    }

    let source_cache = materialize_source_cache(&repo, &resolved_commit)?;

    toml::from_str::<toml::Value>(&request.manifest_toml).context("capsule.toml parse failed")?;
    let manifest = CapsuleManifest::from_toml(&request.manifest_toml)
        .map_err(|err| anyhow!("capsule manifest schema parse failed: {err}"))?;
    manifest
        .validate_for_mode(ValidationMode::Strict)
        .map_err(|errors| {
            anyhow!(
                "capsule manifest validation failed: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;

    let manifest_hash = blake3_digest(request.manifest_toml.as_bytes());
    let draft_id = new_draft_id(&manifest_hash);
    let draft_root = draft_root(&repo.owner, &repo.repo, &resolved_commit, &draft_id)?;
    copy_dir_recursive(&source_cache, &draft_root)
        .with_context(|| format!("failed to project source into {}", draft_root.display()))?;

    let manifest_path = draft_root.join("capsule.toml");
    atomic_write(&manifest_path, request.manifest_toml.as_bytes())?;

    let metadata = DraftMetadata {
        source_handle: source_handle.clone(),
        requested_ref: requested_ref.clone(),
        resolved_commit: resolved_commit.clone(),
        repo_url: repo.source_url_normalized.clone(),
        manifest_source: request.manifest_source.as_str().to_string(),
        manifest_hash: manifest_hash.clone(),
        draft_id: draft_id.clone(),
        created_at: current_unix_millis().to_string(),
    };
    let metadata_json = serde_json::to_vec_pretty(&metadata)?;
    atomic_write(&draft_root.join("draft.json"), &metadata_json)?;

    let _loaded = load_manifest(&manifest_path)
        .with_context(|| format!("failed to load draft manifest {}", manifest_path.display()))?;
    let decision = route_manifest(&manifest_path, ExecutionProfile::Dev, None)
        .with_context(|| format!("failed to route draft manifest {}", manifest_path.display()))?;
    let plan = &decision.plan;
    validate_entrypoint(
        plan.execution_working_directory(),
        plan.execution_entrypoint(),
    )?;

    let default_target = plan.default_target_label().unwrap_or_default();
    let selected_target = plan.selected_target_label().to_string();
    let runtime_or_driver = plan
        .execution_driver()
        .or_else(|| plan.execution_runtime())
        .unwrap_or_default();
    let run_command = plan
        .execution_run_command()
        .or_else(|| plan.execution_entrypoint())
        .unwrap_or_default();
    tracing::info!(
        launch_input.kind = "local_manifest_path",
        source_handle = %source_handle,
        requested_ref = %requested_ref,
        resolved_commit = %resolved_commit,
        manifest_source = request.manifest_source.as_str(),
        manifest_path = %manifest_path.display(),
        manifest_hash = %manifest_hash,
        default_target = %default_target,
        selected_target = %selected_target,
        runtime_or_driver = %runtime_or_driver,
        run_command = %run_command,
        port = ?plan.execution_port(),
        draft_id = %draft_id,
        "desktop GitHub draft manifest prepared"
    );

    Ok(GuestRoute::LocalManifest(LocalManifestRoute {
        manifest_path,
        source_handle,
        label: request.title,
        requested_ref,
        resolved_commit,
        manifest_source: request.manifest_source,
        manifest_hash,
        draft_id,
    }))
}

fn resolve_git_ref(clone_url: &str, requested_ref: &str) -> Result<String> {
    let output = Command::new("git")
        .no_console_window()
        .args(["ls-remote", clone_url, requested_ref])
        .output()
        .with_context(|| format!("failed to resolve GitHub ref {requested_ref}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("git ls-remote failed for {requested_ref}: {stderr}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sha = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|candidate| is_full_sha(candidate))
        .ok_or_else(|| anyhow!("could not resolve {requested_ref} to a commit SHA"))?;
    Ok(sha.to_string())
}

fn materialize_source_cache(
    repo: &crate::source_import_session::NormalizedGitHubRepo,
    resolved_commit: &str,
) -> Result<PathBuf> {
    let cache_dir = ato_path(format!(
        "external-capsules/github/{}/{}/{}",
        repo.owner, repo.repo, resolved_commit
    ))?;
    if cache_dir.join(".git").is_dir() {
        return Ok(cache_dir);
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| anyhow!("source cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp_dir = parent.join(format!(
        ".clone-{}-{}",
        current_unix_millis(),
        std::process::id()
    ));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }

    run_git(
        Command::new("git")
            .arg("clone")
            .arg("--no-checkout")
            .arg(&repo.clone_url)
            .arg(&temp_dir),
        "git clone",
    )?;
    run_git(
        Command::new("git").arg("-C").arg(&temp_dir).args([
            "fetch",
            "--depth",
            "1",
            "origin",
            resolved_commit,
        ]),
        "git fetch resolved commit",
    )?;
    run_git(
        Command::new("git").arg("-C").arg(&temp_dir).args([
            "checkout",
            "--detach",
            resolved_commit,
        ]),
        "git checkout resolved commit",
    )?;

    match fs::rename(&temp_dir, &cache_dir) {
        Ok(()) => Ok(cache_dir),
        Err(err) if cache_dir.exists() => {
            let _ = fs::remove_dir_all(&temp_dir);
            tracing::debug!(
                error = %err,
                cache_dir = %cache_dir.display(),
                "source cache appeared during clone; using existing cache"
            );
            Ok(cache_dir)
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to move source cache {} to {}",
                temp_dir.display(),
                cache_dir.display()
            )
        }),
    }
}

fn run_git(cmd: &mut Command, label: &str) -> Result<()> {
    let output = cmd
        .no_console_window()
        .output()
        .with_context(|| format!("failed to run {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    bail!("{label} failed: stderr={stderr} stdout={stdout}");
}

fn draft_root(owner: &str, repo: &str, resolved_commit: &str, draft_id: &str) -> Result<PathBuf> {
    Ok(ato_path(format!(
        "desktop/github-drafts/{owner}/{repo}/{resolved_commit}/{draft_id}"
    ))?)
}

fn new_draft_id(manifest_hash: &str) -> String {
    let hash_prefix =
        sanitize_draft_path_segment(&manifest_hash.chars().take(12).collect::<String>());
    format!(
        "{}-{}-{}",
        current_unix_millis(),
        std::process::id(),
        hash_prefix
    )
}

fn sanitize_draft_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn current_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("draft"),
        std::process::id()
    ));
    fs::write(&tmp, bytes)
        .with_context(|| format!("failed to write temporary file {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to atomically write {}", path.display()))?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let file_name = entry.file_name();
        if file_name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        let metadata = fs::symlink_metadata(&src_path)?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!("copy {} to {}", src_path.display(), dst_path.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &dst_path,
                    fs::Permissions::from_mode(metadata.permissions().mode()),
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src)?;
    std::os::unix::fs::symlink(target, dst)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src)?;
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(target, dst)?;
    } else {
        std::os::windows::fs::symlink_file(target, dst)?;
    }
    Ok(())
}

fn validate_entrypoint(working_dir: PathBuf, entrypoint: Option<String>) -> Result<()> {
    let Some(entrypoint) = entrypoint else {
        return Ok(());
    };
    let trimmed = entrypoint.trim();
    if trimmed.is_empty()
        || trimmed.contains(' ')
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        return Ok(());
    }
    let looks_like_path = trimmed.contains('/')
        || trimmed.starts_with("./")
        || Path::new(trimmed).extension().is_some();
    if looks_like_path && !working_dir.join(trimmed).exists() {
        bail!(
            "entrypoint {trimmed} does not exist under {}",
            working_dir.display()
        );
    }
    Ok(())
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_id_sanitizes_manifest_hash_prefix_for_path_segments() {
        let draft_id = new_draft_id("blake3:aa8f929fc71dfa9c");

        assert!(draft_id.ends_with("blake3-aa8f9"));
        assert!(!draft_id.contains(':'));
    }
}
