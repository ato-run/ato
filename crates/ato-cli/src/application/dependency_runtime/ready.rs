//! Ready probe runtime (RFC §7.6).
//!
//! v1 implements the `tcp` and `probe` variants. `http` and `unix_socket`
//! are reserved-only and rejected at lock time
//! (`capsule-core::foundation::dependency_contracts::verify_and_lock`),
//! so we do not need runtime support for them here.

use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReadyError {
    #[error("ready probe timed out after {elapsed:?}: {detail}")]
    Timeout { elapsed: Duration, detail: String },

    #[error("ready probe failed during execution: {detail}")]
    ExecutionFailed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// Ready check passed. Returns the elapsed duration since the probe
    /// loop started.
    Ready { elapsed: Duration, attempts: u32 },
}

#[derive(Debug, Clone)]
pub enum ReadyProbeKind {
    /// Open a short TCP connection to `host:port`. Success on connect.
    Tcp { host: String, port: u16 },
    /// Run a command. Success on exit code 0.
    Probe { argv: Vec<String> },
}

/// Wait for a ready probe to succeed or time out.
///
/// `interval` is the minimum spacing between successive attempts. If the
/// caller passes `Duration::ZERO`, the probe loop spins as fast as the
/// kernel allows; production callers should pass at least 100 ms.
pub fn wait_for_ready(
    kind: &ReadyProbeKind,
    timeout: Duration,
    interval: Duration,
) -> Result<ReadyOutcome, ReadyError> {
    let started = Instant::now();
    let mut attempts: u32 = 0;
    let mut last_failure = String::from("not attempted");
    loop {
        if started.elapsed() >= timeout {
            return Err(ReadyError::Timeout {
                elapsed: started.elapsed(),
                detail: format!("last failure: {last_failure}"),
            });
        }
        attempts += 1;
        let outcome = attempt(kind);
        match outcome {
            Ok(()) => {
                return Ok(ReadyOutcome::Ready {
                    elapsed: started.elapsed(),
                    attempts,
                });
            }
            Err(err) => {
                last_failure = err;
            }
        }
        if started.elapsed() >= timeout {
            return Err(ReadyError::Timeout {
                elapsed: started.elapsed(),
                detail: format!("last failure: {last_failure}"),
            });
        }
        if !interval.is_zero() {
            std::thread::sleep(interval);
        }
    }
}

fn attempt(kind: &ReadyProbeKind) -> Result<(), String> {
    match kind {
        ReadyProbeKind::Tcp { host, port } => {
            let addr = format!("{host}:{port}");
            // 1 second connect timeout per attempt; the outer loop owns
            // the overall timeout.
            match TcpStream::connect_timeout(
                &addr
                    .parse()
                    .map_err(|err: std::net::AddrParseError| err.to_string())?,
                Duration::from_secs(1),
            ) {
                Ok(stream) => {
                    drop(stream);
                    Ok(())
                }
                Err(err) => Err(format!("tcp connect to {addr}: {err}")),
            }
        }
        ReadyProbeKind::Probe { argv } => {
            if argv.is_empty() {
                return Err("probe argv is empty".to_string());
            }
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::piped());
            let output = cmd
                .output()
                .map_err(|err| format!("spawn {} failed: {err}", argv[0]))?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!(
                    "probe {} exited {}: {}",
                    argv[0],
                    output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    stderr.trim()
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn tcp_probe_succeeds_when_listener_is_up() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        // Hand the listener off to a thread that accepts forever — this
        // keeps the port live for the duration of the probe.
        let listener = Arc::new(listener);
        let l = listener.clone();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let kind = ReadyProbeKind::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        let outcome = wait_for_ready(&kind, Duration::from_secs(2), Duration::from_millis(20))
            .expect("ready");
        match outcome {
            ReadyOutcome::Ready { attempts, .. } => {
                assert!(attempts >= 1);
            }
        }
    }

    #[test]
    fn tcp_probe_times_out_when_nothing_listens() {
        // Pick a port that nothing is listening on. We bind & immediately
        // drop a listener to grab a port, then probe that port (which is
        // now closed in TIME_WAIT-ish state).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let kind = ReadyProbeKind::Tcp {
            host: "127.0.0.1".to_string(),
            port,
        };
        let err = wait_for_ready(&kind, Duration::from_millis(300), Duration::from_millis(50))
            .expect_err("must time out");
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn probe_command_succeeds_on_exit_zero() {
        let kind = ReadyProbeKind::Probe {
            argv: vec!["/usr/bin/true".to_string()],
        };
        wait_for_ready(&kind, Duration::from_secs(1), Duration::from_millis(20)).expect("ready");
    }

    #[test]
    fn probe_command_times_out_on_nonzero_exit() {
        let kind = ReadyProbeKind::Probe {
            argv: vec!["/usr/bin/false".to_string()],
        };
        let err = wait_for_ready(&kind, Duration::from_millis(300), Duration::from_millis(50))
            .expect_err("must time out");
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn probe_with_empty_argv_returns_failure_then_timeout() {
        let kind = ReadyProbeKind::Probe { argv: vec![] };
        let err = wait_for_ready(&kind, Duration::from_millis(100), Duration::from_millis(20))
            .expect_err("must time out");
        // The retry loop wraps the empty-argv error as a Timeout because
        // it never succeeds within the deadline.
        assert!(matches!(err, ReadyError::Timeout { .. }), "got {err:?}");
    }
}
