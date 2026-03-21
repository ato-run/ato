use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use capsule_core::router::ManifestData;
use toml::Value;
use walkdir::WalkDir;

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
    let manifest_dir = plan
        .manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let run_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let root_dir = manifest_dir
        .join(".tmp")
        .join("ato-auto-provision")
        .join(format!("run-{}", run_id));
    fs::create_dir_all(&root_dir)
        .with_context(|| format!("Failed to create shadow workspace: {}", root_dir.display()))?;
    let workspace_dir = root_dir.join("workspace");
    fs::create_dir_all(&workspace_dir).with_context(|| {
        format!(
            "Failed to create shadow workspace copy root: {}",
            workspace_dir.display()
        )
    })?;
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
) -> Result<std::collections::HashMap<String, String>> {
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
        .map(|key| (key.clone(), synthetic_env_value(key, shadow_workspace)))
        .collect::<std::collections::HashMap<_, _>>();

    if env_values.is_empty() {
        return Ok(env_values);
    }

    let env_path = shadow_execution_working_directory(plan, shadow_workspace)?.join(".env");
    if let Some(parent) = env_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create env parent: {}", parent.display()))?;
    }

    let mut lines = String::new();
    for (key, value) in &env_values {
        lines.push_str(key);
        lines.push('=');
        lines.push_str(value);
        lines.push('\n');
    }
    fs::write(&env_path, lines)
        .with_context(|| format!("Failed to write synthetic env file: {}", env_path.display()))?;
    Ok(env_values)
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
    let relative_source_working_dir = relative_working_dir_from_manifest_root(plan);
    let shadow_working_dir = if relative_source_working_dir.as_os_str().is_empty() {
        shadow_workspace.workspace_dir.clone()
    } else {
        shadow_workspace
            .workspace_dir
            .join(relative_source_working_dir)
    };
    let relative_working_dir = diff_paths(shadow_working_dir, &shadow_workspace.root_dir)
        .unwrap_or_else(|| PathBuf::from("workspace"));

    let Some(targets) = manifest.get_mut("targets").and_then(Value::as_table_mut) else {
        anyhow::bail!("targets table is missing from manifest");
    };
    let Some(target) = targets.get_mut(target_label).and_then(Value::as_table_mut) else {
        anyhow::bail!("targets.{} is missing from manifest", target_label);
    };

    target.insert(
        "working_dir".to_string(),
        Value::String(relative_working_dir.to_string_lossy().to_string()),
    );

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
            if driver == "node" {
                let runtime_tools = target
                    .entry("runtime_tools".to_string())
                    .or_insert_with(|| Value::Table(Default::default()));
                let Some(runtime_tools_table) = runtime_tools.as_table_mut() else {
                    anyhow::bail!("targets.{}.runtime_tools must be a table", target_label);
                };
                runtime_tools_table.insert("node".to_string(), Value::String(selected_version));
            } else {
                target.insert(
                    "runtime_version".to_string(),
                    Value::String(selected_version),
                );
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

fn diff_paths(path: PathBuf, base: &Path) -> Option<PathBuf> {
    pathdiff::diff_paths(path, base)
}

fn relative_working_dir_from_manifest_root(plan: &ManifestData) -> PathBuf {
    diff_paths(plan.execution_working_directory(), &plan.manifest_dir).unwrap_or_default()
}

fn shadow_execution_working_directory(
    plan: &ManifestData,
    shadow_workspace: &ShadowWorkspaceRef,
) -> Result<PathBuf> {
    let relative = relative_working_dir_from_manifest_root(plan);
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
    for entry in WalkDir::new(source_root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path == shadow_root || path.starts_with(shadow_root) {
            continue;
        }
        let relative = match path.strip_prefix(source_root) {
            Ok(relative) if !relative.as_os_str().is_empty() => relative,
            _ => continue,
        };
        if should_skip_snapshot(relative) {
            if entry.file_type().is_dir() {
                continue;
            }
            continue;
        }

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
            ".git" | ".tmp" | "target" | "node_modules" | ".venv"
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
        python_lock_generation_command,
    };

    fn test_plan(dir: &Path, target_manifest: &str) -> ManifestData {
        let manifest_path = dir.join("capsule.toml");
        std::fs::write(&manifest_path, target_manifest).expect("manifest");
        ManifestData {
            manifest: toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read"))
                .expect("parse"),
            manifest_path,
            manifest_dir: dir.to_path_buf(),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: HashMap::new(),
        }
    }

    fn test_shadow_workspace(dir: &Path, run_id: &str) -> ShadowWorkspaceRef {
        let shadow_root = dir.join(".tmp").join(run_id);
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

    #[test]
    fn materializes_shadow_manifest_with_runtime_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
        )
        .expect("manifest");

        let manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read")).expect("parse");
        let plan = ManifestData {
            manifest,
            manifest_path: manifest_path.clone(),
            manifest_dir: dir.path().to_path_buf(),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: HashMap::new(),
        };
        let shadow_root = dir.path().join(".tmp").join("run-1");
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
    fn materializes_synthetic_env_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = ManifestData {
            manifest: toml::from_str(
                r#"
name = "demo"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
"#,
            )
            .expect("manifest"),
            manifest_path: dir.path().join("capsule.toml"),
            manifest_dir: dir.path().to_path_buf(),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: HashMap::new(),
        };
        let shadow_root = dir.path().join(".tmp").join("run-2");
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

        let env_values = materialize_synthetic_env(&plan, &summary, &shadow).expect("env values");
        assert_eq!(
            env_values.get("API_KEY").map(String::as_str),
            Some("ato-placeholder")
        );
        let env_file = workspace_dir.join(".env");
        let rendered = std::fs::read_to_string(env_file).expect("env file");
        assert!(rendered.contains("DATABASE_URL=sqlite://"));
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
}
