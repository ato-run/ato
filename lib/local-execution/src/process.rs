use std::process::Command;

use anyhow::{Context, Result, bail};
use ato_adapter_process::terminate_process_tree;
use serde::{Deserialize, Serialize};

/// Host-observed identity for one owned worker process.
///
/// PID alone is deliberately insufficient: it may be reused after a crash or
/// reboot. Callers must compare the boot session and process start time before
/// signalling the process group recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedProcessIdentity {
    pub pid: u32,
    pub process_start_time: String,
    pub process_group: u32,
    pub boot_session: String,
}

impl OwnedProcessIdentity {
    pub fn current() -> Result<Self> {
        let pid = std::process::id();
        Ok(Self {
            pid,
            process_start_time: process_start_time(pid)
                .context("worker process start time is unavailable")?,
            process_group: current_process_group()?,
            boot_session: boot_session_identity()?,
        })
    }

    pub fn matches_live_process(&self) -> Result<bool> {
        Ok(boot_session_identity()? == self.boot_session
            && process_start_time(self.pid).as_deref() == Some(self.process_start_time.as_str()))
    }
}

/// Configure a worker to outlive the CLI invocation that started it.
#[cfg(unix)]
pub fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Configure a worker to outlive the CLI invocation that started it.
#[cfg(windows)]
pub fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// Terminate only the still-matching process tree represented by `identity`.
pub fn terminate_owned_process(identity: &OwnedProcessIdentity) -> Result<()> {
    if !identity.matches_live_process()? {
        bail!(
            "owned process identity no longer matches; refusing to stop PID {}",
            identity.pid
        );
    }
    if let Err(error) = terminate_process_tree(identity.pid, identity.process_group) {
        // The worker may finish voluntarily between the identity check and
        // the signal. That is the requested cleanup outcome; only propagate
        // the signal failure while the same owned process is still alive.
        if !identity.matches_live_process()? {
            return Ok(());
        }
        return Err(error.into());
    }
    for _ in 0..100 {
        if process_start_time(identity.pid).is_none() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19).map(|value| (*value).to_owned())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_start_time(pid: u32) -> Option<String> {
    command_output("ps", &["-o", "lstart=", "-p", &pid.to_string()])
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToUniversalTime().Ticks"),
        ],
    )
}

#[cfg(target_os = "linux")]
fn boot_session_identity() -> Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

#[cfg(target_os = "macos")]
fn boot_session_identity() -> Result<String> {
    command_output("sysctl", &["-n", "kern.boottime"])
        .context("kernel boot identity is unavailable")
}

#[cfg(windows)]
fn boot_session_identity() -> Result<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().Ticks",
        ],
    )
    .context("Windows boot identity is unavailable")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn boot_session_identity() -> Result<String> {
    bail!("boot/session identity is unavailable on this platform")
}

#[cfg(unix)]
fn current_process_group() -> Result<u32> {
    command_output(
        "ps",
        &["-o", "pgid=", "-p", &std::process::id().to_string()],
    )
    .and_then(|value| value.parse().ok())
    .context("current process group is unavailable")
}

#[cfg(windows)]
fn current_process_group() -> Result<u32> {
    Ok(std::process::id())
}

#[cfg(any(unix, windows))]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_identity_matches_only_the_live_process() {
        let identity = OwnedProcessIdentity::current().unwrap();
        assert_eq!(identity.pid, std::process::id());
        assert!(identity.matches_live_process().unwrap());

        let mut stale = identity;
        stale.process_start_time.push_str("-stale");
        assert!(!stale.matches_live_process().unwrap());
    }
}
