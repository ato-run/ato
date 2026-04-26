use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use capsule_core::types::ExternalCapsuleDependency;

use super::ExternalCapsuleOptions;

pub(super) struct ExternalCapsuleChild {
    pub child: Child,
    stdout_thread: Option<JoinHandle<std::io::Result<()>>>,
    stderr_thread: Option<JoinHandle<std::io::Result<()>>>,
}

impl ExternalCapsuleChild {
    pub(super) fn shutdown(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn spawn_external_capsule_child(
    dependency: &ExternalCapsuleDependency,
    manifest_path: &Path,
    inject_args: &[String],
    options: &ExternalCapsuleOptions,
) -> Result<ExternalCapsuleChild> {
    let executable = std::env::current_exe().context("failed to resolve current ato executable")?;
    let mut command = Command::new(executable);
    command
        .arg("run")
        .arg(manifest_path)
        .arg("--enforcement")
        .arg(&options.enforcement)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if options.sandbox_mode {
        command.arg("--sandbox");
    }
    if options.dangerously_skip_permissions {
        command.arg("--dangerously-skip-permissions");
    }
    if options.assume_yes {
        command.arg("--yes");
    }
    for binding in inject_args {
        command.arg("--inject").arg(binding);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start external capsule dependency '{}'",
            dependency.alias
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    Ok(ExternalCapsuleChild {
        child,
        stdout_thread: Some(spawn_prefixed_stream(stdout, &dependency.alias, false)),
        stderr_thread: Some(spawn_prefixed_stream(stderr, &dependency.alias, true)),
    })
}

fn spawn_prefixed_stream(
    stream: Option<impl std::io::Read + Send + 'static>,
    alias: &str,
    is_stderr: bool,
) -> JoinHandle<std::io::Result<()>> {
    let prefix = format!("[ext:{}] ", alias);
    thread::spawn(move || {
        let Some(stream) = stream else {
            return Ok(());
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            if is_stderr {
                let mut stderr = std::io::stderr();
                stderr.write_all(prefix.as_bytes())?;
                stderr.write_all(line.as_bytes())?;
                stderr.flush()?;
            } else {
                let mut stdout = std::io::stdout();
                stdout.write_all(prefix.as_bytes())?;
                stdout.write_all(line.as_bytes())?;
                stdout.flush()?;
            }
        }
        Ok(())
    })
}
