use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::execution_identity::{
    DependencyIdentity, EnvironmentIdentity, EnvironmentMode, FilesystemIdentity, PlatformIdentity,
    RuntimeIdentity, SourceIdentity, Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;
use serde::Serialize;
use walkdir::WalkDir;

use crate::application::build_materialization::BuildObservation;
use crate::application::source_inventory::collect_source_files;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::runtime::overrides as runtime_overrides;

pub(crate) fn observe_source(
    plan: &ManifestData,
    _launch_spec: &LaunchSpec,
) -> Result<SourceIdentity> {
    Ok(SourceIdentity {
        source_ref: Tracked::known(format!("local:{}", plan.manifest_path.display())),
        source_tree_hash: observe_source_tree_hash(&plan.workspace_root)?,
    })
}

pub(crate) fn observe_dependencies(
    launch_spec: &LaunchSpec,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<DependencyIdentity> {
    let output_hash = observe_dependency_output_hash(&launch_spec.working_dir, launch_ctx)?;
    let derivation_hash = build_observation
        .map(|observation| Tracked::known(observation.input_digest.clone()))
        .unwrap_or_else(|| {
            if launch_spec.required_lockfile.is_none()
                && output_hash.status
                    == capsule_core::execution_identity::TrackingStatus::NotApplicable
            {
                Tracked::not_applicable()
            } else {
                Tracked::unknown("build materialization observation unavailable")
            }
        });
    Ok(DependencyIdentity {
        derivation_hash,
        output_hash,
    })
}

pub(crate) fn observe_runtime(
    execution_plan: &ExecutionPlan,
    launch_spec: &LaunchSpec,
) -> Result<RuntimeIdentity> {
    // Phase Y follow-up: when the entry point is a script-style file
    // (run.sh, main.py, etc.) that is not on PATH, the runtime binary
    // we actually exec is the shebang interpreter, not the script
    // itself. Resolve via the shebang so binary_hash + dynamic_linkage
    // can land Known for script entrypoints.
    let resolved_binary = resolve_binary_path(&launch_spec.command)
        .or_else(|| resolve_script_interpreter(&launch_spec.working_dir, &launch_spec.command));
    let dynamic_linkage = match resolved_binary.as_deref() {
        Some(path) => observe_dynamic_linkage(path),
        None => Tracked::untracked("dynamic linkage requires a resolved runtime binary path"),
    };
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
        dynamic_linkage,
        platform: PlatformIdentity {
            os: execution_plan.reproducibility.platform.os.clone(),
            arch: execution_plan.reproducibility.platform.arch.clone(),
            libc: execution_plan.reproducibility.platform.libc.clone(),
        },
    })
}

/// Phase 8a: enumerate the dynamic library closure of `binary` and return
/// a content hash over the canonical sorted list. macOS uses
/// `otool -L <binary>`, Linux uses `ldd <binary>`. Each line is filtered
/// to its `lib path` token (versioning suffix stripped where stable) and
/// the resulting list is sorted, joined with newlines, and blake3'd.
///
/// Returns:
/// - `Tracked::known("blake3:host:<hash>")` when the platform tool ran
///   successfully and produced parseable output. The `host:` prefix
///   marks the linkage as host-bound (driver / system library closure
///   captured in the host's perspective).
/// - `Tracked::untracked` when the platform tool is missing or returned
///   unparseable output. The classifier still routes Untracked to
///   `BestEffort`, matching the prior behavior.
pub(crate) fn observe_dynamic_linkage(binary: &Path) -> Tracked<String> {
    let entries = match collect_dynamic_linkage_entries(binary) {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => {
            return Tracked::untracked(
                "dynamic linkage tool returned no entries; treating as untracked",
            );
        }
        Err(reason) => {
            return Tracked::untracked(format!("dynamic linkage observer failed: {reason}"));
        }
    };
    let canonical = entries.join("\n");
    let digest = blake3::hash(canonical.as_bytes()).to_hex();
    Tracked::known(format!("host:blake3:{digest}"))
}

#[cfg(target_os = "macos")]
fn collect_dynamic_linkage_entries(binary: &Path) -> std::result::Result<Vec<String>, String> {
    let output = std::process::Command::new("otool")
        .arg("-L")
        .arg(binary)
        .output()
        .map_err(|err| format!("failed to spawn otool: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "otool exited with status {} for {}",
            output.status,
            binary.display()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // otool -L output:
    //   <binary>:
    //   \t/path/to/dylib (compatibility version X.Y.Z, current version A.B.C)
    let mut entries = Vec::new();
    for line in text.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Trim version suffix in parens; the linkage identity is the
        // path + the canonicalized version range, but for hash stability
        // we keep the full line as emitted by otool — versions ARE part
        // of the linkage identity (a libcurl 7.x vs 8.x swap should
        // change the hash).
        entries.push(trimmed.to_string());
    }
    entries.sort();
    Ok(entries)
}

#[cfg(target_os = "linux")]
fn collect_dynamic_linkage_entries(binary: &Path) -> std::result::Result<Vec<String>, String> {
    let output = std::process::Command::new("ldd")
        .arg(binary)
        .output()
        .map_err(|err| format!("failed to spawn ldd: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "ldd exited with status {} for {}",
            output.status,
            binary.display()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // ldd output:
    //   libssl.so.3 => /lib/x86_64-linux-gnu/libssl.so.3 (0x00007f...)
    //   /lib64/ld-linux-x86-64.so.2 (0x00007f...)
    let mut entries = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip the trailing `(0x...)` address — it is the runtime load
        // address and changes per process invocation. The path before
        // it is the stable identity.
        let stable = match trimmed.rsplit_once(" (") {
            Some((head, _)) => head.to_string(),
            None => trimmed.to_string(),
        };
        entries.push(stable);
    }
    entries.sort();
    Ok(entries)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn collect_dynamic_linkage_entries(_binary: &Path) -> std::result::Result<Vec<String>, String> {
    Err("dynamic linkage observation is unimplemented on this platform".to_string())
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

    let mut unknown_keys = vec![
        "fd-layout".to_string(),
        "timezone".to_string(),
        "umask".to_string(),
        "ulimits".to_string(),
    ];
    unknown_keys.sort();

    Ok(EnvironmentIdentity {
        closure_hash: Tracked::known(canonical_hash(&EnvironmentHashInput {
            values: hashed_values,
        })?),
        mode: EnvironmentMode::Partial,
        tracked_keys,
        redacted_keys,
        unknown_keys,
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
    let mut persistent_state = plan
        .state_source_overrides
        .iter()
        .map(|(name, locator)| format!("{name}={locator}"))
        .collect::<Vec<_>>();
    persistent_state.sort();

    let view_hash = canonical_hash(&FilesystemHashInput {
        source_root: &source_root,
        working_directory: &working_directory,
        projection_strategy: projection_strategy.as_str(),
        writable_dirs: &writable_dirs,
        persistent_state: &persistent_state,
        known_readonly_layers: &known_readonly_layers,
    })?;

    Ok(FilesystemIdentity {
        view_hash: Tracked {
            status: capsule_core::execution_identity::TrackingStatus::Untracked,
            value: Some(view_hash),
            reason: Some(
                "filesystem view hash is partial: mount source identities, case sensitivity, symlink policy, tmp policy, and state bindings are not fully observed".to_string(),
            ),
        },
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

fn observe_dependency_output_hash(
    working_dir: &Path,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<Tracked<String>> {
    // Materialized node_modules (mounted from a content-addressed blob by
    // the dependency_materializer) is the most authoritative output identity.
    for mount in launch_ctx.injected_mounts() {
        if mount.target.ends_with("/node_modules") || mount.target == "node_modules" {
            return Ok(Tracked::known(hash_tree(&mount.source).with_context(
                || {
                    format!(
                        "failed to hash materialized dependency output {}",
                        mount.source.display()
                    )
                },
            )?));
        }
    }

    // node_modules in working_dir: hash the tree. Most npm installs are
    // deterministic given a frozen package-lock.json so the tree itself is
    // a stable output identity.
    let node_modules = working_dir.join("node_modules");
    if node_modules.is_dir() {
        return Ok(Tracked::known(hash_tree(&node_modules).with_context(
            || {
                format!(
                    "failed to hash dependency output {}",
                    node_modules.display()
                )
            },
        )?));
    }

    // Python virtualenv: avoid hashing the tree because uv / virtualenv
    // embed the session-specific venv path into bin/activate* and
    // bin/<entry-points>, and write Python bytecode caches with mtime
    // metadata. None of these reflect a real change in installed packages.
    // Use the canonical lockfile (uv.lock, Pipfile.lock, requirements*.txt)
    // as a content-addressed proxy for "what packages got resolved".
    let venv = working_dir.join(".venv");
    if venv.is_dir() {
        if let Some((label, lockfile_hash)) = python_lockfile_identity(working_dir)? {
            return Ok(Tracked::known(format!(
                "blake3:venv-from-{label}:{lockfile_hash}"
            )));
        }
        // No lockfile available — fall back to tree hash (drifts) rather
        // than mark Unknown so dep-bound classification still has a value.
        return Ok(Tracked::known(hash_tree(&venv).with_context(|| {
            format!("failed to hash dependency output {}", venv.display())
        })?));
    }

    if launch_ctx.injected_mounts().is_empty() {
        return Ok(Tracked::not_applicable());
    }

    Ok(Tracked::unknown(
        "dependency output expected but no existing node_modules or .venv output observed",
    ))
}

fn observe_source_tree_hash(working_dir: &Path) -> Result<Tracked<String>> {
    if !working_dir.is_dir() {
        return Ok(Tracked::unknown(format!(
            "launch working directory is not available for source observation: {}",
            working_dir.display()
        )));
    }
    Ok(Tracked::known(hash_source_tree(working_dir)?))
}

/// Look up the canonical Python lockfile for a workspace and return its
/// content hash + a label describing which file we used. Order:
///   1. `uv.lock` — uv's resolved dependency snapshot.
///   2. `Pipfile.lock` — Pipenv's resolution.
///   3. `requirements.txt` (or `requirements/*.txt`) — pip pinning.
///   4. `pyproject.toml` — last-resort fallback when no lockfile is present
///      but the project does declare its dependencies inline.
///
/// Returns `Ok(None)` only when none of the above exist; in that case the
/// caller falls back to tree hashing (with the known drift caveat).
fn python_lockfile_identity(working_dir: &Path) -> Result<Option<(&'static str, String)>> {
    const CANDIDATES: &[(&str, &str)] = &[
        ("uv-lock", "uv.lock"),
        ("pipfile-lock", "Pipfile.lock"),
        ("requirements-txt", "requirements.txt"),
        ("pyproject-toml", "pyproject.toml"),
    ];
    for (label, name) in CANDIDATES {
        let path = working_dir.join(name);
        if path.is_file() {
            let hash = hash_file(&path)
                .with_context(|| format!("failed to hash python lockfile {}", path.display()))?;
            return Ok(Some((label, hash)));
        }
    }
    Ok(None)
}

pub(crate) fn hash_source_tree(working_dir: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    update_hash_text(&mut hasher, "ato-source-tree-v1");
    for relative_path in collect_source_files(working_dir, &[])? {
        update_hash_text(&mut hasher, &relative_path.display().to_string());
        hash_file_into(&mut hasher, &working_dir.join(relative_path))?;
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn resolve_binary_path(command: &str) -> Option<PathBuf> {
    let path = PathBuf::from(command);
    if path.is_absolute() && path.is_file() {
        return Some(path);
    }
    which::which(command).ok().filter(|path| path.is_file())
}

/// Resolve a script-style entrypoint to its shebang interpreter so the
/// runtime observer can hash a real binary and enumerate its dynamic
/// linkage. Returns `None` when:
///   - the command does not point at an existing file under
///     `working_dir`
///   - the file has no `#!` shebang
///   - the shebang interpreter cannot be located on PATH
///
/// `/usr/bin/env <interp>` shebangs are unwrapped (we resolve `<interp>`
/// instead of `env` so the captured runtime binary is the actual
/// language runtime, not the env shim).
fn resolve_script_interpreter(working_dir: &Path, command: &str) -> Option<PathBuf> {
    let script_path = if command.is_empty() {
        return None;
    } else {
        let candidate = PathBuf::from(command);
        if candidate.is_absolute() {
            candidate
        } else {
            working_dir.join(command)
        }
    };
    if !script_path.is_file() {
        return None;
    }

    let mut file = File::open(&script_path).ok()?;
    let mut header = [0_u8; 256];
    let read = file.read(&mut header).ok()?;
    if read < 2 || &header[..2] != b"#!" {
        return None;
    }
    let line_end = header[..read]
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(read);
    let shebang = std::str::from_utf8(&header[2..line_end]).ok()?.trim();
    if shebang.is_empty() {
        return None;
    }

    let mut tokens = shebang.split_whitespace();
    let head = tokens.next()?;
    // /usr/bin/env <interp> [args...] — resolve <interp> directly.
    let interpreter = if head.ends_with("/env") {
        tokens.next().unwrap_or(head)
    } else {
        head
    };
    let interp_path = PathBuf::from(interpreter);
    if interp_path.is_absolute() && interp_path.is_file() {
        return Some(interp_path);
    }
    which::which(interpreter).ok().filter(|path| path.is_file())
}

pub(crate) fn hash_tree(root: &Path) -> Result<String> {
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
    use std::collections::HashMap;
    use std::fs;

    use capsule_core::router::ExecutionProfile;
    use tempfile::tempdir;

    use super::*;

    const TEST_MANIFEST: &str = r#"
schema_version = "0.3"
name = "observer-test"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "main.py"
"#;

    fn test_plan(dir: &Path, manifest: &str) -> ManifestData {
        let manifest_path = dir.join("capsule.toml");
        fs::write(&manifest_path, manifest).expect("manifest");
        let mut parsed: toml::Value = toml::from_str(manifest).expect("parse manifest");
        parsed
            .as_table_mut()
            .expect("manifest table")
            .entry("type".to_string())
            .or_insert_with(|| toml::Value::String("app".to_string()));
        capsule_core::router::execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            dir.to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

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

        let observed = observe_dependency_output_hash(temp.path(), &RuntimeLaunchContext::empty())
            .expect("observe");

        assert_eq!(
            observed.status,
            capsule_core::execution_identity::TrackingStatus::Known
        );
        assert!(observed.value.expect("hash").starts_with("blake3:"));
    }

    #[test]
    fn dependency_output_observer_reports_not_applicable_when_missing_and_no_mounts() {
        let temp = tempdir().expect("tempdir");

        let observed = observe_dependency_output_hash(temp.path(), &RuntimeLaunchContext::empty())
            .expect("observe");

        assert_eq!(
            observed.status,
            capsule_core::execution_identity::TrackingStatus::NotApplicable
        );
    }

    #[test]
    fn environment_observer_marks_non_env_process_state_partial() {
        let temp = tempdir().expect("tempdir");
        let plan = test_plan(temp.path(), TEST_MANIFEST);

        let observed = observe_environment(&plan, &RuntimeLaunchContext::empty()).expect("env");

        assert_eq!(observed.mode, EnvironmentMode::Partial);
        assert!(observed.unknown_keys.contains(&"timezone".to_string()));
        assert!(observed.unknown_keys.contains(&"umask".to_string()));
        assert!(observed.unknown_keys.contains(&"ulimits".to_string()));
    }

    #[test]
    fn filesystem_observer_marks_view_hash_partial() {
        let temp = tempdir().expect("tempdir");
        let plan = test_plan(temp.path(), TEST_MANIFEST);
        let launch_spec = capsule_core::launch_spec::LaunchSpec {
            working_dir: temp.path().to_path_buf(),
            command: "true".to_string(),
            args: Vec::new(),
            env_vars: HashMap::new(),
            runtime: None,
            driver: None,
            language: None,
            required_lockfile: None,
            port: None,
            source: capsule_core::launch_spec::LaunchSpecSource::RunCommand,
        };

        let observed =
            observe_filesystem(&plan, &RuntimeLaunchContext::empty(), &launch_spec).expect("fs");

        assert_eq!(
            observed.view_hash.status,
            capsule_core::execution_identity::TrackingStatus::Untracked
        );
        assert!(observed
            .view_hash
            .value
            .expect("hash")
            .starts_with("blake3:"));
    }

    #[test]
    fn filesystem_observer_records_persistent_state_overrides() {
        let temp = tempdir().expect("tempdir");
        let mut plan = test_plan(temp.path(), TEST_MANIFEST);
        plan.state_source_overrides
            .insert("db".to_string(), "state-abc123".to_string());
        let launch_spec = capsule_core::launch_spec::LaunchSpec {
            working_dir: temp.path().to_path_buf(),
            command: "true".to_string(),
            args: Vec::new(),
            env_vars: HashMap::new(),
            runtime: None,
            driver: None,
            language: None,
            required_lockfile: None,
            port: None,
            source: capsule_core::launch_spec::LaunchSpecSource::RunCommand,
        };

        let observed =
            observe_filesystem(&plan, &RuntimeLaunchContext::empty(), &launch_spec).expect("fs");

        assert_eq!(observed.persistent_state, vec!["db=state-abc123"]);
    }

    #[test]
    fn source_tree_hash_ignores_dependency_directories() {
        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join("main.js"), "console.log(1);\n").expect("write source");
        fs::create_dir(temp.path().join("node_modules")).expect("mkdir node_modules");
        fs::write(
            temp.path().join("node_modules/dep.js"),
            "module.exports = 1;\n",
        )
        .expect("write dep");

        let before = hash_source_tree(temp.path()).expect("before hash");
        fs::write(
            temp.path().join("node_modules/dep.js"),
            "module.exports = 2;\n",
        )
        .expect("mutate dep");
        let after = hash_source_tree(temp.path()).expect("after hash");

        assert_eq!(before, after);
    }
}
