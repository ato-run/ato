use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareSource {
    ManifestLifecycle,
    PackageJsonScript,
    NodeDependencyLockfile(&'static str),
    PythonDependencyLockfile,
    CargoDependencyLockfile,
    CargoDependencyBootstrap,
}

impl PrepareSource {
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::ManifestLifecycle => "capsule.toml [build.lifecycle].prepare",
            Self::PackageJsonScript => "package.json scripts.capsule:prepare",
            Self::NodeDependencyLockfile("package-lock.json") => {
                "package-lock.json dependency resolution"
            }
            Self::NodeDependencyLockfile("yarn.lock") => "yarn.lock dependency resolution",
            Self::NodeDependencyLockfile("pnpm-lock.yaml") => {
                "pnpm-lock.yaml dependency resolution"
            }
            Self::NodeDependencyLockfile("bun.lock") => "bun.lock dependency resolution",
            Self::NodeDependencyLockfile("bun.lockb") => "bun.lockb dependency resolution",
            Self::NodeDependencyLockfile(_) => "node dependency resolution",
            Self::PythonDependencyLockfile => "uv.lock dependency resolution",
            Self::CargoDependencyLockfile => "Cargo.lock dependency resolution",
            Self::CargoDependencyBootstrap => "Cargo.toml lockfile generation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareSpec {
    pub command: String,
    pub source: PrepareSource,
    pub working_dir: PathBuf,
}

#[cfg(test)]
pub fn detect_prepare_command(cwd: &Path) -> Result<Option<PrepareSpec>> {
    detect_prepare_command_with_workdir(cwd, cwd)
}

pub fn detect_prepare_specs(
    project_root: &Path,
    execution_working_directory: &Path,
) -> Result<Vec<PrepareSpec>> {
    let mut specs = detect_dependency_resolution_specs(execution_working_directory)?;
    if let Some(spec) =
        detect_prepare_command_with_workdir(project_root, execution_working_directory)?
    {
        specs.push(spec);
    }
    Ok(specs)
}

fn detect_prepare_command_with_workdir(
    project_root: &Path,
    execution_working_directory: &Path,
) -> Result<Option<PrepareSpec>> {
    let manifest_path = project_root.join("capsule.toml");
    if manifest_path.exists() {
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let parsed: toml::Value = toml::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
        if let Some(command) = parsed
            .get("build")
            .and_then(|v| v.get("lifecycle"))
            .and_then(|v| v.get("prepare"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(Some(PrepareSpec {
                command: command.to_string(),
                source: PrepareSource::ManifestLifecycle,
                working_dir: project_root.to_path_buf(),
            }));
        }
    }

    let mut package_json_roots = vec![execution_working_directory];
    if execution_working_directory != project_root {
        package_json_roots.push(project_root);
    }

    for root in package_json_roots {
        let package_json_path = root.join("package.json");
        if !package_json_path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&package_json_path)
            .with_context(|| format!("Failed to read {}", package_json_path.display()))?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", package_json_path.display()))?;
        let has_capsule_prepare = parsed
            .get("scripts")
            .and_then(|v| v.get("capsule:prepare"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some();
        if has_capsule_prepare {
            return Ok(Some(PrepareSpec {
                command: "npm run capsule:prepare".to_string(),
                source: PrepareSource::PackageJsonScript,
                working_dir: root.to_path_buf(),
            }));
        }
    }

    Ok(None)
}

fn detect_dependency_resolution_specs(cwd: &Path) -> Result<Vec<PrepareSpec>> {
    let mut specs = Vec::new();

    let mut node_specs = Vec::new();
    if cwd.join("package-lock.json").exists() {
        node_specs.push(PrepareSpec {
            command: "npm ci".to_string(),
            source: PrepareSource::NodeDependencyLockfile("package-lock.json"),
            working_dir: cwd.to_path_buf(),
        });
    }
    if cwd.join("yarn.lock").exists() {
        node_specs.push(PrepareSpec {
            command: "yarn install --frozen-lockfile".to_string(),
            source: PrepareSource::NodeDependencyLockfile("yarn.lock"),
            working_dir: cwd.to_path_buf(),
        });
    }
    if cwd.join("pnpm-lock.yaml").exists() {
        node_specs.push(PrepareSpec {
            command: "pnpm install --frozen-lockfile".to_string(),
            source: PrepareSource::NodeDependencyLockfile("pnpm-lock.yaml"),
            working_dir: cwd.to_path_buf(),
        });
    }
    if cwd.join("bun.lock").exists() {
        node_specs.push(PrepareSpec {
            command: "bun install --frozen-lockfile".to_string(),
            source: PrepareSource::NodeDependencyLockfile("bun.lock"),
            working_dir: cwd.to_path_buf(),
        });
    }
    if cwd.join("bun.lockb").exists() {
        node_specs.push(PrepareSpec {
            command: "bun install --frozen-lockfile".to_string(),
            source: PrepareSource::NodeDependencyLockfile("bun.lockb"),
            working_dir: cwd.to_path_buf(),
        });
    }

    match node_specs.len() {
        0 => {}
        1 => specs.extend(node_specs),
        _ => {
            anyhow::bail!(
                "multiple node lockfiles detected; keep only one of package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb"
            )
        }
    }

    if cwd.join("uv.lock").exists() {
        specs.push(PrepareSpec {
            command: "uv sync --frozen".to_string(),
            source: PrepareSource::PythonDependencyLockfile,
            working_dir: cwd.to_path_buf(),
        });
    }

    if let Some(spec) = detect_cargo_prepare_spec(cwd) {
        specs.push(spec);
    }

    Ok(specs)
}

fn detect_cargo_prepare_spec(cwd: &Path) -> Option<PrepareSpec> {
    let candidates = [
        (cwd.join("Cargo.toml"), cwd.join("Cargo.lock"), None),
        (
            cwd.join("src-tauri/Cargo.toml"),
            cwd.join("src-tauri/Cargo.lock"),
            Some("src-tauri/Cargo.toml"),
        ),
    ];

    for (manifest_path, lock_path, manifest_arg) in candidates {
        if !manifest_path.exists() {
            continue;
        }

        let (command, source) = if lock_path.exists() {
            (
                cargo_prepare_command("fetch --locked", manifest_arg),
                PrepareSource::CargoDependencyLockfile,
            )
        } else {
            (
                cargo_prepare_command("generate-lockfile", manifest_arg),
                PrepareSource::CargoDependencyBootstrap,
            )
        };
        return Some(PrepareSpec {
            command,
            source,
            working_dir: cwd.to_path_buf(),
        });
    }

    None
}

fn cargo_prepare_command(subcommand: &str, manifest_arg: Option<&str>) -> String {
    match manifest_arg {
        Some(manifest_path) => format!("cargo {subcommand} --manifest-path {manifest_path}"),
        None => format!("cargo {subcommand}"),
    }
}

pub fn run_prepare_command(spec: &PrepareSpec, json_output: bool) -> Result<()> {
    let mut command = shell_command_for(&spec.command);
    command.current_dir(&spec.working_dir);

    if json_output {
        let output = command
            .output()
            .with_context(|| format!("Failed to execute prepare command: {}", spec.command))?;
        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        anyhow::bail!(
            "Prepare command failed with status {}: {}{}",
            status_label(output.status.code()),
            spec.command,
            if details.is_empty() {
                "".to_string()
            } else {
                format!("\n{}", details)
            }
        );
    }

    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = command
        .status()
        .with_context(|| format!("Failed to execute prepare command: {}", spec.command))?;
    if status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "Prepare command failed with status {}: {}",
        status_label(status.code()),
        spec.command
    )
}

#[cfg(windows)]
fn shell_command_for(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(not(windows))]
fn shell_command_for(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

fn status_label(code: Option<i32>) -> String {
    code.map(|v| v.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{detect_prepare_command, detect_prepare_specs, PrepareSource};

    #[test]
    fn detect_prefers_manifest_prepare_over_package_script() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(
            tmp.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/deno"
run = "main.ts"
[build.lifecycle]
prepare = "pnpm -C apps/dashboard build"
"#,
        )
        .expect("write capsule.toml");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"capsule:prepare":"npm run build"}}"#,
        )
        .expect("write package.json");

        let detected = detect_prepare_command(tmp.path())
            .expect("detect should succeed")
            .expect("spec should exist");
        assert_eq!(detected.source, PrepareSource::ManifestLifecycle);
        assert_eq!(detected.command, "pnpm -C apps/dashboard build");
        assert_eq!(detected.working_dir, tmp.path());
    }

    #[test]
    fn detect_falls_back_to_package_capsule_prepare_script() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"capsule:prepare":"npm run build"}}"#,
        )
        .expect("write package.json");

        let detected = detect_prepare_command(tmp.path())
            .expect("detect should succeed")
            .expect("spec should exist");
        assert_eq!(detected.source, PrepareSource::PackageJsonScript);
        assert_eq!(detected.command, "npm run capsule:prepare");
        assert_eq!(detected.working_dir, tmp.path());
    }

    #[test]
    fn detect_returns_none_when_no_prepare_config_exists() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"build":"npm run test"}}"#,
        )
        .expect("write package.json");

        let detected = detect_prepare_command(tmp.path()).expect("detect should succeed");
        assert!(detected.is_none());
    }

    #[test]
    fn detect_prepare_specs_adds_dependency_resolution_before_prepare_command() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(tmp.path().join("package-lock.json"), "{}\n").expect("write package lock");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .expect("write Cargo.toml");
        std::fs::write(tmp.path().join("Cargo.lock"), "version = 3\n").expect("write cargo lock");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"capsule:prepare":"npm run build"}}"#,
        )
        .expect("write package.json");

        let detected = detect_prepare_specs(tmp.path(), tmp.path()).expect("detect should succeed");
        assert_eq!(detected.len(), 3);
        assert_eq!(
            detected[0].source,
            PrepareSource::NodeDependencyLockfile("package-lock.json")
        );
        assert_eq!(detected[0].command, "npm ci");
        assert_eq!(detected[1].source, PrepareSource::CargoDependencyLockfile);
        assert_eq!(detected[1].command, "cargo fetch --locked");
        assert_eq!(detected[2].source, PrepareSource::PackageJsonScript);
    }

    #[test]
    fn detect_prepare_specs_rejects_ambiguous_node_lockfiles() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(tmp.path().join("package-lock.json"), "{}\n").expect("write package lock");
        std::fs::write(
            tmp.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("write pnpm lock");

        let err = detect_prepare_specs(tmp.path(), tmp.path()).expect_err("must reject ambiguity");
        assert!(err.to_string().contains("multiple node lockfiles detected"));
    }

    #[test]
    fn detect_prepare_specs_uses_execution_working_directory_for_nested_package_script() {
        let tmp = tempdir().expect("tmp dir");
        let app = tmp.path().join("apps").join("desktop");
        std::fs::create_dir_all(&app).expect("create app dir");
        std::fs::write(
            app.join("package.json"),
            r#"{"scripts":{"capsule:prepare":"npm run build"}}"#,
        )
        .expect("write package.json");

        let detected = detect_prepare_specs(tmp.path(), &app).expect("detect should succeed");
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].source, PrepareSource::PackageJsonScript);
        assert_eq!(detected[0].working_dir, app);
    }

    #[test]
    fn detect_prepare_specs_generates_nested_tauri_cargo_lock_before_build() {
        let tmp = tempdir().expect("tmp dir");
        let src_tauri = tmp.path().join("src-tauri");
        std::fs::create_dir_all(&src_tauri).expect("create src-tauri");
        std::fs::write(
            src_tauri.join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .expect("write Cargo.toml");

        let detected = detect_prepare_specs(tmp.path(), tmp.path()).expect("detect should succeed");
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].source, PrepareSource::CargoDependencyBootstrap);
        assert_eq!(
            detected[0].command,
            "cargo generate-lockfile --manifest-path src-tauri/Cargo.toml"
        );
        assert_eq!(detected[0].working_dir, tmp.path());
    }

    #[test]
    fn detect_prepare_specs_fetches_nested_tauri_cargo_lock_when_present() {
        let tmp = tempdir().expect("tmp dir");
        let src_tauri = tmp.path().join("src-tauri");
        std::fs::create_dir_all(&src_tauri).expect("create src-tauri");
        std::fs::write(
            src_tauri.join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .expect("write Cargo.toml");
        std::fs::write(src_tauri.join("Cargo.lock"), "version = 3\n").expect("write Cargo.lock");

        let detected = detect_prepare_specs(tmp.path(), tmp.path()).expect("detect should succeed");
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].source, PrepareSource::CargoDependencyLockfile);
        assert_eq!(
            detected[0].command,
            "cargo fetch --locked --manifest-path src-tauri/Cargo.toml"
        );
        assert_eq!(detected[0].working_dir, tmp.path());
    }
}
