use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    DependencyIdentity, EnvironmentIdentity, EnvironmentMode, FilesystemIdentity, PlatformIdentity,
    RuntimeIdentity, Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;
use serde::Serialize;
use walkdir::WalkDir;

use crate::application::build_materialization::BuildObservation;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::runtime::overrides as runtime_overrides;

pub(crate) fn observe_dependencies(
    launch_spec: &LaunchSpec,
    build_observation: Option<&BuildObservation>,
) -> Result<DependencyIdentity> {
    Ok(DependencyIdentity {
        derivation_hash: build_observation
            .map(|observation| Tracked::known(observation.input_digest.clone()))
            .unwrap_or_else(|| Tracked::unknown("build materialization observation unavailable")),
        output_hash: observe_dependency_output_hash(&launch_spec.working_dir)?,
    })
}

pub(crate) fn observe_runtime(
    execution_plan: &ExecutionPlan,
    launch_spec: &LaunchSpec,
) -> Result<RuntimeIdentity> {
    let resolved_binary = resolve_binary_path(&launch_spec.command);
    Ok(RuntimeIdentity {
        declared: launch_spec
            .runtime
            .clone()
            .or_else(|| launch_spec.driver.clone())
            .or_else(|| launch_spec.language.clone()),
        resolved: resolved_binary
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| launch_spec.runtime.clone()),
        binary_hash: match resolved_binary {
            Some(path) => Tracked::known(
                hash_file(&path)
                    .with_context(|| format!("failed to hash runtime binary {}", path.display()))?,
            ),
            None => Tracked::unknown("runtime binary path not resolved"),
        },
        dynamic_linkage: Tracked::untracked("dynamic linkage observer not implemented"),
        platform: PlatformIdentity {
            os: execution_plan.reproducibility.platform.os.clone(),
            arch: execution_plan.reproducibility.platform.arch.clone(),
            libc: execution_plan.reproducibility.platform.libc.clone(),
        },
    })
}

pub(crate) fn observe_environment(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<EnvironmentIdentity> {
    let mut env = BTreeMap::new();
    env.extend(plan.execution_env());
    env.extend(launch_ctx.merged_env());
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        env.insert("PORT".to_string(), port.to_string());
    }

    let mut tracked_keys = Vec::new();
    let mut redacted_keys = Vec::new();
    let mut hashed_values = BTreeMap::new();
    for (key, value) in env {
        if is_sensitive_env_key(&key) {
            redacted_keys.push(key.clone());
        } else {
            tracked_keys.push(key.clone());
        }
        hashed_values.insert(key, hash_bytes(value.as_bytes()));
    }
    tracked_keys.sort();
    redacted_keys.sort();

    Ok(EnvironmentIdentity {
        closure_hash: Tracked::known(canonical_hash(&EnvironmentHashInput {
            values: hashed_values,
        })?),
        mode: EnvironmentMode::Closed,
        tracked_keys,
        redacted_keys,
        unknown_keys: Vec::new(),
    })
}

pub(crate) fn observe_filesystem(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    launch_spec: &LaunchSpec,
) -> Result<FilesystemIdentity> {
    let mut writable_dirs = launch_ctx
        .injected_mounts()
        .iter()
        .filter(|mount| !mount.readonly)
        .map(|mount| mount.target.clone())
        .collect::<Vec<_>>();
    writable_dirs.sort();
    writable_dirs.dedup();

    let mut known_readonly_layers = launch_ctx
        .injected_mounts()
        .iter()
        .filter(|mount| mount.readonly)
        .map(|mount| mount.target.clone())
        .collect::<Vec<_>>();
    known_readonly_layers.sort();
    known_readonly_layers.dedup();

    let projection_strategy = if launch_ctx.effective_cwd().is_some() {
        "projected-cwd"
    } else {
        "direct"
    }
    .to_string();
    let source_root = plan.workspace_root.display().to_string();
    let working_directory = launch_spec.working_dir.display().to_string();
    let persistent_state = Vec::<String>::new();

    Ok(FilesystemIdentity {
        view_hash: Tracked::known(canonical_hash(&FilesystemHashInput {
            source_root: &source_root,
            working_directory: &working_directory,
            projection_strategy: projection_strategy.as_str(),
            writable_dirs: &writable_dirs,
            persistent_state: &persistent_state,
            known_readonly_layers: &known_readonly_layers,
        })?),
        projection_strategy,
        writable_dirs,
        persistent_state,
        known_readonly_layers,
    })
}

pub(crate) fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.contains("PRIVATE_KEY")
}

fn observe_dependency_output_hash(working_dir: &Path) -> Result<Tracked<String>> {
    for candidate in [working_dir.join("node_modules"), working_dir.join(".venv")] {
        if candidate.is_dir() {
            return Ok(Tracked::known(hash_tree(&candidate).with_context(
                || format!("failed to hash dependency output {}", candidate.display()),
            )?));
        }
    }
    Ok(Tracked::unknown(
        "no existing node_modules or .venv dependency output observed",
    ))
}

fn resolve_binary_path(command: &str) -> Option<PathBuf> {
    let path = PathBuf::from(command);
    if path.is_absolute() && path.is_file() {
        return Some(path);
    }
    which::which(command).ok().filter(|path| path.is_file())
}

fn hash_tree(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort();

    let mut hasher = blake3::Hasher::new();
    update_hash_text(&mut hasher, "ato-tree-v1");
    for path in files {
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to relativize {}", path.display()))?;
        update_hash_text(&mut hasher, &relative.display().to_string());
        hash_file_into(&mut hasher, &path)?;
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    update_hash_text(&mut hasher, "ato-file-v1");
    hash_file_into(&mut hasher, path)?;
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn hash_file_into(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn update_hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String> {
    let canonical =
        serde_jcs::to_vec(value).context("failed to canonicalize execution receipt observation")?;
    Ok(hash_bytes(&canonical))
}

#[derive(Serialize)]
struct EnvironmentHashInput {
    values: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct FilesystemHashInput<'a> {
    source_root: &'a str,
    working_directory: &'a str,
    projection_strategy: &'a str,
    writable_dirs: &'a [String],
    persistent_state: &'a [String],
    known_readonly_layers: &'a [String],
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn sensitive_env_keys_are_redacted_by_name() {
        assert!(is_sensitive_env_key("OPENAI_API_KEY"));
        assert!(is_sensitive_env_key("github_token"));
        assert!(!is_sensitive_env_key("PATH"));
    }

    #[test]
    fn canonical_hash_is_stable_for_sorted_maps() {
        let mut left = BTreeMap::new();
        left.insert("B".to_string(), "2".to_string());
        left.insert("A".to_string(), "1".to_string());
        let mut right = BTreeMap::new();
        right.insert("A".to_string(), "1".to_string());
        right.insert("B".to_string(), "2".to_string());

        let left_hash = canonical_hash(&EnvironmentHashInput { values: left }).expect("left hash");
        let right_hash =
            canonical_hash(&EnvironmentHashInput { values: right }).expect("right hash");
        assert_eq!(left_hash, right_hash);
    }

    #[test]
    fn dependency_output_observer_hashes_existing_node_modules() {
        let temp = tempdir().expect("tempdir");
        let node_modules = temp.path().join("node_modules");
        fs::create_dir(&node_modules).expect("mkdir");
        fs::write(node_modules.join("dep.js"), "module.exports = 1;").expect("write");

        let observed = observe_dependency_output_hash(temp.path()).expect("observe");

        assert_eq!(
            observed.status,
            capsule_core::execution_identity::TrackingStatus::Known
        );
        assert!(observed.value.expect("hash").starts_with("blake3:"));
    }

    #[test]
    fn dependency_output_observer_reports_unknown_when_missing() {
        let temp = tempdir().expect("tempdir");

        let observed = observe_dependency_output_hash(temp.path()).expect("observe");

        assert_eq!(
            observed.status,
            capsule_core::execution_identity::TrackingStatus::Unknown
        );
        assert!(observed.reason.expect("reason").contains("no existing"));
    }
}
