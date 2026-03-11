use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareSource {
    ManifestLifecycle,
    PackageJsonScript,
}

impl PrepareSource {
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::ManifestLifecycle => "capsule.toml [build.lifecycle].prepare",
            Self::PackageJsonScript => "package.json scripts.capsule:prepare",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareSpec {
    pub command: String,
    pub source: PrepareSource,
}

pub fn detect_prepare_command(cwd: &Path) -> Result<Option<PrepareSpec>> {
    let manifest_path = cwd.join("capsule.toml");
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
            }));
        }
    }

    let package_json_path = cwd.join("package.json");
    if package_json_path.exists() {
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
            }));
        }
    }

    Ok(None)
}

pub fn run_prepare_command(spec: &PrepareSpec, cwd: &Path, json_output: bool) -> Result<()> {
    let mut command = shell_command_for(&spec.command);
    command.current_dir(cwd);

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
    cmd.arg("-lc").arg(command);
    cmd
}

fn status_label(code: Option<i32>) -> String {
    code.map(|v| v.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{detect_prepare_command, PrepareSource};

    #[test]
    fn detect_prefers_manifest_prepare_over_package_script() {
        let tmp = tempdir().expect("tmp dir");
        std::fs::write(
            tmp.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "deno"
entrypoint = "main.ts"

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
}
