//! The host pseudo-terminal primitive.
//!
//! A Terminal surface needs a real PTY: a workload that asks whether its input
//! is a terminal behaves differently when it is not, and the declared terminal
//! exchanges are defined in terms of that behavior. This crate is the smallest
//! place that can own one: it allocates the PTY, starts the already-prepared
//! command as a session leader with the slave as its controlling terminal,
//! reads bounded output and writes input.
//!
//! It is deliberately not a general shell runner: it starts exactly one
//! command with an explicitly supplied environment, and it decides nothing
//! about Contract K. The Adapter that uses it records what each exchange
//! measured.

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};

/// Environment names a workload may inherit from the caller: the same minimal
/// base the process Adapter gives one, and nothing else implicitly.
const BASE_ENVIRONMENT: [&str; 3] = ["PATH", "SYSTEMROOT", "WINDIR"];

/// A running workload attached to a pseudo terminal this process owns.
pub struct PtySession {
    master: std::fs::File,
    child: std::process::Child,
    stopped: bool,
}

impl PtySession {
    /// Start `command` on a new PTY of the declared geometry.
    ///
    /// `environment` is the complete environment of the child apart from the
    /// minimal base above; nothing else is inherited. The command must not be
    /// empty. The child becomes a session leader and acquires the slave as its
    /// controlling terminal, so a terminal-aware program sees a terminal and
    /// the whole session can be stopped through [`PtySession::stop`].
    pub fn spawn(
        command: &[String],
        environment: &BTreeMap<String, String>,
        cwd: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Self, String> {
        let Some((program, arguments)) = command.split_first() else {
            return Err("pty command is empty".to_owned());
        };
        // SAFETY: openpty either fails or writes both descriptors; they are
        // owned here from the moment they are non-negative..
        let (master, slave) = unsafe {
            let mut master: libc::c_int = -1;
            let mut slave: libc::c_int = -1;
            let mut size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            ) != 0
            {
                return Err(format!(
                    "pty could not be allocated: {}",
                    std::io::Error::last_os_error()
                ));
            }
            (master, slave)
        };
        // SAFETY: both descriptors came from openpty and are owned from here.
        let master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let duplicated = || {
            slave
                .try_clone()
                .map(std::process::Stdio::from)
                .map_err(|error| format!("pty could not be shared with the command: {error}"))
        };
        let mut probe = std::process::Command::new(program);
        probe.args(arguments).current_dir(cwd).env_clear();
        for name in BASE_ENVIRONMENT {
            if let Ok(value) = std::env::var(name) {
                probe.env(name, value);
            }
        }
        probe
            .envs(environment)
            .stdin(duplicated()?)
            .stdout(duplicated()?)
            .stderr(duplicated()?);
        // SAFETY: between fork and exec the closure calls two async-signal-safe
        // functions and reports failure through the child's exit path.
        unsafe {
            use std::os::unix::process::CommandExt as _;
            probe.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = probe
            .spawn()
            .map_err(|error| format!("pty could not start its command: {error}"))?;
        drop(slave);
        Ok(Self {
            master,
            child,
            stopped: false,
        })
    }

    /// The workload's pid, which is also its session and process-group id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Write `bytes` to the terminal.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.master
            .write_all(bytes)
            .and_then(|()| self.master.flush())
            .map_err(|error| format!("pty input could not be written: {error}"))
    }

    /// Read until `done` accepts the capture, `bound` bytes have been kept, the
    /// deadline passes, or the workload closes the terminal.
    ///
    /// Returns `Ok(true)` when `done` accepted the capture. The capture is
    /// appended to and never replaced, so startup output stays in the evidence.
    pub fn read_until(
        &mut self,
        captured: &mut Vec<u8>,
        bound: usize,
        deadline: Instant,
        done: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        let mut buffer = [0_u8; 4096];
        loop {
            if done(captured) {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            if !self.readable(remaining)? {
                if let Some(status) = self
                    .child
                    .try_wait()
                    .map_err(|error| format!("pty could not be observed: {error}"))?
                {
                    return Err(format!("the workload ended before completion ({status})"));
                }
                return Ok(false);
            }
            match self.master.read(&mut buffer) {
                Ok(0) => {
                    return Err("the workload closed its terminal before completion".to_owned());
                }
                Ok(read) => {
                    if captured.len() + read > bound {
                        return Err(format!(
                            "the workload produced more than the declared {bound} bytes"
                        ));
                    }
                    captured.extend_from_slice(&buffer[..read]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("pty output could not be read: {error}")),
            }
        }
    }

    /// Whether the master is readable within `budget`.
    fn readable(&self, budget: Duration) -> Result<bool, String> {
        let mut descriptor = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = budget.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one fully initialized pollfd is passed for one descriptor.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        match result {
            0 => Ok(false),
            value if value > 0 => Ok(descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0),
            _ => {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    Ok(false)
                } else {
                    Err(format!("pty could not be polled: {error}"))
                }
            }
        }
    }

    /// `Some(description)` once the workload has exited on its own.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("the workload exited ({status})")),
            Ok(None) => None,
            Err(error) => Some(format!("the workload state could not be read: {error}")),
        }
    }

    /// Stop the workload's whole session.
    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        // The workload is a session leader, so its pid is its process group: a
        // helper it forked is stopped too, not only the leader.
        let pid = self.child.id();
        // SAFETY: kill(-pgid) on a group this process started and owns.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let alive = unsafe { libc::kill(-(pid as i32), 0) } == 0;
            if !alive || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: same owned group; SIGKILL is the last resort before reaping.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        self.child
            .wait()
            .map(|_| ())
            .map_err(|error| format!("pty stop could not be confirmed: {error}"))
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // A session nobody stopped explicitly is still this process's: it is
        // terminated here so nothing outlives the handle.
        if let Err(error) = self.stop() {
            eprintln!("[ato pty] {error}");
        }
    }
}
