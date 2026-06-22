use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::orchestrator::resolve_ato_binary;
use crate::proc_util::CommandNoWindowExt;
use crate::source_import_session::ImportOutput;

/// Run `ato import <repo> --emit-json` and parse the JSON output.
///
/// The returned `ImportOutput.run.status` is always `"not_run"` because
/// `--run` is not passed. Use this for the initial inference step.
pub(crate) fn infer(repo: &str) -> Result<ImportOutput> {
    let ato = resolve_ato_binary()?;
    let output = Command::new(&ato)
        .no_console_window()
        .arg("import")
        .arg(repo)
        .arg("--emit-json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to spawn {} import --emit-json", ato.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ato import failed (status {}): {}",
            output.status,
            head_lines(&stderr, 20),
        );
    }

    parse_import_output(&output.stdout)
}

/// Run `ato import <repo> --recipe <recipe_path> --run --keep-alive --emit-json`.
///
/// The returned `ImportOutput.run.status` is `"running"` or `"failed"`.
/// A failed shadow-workspace run still exits 0 at the CLI level — the
/// failure shows up inside the JSON's `run` field, not as a process error.
///
/// When `allow_unsafe` is true, the child process inherits
/// `CAPSULE_ALLOW_UNSAFE=1` so source/native execution is permitted.
pub(crate) fn run_with_recipe(
    repo: &str,
    recipe_path: &Path,
    allow_unsafe: bool,
) -> Result<ImportOutput> {
    let ato = resolve_ato_binary()?;
    let mut cmd = Command::new(&ato);
    cmd.no_console_window()
        .arg("import")
        .arg(repo)
        .arg("--recipe")
        .arg(recipe_path)
        .arg("--run")
        .arg("--keep-alive")
        .arg("--emit-json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("ATO_DESKTOP_PARENT_PID", std::process::id().to_string());
    if allow_unsafe {
        cmd.env("CAPSULE_ALLOW_UNSAFE", "1");
    }
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn {} import --run", ato.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ato import --run failed (status {}): {}",
            output.status,
            head_lines(&stderr, 20),
        );
    }

    parse_import_output(&output.stdout)
}

pub(crate) fn stop_import_preview_session(run_session_id: &str) -> Result<()> {
    let ato = resolve_ato_binary()?;
    let output = Command::new(&ato)
        .no_console_window()
        .arg("stop")
        .arg(run_session_id)
        .arg("--force")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to spawn {} stop", ato.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ato stop failed (status {}): {}",
            output.status,
            head_lines(&stderr, 20),
        );
    }
    Ok(())
}

pub(crate) fn sweep_stale_import_preview_sessions() -> Result<()> {
    let ato = resolve_ato_binary()?;
    let output = Command::new(&ato)
        .no_console_window()
        .arg("internal")
        .arg("import-preview-sweep")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "failed to spawn {} internal import-preview-sweep",
                ato.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ato import-preview-sweep failed (status {}): {}",
            output.status,
            head_lines(&stderr, 20),
        );
    }
    Ok(())
}

fn parse_import_output(stdout: &[u8]) -> Result<ImportOutput> {
    let stdout = std::str::from_utf8(stdout).context("ato import emitted non-utf8 stdout")?;
    let trimmed = stdout.trim();
    serde_json::from_str::<ImportOutput>(trimmed)
        .with_context(|| format!("ato import emitted invalid JSON ({} bytes)", trimmed.len()))
}

fn head_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inferred_output() {
        let json = br#"{
            "source": {
                "source_url_normalized": "https://github.com/owner/repo",
                "source_host": "github.com",
                "repo_namespace": "owner",
                "repo_name": "repo",
                "revision_id": "abc",
                "source_tree_hash": "blake3:tree",
                "subdir": "."
            },
            "recipe": {
                "origin": "inference",
                "target_label": "web",
                "platform_os": "darwin",
                "platform_arch": "arm64",
                "recipe_toml": "schema_version = \"0.3\"\n",
                "recipe_hash": "blake3:recipe"
            },
            "run": {
                "status": "not_run",
                "phase": null,
                "error_class": null,
                "error_excerpt": null
            }
        }"#;
        let output = parse_import_output(json).expect("parsed");
        assert_eq!(output.source.repo_namespace, "owner");
        assert_eq!(output.recipe.origin, "inference");
        assert_eq!(output.run.status, "not_run");
    }

    #[test]
    fn parses_failed_run_output() {
        let json = br#"{
            "source": {
                "source_url_normalized": "https://github.com/owner/repo",
                "source_host": "github.com",
                "repo_namespace": "owner",
                "repo_name": "repo",
                "revision_id": "abc",
                "source_tree_hash": "blake3:tree",
                "subdir": "."
            },
            "recipe": {
                "origin": "manual",
                "target_label": null,
                "platform_os": "darwin",
                "platform_arch": "arm64",
                "recipe_toml": "schema_version = \"0.3\"\n",
                "recipe_hash": "blake3:recipe"
            },
            "run": {
                "status": "failed",
                "phase": "install",
                "error_class": "node_gyp_missing_distutils",
                "error_excerpt": "ModuleNotFoundError: No module named 'distutils'"
            }
        }"#;
        let output = parse_import_output(json).expect("parsed");
        assert_eq!(output.run.status, "failed");
        assert_eq!(output.run.phase.as_deref(), Some("install"));
        assert_eq!(
            output.run.error_class.as_deref(),
            Some("node_gyp_missing_distutils")
        );
    }

    #[test]
    fn rejects_invalid_json() {
        let err = parse_import_output(b"not json").unwrap_err();
        assert!(format!("{err:#}").contains("invalid JSON"));
    }
}
