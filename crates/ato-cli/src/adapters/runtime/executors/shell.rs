use anyhow::{Context, Result};
use std::process::{Command, Stdio};

use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;

use crate::common::proxy;
use crate::runtime::overrides as runtime_overrides;

use super::launch_context::RuntimeLaunchContext;
use super::source::{CapsuleProcess, ExecuteMode};

pub fn execute(
    plan: &ManifestData,
    mode: ExecuteMode,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<CapsuleProcess> {
    let run_command = plan
        .execution_run_command()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("shell executor requires targets.<label>.run_command"))?;

    let launch_spec = derive_launch_spec(plan)?;
    let working_dir = launch_spec.working_dir;
    let run_command = normalize_local_shell_command(&run_command, &working_dir);
    let mut cmd = shell_command(&run_command);
    cmd.current_dir(working_dir);

    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        cmd.env("PORT", port.to_string());
    }
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    match mode {
        ExecuteMode::Foreground => {
            cmd.stdin(Stdio::inherit());
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }
        ExecuteMode::Background => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
        ExecuteMode::Piped => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        }
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute shell command: {}", run_command))?;

    Ok(CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx: None,
        workload_pid: None,
        log_path: None,
    })
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-lc", command]);
        cmd
    }
}

fn normalize_local_shell_command(command: &str, working_dir: &std::path::Path) -> String {
    let Ok(mut tokens) = shell_words::split(command) else {
        return command.to_string();
    };
    let Some(first) = tokens.first_mut() else {
        return command.to_string();
    };
    if !first.contains('/') && working_dir.join(first.as_str()).is_file() {
        *first = format!("./{first}");
        return shell_words::join(tokens.iter().map(String::as_str));
    }
    command.to_string()
}
