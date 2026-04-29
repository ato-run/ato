use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use serde::Serialize;

use crate::reporters::CliReporter;

#[derive(Debug, Serialize)]
pub struct LockResult {
    pub manifest_path: PathBuf,
    pub lockfile_path: PathBuf,
}

pub fn execute(
    path: PathBuf,
    timings: bool,
    json_output: bool,
    reporter: Arc<CliReporter>,
) -> Result<LockResult> {
    let manifest_path = resolve_manifest_path(&path);
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read manifest {}", manifest_path.display()))?;
    let manifest_raw: toml::Value = toml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse manifest {}", manifest_path.display()))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create ato lock runtime")?;
    let core_reporter: Arc<dyn CapsuleReporter + 'static> = reporter.clone();
    let lockfile_path = runtime.block_on(capsule_core::lockfile::generate_and_write_lockfile(
        &manifest_path,
        &manifest_raw,
        &manifest_text,
        core_reporter,
        timings,
    ))?;

    let result = LockResult {
        manifest_path,
        lockfile_path,
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Generated lockfile: {}", result.lockfile_path.display());
    }

    Ok(result)
}

fn resolve_manifest_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_command_resolves_directory_to_capsule_toml() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path();
        assert_eq!(resolve_manifest_path(path), path.join("capsule.toml"));
    }

    #[test]
    fn lock_command_accepts_explicit_manifest_path() {
        let path = PathBuf::from("demo/capsule.toml");
        assert_eq!(resolve_manifest_path(&path), path);
    }
}
