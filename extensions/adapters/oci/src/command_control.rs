//! Bounded owner-side OCI commands. Workload cleanup gets its own budget.
use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CommandControl {
    deadline_ms: u64,
    started: Instant,
    initial_remaining: Duration,
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
impl CommandControl {
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            started: Instant::now(),
            initial_remaining: Duration::from_millis(deadline_ms.saturating_sub(now_ms())),
        }
    }
    pub fn cleanup(budget: Duration) -> Self {
        Self::new(now_ms().saturating_add(budget.as_millis().min(u64::MAX as u128) as u64))
    }
    pub fn cap(mut self, remaining: Duration) -> Self {
        self.initial_remaining = self.initial_remaining.min(remaining);
        self
    }
    pub fn remaining(&self) -> io::Result<Duration> {
        let remaining = Duration::from_millis(self.deadline_ms.saturating_sub(now_ms())).min(
            self.initial_remaining
                .saturating_sub(self.started.elapsed()),
        );
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "OCI command deadline exceeded",
            ));
        }
        Ok(remaining)
    }
    pub fn output(&self, command: &mut Command) -> io::Result<Output> {
        self.output_with_limit(command, OUTPUT_LIMIT)
    }
    pub fn output_with_limit(&self, command: &mut Command, limit: u64) -> io::Result<Output> {
        self.remaining()?;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut owned = OwnedCommand {
            child: command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            finished: false,
        };
        let (tx, rx) = mpsc::channel();
        let output_bytes = Arc::new(AtomicU64::new(0));
        for (index, stream) in [
            Box::new(owned.child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(owned.child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ]
        .into_iter()
        .enumerate()
        {
            let tx = tx.clone();
            let output_bytes = output_bytes.clone();
            std::thread::spawn(move || {
                let result = (|| {
                    let mut bytes = Vec::new();
                    let mut stream = stream;
                    let mut buffer = [0u8; 65536];
                    loop {
                        let n = stream.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        if output_bytes
                            .fetch_add(n as u64, Ordering::Relaxed)
                            .saturating_add(n as u64)
                            > limit
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "OCI command output limit",
                            ));
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Ok(bytes)
                })();
                let _ = tx.send((index, result));
            });
        }
        drop(tx);
        let mut streams = [None, None];
        let mut status = None;
        loop {
            while let Ok((index, result)) = rx.try_recv() {
                streams[index] = Some(result?);
            }
            if status.is_none() {
                status = owned.child.try_wait()?;
            }
            if let (Some(status), Some(stdout), Some(stderr)) =
                (status, streams[0].as_ref(), streams[1].as_ref())
            {
                owned.finished = true;
                return Ok(Output {
                    status,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                });
            }
            std::thread::sleep(self.remaining()?.min(Duration::from_millis(10)));
        }
    }
}
struct OwnedCommand {
    child: Child,
    finished: bool,
}
impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        #[cfg(unix)]
        {
            if let Some(group) = rustix::process::Pid::from_raw(self.child.id() as i32) {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
        }
        let _ = self.child.kill();
        let cleanup = Instant::now() + Duration::from_secs(5);
        while Instant::now() < cleanup {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_command_never_spawns() {
        let mut command = Command::new("a-command-that-does-not-exist");
        assert_eq!(
            CommandControl::new(0)
                .output(&mut command)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_output_cannot_extend_the_frozen_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 10 & wait"]);
        let started = Instant::now();
        assert_eq!(
            CommandControl::cleanup(Duration::from_millis(100))
                .output(&mut command)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[cfg(all(test, unix))]
mod output_tests {
    use super::*;
    #[test]
    fn stdout_and_stderr_share_one_output_budget() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 1234; printf 5678 >&2"]);
        assert_eq!(
            CommandControl::cleanup(Duration::from_secs(2))
                .output_with_limit(&mut command, 6)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
