use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_runs_dir;
use serde::Serialize;

#[derive(Serialize)]
struct ExplainHashOutput {
    capsule_id: String,
    requested_ref: Option<String>,
    resolved_commit: Option<String>,
    source_tree_hash: Option<String>,
    derivation_hash: Option<String>,
    derivation_key_inputs: Vec<ExplainHashKey>,
}

#[derive(Serialize)]
struct ExplainHashKey {
    key: &'static str,
    digest: Option<String>,
    stability: &'static str,
}

pub(crate) fn execute_explain_hash_command(capsule: &str) -> Result<()> {
    let session = latest_session_for_capsule(capsule)?;
    let derivation_hash = session
        .as_ref()
        .and_then(|value| value.get("derivation_hash"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let output = ExplainHashOutput {
        capsule_id: capsule.to_string(),
        requested_ref: None,
        resolved_commit: None,
        source_tree_hash: None,
        derivation_hash,
        derivation_key_inputs: vec![
            pinned("schema_version", Some("1".to_string())),
            pinned("ecosystem", None),
            pinned("package_manager", None),
            pinned("package_manager_compat_class", None),
            pinned("runtime_compat_class", None),
            pinned("platform_triple", None),
            variable("lockfile_digest", None),
            variable("manifest_digest", None),
            variable("path_dependency_digest", None),
            pinned("install_policy_digest", None),
            variable("env_allowlist_digest", None),
        ],
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("serialize explain-hash output")?
    );
    Ok(())
}

fn pinned(key: &'static str, digest: Option<String>) -> ExplainHashKey {
    ExplainHashKey {
        key,
        digest,
        stability: "pinned",
    }
}

fn variable(key: &'static str, digest: Option<String>) -> ExplainHashKey {
    ExplainHashKey {
        key,
        digest,
        stability: "variable",
    }
}

fn latest_session_for_capsule(capsule: &str) -> Result<Option<serde_json::Value>> {
    let runs = ato_runs_dir();
    if !runs.exists() {
        return Ok(None);
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf, serde_json::Value)> = None;
    for entry in fs::read_dir(&runs).with_context(|| format!("read {}", runs.display()))? {
        let entry = entry?;
        let path = entry.path().join("session.json");
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        if value.get("capsule_id").and_then(serde_json::Value::as_str) != Some(capsule) {
            continue;
        }
        let modified = modified_time(&path)?;
        if newest
            .as_ref()
            .map(|(current, _, _)| modified > *current)
            .unwrap_or(true)
        {
            newest = Some((modified, path, value));
        }
    }
    Ok(newest.map(|(_, _, value)| value))
}

fn modified_time(path: &Path) -> Result<std::time::SystemTime> {
    fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .modified()
        .with_context(|| format!("modified time {}", path.display()))
}
