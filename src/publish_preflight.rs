use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

pub const CI_WORKFLOW_REL_PATH: &str = ".github/workflows/ato-publish.yml";

#[derive(Debug, Clone, Serialize)]
pub struct GitCheckResult {
    pub inside_work_tree: bool,
    pub origin: Option<String>,
    pub manifest_repository: Option<String>,
    pub repository_match: Option<bool>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiWorkflowCheckResult {
    pub path: String,
    pub exists: bool,
    pub has_oidc_permission: bool,
    pub has_tag_trigger: bool,
    pub has_checksum_verification: bool,
}

pub fn find_manifest_repository(manifest_raw: &str) -> Option<String> {
    let parsed = toml::from_str::<toml::Value>(manifest_raw).ok()?;
    parsed
        .get("metadata")
        .and_then(|v| v.get("repository"))
        .and_then(|v| v.as_str())
        .or_else(|| parsed.get("repository").and_then(|v| v.as_str()))
        .map(|v| v.to_string())
}

pub fn validate_ci_workflow(cwd: &Path) -> Result<CiWorkflowCheckResult> {
    let path = cwd.join(CI_WORKFLOW_REL_PATH);
    if !path.exists() {
        anyhow::bail!(
            "CI workflow not found: {}. Run `ato gen-ci` first.",
            path.display()
        );
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read CI workflow: {}", path.display()))?;

    let has_oidc_permission = content.contains("id-token: write");
    if !has_oidc_permission {
        anyhow::bail!(
            "CI workflow is missing `id-token: write` permission. Regenerate with `ato gen-ci`."
        );
    }

    let has_tag_trigger =
        content.contains("push:") && content.contains("tags:") && content.contains("v*.*.*");
    if !has_tag_trigger {
        anyhow::bail!(
            "CI workflow is missing tag-based trigger (`on.push.tags`). Regenerate with `ato gen-ci`."
        );
    }

    let has_checksum_verification =
        content.contains("ATO_VERSION") && content.contains("sha256sum -c");
    if !has_checksum_verification {
        anyhow::bail!(
            "CI workflow is missing pinned checksum verification. Regenerate with `ato gen-ci`."
        );
    }

    Ok(CiWorkflowCheckResult {
        path: CI_WORKFLOW_REL_PATH.to_string(),
        exists: true,
        has_oidc_permission,
        has_tag_trigger,
        has_checksum_verification,
    })
}

pub fn run_git_checks(manifest_repo: Option<&str>) -> Result<GitCheckResult> {
    let inside = run_git(&["rev-parse", "--is-inside-work-tree"])
        .ok()
        .map(|v| v == "true")
        .unwrap_or(false);
    if !inside {
        anyhow::bail!(
            "Current directory is not inside a Git repository.\nRun `git init` first, or execute `ato publish --dry-run` from an existing Git repository root."
        );
    }

    let origin_raw = run_git(&["remote", "get-url", "origin"]).ok();
    let origin_norm = origin_raw.as_deref().and_then(normalize_origin_to_repo);

    let manifest_repository = manifest_repo.map(normalize_repository_value).transpose()?;

    if let (Some(expected_repo), Some(actual_repo)) =
        (manifest_repository.as_deref(), origin_norm.as_deref())
    {
        if expected_repo != actual_repo {
            anyhow::bail!(
                "Repository mismatch: capsule.toml repository '{}' != git origin '{}'",
                expected_repo,
                actual_repo
            );
        }
    }

    if manifest_repository.is_some() && origin_norm.is_none() {
        anyhow::bail!(
            "capsule.toml has repository but `git remote origin` is missing or not a GitHub repository"
        );
    }

    let dirty = run_git(&["status", "--porcelain"])
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    Ok(GitCheckResult {
        inside_work_tree: true,
        origin: origin_norm.clone(),
        manifest_repository: manifest_repository.clone(),
        repository_match: manifest_repository
            .as_ref()
            .map(|repo| origin_norm.as_ref().map(|o| o == repo).unwrap_or(false)),
        dirty,
    })
}

pub fn git_current_branch() -> Result<String> {
    run_git(&["rev-parse", "--abbrev-ref", "HEAD"])
}

pub fn run_git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed with status {}",
            args.join(" "),
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn normalize_repository_value(value: &str) -> Result<String> {
    let raw = value.trim();
    if raw.is_empty() {
        anyhow::bail!("repository is empty");
    }
    if raw.contains("://") {
        let parsed = reqwest::Url::parse(raw).with_context(|| "Invalid repository URL")?;
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        if host != "github.com" && host != "www.github.com" {
            anyhow::bail!("repository must point to github.com");
        }
        let mut segs = parsed
            .path_segments()
            .context("repository URL has no path")?;
        let owner = segs.next().unwrap_or("").trim();
        let repo = segs.next().unwrap_or("").trim_end_matches(".git").trim();
        if owner.is_empty() || repo.is_empty() {
            anyhow::bail!("repository URL must include owner/repo");
        }
        return Ok(format!("{}/{}", owner, repo));
    }

    let raw = raw.trim_end_matches('/');
    let raw = raw
        .strip_prefix("github.com/")
        .or_else(|| raw.strip_prefix("www.github.com/"))
        .unwrap_or(raw);
    let mut it = raw.split('/');
    let owner = it.next().unwrap_or("").trim();
    let repo = it.next().unwrap_or("").trim_end_matches(".git").trim();
    if owner.is_empty() || repo.is_empty() || it.next().is_some() {
        anyhow::bail!("repository must be 'owner/repo', 'github.com/owner/repo', or GitHub URL");
    }
    Ok(format!("{}/{}", owner, repo))
}

pub fn normalize_origin_to_repo(origin: &str) -> Option<String> {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(without_prefix) = trimmed.strip_prefix("git@github.com:") {
        let repo = without_prefix.trim_end_matches(".git").trim();
        return if repo.split('/').count() == 2 {
            Some(repo.to_string())
        } else {
            None
        };
    }

    normalize_repository_value(trimmed).ok()
}
