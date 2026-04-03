use anyhow::Result;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;

use crate::executors::launch_context::RuntimeLaunchContext;
use crate::runtime::overrides as runtime_overrides;

use super::types::ProvisioningIssue;

pub fn collect_issues(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<Vec<ProvisioningIssue>> {
    let mut issues = Vec::new();

    if let Some(issue) = detect_missing_lockfile(plan) {
        issues.push(issue);
    }
    if let Some(issue) = detect_missing_required_env(plan, launch_ctx) {
        issues.push(issue);
    }
    if let Some(issue) = detect_runtime_selection_required(plan) {
        issues.push(issue);
    }

    Ok(issues)
}

fn detect_missing_lockfile(plan: &ManifestData) -> Option<ProvisioningIssue> {
    let launch_spec = derive_launch_spec(plan).ok();
    let driver = plan.execution_driver()?.trim().to_ascii_lowercase();
    let target = plan.selected_target_label().to_string();

    let (working_dir, candidates) = match driver.as_str() {
        "node" => {
            let working_dir = launch_spec
                .as_ref()
                .map(|spec| spec.working_dir.clone())
                .unwrap_or_else(|| plan.execution_working_directory());
            let candidates = vec![
                working_dir.join("package-lock.json"),
                working_dir.join("yarn.lock"),
                working_dir.join("pnpm-lock.yaml"),
                working_dir.join("bun.lock"),
                working_dir.join("bun.lockb"),
            ];
            (working_dir, candidates)
        }
        "python" => {
            let working_dir = launch_spec
                .as_ref()
                .map(|spec| spec.working_dir.clone())
                .unwrap_or_else(|| plan.execution_working_directory());
            if resolve_python_requirements_path(&working_dir).is_some() {
                return None;
            }
            let candidates = vec![working_dir.join("uv.lock")];
            (working_dir, candidates)
        }
        _ => return None,
    };

    if candidates.iter().any(|candidate| candidate.exists()) {
        return None;
    }

    Some(ProvisioningIssue::MissingLockfile {
        target,
        driver,
        working_dir,
        candidates: candidates
            .into_iter()
            .map(|candidate| candidate.display().to_string())
            .collect(),
    })
}

fn resolve_python_requirements_path(working_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    [
        working_dir.join("requirements.txt"),
        working_dir.join("source").join("requirements.txt"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn detect_missing_required_env(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Option<ProvisioningIssue> {
    let required = plan.execution_required_envs();
    if required.is_empty() {
        return None;
    }

    let base_env = runtime_overrides::merged_env(plan.execution_env());
    let launch_env = launch_ctx.merged_env();
    let missing_keys: Vec<String> = required
        .into_iter()
        .filter(|name| {
            if launch_env
                .get(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return false;
            }
            if base_env
                .get(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return false;
            }
            std::env::var(name)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        })
        .collect();

    if missing_keys.is_empty() {
        return None;
    }

    Some(ProvisioningIssue::MissingRequiredEnv {
        target: plan.selected_target_label().to_string(),
        missing_keys,
    })
}

fn detect_runtime_selection_required(plan: &ManifestData) -> Option<ProvisioningIssue> {
    let runtime = plan.execution_runtime()?.trim().to_ascii_lowercase();
    let driver = plan.execution_driver()?.trim().to_ascii_lowercase();
    if runtime != "source" && runtime != "web" {
        return None;
    }

    let is_supported_driver = matches!(driver.as_str(), "node" | "python" | "deno");
    if !is_supported_driver {
        return None;
    }

    let has_explicit_runtime = if driver == "deno" {
        plan.execution_runtime_version().is_some()
    } else {
        plan.execution_runtime_version().is_some()
            || plan.execution_runtime_tool_version(&driver).is_some()
    };
    if has_explicit_runtime {
        return None;
    }

    Some(ProvisioningIssue::RuntimeSelectionRequired {
        target: plan.selected_target_label().to_string(),
        runtime,
        driver,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use capsule_core::router::{ExecutionProfile, ManifestData};

    use crate::executors::launch_context::RuntimeLaunchContext;

    use super::collect_issues;
    use super::ProvisioningIssue;

    fn manifest_data(target: toml::map::Map<String, toml::Value>) -> ManifestData {
        let mut manifest = toml::map::Map::new();
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::Value::Table(manifest),
            PathBuf::from("/tmp/capsule.toml"),
            PathBuf::from("/tmp"),
            ExecutionProfile::Dev,
            Some("default"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn detects_missing_required_env() {
        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("node".to_string()),
        );
        target.insert(
            "run_command".to_string(),
            toml::Value::String("node server.js".to_string()),
        );
        target.insert(
            "required_env".to_string(),
            toml::Value::Array(vec![toml::Value::String("DATABASE_URL".to_string())]),
        );

        let issues = collect_issues(&manifest_data(target), &RuntimeLaunchContext::empty())
            .expect("issues must resolve");
        assert!(issues.iter().any(|issue| matches!(
            issue,
            ProvisioningIssue::MissingRequiredEnv { missing_keys, .. }
                if missing_keys == &vec!["DATABASE_URL".to_string()]
        )));
    }

    #[test]
    fn detects_missing_node_lockfile_and_runtime_selection() {
        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("node".to_string()),
        );
        target.insert(
            "run_command".to_string(),
            toml::Value::String("node server.js".to_string()),
        );

        let issues = collect_issues(&manifest_data(target), &RuntimeLaunchContext::empty())
            .expect("issues must resolve");
        assert!(issues.iter().any(|issue| matches!(
            issue,
            ProvisioningIssue::MissingLockfile { driver, .. } if driver == "node"
        )));
        assert!(issues.iter().any(|issue| matches!(
            issue,
            ProvisioningIssue::RuntimeSelectionRequired { driver, .. } if driver == "node"
        )));
    }

    #[test]
    fn ignores_missing_node_lockfile_when_yarn_lock_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("yarn.lock"), "# yarn lockfile v1\n")
            .expect("write yarn lock");

        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("node".to_string()),
        );
        target.insert(
            "run_command".to_string(),
            toml::Value::String("npm:vite --host 127.0.0.1 --port 5175".to_string()),
        );

        let mut manifest = manifest_data(target);
        manifest.manifest_dir = temp.path().to_path_buf();
        manifest.manifest_path = temp.path().join("capsule.toml");

        let issues =
            collect_issues(&manifest, &RuntimeLaunchContext::empty()).expect("issues must resolve");
        assert!(!issues.iter().any(|issue| matches!(
            issue,
            ProvisioningIssue::MissingLockfile { driver, .. } if driver == "node"
        )));
    }
}
