//! Desktop process boundary for Ato.
//!
//! The native launcher intentionally delegates computation advancement to the
//! `ato` binary and opens the web console for visual interaction. It does not
//! link application semantics or providers into the desktop process.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use ato_ipc::computation::{ComputationCommand, ComputationCommandResult};

pub fn dispatch(request: &ComputationCommand) -> Result<ComputationCommandResult> {
    let output = match request {
        ComputationCommand::Run { source } => ato_command()?.args(["run", source]).output()?,
        ComputationCommand::ListRuns => ato_command()?.args(["ps", "--json"]).output()?,
    };
    Ok(result(output))
}

pub fn launch_console() -> Result<()> {
    let url = "https://app.ato.run";
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()?
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).status()?
    } else {
        Command::new("xdg-open").arg(url).status()?
    };
    if !status.success() {
        bail!("failed to open {url}");
    }
    Ok(())
}

fn ato_command() -> Result<Command> {
    if let Some(path) = std::env::var_os("ATO_BIN") {
        return Ok(Command::new(path));
    }
    let executable = std::env::current_exe()?;
    let sibling = executable.with_file_name(if cfg!(windows) { "ato.exe" } else { "ato" });
    if sibling.is_file() {
        return Ok(Command::new(sibling));
    }
    let path = find_on_path(if cfg!(windows) { "ato.exe" } else { "ato" })
        .context("ato binary is unavailable; set ATO_BIN")?;
    Ok(Command::new(path))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn result(output: Output) -> ComputationCommandResult {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    ComputationCommandResult {
        success: output.status.success(),
        output: text,
    }
}
