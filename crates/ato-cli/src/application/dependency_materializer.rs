use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use capsule_core::common::paths::ato_cache_dir;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;

use crate::application::execution_observers;
use crate::executors::launch_context::{InjectedMount, RuntimeLaunchContext};

const DERIVATION_VERSION: &str = "ato-node-dependency-materializer-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyMaterialization {
    pub(crate) derivation_hash: String,
    pub(crate) output_hash: String,
    pub(crate) mount: InjectedMount,
}

pub(crate) fn materialize_for_run(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<Option<DependencyMaterialization>> {
    let launch_spec = derive_launch_spec(plan)?;
    let working_dir = launch_ctx
        .effective_cwd()
        .cloned()
        .unwrap_or_else(|| launch_spec.working_dir.clone());
    materialize_node_dependencies(&working_dir)
}

fn materialize_node_dependencies(working_dir: &Path) -> Result<Option<DependencyMaterialization>> {
    let package_json = working_dir.join("package.json");
    let package_lock = working_dir.join("package-lock.json");
    if !package_json.is_file() || !package_lock.is_file() {
        return Ok(None);
    }

    let source_node_modules = working_dir.join("node_modules");
    if source_node_modules.exists() {
        return Ok(None);
    }

    let derivation_hash = node_derivation_hash(working_dir)?;
    let materialization_dir = dependency_cache_root().join(safe_hash_dir(&derivation_hash));
    let work_dir = materialization_dir.join("work");
    let node_modules = work_dir.join("node_modules");
    if !node_modules.is_dir() {
        prepare_clean_dir(&work_dir)?;
        fs::copy(&package_json, work_dir.join("package.json")).with_context(|| {
            format!(
                "failed to copy {} into dependency materialization",
                package_json.display()
            )
        })?;
        fs::copy(&package_lock, work_dir.join("package-lock.json")).with_context(|| {
            format!(
                "failed to copy {} into dependency materialization",
                package_lock.display()
            )
        })?;
        run_npm_ci(&work_dir)?;
    }

    let output_hash = execution_observers::hash_tree(&node_modules)?;
    Ok(Some(DependencyMaterialization {
        derivation_hash,
        output_hash,
        mount: InjectedMount {
            source: node_modules,
            target: working_dir.join("node_modules").display().to_string(),
            readonly: true,
        },
    }))
}

fn node_derivation_hash(working_dir: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    update_text(&mut hasher, DERIVATION_VERSION);
    update_file(&mut hasher, &working_dir.join("package.json"))?;
    update_file(&mut hasher, &working_dir.join("package-lock.json"))?;
    let npm_version = Command::new("npm")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    update_text(&mut hasher, &npm_version);
    update_text(&mut hasher, std::env::consts::OS);
    update_text(&mut hasher, std::env::consts::ARCH);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn run_npm_ci(work_dir: &Path) -> Result<()> {
    let output = Command::new("npm")
        .arg("ci")
        .arg("--ignore-scripts")
        .arg("--no-audit")
        .arg("--fund=false")
        .current_dir(work_dir)
        .output()
        .with_context(|| "failed to launch npm ci for dependency materialization")?;
    if !output.status.success() {
        bail!(
            "npm ci dependency materialization failed: {}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    Ok(())
}

fn prepare_clean_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clean {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn dependency_cache_root() -> PathBuf {
    ato_cache_dir()
        .join("dependency-materializations")
        .join("node")
}

fn safe_hash_dir(hash: &str) -> String {
    hash.replace(':', "_")
}

fn update_file(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    update_text(
        hasher,
        &path.file_name().unwrap_or_default().to_string_lossy(),
    );
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(&bytes);
    Ok(())
}

fn update_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn derivation_hash_changes_when_lockfile_changes() {
        let temp = tempdir().expect("tempdir");
        write_node_manifest(temp.path(), "left");
        let left = node_derivation_hash(temp.path()).expect("left hash");
        write_node_manifest(temp.path(), "right");
        let right = node_derivation_hash(temp.path()).expect("right hash");

        assert_ne!(left, right);
        assert!(left.starts_with("blake3:"));
        assert!(right.starts_with("blake3:"));
    }

    #[test]
    fn materializer_skips_preexisting_source_tree_node_modules() {
        let temp = tempdir().expect("tempdir");
        write_node_manifest(temp.path(), "left");
        fs::create_dir(temp.path().join("node_modules")).expect("node_modules");

        let result = materialize_node_dependencies(temp.path()).expect("skip");

        assert!(result.is_none());
    }

    #[test]
    fn unsupported_project_is_skipped() {
        let temp = tempdir().expect("tempdir");

        let result = materialize_node_dependencies(temp.path()).expect("skip");

        assert!(result.is_none());
    }

    fn write_node_manifest(root: &Path, marker: &str) {
        fs::write(
            root.join("package.json"),
            r#"{"name":"demo","version":"1.0.0","dependencies":{}}"#,
        )
        .expect("package json");
        fs::write(
            root.join("package-lock.json"),
            format!(
                r#"{{
  "name": "demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {{
    "": {{
      "name": "demo",
      "version": "1.0.0",
      "dependencies": {{}},
      "marker": "{marker}"
    }}
  }}
}}"#
            ),
        )
        .expect("package lock");
    }
}
