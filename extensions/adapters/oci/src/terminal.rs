//! Trusted host attach client for the container's main PTY. Its pipes never
//! contain daemon credentials; resize is fixed to the owned container id.
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, ensure};

use crate::{DockerClient, command_control::CommandControl};

pub struct OciTerminal {
    child: Child,
    docker: DockerClient,
    container_id: String,
}

impl OciTerminal {
    pub(crate) fn start(docker: DockerClient, container_id: &str) -> Result<Self> {
        ensure!(
            container_id.len() == 64
                && container_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "invalid owned container identity"
        );
        let child = docker
            .command()
            .args([
                "start",
                "--attach",
                "--interactive",
                "--detach-keys=ctrl-@,ctrl-@,ctrl-@,ctrl-@",
                container_id,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start owned OCI PTY attachment")?;
        Ok(Self {
            child,
            docker,
            container_id: container_id.to_owned(),
        })
    }

    pub fn take_io(&mut self) -> Result<(ChildStdin, ChildStdout, ChildStderr)> {
        Ok((
            self.child
                .stdin
                .take()
                .context("terminal input already taken")?,
            self.child
                .stdout
                .take()
                .context("terminal output already taken")?,
            self.child
                .stderr
                .take()
                .context("terminal error output already taken")?,
        ))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        ensure!(
            (2..=500).contains(&cols) && (2..=200).contains(&rows),
            "invalid terminal size"
        );
        // Reuse Docker's owner-configured transport. Only this fixed method,
        // owned hex id and bounded numeric query reach the daemon, never an
        // app URL or arbitrary HTTP request. No socket enters the workload.
        let mut command = self.docker.command().command;
        command.args(["system", "dial-stdio"]);
        let request = format!(
            "POST /containers/{}/resize?h={rows}&w={cols} HTTP/1.1\r\nHost: docker\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            self.container_id
        );
        let output = CommandControl::cleanup(Duration::from_secs(2)).output_with_input(
            &mut command,
            request.as_bytes(),
            8192,
        )?;
        ensure!(
            output.status.success()
                && (output.stdout.starts_with(b"HTTP/1.1 200 ")
                    || output.stdout.starts_with(b"HTTP/1.0 200 ")),
            "owned OCI terminal resize was refused"
        );
        Ok(())
    }

    pub fn exited(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child
                .kill()
                .context("stop owned terminal attach client")?;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if self
                .child
                .try_wait()
                .context("reap owned terminal attach client")?
                .is_some()
            {
                return Ok(());
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "terminal attach cleanup was not confirmed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OciTerminal {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
