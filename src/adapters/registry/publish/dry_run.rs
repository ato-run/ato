use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::publish_preflight::{
    self, find_manifest_repository, CiWorkflowCheckResult, GitCheckResult,
};

#[derive(Debug, Clone)]
pub struct PublishDryRunArgs {
    pub json_output: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishDryRunResult {
    pub capsule_name: String,
    pub version: String,
    pub artifact_path: PathBuf,
    pub artifact_size_bytes: u64,
    pub git: GitCheckResult,
    pub ci_workflow: CiWorkflowCheckResult,
}

pub async fn execute(args: PublishDryRunArgs) -> Result<PublishDryRunResult> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    let manifest_raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml: {}", err))?;

    let manifest_repo = find_manifest_repository(&manifest_raw);
    let git = publish_preflight::run_git_checks(manifest_repo.as_deref())?;
    let ci_workflow = publish_preflight::validate_ci_workflow(&cwd)?;

    if !args.json_output {
        eprintln!("🔍 Validating capsule.toml... OK");
        eprintln!();
        eprintln!("🐙 Performing Repository Checks...");
        eprintln!("   ✔ Git repository detected.");
        if let Some(origin) = &git.origin {
            eprintln!("   ✔ Origin: {}", origin);
        }
        if let Some(true) = git.repository_match {
            if let Some(repo) = &git.manifest_repository {
                eprintln!("   ✔ Remote origin matches '{}'.", repo);
            }
        } else if git.manifest_repository.is_none() {
            eprintln!("   ⚠️  No repository set in capsule.toml ([metadata].repository).");
        }
        if git.dirty {
            eprintln!("   ⚠️  Warning: Uncommitted changes detected.");
            eprintln!("      CI builds only committed code. Local result may differ.");
        } else {
            eprintln!("   ✔ Working tree is clean.");
        }

        eprintln!();
        eprintln!("🛡️  Validating CI Workflow (.github/workflows/ato-publish.yml)...");
        eprintln!("   ✔ Workflow file exists.");
        eprintln!("   ✔ OIDC permissions are configured (id-token: write).");
        eprintln!("   ✔ Secure binary verification is enabled (sha256sum).");
        eprintln!("   ✔ Tag-based trigger is configured.");
        eprintln!();
        eprintln!("📦 Simulating deterministic build...");
    }

    let version = if manifest.version.trim().is_empty() {
        "auto"
    } else {
        manifest.version.trim()
    };

    let artifact_path =
        crate::publish_ci::build_capsule_artifact(&manifest_path, &manifest.name, version)
            .with_context(|| "Failed to build local dry-run artifact")?;
    let artifact_size_bytes = fs::metadata(&artifact_path)
        .with_context(|| format!("Failed to inspect {}", artifact_path.display()))?
        .len();

    Ok(PublishDryRunResult {
        capsule_name: manifest.name,
        version: version.to_string(),
        artifact_path,
        artifact_size_bytes,
        git,
        ci_workflow,
    })
}
