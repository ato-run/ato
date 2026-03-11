use anyhow::{Context, Result};
use std::process::{Command, Stdio};
use tracing::warn;

use crate::router::ManifestData;

pub fn execute(plan: &ManifestData) -> Result<i32> {
    let component = resolve_component(plan)?;
    let component_path = plan.resolve_path(&component);

    if which::which("wasmtime").is_err() {
        anyhow::bail!("wasmtime is not installed or not on PATH");
    }

    let mut cmd = Command::new("wasmtime");
    cmd.arg("run").arg(component_path);

    let mut args = plan.targets_wasm_args();
    if args.is_empty() {
        if let Some(entrypoint) = plan.execution_entrypoint() {
            if let Ok(mut parsed) = shell_words::split(&entrypoint) {
                if !parsed.is_empty() {
                    parsed.remove(0);
                    args = parsed;
                }
            }
        }
    }

    if !args.is_empty() {
        cmd.arg("--").args(args);
    }

    for (k, v) in plan.execution_env() {
        cmd.env(k, v);
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to run wasmtime")?;

    Ok(status.code().unwrap_or(1))
}

fn resolve_component(plan: &ManifestData) -> Result<String> {
    if let Some(component) = plan.targets_wasm_component() {
        return Ok(component);
    }

    if let Some(entrypoint) = plan.execution_entrypoint() {
        if is_wasm_path(&entrypoint) {
            return Ok(entrypoint);
        }

        if let Ok(parsed) = shell_words::split(&entrypoint) {
            if let Some(first) = parsed.first() {
                if is_wasm_path(first) {
                    return Ok(first.to_string());
                }
            }
        }
    }

    warn!("Wasm runtime selected but no component path found");
    anyhow::bail!("Wasm runtime selected but no component path found")
}

fn is_wasm_path(value: &str) -> bool {
    value.ends_with(".wasm") || value.ends_with(".component")
}
