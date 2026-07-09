//! Host ↔ guest-agent control channel over the Firecracker vsock UDS.
//!
//! v1.2 PR 3d: moved here from the cli's `ready_state::binding_host` so that BOTH
//! sides of the lifecycle share one transport implementation: the
//! `FirecrackerBackend` build path (the supervisor build drive delivers a
//! placeholder binding, then `StopWorkload` + `Revoke` before the pre-bind
//! snapshot) and the cli run gate (bind-before-expose, renewal, stop-scrub).
//! The cli re-exports these types, so its call sites are unchanged.
//!
//! Firecracker exposes a host Unix socket (`uds_path`); to reach a guest AF_VSOCK
//! listener on `guest_port` the host connects to that UDS and sends `CONNECT
//! <guest_port>\n`, expecting `OK <host_port>\n`, after which the stream carries the
//! newline-delimited JSON control protocol to/from the guest-agent. All reads/writes
//! are timeout-bounded; any protocol error fails closed.

use anyhow::{Context, Result, bail};
use protocol::binding_control::{AgentToHost, HostToAgent};

/// The guest-agent's AF_VSOCK control port — single source for the port every
/// host-side connect (build drive, run gate, renewal, stop-scrub) dials.
pub const GUEST_AGENT_VSOCK_PORT: u32 = 1025;

/// The host's request/response channel to the guest-agent (vsock in production; an
/// in-process mock in tests).
pub trait AgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost>;
}

/// PR B (#912): the real host→guest-agent channel over Firecracker's vsock UDS.
pub struct FirecrackerAgentChannel {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

impl FirecrackerAgentChannel {
    /// Connect through the FC vsock UDS to the guest-agent on `guest_port`.
    pub fn connect(
        uds_path: &std::path::Path,
        guest_port: u32,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(uds_path)
            .with_context(|| format!("connect FC vsock UDS {}", uds_path.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        // Firecracker host→guest CONNECT handshake.
        writeln!(stream, "CONNECT {guest_port}")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("OK") {
            bail!("guest-agent vsock CONNECT rejected on port {guest_port}: {line:?}");
        }
        Ok(FirecrackerAgentChannel {
            reader,
            writer: stream,
        })
    }

    /// v1.2 PR 3d: `connect`, retried until `total` elapses. The build drive dials
    /// right after `InstanceStart`, when the guest is still booting — the UDS accepts
    /// but the guest side refuses `CONNECT` until the agent's vsock listener is up, so
    /// a bounded retry (not a single attempt) is the honest build-side connect.
    pub fn connect_with_retry(
        uds_path: &std::path::Path,
        guest_port: u32,
        total: std::time::Duration,
    ) -> Result<Self> {
        let start = std::time::Instant::now();
        let per_attempt = std::time::Duration::from_secs(2);
        loop {
            match Self::connect(uds_path, guest_port, per_attempt) {
                Ok(c) => return Ok(c),
                Err(e) if start.elapsed() >= total => {
                    return Err(e.context(format!(
                        "guest-agent vsock listener never accepted within {total:?}"
                    )));
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
    }
}

impl AgentChannel for FirecrackerAgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost> {
        use std::io::{BufRead, Write};
        let json = serde_json::to_string(&msg)?;
        writeln!(self.writer, "{json}")?;
        self.writer.flush()?;
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 || line.trim().is_empty() {
            bail!("guest-agent closed the vsock stream / empty response");
        }
        serde_json::from_str(&line).with_context(|| format!("parse agent response: {line:?}"))
    }
}
