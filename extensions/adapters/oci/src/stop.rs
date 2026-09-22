//! Stopping a container, and telling apart how it stopped.
//!
//! `docker rm --force` is SIGKILL: it never gives a workload the chance to
//! flush. A stop here is the image's stop signal, a bounded grace period, and
//! only then SIGKILL. What happened is recorded, because "the container is
//! gone" and "the application shut down cleanly" are different facts, and a
//! stop the Runner could not confirm must never be reported as one.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Upper bound for a single Docker CLI call. A daemon that does not answer
/// within this is treated as unable to confirm anything.
pub(crate) const DOCKER_CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for a stop, in two phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopBudget {
    /// After the stop signal, before SIGKILL.
    pub graceful: Duration,
    /// After SIGKILL, before giving up on confirmation.
    pub force: Duration,
}

impl StopBudget {
    pub const DEFAULT: StopBudget = StopBudget {
        graceful: Duration::from_secs(10),
        force: Duration::from_secs(5),
    };
}

/// How a container stopped — or that it could not be confirmed to have.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopOutcome {
    /// It had already exited before the stop began.
    AlreadyExited { exit_code: i32 },
    /// It exited within the grace period after its stop signal.
    Graceful { exit_code: i32 },
    /// It had to be killed.
    Forced { exit_code: i32 },
    /// It could not be confirmed stopped: still running, or Docker did not
    /// answer. Nothing may treat its writes as finished.
    Unconfirmed { reason: String },
}

impl StopOutcome {
    /// True when the container is known not to be running.
    pub fn is_confirmed(&self) -> bool {
        !matches!(self, Self::Unconfirmed { .. })
    }

    fn severity(&self) -> u8 {
        match self {
            Self::AlreadyExited { .. } | Self::Graceful { .. } => 0,
            Self::Forced { .. } => 1,
            Self::Unconfirmed { .. } => 2,
        }
    }

    /// The least favourable of several outcomes, e.g. across a group.
    pub fn worst<'a>(outcomes: impl IntoIterator<Item = &'a StopOutcome>) -> Option<StopOutcome> {
        outcomes
            .into_iter()
            .max_by_key(|outcome| outcome.severity())
            .cloned()
    }
}

/// Run a Docker CLI call, killing it if the daemon does not answer in time.
/// Output is drained concurrently so a large listing cannot fill the pipe and
/// stall the CLI until the timeout.
pub(crate) fn docker_output<'a>(
    docker: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
    timeout: Duration,
) -> Result<Output> {
    use std::io::Read;

    let mut child = Command::new(docker)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start Docker CLI")?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("wait for Docker CLI")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Docker CLI did not answer within {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// `(running, exit_code)` of a container.
pub(crate) fn container_state(docker: &Path, container: &str) -> Result<(bool, i32)> {
    let output = docker_output(
        docker,
        [
            "inspect",
            "--format",
            "{{.State.Running}}|{{.State.ExitCode}}",
            container,
        ],
        DOCKER_CALL_TIMEOUT,
    )?;
    if !output.status.success() {
        bail!(
            "inspect OCI container state failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value = String::from_utf8(output.stdout).context("invalid container state output")?;
    let (running, code) = value
        .trim()
        .split_once('|')
        .context("Docker inspect omitted container state")?;
    Ok((
        running == "true",
        code.parse().context("invalid OCI exit code")?,
    ))
}

fn stop_signal(docker: &Path, container: &str) -> String {
    docker_output(
        docker,
        ["inspect", "--format", "{{.Config.StopSignal}}", container],
        DOCKER_CALL_TIMEOUT,
    )
    .ok()
    .filter(|output| output.status.success())
    .and_then(|output| String::from_utf8(output.stdout).ok())
    .map(|signal| signal.trim().to_owned())
    .filter(|signal| {
        !signal.is_empty()
            && signal
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'-')
    })
    .unwrap_or_else(|| "SIGTERM".to_owned())
}

fn wait_until_stopped(docker: &Path, container: &str, budget: Duration) -> Result<Option<i32>> {
    let deadline = Instant::now() + budget;
    loop {
        let (running, code) = container_state(docker, container)?;
        if !running {
            return Ok(Some(code));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stop signal → grace → SIGKILL → confirm. Never removes the container:
/// removal is only ever done after a confirmed stop, by the caller.
pub(crate) fn stop_container(docker: &Path, container: &str, budget: StopBudget) -> StopOutcome {
    let unconfirmed = |reason: String| StopOutcome::Unconfirmed { reason };
    match container_state(docker, container) {
        Ok((false, exit_code)) => return StopOutcome::AlreadyExited { exit_code },
        Ok((true, _)) => {}
        Err(error) => return unconfirmed(format!("{error:#}")),
    }
    let signal = stop_signal(docker, container);
    // A failed signal is not fatal on its own: the container may have exited
    // in between, which the wait below observes.
    let _ = docker_output(
        docker,
        ["kill", "--signal", signal.as_str(), container],
        DOCKER_CALL_TIMEOUT,
    );
    match wait_until_stopped(docker, container, budget.graceful) {
        Ok(Some(exit_code)) => return StopOutcome::Graceful { exit_code },
        Ok(None) => {}
        Err(error) => return unconfirmed(format!("{error:#}")),
    }
    let _ = docker_output(
        docker,
        ["kill", "--signal", "SIGKILL", container],
        DOCKER_CALL_TIMEOUT,
    );
    match wait_until_stopped(docker, container, budget.force) {
        Ok(Some(exit_code)) => StopOutcome::Forced { exit_code },
        Ok(None) => unconfirmed("container still running after SIGKILL".to_owned()),
        Err(error) => unconfirmed(format!("{error:#}")),
    }
}

/// Remove a container already confirmed stopped. `--force` is deliberately
/// absent: removal must never be what stops it.
pub(crate) fn remove_stopped_container(docker: &Path, container: &str) -> Result<()> {
    let output = docker_output(docker, ["rm", container], DOCKER_CALL_TIMEOUT)?;
    if !output.status.success() {
        bail!(
            "remove stopped OCI container failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_outcome_of_a_group_wins() {
        let outcomes = [
            StopOutcome::Graceful { exit_code: 0 },
            StopOutcome::Forced { exit_code: 137 },
            StopOutcome::AlreadyExited { exit_code: 1 },
        ];
        assert_eq!(
            StopOutcome::worst(&outcomes),
            Some(StopOutcome::Forced { exit_code: 137 })
        );
        let with_unknown = [
            StopOutcome::Graceful { exit_code: 0 },
            StopOutcome::Unconfirmed {
                reason: "daemon".to_owned(),
            },
        ];
        assert!(!StopOutcome::worst(&with_unknown).unwrap().is_confirmed());
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_never_answers_is_not_a_confirmed_stop() {
        // A stand-in Docker CLI that hangs: the stop must come back
        // unconfirmed rather than block or report success.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(&fake, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = docker_output(&fake, ["inspect"], Duration::from_millis(200));
        assert!(output.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_exited_container_is_reported_as_already_exited() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(&fake, "#!/bin/sh\necho 'false|3'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            stop_container(&fake, "c1", StopBudget::DEFAULT),
            StopOutcome::AlreadyExited { exit_code: 3 }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_container_that_ignores_every_signal_is_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        // Always running; `kill` succeeds but changes nothing.
        std::fs::write(
            &fake,
            "#!/bin/sh\ncase \"$1\" in inspect) echo 'true|0';; *) exit 0;; esac\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = stop_container(
            &fake,
            "c1",
            StopBudget {
                graceful: Duration::from_millis(200),
                force: Duration::from_millis(200),
            },
        );
        assert!(!outcome.is_confirmed(), "{outcome:?}");
    }
}
