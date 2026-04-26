use anyhow::{Context, Result};
use std::process::{Command, Stdio};
use tracing::warn;

use capsule_core::router::ManifestData;

use capsule_core::CapsuleReporter;

use crate::common::proxy;

use super::launch_context::RuntimeLaunchContext;

pub fn execute(
    plan: &ManifestData,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<i32> {
    let component = resolve_component(plan)?;
    let component_path = plan.resolve_path(&component);

    if which::which("wasmtime").is_err() {
        futures::executor::block_on(
            reporter.warn("wasmtime is not installed or not on PATH".to_string()),
        )?;
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

    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
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
