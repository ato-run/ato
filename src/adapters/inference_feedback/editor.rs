use anyhow::{Context, Result};
use capsule_core::common::paths::workspace_tmp_dir;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::format::automatic_editor_command;

#[allow(dead_code)]
pub fn build_manual_manifest_path(base_dir: &Path, repository: &str, attempt_id: &str) -> PathBuf {
    let repo_path = crate::install::normalize_github_repository(repository)
        .ok()
        .and_then(|value| {
            value
                .split_once('/')
                .map(|(owner, repo)| PathBuf::from("github.com").join(owner).join(repo))
        })
        .unwrap_or_else(|| {
            let sanitized = repository
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            PathBuf::from("github.com").join(if sanitized.is_empty() {
                "repository".to_string()
            } else {
                sanitized
            })
        });

    workspace_tmp_dir(base_dir)
        .join("inference")
        .join(repo_path)
        .join(attempt_id)
        .join("capsule.toml")
}

pub fn write_manual_manifest(path: &Path, manifest_text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create temp manifest directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(path, manifest_text)
        .with_context(|| format!("failed to write temp manifest: {}", path.display()))?;
    Ok(())
}

pub fn read_manual_manifest(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read edited manifest: {}", path.display()))
}

pub fn open_editor(path: &Path) -> Result<()> {
    let editor_command = resolved_editor_command()
        .ok_or_else(|| anyhow::anyhow!("No editor launcher is available for manual fix mode"))?;
    let (program, args) = editor_command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("editor command was empty"))?;

    let status = Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", editor_command.join(" ")))?;
    if !status.success() {
        anyhow::bail!(
            "editor '{}' exited with status {}",
            editor_command.join(" "),
            status
        );
    }
    Ok(())
}

pub fn can_open_editor_automatically() -> bool {
    resolved_editor_command().is_some()
}

pub(super) fn resolved_editor_command() -> Option<Vec<String>> {
    configured_editor_command().or_else(automatic_editor_command)
}

pub(super) fn configured_editor_command() -> Option<Vec<String>> {
    configured_editor_command_from_values(
        std::env::var("VISUAL").ok(),
        std::env::var("EDITOR").ok(),
    )
}

pub(super) fn configured_editor_command_from_values(
    visual: Option<String>,
    editor: Option<String>,
) -> Option<Vec<String>> {
    normalize_editor_value(visual)
        .or_else(|| normalize_editor_value(editor))
        .and_then(parse_editor_command)
}

fn normalize_editor_value(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn parse_editor_command(value: String) -> Option<Vec<String>> {
    match shell_words::split(&value) {
        Ok(parts) if !parts.is_empty() => Some(parts),
        _ if !value.is_empty() => Some(vec![value]),
        _ => None,
    }
}
