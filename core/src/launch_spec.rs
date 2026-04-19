use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::router::ManifestData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchSpecSource {
    Entrypoint,
    RunCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub working_dir: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub required_lockfile: Option<PathBuf>,
    pub runtime: Option<String>,
    pub driver: Option<String>,
    pub language: Option<String>,
    pub port: Option<u16>,
    pub source: LaunchSpecSource,
}

pub fn derive_launch_spec(plan: &ManifestData) -> Result<LaunchSpec> {
    let runtime = plan.execution_runtime();
    let driver = plan.execution_driver();
    let language = resolve_launch_language(plan, driver.as_deref());
    let env_vars = plan.execution_env();
    let port = plan.execution_port();

    if let Some(entrypoint) = plan
        .execution_entrypoint()
        .filter(|value| !value.trim().is_empty())
    {
        let working_dir = resolve_launch_working_dir(plan, &entrypoint);
        let required_lockfile =
            resolve_required_lockfile(plan, &working_dir, language.as_deref(), driver.as_deref());

        return Ok(LaunchSpec {
            working_dir,
            command: entrypoint,
            args: plan.targets_oci_cmd(),
            env_vars,
            required_lockfile,
            runtime,
            driver,
            language,
            port,
            source: LaunchSpecSource::Entrypoint,
        });
    }

    let run_command = plan
        .execution_run_command()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "target '{}' requires entrypoint or run_command",
                plan.selected_target_label()
            )
        })?;

    derive_run_command_launch_spec(
        plan,
        &run_command,
        runtime,
        driver,
        language,
        env_vars,
        port,
    )
}

fn derive_run_command_launch_spec(
    plan: &ManifestData,
    run_command: &str,
    runtime: Option<String>,
    driver: Option<String>,
    language: Option<String>,
    env_vars: HashMap<String, String>,
    port: Option<u16>,
) -> Result<LaunchSpec> {
    let tokens = shell_words::split(run_command).unwrap_or_else(|_| vec![run_command.to_string()]);
    let Some(first) = tokens.first().map(String::as_str) else {
        return Err(anyhow!("run_command is empty"));
    };

    let resolved_driver = driver
        .as_deref()
        .or(language.as_deref())
        .unwrap_or_default();

    let (command, args) = match resolved_driver {
        value if value.eq_ignore_ascii_case("node") => {
            if first == "node" {
                let command = tokens.get(1).cloned().ok_or_else(|| {
                    anyhow!("source/node run_command must include a script entrypoint")
                })?;
                (command, tokens.into_iter().skip(2).collect::<Vec<_>>())
            } else if first.starts_with("npm:") || matches!(first, "npm" | "pnpm" | "yarn" | "bun")
            {
                // Accept package manager invocations: `npm run dev`, `pnpm run dev`,
                // `yarn dev`, `bun run dev`, etc.
                (
                    tokens[0].clone(),
                    tokens.into_iter().skip(1).collect::<Vec<_>>(),
                )
            } else {
                return Err(anyhow!(
                    "source/node run_command must start with 'node', 'npm:<package>', or a package manager (npm/pnpm/yarn/bun), got '{}'",
                    first
                ));
            }
        }
        value if value.eq_ignore_ascii_case("python") => {
            if !matches!(first, "python" | "python3" | "uv") {
                return Err(anyhow!(
                    "source/python run_command must start with 'python', 'python3', or 'uv', got '{}'",
                    first
                ));
            }
            let command = if first == "uv" {
                let Some(index) = tokens
                    .iter()
                    .position(|token| matches!(token.as_str(), "python" | "python3"))
                else {
                    return Err(anyhow!(
                        "source/python uv run_command must include python or python3"
                    ));
                };
                tokens.get(index + 1).cloned().ok_or_else(|| {
                    anyhow!("source/python run_command must include a script entrypoint")
                })?
            } else {
                tokens.get(1).cloned().ok_or_else(|| {
                    anyhow!("source/python run_command must include a script entrypoint")
                })?
            };
            let command_index = tokens
                .iter()
                .position(|token| token == &command)
                .unwrap_or(1);
            (
                command,
                tokens
                    .into_iter()
                    .skip(command_index + 1)
                    .collect::<Vec<_>>(),
            )
        }
        _ => (
            tokens[0].clone(),
            tokens.into_iter().skip(1).collect::<Vec<_>>(),
        ),
    };

    let working_dir = resolve_launch_working_dir(plan, &command);
    let required_lockfile =
        resolve_required_lockfile(plan, &working_dir, language.as_deref(), driver.as_deref());

    Ok(LaunchSpec {
        working_dir,
        command,
        args,
        env_vars,
        required_lockfile,
        runtime,
        driver,
        language,
        port,
        source: LaunchSpecSource::RunCommand,
    })
}

fn resolve_launch_working_dir(plan: &ManifestData, command: &str) -> PathBuf {
    if let Some(working_dir) = plan
        .execution_working_dir()
        .filter(|value| !value.trim().is_empty())
    {
        return plan.manifest_dir.join(working_dir);
    }

    let source_dir = plan.manifest_dir.join("source");
    if source_dir.is_dir() {
        // For package manager commands (npm, pnpm, yarn, bun), the command itself
        // is a system binary — not a file inside source/. Check instead whether
        // source/ looks like a Node.js project (has package.json).
        // For any command, prefer source/ when source/package.json exists —
        // system binaries like npm/pnpm/node/vite all need to run where
        // package.json (and node_modules) live.
        if source_dir.join("package.json").exists() {
            return source_dir;
        }
        if command_path_exists(&source_dir, command) {
            return source_dir;
        }
    }

    plan.manifest_dir.clone()
}

fn command_path_exists(base: &Path, command: &str) -> bool {
    let path = Path::new(command.trim());
    if path.is_absolute() {
        return path.exists();
    }
    base.join(path).exists()
}

fn resolve_launch_language(plan: &ManifestData, driver: Option<&str>) -> Option<String> {
    if let Some(language) = plan
        .execution_language()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(language);
    }

    if let Some(driver) = driver.filter(|value| !value.trim().is_empty()) {
        return Some(driver.to_string());
    }

    let entrypoint = plan.execution_entrypoint()?;
    let lowered = entrypoint.trim().to_ascii_lowercase();
    if lowered.ends_with(".py") {
        return Some("python".to_string());
    }
    if lowered.ends_with(".js")
        || lowered.ends_with(".cjs")
        || lowered.ends_with(".mjs")
        || lowered.ends_with(".ts")
    {
        return Some("node".to_string());
    }
    None
}

fn resolve_required_lockfile(
    plan: &ManifestData,
    working_dir: &Path,
    language: Option<&str>,
    driver: Option<&str>,
) -> Option<PathBuf> {
    let resolved = driver.or(language).unwrap_or_default();
    if resolved.eq_ignore_ascii_case("node") {
        return first_existing_path([
            working_dir.join("package-lock.json"),
            working_dir.join("yarn.lock"),
            working_dir.join("pnpm-lock.yaml"),
            working_dir.join("bun.lock"),
            working_dir.join("bun.lockb"),
            plan.manifest_dir.join("package-lock.json"),
            plan.manifest_dir.join("yarn.lock"),
            plan.manifest_dir.join("pnpm-lock.yaml"),
            plan.manifest_dir.join("bun.lock"),
            plan.manifest_dir.join("bun.lockb"),
            plan.manifest_dir.join("source").join("package-lock.json"),
            plan.manifest_dir.join("source").join("yarn.lock"),
            plan.manifest_dir.join("source").join("pnpm-lock.yaml"),
            plan.manifest_dir.join("source").join("bun.lock"),
            plan.manifest_dir.join("source").join("bun.lockb"),
        ])
        .or_else(|| {
            is_provider_workspace_marker(plan, working_dir)
                .then(|| plan.manifest_dir.join("package-lock.json"))
        });
    }

    if resolved.eq_ignore_ascii_case("python") {
        return first_existing_path([
            working_dir.join("uv.lock"),
            plan.manifest_dir.join("uv.lock"),
            plan.manifest_dir.join("source").join("uv.lock"),
        ])
        .or_else(|| {
            is_provider_workspace_marker(plan, working_dir)
                .then(|| plan.manifest_dir.join("uv.lock"))
        });
    }

    None
}

fn is_provider_workspace_marker(plan: &ManifestData, working_dir: &Path) -> bool {
    [
        working_dir.join("resolution.json"),
        plan.manifest_dir.join("resolution.json"),
        plan.manifest_dir.join("source").join("resolution.json"),
    ]
    .into_iter()
    .any(|path| path.exists())
}

fn first_existing_path<const N: usize>(candidates: [PathBuf; N]) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::router::{execution_descriptor_from_manifest_parts, ExecutionProfile, ManifestData};

    fn plan_from_manifest(tmp: &tempfile::TempDir, manifest: &str) -> ManifestData {
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(&manifest_path, manifest).expect("write manifest");
        let parsed: toml::Value = toml::from_str(manifest).expect("parse manifest");
        execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            tmp.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn derive_launch_spec_prefers_source_dir_for_entrypoint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(
            tmp.path().join("source").join("main.js"),
            "console.log('ok');",
        )
        .expect("write entrypoint");
        let plan = plan_from_manifest(
            &tmp,
            r#"
[targets.app]
runtime = "source"
driver = "node"
entrypoint = "main.js"
"#,
        );

        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(spec.working_dir, tmp.path());
        assert_eq!(spec.command, "main.js");
        assert_eq!(spec.source, LaunchSpecSource::Entrypoint);
    }

    #[test]
    fn derive_launch_spec_parses_node_run_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(
            tmp.path().join("source").join("lib.js"),
            "console.log('ok');",
        )
        .expect("write script");
        std::fs::write(
            tmp.path().join("source").join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'",
        )
        .expect("write lock");
        let plan = plan_from_manifest(
            &tmp,
            r#"
[targets.app]
runtime = "source"
driver = "node"
run_command = "node lib.js fixtures/db.json --port 3000"
"#,
        );

        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(spec.working_dir, tmp.path());
        assert_eq!(spec.command, "lib.js");
        assert_eq!(spec.args, vec!["fixtures/db.json", "--port", "3000"]);
        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("source").join("pnpm-lock.yaml"))
        );
        assert_eq!(spec.source, LaunchSpecSource::RunCommand);
    }

    #[test]
    fn derive_launch_spec_accepts_node_npm_specifier_run_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(
            tmp.path().join("source").join("package-lock.json"),
            "{\"lockfileVersion\":3}",
        )
        .expect("write lock");
        let plan = plan_from_manifest(
            &tmp,
            r#"
[targets.app]
runtime = "source"
driver = "node"
run_command = "npm:vite --host 127.0.0.1 --port 5175"
"#,
        );

        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(spec.working_dir, tmp.path());
        assert_eq!(spec.command, "npm:vite");
        assert_eq!(spec.args, vec!["--host", "127.0.0.1", "--port", "5175"]);
        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("source").join("package-lock.json"))
        );
        assert_eq!(spec.source, LaunchSpecSource::RunCommand);
    }

    #[test]
    fn derive_launch_spec_accepts_yarn_lock_for_node_run_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("mkdir source");
        std::fs::write(
            tmp.path().join("source").join("yarn.lock"),
            "# yarn lockfile v1\n",
        )
        .expect("write lock");
        let plan = plan_from_manifest(
            &tmp,
            r#"
[targets.app]
runtime = "source"
driver = "node"
run_command = "npm:vite --host 127.0.0.1 --port 5175"
"#,
        );

        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("source").join("yarn.lock"))
        );
    }

    #[test]
    fn derive_launch_spec_honors_explicit_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("app")).expect("mkdir app");
        std::fs::write(tmp.path().join("app").join("server.py"), "print('ok')")
            .expect("write script");
        std::fs::write(tmp.path().join("app").join("uv.lock"), "version = 1").expect("write lock");
        let plan = plan_from_manifest(
            &tmp,
            r#"
[targets.app]
runtime = "source"
driver = "python"
working_dir = "app"
entrypoint = "server.py"
"#,
        );

        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(spec.working_dir, tmp.path().join("app"));
        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("app").join("uv.lock"))
        );
    }
}
