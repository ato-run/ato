use anyhow::{Context, Result};
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use capsule_core::router::ManifestData;

use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;

const STATIC_SERVER_SCRIPT: &str = include_str!("../../../assets/static_file_server.ts");

pub fn execute(plan: &ManifestData, _reporter: std::sync::Arc<CliReporter>) -> Result<()> {
    let (deno_bin, args) = build_static_server_command(plan)?;
    let status = Command::new(deno_bin)
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to launch deno file server for runtime=web static target")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "deno file server exited with status {}",
            status
        ))
    }
}

pub fn spawn_background(plan: &ManifestData) -> Result<std::process::Child> {
    let (deno_bin, args) = build_static_server_command(plan)?;
    Command::new(deno_bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to launch deno file server for runtime=web static target in background")
}

fn build_static_server_command(plan: &ManifestData) -> Result<(PathBuf, Vec<String>)> {
    let driver = plan
        .execution_driver()
        .map(|v| v.trim().to_ascii_lowercase())
        .ok_or_else(|| anyhow::anyhow!("runtime=web target requires driver"))?;
    if driver != "static" {
        anyhow::bail!(
            "open_web executor only supports runtime=web driver=static (got '{}')",
            driver
        );
    }

    let entrypoint = plan
        .execution_entrypoint()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("runtime=web target requires entrypoint"))?;
    let port = runtime_overrides::override_port(plan.execution_port()).ok_or_else(|| {
        anyhow::anyhow!(
            "runtime=web target '{}' requires targets.<label>.port",
            plan.selected_target_label()
        )
    })?;

    let serve_dir = resolve_static_serve_dir(&plan.manifest_dir, &entrypoint)?;
    let deno_bin = runtime_manager::ensure_deno_binary(plan)?;
    let script_path = ensure_static_server_script()?;
    let args = build_deno_file_server_args(&script_path, &serve_dir, port);
    Ok((deno_bin, args))
}

fn resolve_static_serve_dir(manifest_dir: &Path, entrypoint: &str) -> Result<PathBuf> {
    let path = manifest_dir.join(entrypoint.trim());
    let root = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());
    let normalized_path = normalize_path(&path);

    if !normalized_path.starts_with(&root) {
        anyhow::bail!(
            "runtime=web static entrypoint '{}' resolves outside manifest directory",
            entrypoint
        );
    }

    if !path.exists() || !path.is_dir() {
        anyhow::bail!(
            "runtime=web static entrypoint '{}' must be an existing directory",
            entrypoint
        );
    }

    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve static entrypoint path '{}'", entrypoint))?;

    if !canonical_path.starts_with(&root) {
        anyhow::bail!(
            "runtime=web static entrypoint '{}' resolves outside manifest directory",
            entrypoint
        );
    }

    Ok(canonical_path)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }

    normalized
}

fn ensure_static_server_script() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home directory"))?;
    let script_path = home
        .join(".ato")
        .join("cache")
        .join("scripts")
        .join("static_file_server.ts");
    if let Some(parent) = script_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&script_path, STATIC_SERVER_SCRIPT)
        .with_context(|| format!("Failed to write {}", script_path.display()))?;
    Ok(script_path)
}

fn build_deno_file_server_args(script_path: &Path, serve_dir: &Path, port: u16) -> Vec<String> {
    vec![
        "run".to_string(),
        "--no-prompt".to_string(),
        format!("--allow-read={}", serve_dir.to_string_lossy()),
        format!("--allow-net=127.0.0.1:{port},localhost:{port}"),
        script_path.to_string_lossy().to_string(),
        serve_dir.to_string_lossy().to_string(),
        port.to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deno_file_server_args_are_hardened_for_loopback_only() {
        let args = build_deno_file_server_args(
            Path::new("/tmp/static_file_server.ts"),
            Path::new("/tmp/site"),
            61357,
        );
        let rendered = args.join(" ");
        assert!(rendered.contains("--allow-read=/tmp/site"));
        assert!(rendered.contains("--allow-net=127.0.0.1:61357,localhost:61357"));
        assert!(rendered.contains("/tmp/static_file_server.ts"));
        assert!(rendered.contains("--host 127.0.0.1"));
        assert!(rendered.contains("61357"));
    }
}
