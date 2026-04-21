use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

use capsule_core::types::{
    CapsuleManifest, WorkspaceAppSpec, WorkspaceDependencySpec, WorkspaceServiceSpec,
    WorkspaceSetupSpec,
};

use crate::install;
use crate::install::support::can_prompt_interactively;

#[derive(Debug)]
pub(crate) struct SetupCommandArgs {
    pub(crate) path: PathBuf,
    pub(crate) registry: Option<String>,
    pub(crate) yes: bool,
    pub(crate) json: bool,
    pub(crate) dry_run: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupResult {
    pub(crate) project_root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_app: Option<String>,
    pub(crate) package_manager_steps: Vec<SetupStepResult>,
    pub(crate) capsule_dependencies: Vec<SetupCapsuleDependencyResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) service_preferences: Vec<SetupServicePreferenceResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) app_personalization: Vec<SetupAppPersonalizationResult>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupStepResult {
    pub(crate) label: String,
    pub(crate) command: String,
    pub(crate) status: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupCapsuleDependencyResult {
    pub(crate) name: String,
    pub(crate) capsule_ref: String,
    pub(crate) version: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) install_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupServicePreferenceResult {
    pub(crate) name: String,
    pub(crate) capsule_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupAppPersonalizationResult {
    pub(crate) name: String,
    pub(crate) capsule_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) privacy_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupStepPlan {
    label: String,
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapsuleDependencyPlan {
    name: String,
    capsule_ref: String,
    version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServicePreferencePlan {
    name: String,
    capsule_ref: String,
    mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppPersonalizationPlan {
    name: String,
    capsule_ref: String,
    model_tier: Option<String>,
    privacy_mode: Option<String>,
}

pub(crate) fn execute_setup_command(args: SetupCommandArgs) -> Result<()> {
    let project_root = args
        .path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", args.path.display()))?;
    let plan = detect_setup_plan(&project_root)?;

    let mut result = SetupResult {
        project_root: project_root.clone(),
        default_app: plan.default_app,
        package_manager_steps: Vec::new(),
        capsule_dependencies: Vec::new(),
        service_preferences: plan
            .service_preferences
            .into_iter()
            .map(|service| SetupServicePreferenceResult {
                name: service.name,
                capsule_ref: service.capsule_ref,
                mode: service.mode,
            })
            .collect(),
        app_personalization: plan
            .app_personalization
            .into_iter()
            .map(|app| SetupAppPersonalizationResult {
                name: app.name,
                capsule_ref: app.capsule_ref,
                model_tier: app.model_tier,
                privacy_mode: app.privacy_mode,
            })
            .collect(),
        warnings: plan.warnings,
    };

    if plan.steps.is_empty()
        && plan.capsule_dependencies.is_empty()
        && result.service_preferences.is_empty()
        && result.app_personalization.is_empty()
        && result.default_app.is_none()
    {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!(
                "No declared dependencies found for setup in {}",
                project_root.display()
            );
        }
        return Ok(());
    }

    let can_prompt = !args.json
        && can_prompt_interactively(
            std::io::stdin().is_terminal(),
            std::io::stderr().is_terminal(),
        );
    let rt = tokio::runtime::Runtime::new()?;

    for dependency in plan.capsule_dependencies {
        if args.dry_run {
            result
                .capsule_dependencies
                .push(SetupCapsuleDependencyResult {
                    name: dependency.name,
                    capsule_ref: dependency.capsule_ref,
                    version: dependency.version.unwrap_or_else(|| "latest".to_string()),
                    status: "planned".to_string(),
                    install_path: None,
                });
            continue;
        }

        let install_result = rt.block_on(install::install_app(
            &dependency.capsule_ref,
            args.registry.as_deref(),
            dependency.version.as_deref(),
            None,
            false,
            args.yes,
            install::ProjectionPreference::Skip,
            false,
            false,
            args.json,
            can_prompt,
        ))?;

        result
            .capsule_dependencies
            .push(SetupCapsuleDependencyResult {
                name: dependency.name,
                capsule_ref: dependency.capsule_ref,
                version: install_result.version,
                status: "installed".to_string(),
                install_path: Some(install_result.path),
            });
    }

    for step in plan.steps {
        let command = render_command(&step.program, &step.args);
        if args.dry_run {
            result.package_manager_steps.push(SetupStepResult {
                label: step.label,
                command,
                status: "planned".to_string(),
            });
            continue;
        }

        ensure_command_available(&step.program)?;
        let status = Command::new(&step.program)
            .args(&step.args)
            .current_dir(&project_root)
            .status()
            .with_context(|| format!("failed to run {}", step.program))?;
        if !status.success() {
            anyhow::bail!("setup step failed: {} ({})", step.label, command);
        }

        result.package_manager_steps.push(SetupStepResult {
            label: step.label,
            command,
            status: "completed".to_string(),
        });
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!("Setup complete for {}", result.project_root.display());
    if let Some(default_app) = &result.default_app {
        println!("  default_app: {}", default_app);
    }
    for dependency in &result.capsule_dependencies {
        println!(
            "  capsule: {} -> {} ({})",
            dependency.name, dependency.capsule_ref, dependency.status
        );
    }
    for service in &result.service_preferences {
        println!(
            "  service: {} -> {}{}",
            service.name,
            service.capsule_ref,
            service
                .mode
                .as_deref()
                .map(|value| format!(" mode={value}"))
                .unwrap_or_default()
        );
    }
    for app in &result.app_personalization {
        let mut details = Vec::new();
        if let Some(model_tier) = app.model_tier.as_deref() {
            details.push(format!("model_tier={model_tier}"));
        }
        if let Some(privacy_mode) = app.privacy_mode.as_deref() {
            details.push(format!("privacy_mode={privacy_mode}"));
        }
        if !details.is_empty() {
            println!("  personalization: {} [{}]", app.name, details.join(", "));
        }
    }
    for step in &result.package_manager_steps {
        println!("  step: {} [{}]", step.label, step.status);
        println!("    {}", step.command);
    }
    for warning in &result.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

#[derive(Debug, Default)]
struct SetupPlan {
    steps: Vec<SetupStepPlan>,
    capsule_dependencies: Vec<CapsuleDependencyPlan>,
    default_app: Option<String>,
    service_preferences: Vec<ServicePreferencePlan>,
    app_personalization: Vec<AppPersonalizationPlan>,
    warnings: Vec<String>,
}

fn detect_setup_plan(project_root: &Path) -> Result<SetupPlan> {
    let mut plan = SetupPlan::default();
    if let Some(workspace) = detect_manifest_workspace_setup(project_root)? {
        plan.default_app = workspace.default_app.clone();
        plan.capsule_dependencies = workspace.capsule_dependencies;
        plan.service_preferences = workspace.service_preferences;
        plan.app_personalization = workspace.app_personalization;
    } else {
        plan.capsule_dependencies = detect_lockfile_capsule_dependencies(project_root)?;
    }
    plan.steps = detect_package_manager_steps(project_root)?;

    if project_root.join("deno.json").exists() || project_root.join("deno.lock").exists() {
        plan.warnings.push(
            "Deno dependencies are not fetched explicitly yet; they are resolved on first run/build."
                .to_string(),
        );
    }

    Ok(plan)
}

fn detect_lockfile_capsule_dependencies(project_root: &Path) -> Result<Vec<CapsuleDependencyPlan>> {
    let lock_path = project_root.join(capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME);
    if !lock_path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(&lock_path)
        .with_context(|| format!("failed to read {}", lock_path.display()))?;
    let lock = serde_json::from_str::<capsule_core::lockfile::CapsuleLock>(&raw)
        .with_context(|| format!("failed to parse {}", lock_path.display()))?;

    let mut plans = Vec::new();
    for dependency in lock.capsule_dependencies {
        let Some(capsule_ref) = normalize_capsule_dependency_ref(&dependency) else {
            continue;
        };
        plans.push(CapsuleDependencyPlan {
            name: dependency.name,
            capsule_ref,
            version: dependency.resolved_version.clone(),
        });
    }
    Ok(plans)
}

#[derive(Debug, Default)]
struct ManifestWorkspaceSetupPlan {
    default_app: Option<String>,
    capsule_dependencies: Vec<CapsuleDependencyPlan>,
    service_preferences: Vec<ServicePreferencePlan>,
    app_personalization: Vec<AppPersonalizationPlan>,
}

fn detect_manifest_workspace_setup(
    project_root: &Path,
) -> Result<Option<ManifestWorkspaceSetupPlan>> {
    let manifest_path = project_root.join("capsule.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = CapsuleManifest::from_toml_with_path(&raw, &manifest_path)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let Some(workspace) = manifest.workspace else {
        return Ok(None);
    };

    Ok(Some(build_manifest_workspace_setup_plan(workspace)))
}

fn build_manifest_workspace_setup_plan(
    workspace: WorkspaceSetupSpec,
) -> ManifestWorkspaceSetupPlan {
    let mut plan = ManifestWorkspaceSetupPlan {
        default_app: workspace.default_app.clone(),
        ..ManifestWorkspaceSetupPlan::default()
    };

    append_workspace_app_plans(
        &mut plan.capsule_dependencies,
        &mut plan.app_personalization,
        workspace.apps,
    );
    append_workspace_dependency_plans(&mut plan.capsule_dependencies, "tool", workspace.tools);
    append_workspace_service_plans(
        &mut plan.capsule_dependencies,
        &mut plan.service_preferences,
        workspace.services,
    );
    plan
}

fn append_workspace_app_plans(
    dependencies: &mut Vec<CapsuleDependencyPlan>,
    personalization: &mut Vec<AppPersonalizationPlan>,
    entries: std::collections::BTreeMap<String, WorkspaceAppSpec>,
) {
    for (name, spec) in entries {
        let Some(capsule_ref) = normalize_source_ref(spec.dependency.source.as_str()) else {
            continue;
        };
        dependencies.push(CapsuleDependencyPlan {
            name: format!("app:{}", name),
            capsule_ref: capsule_ref.clone(),
            version: spec.dependency.version.clone(),
        });
        if let Some(personalization_spec) = spec.personalization {
            personalization.push(AppPersonalizationPlan {
                name,
                capsule_ref,
                model_tier: personalization_spec.model_tier,
                privacy_mode: personalization_spec.privacy_mode,
            });
        }
    }
}

fn append_workspace_dependency_plans(
    plans: &mut Vec<CapsuleDependencyPlan>,
    kind: &str,
    entries: std::collections::BTreeMap<String, WorkspaceDependencySpec>,
) {
    for (name, spec) in entries {
        let Some(capsule_ref) = normalize_source_ref(spec.source.as_str()) else {
            continue;
        };
        plans.push(CapsuleDependencyPlan {
            name: format!("{}:{}", kind, name),
            capsule_ref,
            version: spec.version,
        });
    }
}

fn append_workspace_service_plans(
    dependencies: &mut Vec<CapsuleDependencyPlan>,
    preferences: &mut Vec<ServicePreferencePlan>,
    entries: std::collections::BTreeMap<String, WorkspaceServiceSpec>,
) {
    for (name, spec) in entries {
        let Some(capsule_ref) = normalize_source_ref(spec.dependency.source.as_str()) else {
            continue;
        };
        dependencies.push(CapsuleDependencyPlan {
            name: format!("service:{}", name),
            capsule_ref: capsule_ref.clone(),
            version: spec.dependency.version.clone(),
        });
        preferences.push(ServicePreferencePlan {
            name,
            capsule_ref,
            mode: spec.mode,
        });
    }
}

fn normalize_capsule_dependency_ref(
    dependency: &capsule_core::lockfile::LockedCapsuleDependency,
) -> Option<String> {
    normalize_source_ref(dependency.source.as_str())
}

fn normalize_source_ref(source: &str) -> Option<String> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }

    source
        .strip_prefix("capsule://store/")
        .or_else(|| source.strip_prefix("capsule://ato.run/"))
        .or_else(|| source.strip_prefix("capsule://registry/"))
        .map(str::to_string)
        .or_else(|| source.contains('/').then(|| source.to_string()))
}

fn detect_package_manager_steps(project_root: &Path) -> Result<Vec<SetupStepPlan>> {
    let mut steps = Vec::new();

    if project_root.join("Cargo.toml").exists() {
        let mut args = vec!["fetch".to_string()];
        if project_root.join("Cargo.lock").exists() {
            args.push("--locked".to_string());
        }
        steps.push(SetupStepPlan {
            label: "Fetch Rust crates".to_string(),
            program: "cargo".to_string(),
            args,
        });
    }

    if project_root.join("pyproject.toml").exists() {
        let mut args = vec!["sync".to_string()];
        if project_root.join("uv.lock").exists() {
            args.push("--frozen".to_string());
        }
        steps.push(SetupStepPlan {
            label: "Sync Python dependencies".to_string(),
            program: "uv".to_string(),
            args,
        });
    } else if project_root.join("requirements.txt").exists() {
        steps.push(SetupStepPlan {
            label: "Install Python requirements".to_string(),
            program: "python3".to_string(),
            args: vec![
                "-m".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "-r".to_string(),
                "requirements.txt".to_string(),
            ],
        });
    }

    if let Some(step) = detect_node_setup_step(project_root)? {
        steps.push(step);
    }

    Ok(steps)
}

fn detect_node_setup_step(project_root: &Path) -> Result<Option<SetupStepPlan>> {
    if !project_root.join("package.json").exists() {
        return Ok(None);
    }

    if project_root.join("pnpm-lock.yaml").exists() {
        return Ok(Some(SetupStepPlan {
            label: "Install Node dependencies (pnpm)".to_string(),
            program: "pnpm".to_string(),
            args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
        }));
    }
    if project_root.join("package-lock.json").exists() {
        return Ok(Some(SetupStepPlan {
            label: "Install Node dependencies (npm)".to_string(),
            program: "npm".to_string(),
            args: vec!["ci".to_string()],
        }));
    }
    if project_root.join("yarn.lock").exists() {
        return Ok(Some(SetupStepPlan {
            label: "Install Node dependencies (yarn)".to_string(),
            program: "yarn".to_string(),
            args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
        }));
    }
    if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
        return Ok(Some(SetupStepPlan {
            label: "Install Node dependencies (bun)".to_string(),
            program: "bun".to_string(),
            args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
        }));
    }

    let package_json = fs::read_to_string(project_root.join("package.json"))
        .context("failed to read package.json")?;
    let package_value = serde_json::from_str::<serde_json::Value>(&package_json)
        .context("failed to parse package.json")?;
    let package_manager = package_value
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.split_once('@').map(|(name, _)| name).or(Some(value)));

    Ok(Some(match package_manager {
        Some("pnpm") => SetupStepPlan {
            label: "Install Node dependencies (pnpm)".to_string(),
            program: "pnpm".to_string(),
            args: vec!["install".to_string()],
        },
        Some("yarn") => SetupStepPlan {
            label: "Install Node dependencies (yarn)".to_string(),
            program: "yarn".to_string(),
            args: vec!["install".to_string()],
        },
        Some("bun") => SetupStepPlan {
            label: "Install Node dependencies (bun)".to_string(),
            program: "bun".to_string(),
            args: vec!["install".to_string()],
        },
        _ => SetupStepPlan {
            label: "Install Node dependencies (npm)".to_string(),
            program: "npm".to_string(),
            args: vec!["install".to_string()],
        },
    }))
}

fn ensure_command_available(program: &str) -> Result<()> {
    which::which(program)
        .with_context(|| format!("required command '{}' was not found in PATH", program))?;
    Ok(())
}

fn render_command(program: &str, args: &[String]) -> String {
    let mut command = vec![program.to_string()];
    command.extend(args.iter().cloned());
    command.join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;

    #[test]
    fn detects_capsule_dependencies_from_lockfile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let lock = capsule_core::lockfile::CapsuleLock {
            version: "1".to_string(),
            meta: capsule_core::lockfile::LockMeta {
                created_at: "2026-04-03T00:00:00Z".to_string(),
                manifest_hash: "sha256:deadbeef".to_string(),
            },
            allowlist: None,
            capsule_dependencies: vec![capsule_core::lockfile::LockedCapsuleDependency {
                name: "auth".to_string(),
                source: "capsule://store/acme/auth-svc".to_string(),
                source_type: "store".to_string(),
                injection_bindings: BTreeMap::new(),
                resolved_version: Some("1.2.3".to_string()),
                digest: None,
                sha256: None,
                artifact_url: None,
            }],
            injected_data: HashMap::new(),
            tools: None,
            runtimes: None,
            targets: HashMap::new(),
        };
        fs::write(
            temp.path()
                .join(capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME),
            serde_json::to_vec_pretty(&lock).expect("serialize lock"),
        )
        .expect("write lock");

        let deps = detect_lockfile_capsule_dependencies(temp.path()).expect("detect deps");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].capsule_ref, "acme/auth-svc");
        assert_eq!(deps[0].version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn detects_workspace_capsule_dependencies_from_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desky"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
run = "open Desky.app"

[workspace]
default_app = "desky"

[workspace.apps.desky]
source = "ato/desky"

[workspace.tools.opencode]
source = "capsule://store/ato/opencode-engine"
version = "0.4.0"

[workspace.services.ollama]
source = "ato/ollama-runtime"
mode = "reuse-if-present"
"#,
        )
        .expect("write manifest");

        let setup = detect_manifest_workspace_setup(temp.path())
            .expect("detect workspace setup")
            .expect("workspace setup present");
        let deps = setup.capsule_dependencies;
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "app:desky");
        assert_eq!(deps[0].capsule_ref, "ato/desky");
        assert_eq!(deps[1].name, "tool:opencode");
        assert_eq!(deps[1].capsule_ref, "ato/opencode-engine");
        assert_eq!(deps[1].version.as_deref(), Some("0.4.0"));
        assert_eq!(deps[2].name, "service:ollama");
        assert_eq!(deps[2].capsule_ref, "ato/ollama-runtime");
        assert_eq!(setup.default_app.as_deref(), Some("desky"));
        assert_eq!(
            setup.service_preferences[0].mode.as_deref(),
            Some("reuse-if-present")
        );
    }

    #[test]
    fn prefers_workspace_manifest_over_lockfile_dependencies() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desky"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
run = "open Desky.app"

[workspace.apps.desky]
source = "ato/desky"
"#,
        )
        .expect("write manifest");

        let lock = capsule_core::lockfile::CapsuleLock {
            version: "1".to_string(),
            meta: capsule_core::lockfile::LockMeta {
                created_at: "2026-04-03T00:00:00Z".to_string(),
                manifest_hash: "sha256:deadbeef".to_string(),
            },
            allowlist: None,
            capsule_dependencies: vec![capsule_core::lockfile::LockedCapsuleDependency {
                name: "auth".to_string(),
                source: "capsule://store/acme/auth-svc".to_string(),
                source_type: "store".to_string(),
                injection_bindings: BTreeMap::new(),
                resolved_version: Some("1.2.3".to_string()),
                digest: None,
                sha256: None,
                artifact_url: None,
            }],
            injected_data: HashMap::new(),
            tools: None,
            runtimes: None,
            targets: HashMap::new(),
        };
        fs::write(
            temp.path()
                .join(capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME),
            serde_json::to_vec_pretty(&lock).expect("serialize lock"),
        )
        .expect("write lock");

        let plan = detect_setup_plan(temp.path()).expect("detect plan");
        let deps = plan.capsule_dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].capsule_ref, "ato/desky");
    }

    #[test]
    fn reflects_personalization_defaults_from_workspace_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desky"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
run = "open Desky.app"

[workspace.apps.desky]
source = "ato/desky"

[workspace.apps.desky.personalization]
model_tier = "balanced"
privacy_mode = "strict"
"#,
        )
        .expect("write manifest");

        let plan = detect_setup_plan(temp.path()).expect("detect plan");
        assert_eq!(plan.app_personalization.len(), 1);
        assert_eq!(plan.app_personalization[0].name, "desky");
        assert_eq!(
            plan.app_personalization[0].model_tier.as_deref(),
            Some("balanced")
        );
        assert_eq!(
            plan.app_personalization[0].privacy_mode.as_deref(),
            Some("strict")
        );
    }

    #[test]
    fn detects_package_manager_steps_for_tauri_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname='sample'\nversion='0.1.0'\n",
        )
        .expect("write cargo toml");
        fs::write(temp.path().join("Cargo.lock"), "# lock\n").expect("write cargo lock");
        fs::write(
            temp.path().join("package.json"),
            "{\"packageManager\":\"pnpm@9.0.0\"}",
        )
        .expect("write package json");
        fs::write(
            temp.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("write pnpm lock");

        let steps = detect_package_manager_steps(temp.path()).expect("detect steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].program, "cargo");
        assert_eq!(steps[1].program, "pnpm");
        assert!(steps[1]
            .args
            .iter()
            .any(|value| value == "--frozen-lockfile"));
    }
}
