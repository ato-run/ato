use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::CapsuleReporter;

const ATO_RELEASE_BASE_URL: &str = "https://dl.ato.run";
const ENV_RELEASE_BASE_URL: &str = "ATO_RELEASE_BASE_URL";
const WORKFLOW_REL_PATH: &str = ".github/workflows/ato-publish.yml";
const TARGET_ARCHIVE: &str = "ato-cli-x86_64-unknown-linux-gnu.tar.xz";
const VERSIONED_CHECKSUM_PATH: &str = "/ato/releases/{version}/SHA256SUMS";
const LATEST_CHECKSUM_PATH: &str = "/ato/latest/SHA256SUMS";

#[derive(Debug, Clone)]
pub struct WorkflowSyncOutcome {
    pub workflow_path: PathBuf,
    pub changed: bool,
    pub created: bool,
    pub used_latest_fallback: bool,
}

pub fn execute(reporter: std::sync::Arc<crate::reporters::CliReporter>) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;

    if !cwd.join(".git").exists() {
        futures::executor::block_on(reporter.warn(
            "⚠️  .git directory was not found. Continuing, but this command is intended for git repositories."
                .to_string(),
        ))?;
    }

    let outcome = sync_workflow_in_dir(&cwd)?;
    if outcome.used_latest_fallback {
        futures::executor::block_on(
            reporter.warn(
                "⚠️  Versioned release checksum was unavailable; fell back to latest channel."
                    .to_string(),
            ),
        )?;
    }

    if !outcome.changed {
        futures::executor::block_on(reporter.notify(format!(
            "✅ CI workflow is already up-to-date: {}",
            outcome.workflow_path.display()
        )))?;
    } else {
        let action = if outcome.created {
            "Generated"
        } else {
            "Updated"
        };
        futures::executor::block_on(reporter.notify(format!(
            "✅ {} CI workflow: {}",
            action,
            outcome.workflow_path.display()
        )))?;
        futures::executor::block_on(
            reporter.notify("   Next step: commit and push this workflow file.".to_string()),
        )?;
    }
    futures::executor::block_on(reporter.notify(
        "🔐 Workflow uses keyless OIDC CI publish (no signing key secret required).".to_string(),
    ))?;
    Ok(())
}

pub fn sync_workflow_in_dir(cwd: &Path) -> Result<WorkflowSyncOutcome> {
    let manifest_path = cwd.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml not found in current directory: {}",
            cwd.display()
        );
    }

    let workflow_path = cwd.join(WORKFLOW_REL_PATH);
    let ato_version = env!("CARGO_PKG_VERSION");
    let release_base_url = resolve_release_base_url();
    let checksum_resolution =
        resolve_release_checksum(&release_base_url, ato_version, TARGET_ARCHIVE)?;
    let workflow = render_workflow(
        &release_base_url,
        ato_version,
        &checksum_resolution.checksum,
        &checksum_resolution.archive_path,
    );

    let previous = fs::read_to_string(&workflow_path).ok();
    let changed = previous.as_deref() != Some(workflow.as_str());
    let created = changed && previous.is_none();

    if changed {
        if let Some(parent) = workflow_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create workflow directory: {}", parent.display())
            })?;
        }
        fs::write(&workflow_path, workflow)
            .with_context(|| format!("Failed to write workflow: {}", workflow_path.display()))?;
    }

    Ok(WorkflowSyncOutcome {
        workflow_path,
        changed,
        created,
        used_latest_fallback: checksum_resolution.used_latest_fallback,
    })
}

fn resolve_release_base_url() -> String {
    std::env::var(ENV_RELEASE_BASE_URL)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ATO_RELEASE_BASE_URL.to_string())
}

struct ChecksumResolution {
    checksum: String,
    archive_path: String,
    used_latest_fallback: bool,
}

fn resolve_release_checksum(
    release_base_url: &str,
    version: &str,
    target_archive: &str,
) -> Result<ChecksumResolution> {
    let versioned_path = VERSIONED_CHECKSUM_PATH.replace("{version}", version);
    match fetch_checksum_from_path(release_base_url, &versioned_path, target_archive) {
        Ok(checksum) => Ok(ChecksumResolution {
            checksum,
            archive_path: "/ato/releases/${ATO_VERSION}/ato-cli-x86_64-unknown-linux-gnu.tar.xz"
                .to_string(),
            used_latest_fallback: false,
        }),
        Err(versioned_err) => {
            let checksum =
                fetch_checksum_from_path(release_base_url, LATEST_CHECKSUM_PATH, target_archive)
                    .with_context(|| {
                    format!(
                        "Failed to resolve checksum from both versioned and latest channels.\nversioned_error: {versioned_err}"
                    )
                    })?;
            Ok(ChecksumResolution {
                checksum,
                archive_path:
                    "/ato/releases/${ATO_VERSION}/ato-cli-x86_64-unknown-linux-gnu.tar.xz"
                        .to_string(),
                used_latest_fallback: true,
            })
        }
    }
}

fn fetch_checksum_from_path(
    release_base_url: &str,
    checksum_path: &str,
    target_archive: &str,
) -> Result<String> {
    let url = format!("{release_base_url}{checksum_path}");
    let body = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("Failed to create HTTP client")?
        .get(&url)
        .send()
        .with_context(|| {
            format!(
                "Failed to fetch {} (set {} for staging mirrors if needed)",
                url, ENV_RELEASE_BASE_URL
            )
        })?
        .error_for_status()
        .with_context(|| {
            format!(
                "Failed to fetch {} (set {} for staging mirrors if needed)",
                url, ENV_RELEASE_BASE_URL
            )
        })?
        .text()
        .with_context(|| format!("Failed to read body: {}", url))?;

    parse_checksum_line(&body, target_archive).with_context(|| {
        format!(
            "Failed to extract checksum for '{}' from {}",
            target_archive, url
        )
    })
}

fn parse_checksum_line(input: &str, target_archive: &str) -> Result<String> {
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let hash = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        let file = match parts.next() {
            Some(v) => v.trim().trim_start_matches('*'),
            None => continue,
        };
        if file == target_archive {
            if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(hash.to_ascii_lowercase());
            }
            anyhow::bail!("Checksum format is invalid for {}", target_archive);
        }
    }
    anyhow::bail!("Target archive not found: {}", target_archive);
}

fn render_workflow(
    release_base_url: &str,
    ato_version: &str,
    ato_checksum: &str,
    archive_path: &str,
) -> String {
    format!(
        r#"name: Publish Capsule to Ato Store

on:
  push:
    tags:
      - "v*.*.*"
  workflow_dispatch:

concurrency:
  group: ${{{{ github.workflow }}}}-${{{{ github.ref }}}}
  cancel-in-progress: true

jobs:
  publish:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write

    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Install Ato CLI (Pinned & Verified)
        env:
          ATO_VERSION: "{ato_version}"
          ATO_CHECKSUM: "{ato_checksum}"
        run: |
          set -euo pipefail
                    archive_name="$(basename "{archive_path}")"
                    archive_dir="${{archive_name%.tar.xz}}"
                    curl -fsSL -o "$archive_name" "{release_base_url}{archive_path}"
                    echo "${{{{ATO_CHECKSUM}}}}  $archive_name" | sha256sum -c -
                    tar -xJf "$archive_name"
                    chmod +x "./${{archive_dir}}/ato"
                    sudo mv "./${{archive_dir}}/ato" /usr/local/bin/ato
                    rm -rf "$archive_name" "$archive_dir"

      - name: Publish to Ato Store
        run: ato publish --ci
"#
    )
}

#[cfg(test)]
mod tests {
    use super::parse_checksum_line;

    #[test]
    fn parses_matching_checksum_line() {
        let input = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ato-cli-x86_64-unknown-linux-gnu.tar.xz\n";
        let hash = parse_checksum_line(input, "ato-cli-x86_64-unknown-linux-gnu.tar.xz")
            .expect("must parse");
        assert_eq!(
            hash,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn supports_gnu_style_star_prefix() {
        let input = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *ato-cli-x86_64-unknown-linux-gnu.tar.xz\n";
        let hash = parse_checksum_line(input, "ato-cli-x86_64-unknown-linux-gnu.tar.xz")
            .expect("must parse");
        assert_eq!(
            hash,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }
}
