use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_cache_dir;
use capsule_core::router::ManifestData;
use toml::Value;
use walkdir::WalkDir;

use crate::application::secrets::store::{write_secure_file, SecretStore};

use super::dependency_root::relative_dependency_root_from_manifest;
use super::types::{
    ProvisioningAction, ProvisioningAudit, ProvisioningMaterializationStatus, ProvisioningPlan,
    ShadowWorkspaceRef,
};

const DEFAULT_DENO_RUNTIME_VERSION: &str = "1.46.3";
const DEFAULT_NODE_RUNTIME_VERSION: &str = "20.11.0";
const DEFAULT_PYTHON_RUNTIME_VERSION: &str = "3.11.10";

enum ShadowLockfileMaterialization {
    Applied { detail: String },
    Skipped { detail: String },
}

pub fn prepare_shadow_workspace(
    plan: &ManifestData,
    audit: &ProvisioningAudit,
) -> Result<ShadowWorkspaceRef> {
    let manifest_dir = &plan.workspace_root;
    let run_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let root_dir = ato_cache_dir()
        .join("auto-provision")
        .join(format!("run-{}", run_id));
    fs::create_dir_all(&root_dir)
        .with_context(|| format!("Failed to create shadow workspace: {}", root_dir.display()))?;
    let workspace_dir = root_dir.clone();
    copy_workspace_snapshot(manifest_dir, &workspace_dir, &root_dir)?;

    let audit_path = root_dir.join("audit.json");
    write_audit(&audit_path, audit)?;

    Ok(ShadowWorkspaceRef {
        root_dir,
        workspace_dir,
        audit_path,
        manifest_path: None,
    })
}

pub fn materialize_synthetic_env(
    plan: &ManifestData,
    summary: &ProvisioningPlan,
    shadow_workspace: &ShadowWorkspaceRef,
    audit: &mut ProvisioningAudit,
) -> Result<std::collections::HashMap<String, String>> {
    // Open the secret store once (best-effort — if unavailable, all keys fall back to placeholders).
    let store = SecretStore::open().ok();

    let env_values = summary
        .actions
        .iter()
        .filter_map(|action| match action {
            ProvisioningAction::InjectSyntheticEnv {
                target,
                missing_keys,
                ..
            } if target == plan.selected_target_label() => Some(missing_keys.as_slice()),
            _ => None,
        })
        .flatten()
        .map(|key| {
            let value = resolve_env_value(key, shadow_workspace, store.as_ref());
            (key.clone(), value)
        })
        .collect::<std::collections::HashMap<_, _>>();

    if env_values.is_empty() {
        return Ok(env_values);
    }

    let env_path = shadow_execution_working_directory(plan, shadow_workspace)?.join(".env");

    // Build the .env content; skip multiline values (e.g. PEM certificates) to prevent
    // breaking the KEY=value\n format — they get injected as placeholders instead.
    let mut lines = String::new();
    for (key, value) in &env_values {
        if value.contains('\n') {
            // Multiline secrets would corrupt .env syntax; keep the synthetic placeholder.
            let placeholder = synthetic_env_value(key, shadow_workspace);
            lines.push_str(key);
            lines.push('=');
            lines.push_str(&placeholder);
        } else {
            lines.push_str(key);
            lines.push('=');
            lines.push_str(value);
        }
        lines.push('\n');
    }

    // Write with 0600 permissions so real secrets are not world-readable.
    write_secure_file(&env_path, lines.as_bytes())
        .with_context(|| format!("Failed to write synthetic env file: {}", env_path.display()))?;

    let (real_keys, placeholder_keys): (Vec<_>, Vec<_>) = env_values
        .iter()
        .partition(|(_, v)| v.as_str() != "ato-placeholder");

    let audit_detail = {
        let fmt_keys = |pairs: &[(&String, &String)]| -> String {
            pairs
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = Vec::new();
        if !real_keys.is_empty() {
            parts.push(format!(
                "resolved from secret store: {}",
                fmt_keys(&real_keys)
            ));
        }
        if !placeholder_keys.is_empty() {
            parts.push(format!(
                "synthetic placeholder: {}",
                fmt_keys(&placeholder_keys)
            ));
        }
        format!(
            "wrote .env at {} ({})",
            env_path.display(),
            parts.join("; ")
        )
    };

    audit.record_materialization(
        "synthetic_env",
        plan.selected_target_label(),
        plan.execution_driver().as_deref(),
        ProvisioningMaterializationStatus::Applied,
        audit_detail,
    );
    Ok(env_values)
}

/// Resolves the value for a single env key:
/// - DB/Redis/Port keys always use their fixed synthetic values (not from the store).
/// - All other keys try the secret store first; fall back to `synthetic_env_value` if absent.
fn resolve_env_value(
    key: &str,
    shadow_workspace: &ShadowWorkspaceRef,
    store: Option<&SecretStore>,
) -> String {
    let synthetic = synthetic_env_value(key, shadow_workspace);
    // Only query the store when the synthetic value is a placeholder; DB/Redis/Port
    // synthetic values are intentional and must not be overridden by stored secrets.
    if synthetic != "ato-placeholder" {
        return synthetic;
    }
    if let Some(store) = store {
        if let Ok(Some(real_value)) = store.get(key) {
            return real_value;
        }
    }
    synthetic
}

pub fn materialize_shadow_lockfiles(
    plan: &ManifestData,
    summary: &ProvisioningPlan,
    shadow_workspace: &ShadowWorkspaceRef,
    audit: &mut ProvisioningAudit,
) -> Result<()> {
    let working_dir = shadow_execution_working_directory(plan, shadow_workspace)?;
    for action in &summary.actions {
        let ProvisioningAction::GenerateShadowLockfile { target, driver, .. } = action else {
            continue;
        };
        if target != plan.selected_target_label() {
            continue;
        }
        let result = match driver.as_str() {
            "node" => generate_node_lockfile(&working_dir),
            "python" => generate_python_lockfile(&working_dir),
            _ => Ok(ShadowLockfileMaterialization::Skipped {
                detail: format!("unsupported shadow lockfile driver: {}", driver),
            }),
        };

        match result {
            Ok(ShadowLockfileMaterialization::Applied { detail }) => audit.record_materialization(
                "shadow_lockfile",
                target,
                Some(driver),
                ProvisioningMaterializationStatus::Applied,
                detail,
            ),
            Ok(ShadowLockfileMaterialization::Skipped { detail }) => audit.record_materialization(
                "shadow_lockfile",
                target,
                Some(driver),
                ProvisioningMaterializationStatus::Skipped,
                detail,
            ),
            Err(error) => {
                audit.record_materialization(
                    "shadow_lockfile",
                    target,
                    Some(driver),
                    ProvisioningMaterializationStatus::Failed,
                    error.to_string(),
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

pub fn materialize_shadow_manifest(
    plan: &ManifestData,
    summary: &ProvisioningPlan,
    shadow_workspace: &ShadowWorkspaceRef,
) -> Result<Option<PathBuf>> {
    if !summary.actions.iter().any(|action| {
        matches!(
            action,
            ProvisioningAction::SelectRuntime { .. }
                | ProvisioningAction::GenerateShadowLockfile { .. }
                | ProvisioningAction::InjectSyntheticEnv { .. }
        )
    }) {
        return Ok(None);
    }

    let mut manifest = plan.manifest.clone();
    let target_label = plan.selected_target_label();
    let relative_source_working_dir = relative_dependency_root_from_manifest(plan);
    let shadow_working_dir = if relative_source_working_dir.as_os_str().is_empty() {
        shadow_workspace.workspace_dir.clone()
    } else {
        shadow_workspace
            .workspace_dir
            .join(relative_source_working_dir)
    };
    let relative_working_dir =
        pathdiff::diff_paths(shadow_working_dir, &shadow_workspace.root_dir).unwrap_or_default();

    // For v0.3 flat manifests (no [targets] table), synthesize one from top-level fields
    let is_v03_flat = manifest.get("targets").is_none()
        && (manifest.get("run").is_some() || manifest.get("runtime").is_some());
    if is_v03_flat {
        let manifest_table = manifest
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("manifest must be a TOML table"))?;
        let mut target_table = toml::value::Table::new();
        for key in [
            "runtime",
            "run",
            "build",
            "port",
            "runtime_version",
            "working_dir",
            "runtime_tools",
        ] {
            if let Some(val) = manifest_table.remove(key) {
                if key == "runtime" {
                    if let Some(rt_str) = val.as_str() {
                        if let Some((rt, drv)) = rt_str.split_once('/') {
                            target_table
                                .insert("runtime".to_string(), Value::String(rt.to_string()));
                            target_table
                                .insert("driver".to_string(), Value::String(drv.to_string()));
                            continue;
                        }
                    }
                }
                if key == "run" {
                    target_table.insert("run_command".to_string(), val);
                } else if key == "build" {
                    target_table.insert("build_command".to_string(), val);
                } else {
                    target_table.insert(key.to_string(), val);
                }
            }
        }
        let mut targets_table = toml::value::Table::new();
        targets_table.insert(target_label.to_string(), Value::Table(target_table));
        manifest_table.insert("targets".to_string(), Value::Table(targets_table));
        manifest_table.insert(
            "default_target".to_string(),
            Value::String(target_label.to_string()),
        );
    }

    let Some(targets) = manifest.get_mut("targets").and_then(Value::as_table_mut) else {
        anyhow::bail!("targets table is missing from manifest");
    };
    let Some(target) = targets.get_mut(target_label).and_then(Value::as_table_mut) else {
        anyhow::bail!("targets.{} is missing from manifest", target_label);
    };

    if relative_working_dir.as_os_str().is_empty() {
        target.remove("working_dir");
    } else {
        target.insert(
            "working_dir".to_string(),
            Value::String(relative_working_dir.to_string_lossy().to_string()),
        );
    }

    for action in &summary.actions {
        if let ProvisioningAction::SelectRuntime {
            target: action_target,
            runtime,
            driver,
            ..
        } = action
        {
            if action_target != target_label {
                continue;
            }

            let selected_version = default_runtime_version(runtime, driver);
            target.insert(
                "runtime_version".to_string(),
                Value::String(selected_version.clone()),
            );
            if driver == "node" {
                let runtime_tools = target
                    .entry("runtime_tools".to_string())
                    .or_insert_with(|| Value::Table(Default::default()));
                let Some(runtime_tools_table) = runtime_tools.as_table_mut() else {
                    anyhow::bail!("targets.{}.runtime_tools must be a table", target_label);
                };
                runtime_tools_table.insert("node".to_string(), Value::String(selected_version));
            }
        }
    }

    let manifest_path = shadow_workspace.root_dir.join("capsule.toml");
    let text = toml::to_string_pretty(&manifest).context("Failed to serialize shadow manifest")?;
    fs::write(&manifest_path, text).with_context(|| {
        format!(
            "Failed to write shadow manifest: {}",
            manifest_path.display()
        )
    })?;
    Ok(Some(manifest_path))
}

pub fn write_audit(path: &PathBuf, audit: &ProvisioningAudit) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(audit).context("Failed to serialize provisioning audit")?;
    fs::write(path, bytes)
        .with_context(|| format!("Failed to write provisioning audit: {}", path.display()))?;
    Ok(())
}

fn default_runtime_version(runtime: &str, driver: &str) -> String {
    match (runtime, driver) {
        (_, "deno") => DEFAULT_DENO_RUNTIME_VERSION.to_string(),
        (_, "python") => DEFAULT_PYTHON_RUNTIME_VERSION.to_string(),
        (_, "node") => DEFAULT_NODE_RUNTIME_VERSION.to_string(),
        _ => DEFAULT_NODE_RUNTIME_VERSION.to_string(),
    }
}

fn shadow_execution_working_directory(
    plan: &ManifestData,
    shadow_workspace: &ShadowWorkspaceRef,
) -> Result<PathBuf> {
    let relative = relative_dependency_root_from_manifest(plan);
    let working_dir = if relative.as_os_str().is_empty() {
        shadow_workspace.workspace_dir.clone()
    } else {
        shadow_workspace.workspace_dir.join(relative)
    };
    fs::create_dir_all(&working_dir).with_context(|| {
        format!(
            "Failed to prepare shadow execution working directory: {}",
            working_dir.display()
        )
    })?;
    Ok(working_dir)
}

fn synthetic_env_value(key: &str, shadow_workspace: &ShadowWorkspaceRef) -> String {
    let upper = key.to_ascii_uppercase();
    if matches!(upper.as_str(), "DATABASE_URL" | "DB_URL" | "SQLITE_URL") {
        let db_path = shadow_workspace.root_dir.join("state").join("app.db");
        return format!("sqlite://{}", db_path.display());
    }
    if upper.contains("REDIS") && upper.ends_with("_URL") {
        return "redis://127.0.0.1:6379/0".to_string();
    }
    if upper == "PORT" {
        return "3000".to_string();
    }
    if upper.ends_with("_KEY") || upper.ends_with("_TOKEN") || upper.ends_with("_SECRET") {
        return "ato-placeholder".to_string();
    }
    "ato-placeholder".to_string()
}

fn generate_node_lockfile(working_dir: &Path) -> Result<ShadowLockfileMaterialization> {
    let package_json = working_dir.join("package.json");
    let package_lock = working_dir.join("package-lock.json");
    if !package_json.exists() || package_lock.exists() {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: if package_lock.exists() {
                "package-lock.json already exists".to_string()
            } else {
                "package.json not found".to_string()
            },
        });
    }
    if is_provider_workspace(working_dir) {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: "provider-backed workspace derives package-lock.json during runtime prep"
                .to_string(),
        });
    }
    if !command_exists("npm") {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: "npm not available on PATH".to_string(),
        });
    }

    let status = Command::new("npm")
        .args(["install", "--package-lock-only", "--ignore-scripts"])
        .current_dir(working_dir)
        .status()
        .with_context(|| format!("Failed to run npm in {}", working_dir.display()))?;
    if status.success() {
        Ok(ShadowLockfileMaterialization::Applied {
            detail:
                "generated package-lock.json with npm install --package-lock-only --ignore-scripts"
                    .to_string(),
        })
    } else {
        anyhow::bail!(
            "shadow lockfile generation failed for node workspace: {} (command: npm install --package-lock-only --ignore-scripts, status: {})",
            working_dir.display(),
            status
        )
    }
}

fn generate_python_lockfile(working_dir: &Path) -> Result<ShadowLockfileMaterialization> {
    let uv_lock = working_dir.join("uv.lock");
    if uv_lock.exists() {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: "uv.lock already exists".to_string(),
        });
    }
    if is_provider_workspace(working_dir) {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: "provider-backed workspace derives uv.lock during runtime prep".to_string(),
        });
    }
    let Some((program, args)) = python_lock_generation_command(working_dir) else {
        return Ok(ShadowLockfileMaterialization::Skipped {
            detail: python_lock_generation_skip_reason(working_dir),
        });
    };
    let command_summary = format!("{} {}", program, args.join(" "));

    let status = Command::new(&program)
        .args(&args)
        .current_dir(working_dir)
        .status()
        .with_context(|| {
            format!(
                "Failed to run {} {} in {}",
                program,
                args.join(" "),
                working_dir.display()
            )
        })?;
    if status.success() {
        Ok(ShadowLockfileMaterialization::Applied {
            detail: format!("generated uv.lock with {}", command_summary),
        })
    } else {
        anyhow::bail!(
            "shadow lockfile generation failed for python workspace: {} (command: {}, status: {})",
            working_dir.display(),
            command_summary,
            status
        )
    }
}

fn is_provider_workspace(working_dir: &Path) -> bool {
    working_dir.join("resolution.json").exists()
}

fn command_exists(command: &str) -> bool {
    which::which(command).is_ok()
}

fn python_lock_generation_command(working_dir: &Path) -> Option<(String, Vec<String>)> {
    if !command_exists("uv") {
        return None;
    }

    if working_dir.join("pyproject.toml").exists() {
        return Some(("uv".to_string(), vec!["lock".to_string()]));
    }

    if working_dir.join("requirements.txt").exists() {
        return Some((
            "uv".to_string(),
            vec![
                "pip".to_string(),
                "compile".to_string(),
                "requirements.txt".to_string(),
                "-o".to_string(),
                "uv.lock".to_string(),
            ],
        ));
    }

    None
}

fn python_lock_generation_skip_reason(working_dir: &Path) -> String {
    if !command_exists("uv") {
        return "uv not available on PATH".to_string();
    }
    if !working_dir.join("pyproject.toml").exists()
        && !working_dir.join("requirements.txt").exists()
    {
        return "no pyproject.toml or requirements.txt found".to_string();
    }
    "no supported python lockfile generation strategy matched".to_string()
}

fn copy_workspace_snapshot(
    source_root: &Path,
    destination_root: &Path,
    shadow_root: &Path,
) -> Result<()> {
    // Use filter_entry to prune excluded directories before WalkDir descends into them.
    // This is critical for large directories like node_modules (10k+ files) and artifacts.
    for entry in WalkDir::new(source_root)
        .into_iter()
        .filter_entry(|e| {
            if e.path().starts_with(shadow_root) {
                return false;
            }
            let relative = match e.path().strip_prefix(source_root) {
                Ok(r) => r,
                Err(_) => return true,
            };
            relative.as_os_str().is_empty() || !should_skip_snapshot(relative)
        })
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let relative = match path.strip_prefix(source_root) {
            Ok(relative) if !relative.as_os_str().is_empty() => relative,
            _ => continue,
        };

        let destination = destination_root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination).with_context(|| {
                format!(
                    "Failed to create shadow directory: {}",
                    destination.display()
                )
            })?;
            continue;
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create shadow parent directory: {}",
                    parent.display()
                )
            })?;
        }
        fs::copy(path, &destination).with_context(|| {
            format!(
                "Failed to copy workspace file {} -> {}",
                path.display(),
                destination.display()
            )
        })?;
    }

    Ok(())
}

fn should_skip_snapshot(relative: &Path) -> bool {
    relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(
            name.as_ref(),
            ".git"
                | ".ato"
                | ".tmp"
                | "target"
                | "node_modules"
                | ".venv"
                | "artifacts"
                | "locks"
                | "dist"
                | ".next"
                | "__pycache__"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use capsule_core::router::{ExecutionProfile, ManifestData};

    use crate::runtime::provisioning::types::{
        ProvisioningAction, ProvisioningAudit, ProvisioningMaterializationStatus, ProvisioningPlan,
        ProvisioningSafetyClass, ShadowWorkspaceRef,
    };

    use super::{
        materialize_shadow_lockfiles, materialize_shadow_manifest, materialize_synthetic_env,
        prepare_shadow_workspace, python_lock_generation_command,
    };

    fn test_plan(dir: &Path, target_manifest: &str) -> ManifestData {
        let manifest_path = dir.join("capsule.toml");
        std::fs::write(&manifest_path, target_manifest).expect("manifest");
        let mut manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read")).expect("parse");
        manifest
            .as_table_mut()
            .expect("manifest table")
            .entry("type".to_string())
            .or_insert_with(|| toml::Value::String("app".to_string()));
        capsule_core::router::execution_descriptor_from_manifest_parts(
            manifest,
            manifest_path,
            dir.to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    fn test_shadow_workspace(dir: &Path, run_id: &str) -> ShadowWorkspaceRef {
        let shadow_root = dir.join(".ato").join("test-scratch").join(run_id);
        let workspace_dir = shadow_root.join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace root");
        ShadowWorkspaceRef {
            root_dir: shadow_root.clone(),
            workspace_dir,
            audit_path: shadow_root.join("audit.json"),
            manifest_path: None,
        }
    }

    fn test_audit(plan: &ManifestData, summary: &ProvisioningPlan) -> ProvisioningAudit {
        ProvisioningAudit::new(
            plan,
            &crate::runtime::provisioning::AutoProvisioningOptions {
                preview_mode: false,
                background: false,
            },
            summary,
        )
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir for touch");
        }
        std::fs::write(path, "").expect("touch test fixture");
    }

    // Tests for `relative_dependency_root_from_manifest` and
    // `looks_like_source_project` live with the resolver itself in
    // `provisioning/dependency_root.rs`. Shadow tests below exercise
    // shadow-workspace behaviour that depends on the resolver being correct.

    #[test]
    fn materializes_shadow_manifest_with_runtime_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
run = "node server.js""#,
        )
        .expect("manifest");

        let manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read")).expect("parse");
        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            manifest,
            manifest_path.clone(),
            dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor");
        let shadow_root = dir.path().join(".ato").join("test-scratch").join("run-1");
        std::fs::create_dir_all(&shadow_root).expect("shadow root");
        let shadow = ShadowWorkspaceRef {
            root_dir: shadow_root.clone(),
            workspace_dir: shadow_root.join("workspace"),
            audit_path: shadow_root.join("audit.json"),
            manifest_path: None,
        };
        std::fs::create_dir_all(&shadow.workspace_dir).expect("workspace root");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::SelectRuntime {
                target: "app".to_string(),
                runtime: "source".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };

        let shadow_manifest = materialize_shadow_manifest(&plan, &summary, &shadow)
            .expect("shadow manifest result")
            .expect("shadow manifest path");
        let rendered = std::fs::read_to_string(shadow_manifest).expect("rendered manifest");
        let parsed: toml::Value = toml::from_str(&rendered).expect("parsed shadow manifest");
        assert_eq!(
            parsed
                .get("targets")
                .and_then(|targets| targets.get("app"))
                .and_then(|target| target.get("runtime_tools"))
                .and_then(|tools| tools.get("node"))
                .and_then(toml::Value::as_str),
            Some("20.11.0")
        );
        assert_eq!(
            parsed
                .get("targets")
                .and_then(|targets| targets.get("app"))
                .and_then(|target| target.get("working_dir"))
                .and_then(toml::Value::as_str),
            Some("workspace")
        );
    }

    #[test]
    fn materializes_shadow_manifest_preserves_build_command_from_flat_v03() {
        // Regression test for #301: flat v0.3 `build = "..."` must survive into the
        // shadow manifest so that run_v03_lifecycle_steps can execute it.
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
schema_version = "0.3"
name = "my-app"
version = "0.1.0"
type = "app"
runtime = "web/node"
run = "node server.js"
build = "npm run build"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-301");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::SelectRuntime {
                target: "app".to_string(),
                runtime: "web".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };

        let shadow_manifest = materialize_shadow_manifest(&plan, &summary, &shadow)
            .expect("shadow manifest result")
            .expect("shadow manifest path");
        let rendered = std::fs::read_to_string(shadow_manifest).expect("rendered manifest");
        let parsed: toml::Value = toml::from_str(&rendered).expect("parsed shadow manifest");

        assert_eq!(
            parsed
                .get("targets")
                .and_then(|targets| targets.get("app"))
                .and_then(|target| target.get("build_command"))
                .and_then(toml::Value::as_str),
            Some("npm run build"),
            "build_command must be present in shadow manifest targets"
        );
        assert_eq!(
            parsed
                .get("targets")
                .and_then(|targets| targets.get("app"))
                .and_then(|target| target.get("run_command"))
                .and_then(toml::Value::as_str),
            Some("node server.js"),
            "run_command must be present in shadow manifest targets"
        );
    }

    #[test]
    fn materializes_synthetic_env_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str(
                r#"
name = "demo"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
            )
            .expect("manifest"),
            dir.path().join("capsule.toml"),
            dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor");
        let shadow_root = dir.path().join(".ato").join("test-scratch").join("run-2");
        let workspace_dir = shadow_root.join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace root");
        let shadow = ShadowWorkspaceRef {
            root_dir: shadow_root.clone(),
            workspace_dir: workspace_dir.clone(),
            audit_path: shadow_root.join("audit.json"),
            manifest_path: None,
        };
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::InjectSyntheticEnv {
                target: "app".to_string(),
                missing_keys: vec!["DATABASE_URL".to_string(), "API_KEY".to_string()],
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };

        let mut audit = test_audit(&plan, &summary);
        let env_values =
            materialize_synthetic_env(&plan, &summary, &shadow, &mut audit).expect("env values");
        assert_eq!(
            env_values.get("API_KEY").map(String::as_str),
            Some("ato-placeholder")
        );
        let env_file = workspace_dir.join(".env");
        let rendered = std::fs::read_to_string(env_file).expect("env file");
        assert!(rendered.contains("DATABASE_URL=sqlite://"));
        assert_eq!(audit.materialization_records.len(), 1);
        let record = &audit.materialization_records[0];
        assert_eq!(record.stage, "synthetic_env");
        assert_eq!(record.status, ProvisioningMaterializationStatus::Applied);
        assert!(record.detail.contains("DATABASE_URL"));
    }

    /// `resolve_env_value` must return "ato-placeholder" when no store is available.
    #[test]
    fn resolve_env_value_returns_placeholder_without_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shadow = test_shadow_workspace(dir.path(), "run-rv1");
        let value = super::resolve_env_value("OPENAI_API_KEY", &shadow, None);
        assert_eq!(value, "ato-placeholder");
    }

    /// DB/Redis/Port keys must keep their synthetic values even when the secret store
    /// hypothetically has an entry for them (the store is irrelevant for these keys).
    #[test]
    fn resolve_env_value_preserves_db_synthetic_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shadow = test_shadow_workspace(dir.path(), "run-rv2");
        // DATABASE_URL synthetic value should be an sqlite:// URL, not "ato-placeholder"
        let value = super::resolve_env_value("DATABASE_URL", &shadow, None);
        assert!(
            value.starts_with("sqlite://"),
            "expected sqlite:// but got: {value}"
        );
    }

    /// The .env file written by materialize_synthetic_env must have mode 0600.
    #[cfg(unix)]
    #[test]
    fn synthetic_env_file_has_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str(
                r#"
name = "demo"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
            )
            .expect("manifest"),
            dir.path().join("capsule.toml"),
            dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor");
        let shadow = test_shadow_workspace(dir.path(), "run-sec");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::InjectSyntheticEnv {
                target: "app".to_string(),
                missing_keys: vec!["API_KEY".to_string()],
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = test_audit(&plan, &summary);
        materialize_synthetic_env(&plan, &summary, &shadow, &mut audit).expect("env values");
        let env_file = shadow.workspace_dir.join(".env");
        let mode = std::fs::metadata(&env_file)
            .expect("env metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "expected 0600 permissions but got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn python_lock_generation_prefers_pyproject() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname='demo'\nversion='0.1.0'\n",
        )
        .expect("pyproject");
        if which::which("uv").is_err() {
            return;
        }

        let command = python_lock_generation_command(dir.path()).expect("command");
        assert_eq!(command.0, "uv");
        assert_eq!(command.1, vec!["lock".to_string()]);
    }

    #[test]
    fn python_lock_generation_supports_requirements_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("requirements.txt"), "requests==2.32.0\n")
            .expect("requirements");
        if which::which("uv").is_err() {
            return;
        }

        let command = python_lock_generation_command(dir.path()).expect("command");
        assert_eq!(command.0, "uv");
        assert_eq!(
            command.1,
            vec![
                "pip".to_string(),
                "compile".to_string(),
                "requirements.txt".to_string(),
                "-o".to_string(),
                "uv.lock".to_string(),
            ]
        );
    }

    #[test]
    fn records_skipped_python_lockfile_reason_in_audit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
run_command = "python app.py"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-3");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::GenerateShadowLockfile {
                target: "app".to_string(),
                driver: "python".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = test_audit(&plan, &summary);

        materialize_shadow_lockfiles(&plan, &summary, &shadow, &mut audit).expect("lockfiles");

        assert_eq!(audit.materialization_records.len(), 1);
        let record = &audit.materialization_records[0];
        assert_eq!(record.status, ProvisioningMaterializationStatus::Skipped);
        assert_eq!(record.driver.as_deref(), Some("python"));
        assert!(record.detail.contains("pyproject.toml") || record.detail.contains("uv"));
    }

    #[test]
    fn records_failed_node_lockfile_generation_in_audit() {
        if which::which("npm").is_err() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node app.js"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-node-fail");
        std::fs::write(
            shadow.workspace_dir.join("package.json"),
            "{ invalid json }",
        )
        .expect("package json");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::GenerateShadowLockfile {
                target: "app".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = test_audit(&plan, &summary);

        let error = materialize_shadow_lockfiles(&plan, &summary, &shadow, &mut audit)
            .expect_err("npm should fail on invalid package.json");

        assert!(error
            .to_string()
            .contains("shadow lockfile generation failed"));
        assert_eq!(audit.materialization_records.len(), 1);
        let record = &audit.materialization_records[0];
        assert_eq!(record.status, ProvisioningMaterializationStatus::Failed);
        assert_eq!(record.driver.as_deref(), Some("node"));
        assert!(record
            .detail
            .contains("npm install --package-lock-only --ignore-scripts"));
    }

    #[test]
    fn records_failed_python_lockfile_generation_in_audit() {
        if which::which("uv").is_err() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
run_command = "python app.py"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-python-fail");
        std::fs::write(
            shadow.workspace_dir.join("pyproject.toml"),
            "[project\nname = 'demo'",
        )
        .expect("pyproject");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::GenerateShadowLockfile {
                target: "app".to_string(),
                driver: "python".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = test_audit(&plan, &summary);

        let error = materialize_shadow_lockfiles(&plan, &summary, &shadow, &mut audit)
            .expect_err("uv should fail on invalid pyproject.toml");

        assert!(error
            .to_string()
            .contains("shadow lockfile generation failed"));
        assert_eq!(audit.materialization_records.len(), 1);
        let record = &audit.materialization_records[0];
        assert_eq!(record.status, ProvisioningMaterializationStatus::Failed);
        assert_eq!(record.driver.as_deref(), Some("python"));
        assert!(record.detail.contains("uv lock"));
    }

    // --- Read-only guarantee tests (policy: issue #169) ---
    // All provisioning operations must write only to the shadow workspace under
    // `.ato/tmp/ato-auto-provision/run-<id>/`. The original source directory must
    // never be modified or have new files created directly in it.

    #[test]
    fn prepare_shadow_workspace_does_not_modify_original_source_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original_pkg = r#"{"name":"demo","version":"0.1.0"}"#;
        let original_server = "console.log('hello');";
        std::fs::write(dir.path().join("package.json"), original_pkg).expect("package.json");
        std::fs::write(dir.path().join("server.js"), original_server).expect("server.js");

        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        );
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: Vec::new(),
        };
        let audit = test_audit(&plan, &summary);

        let shadow = prepare_shadow_workspace(&plan, &audit).expect("shadow workspace");

        // Original source files must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("package.json")).expect("read"),
            original_pkg,
            "package.json in original dir must not be modified"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("server.js")).expect("read"),
            original_server,
            "server.js in original dir must not be modified"
        );
        // No new lockfile must appear directly in the original dir.
        assert!(
            !dir.path().join("package-lock.json").exists(),
            "package-lock.json must not be created in original dir"
        );
        // Shadow workspace must contain copies of the source files with correct content.
        assert_eq!(
            std::fs::read_to_string(shadow.workspace_dir.join("package.json"))
                .expect("shadow read"),
            original_pkg,
            "shadow workspace must contain an accurate copy of package.json"
        );
        assert_eq!(
            std::fs::read_to_string(shadow.workspace_dir.join("server.js")).expect("shadow read"),
            original_server,
            "shadow workspace must contain an accurate copy of server.js"
        );
    }

    #[test]
    fn materialize_shadow_manifest_does_not_modify_original_capsule_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        );
        let original_toml =
            std::fs::read_to_string(dir.path().join("capsule.toml")).expect("original manifest");

        let shadow = test_shadow_workspace(dir.path(), "run-ro-manifest");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::SelectRuntime {
                target: "app".to_string(),
                runtime: "source".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };

        let shadow_manifest = materialize_shadow_manifest(&plan, &summary, &shadow)
            .expect("materialize")
            .expect("path");

        // Shadow manifest must be written inside shadow root, not the original dir.
        assert!(
            shadow_manifest.starts_with(&shadow.root_dir),
            "shadow manifest {:?} must be under shadow root {:?}",
            shadow_manifest,
            shadow.root_dir
        );
        // Original capsule.toml must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("capsule.toml")).expect("read after"),
            original_toml,
            "original capsule.toml must not be modified during shadow manifest materialization"
        );
    }

    #[test]
    fn materialize_synthetic_env_does_not_write_env_to_original_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-ro-env");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::InjectSyntheticEnv {
                target: "app".to_string(),
                missing_keys: vec!["DATABASE_URL".to_string(), "API_KEY".to_string()],
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };

        let mut audit = test_audit(&plan, &summary);
        materialize_synthetic_env(&plan, &summary, &shadow, &mut audit)
            .expect("env materialization");

        // .env must NOT be written to the original source directory.
        assert!(
            !dir.path().join(".env").exists(),
            ".env must not be created in the original source directory"
        );
        // .env must be written only to the shadow workspace.
        assert!(
            shadow.workspace_dir.join(".env").exists(),
            ".env must be created in the shadow workspace"
        );
    }

    #[test]
    fn materialize_shadow_lockfiles_node_does_not_create_lockfile_in_original_dir() {
        if which::which("npm").is_err() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let original_pkg =
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node server.js"}}"#;
        std::fs::write(dir.path().join("package.json"), original_pkg).expect("package.json");

        let plan = test_plan(
            dir.path(),
            r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        );
        let shadow = test_shadow_workspace(dir.path(), "run-ro-lockfile");
        std::fs::write(shadow.workspace_dir.join("package.json"), original_pkg)
            .expect("shadow package.json");
        let summary = ProvisioningPlan {
            issues: Vec::new(),
            actions: vec![ProvisioningAction::GenerateShadowLockfile {
                target: "app".to_string(),
                driver: "node".to_string(),
                safety: ProvisioningSafetyClass::SafeDefault,
            }],
        };
        let mut audit = test_audit(&plan, &summary);

        // The result is intentionally not unwrapped: the test validates the read-only guarantee
        // regardless of whether lockfile generation succeeds or is skipped. In either case no
        // files must be written to the original source directory.
        let _ = materialize_shadow_lockfiles(&plan, &summary, &shadow, &mut audit);

        // package-lock.json must not be created in the original source directory.
        assert!(
            !dir.path().join("package-lock.json").exists(),
            "package-lock.json must not be created in the original source directory"
        );
        // Original package.json must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("package.json")).expect("read"),
            original_pkg,
            "package.json in original dir must not be modified"
        );
    }
}
