//! PR 4 (#863): the host-side **bound-ready gate**.
//!
//! Before user traffic is exposed, the host delivers the session's binding leases to
//! the guest-agent over an [`AgentChannel`] and blocks until the agent reports
//! **bound-ready** — else it **fails closed** (never exposes an unbound session). The
//! transport is a trait so this is testable in-process now; PR 7 supplies a real vsock
//! channel. No-binding sessions have no leases, so the gate returns immediately.

use anyhow::{Result, bail};
use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::BindingLease;

/// The host's request/response channel to the guest-agent (vsock in production; an
/// in-process mock in tests).
#[allow(dead_code)] // wired into the run gate in PR C
pub(crate) trait AgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost>;
}

/// PR B (#912): the real host→guest-agent channel over Firecracker's vsock UDS.
///
/// Firecracker exposes a host Unix socket (`uds_path`); to reach a guest AF_VSOCK
/// listener on `guest_port` the host connects to that UDS and sends `CONNECT
/// <guest_port>\n`, expecting `OK <host_port>\n`, after which the stream carries the
/// newline-delimited JSON control protocol to/from the guest-agent. All reads/writes
/// are timeout-bounded; any protocol error fails closed.
#[allow(dead_code)] // wired into the run gate in PR C
pub(crate) struct FirecrackerAgentChannel {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

#[allow(dead_code)]
impl FirecrackerAgentChannel {
    /// Connect through the FC vsock UDS to the guest-agent on `guest_port`.
    pub(crate) fn connect(
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
        write!(stream, "CONNECT {guest_port}\n")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("OK") {
            bail!("guest-agent vsock CONNECT rejected on port {guest_port}: {line:?}");
        }
        Ok(FirecrackerAgentChannel { reader, writer: stream })
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

/// Deliver every lease, then poll bound-ready up to `max_polls` times. Returns `Ok`
/// only when the agent reports bound-ready; otherwise **fails closed** with the still
/// pending binding names. The caller must not expose the port unless this returns `Ok`.
#[allow(dead_code)] // wired into the run gate in PR 6
pub(crate) fn establish_bindings(
    channel: &mut dyn AgentChannel,
    leases: &[BindingLease],
    max_polls: u32,
) -> Result<()> {
    for lease in leases {
        match channel.request(HostToAgent::Deliver(lease.to_delivery()))? {
            AgentToHost::Ack { .. } => {}
            AgentToHost::Error { message } => bail!("binding delivery failed: {message}"),
            other => bail!("unexpected agent response to Deliver: {other:?}"),
        }
    }
    let mut last_pending: Vec<String> = Vec::new();
    for _ in 0..max_polls.max(1) {
        match channel.request(HostToAgent::QueryBoundReady)? {
            AgentToHost::BoundReady { ready: true, .. } => return Ok(()),
            AgentToHost::BoundReady { ready: false, pending } => {
                last_pending = pending.iter().map(|n| n.as_str().to_string()).collect();
            }
            other => bail!("unexpected agent response to QueryBoundReady: {other:?}"),
        }
    }
    // Fail closed — an unbound session must never serve.
    bail!("bound-ready gate not satisfied; pending bindings: {last_pending:?}");
}

/// PR 6: the **"post-bind state is dirty"** hard invariant (contract §"Hard
/// invariants"). Once ANY binding lease has been attached, the session/VM is **dirty**:
/// its memory and tmpfs may carry the secret, so **no** post-bind snapshot / checkpoint
/// / re-seal is ever allowed — a Ready-State seal must always come from a *pre-bind*
/// boot. This guard fails closed on any such attempt. Bindings live only in the
/// session; the on-disk artifact stays pre-bind + secret-free (#831/#834).
pub(crate) fn ensure_pre_bind_before_seal(session_is_bound: bool) -> Result<()> {
    if session_is_bound {
        bail!(
            "refusing to seal/snapshot a BOUND session: post-bind state is dirty (a lease may \
             live in VM memory or tmpfs). A Ready-State seal must come from a pre-bind boot — \
             never re-seal after binding."
        );
    }
    Ok(())
}

/// PR 5: revoke a single lease by id — the agent scrubs that binding's tmpfs file
/// immediately (e.g. a TTL that was not renewed). Returns the agent's `Scrubbed` ack.
#[allow(dead_code)] // wired into lease renewal in a later PR
pub(crate) fn revoke_binding(
    channel: &mut dyn AgentChannel,
    id: protocol::binding_lease::BindingLeaseId,
) -> Result<()> {
    match channel.request(HostToAgent::Revoke { id })? {
        AgentToHost::Scrubbed { .. } => Ok(()),
        AgentToHost::Error { message } => bail!("revoke failed: {message}"),
        other => bail!("unexpected agent response to Revoke: {other:?}"),
    }
}

/// PR 5: stop-scrub — on `ato stop`, ask the agent to revoke + scrub **all** bindings
/// (tmpfs wipe) BEFORE the host tears the VM/tap/overlay down. **Never re-seals.** This
/// is best-effort: the VM teardown destroys the tmpfs regardless, so a channel error is
/// logged, not fatal.
#[allow(dead_code)] // wired into ato stop in PR 6
pub(crate) fn stop_scrub(channel: &mut dyn AgentChannel) -> Result<()> {
    match channel.request(HostToAgent::Stop) {
        // The agent scrubs all and reports the (now-unbound) state back.
        Ok(AgentToHost::BoundReady { .. }) | Ok(AgentToHost::Scrubbed { .. }) => Ok(()),
        Ok(AgentToHost::Error { message }) => bail!("stop-scrub reported error: {message}"),
        Ok(other) => bail!("unexpected agent response to Stop: {other:?}"),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guest_agent::{BindingSession, TmpfsBindingSink};
    use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

    /// In-process channel: drives a real guest-agent session + tmpfs sink.
    struct MockChannel {
        session: BindingSession<TmpfsBindingSink>,
        now_ms: u64,
    }
    impl AgentChannel for MockChannel {
        fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost> {
            Ok(self.session.handle(msg, self.now_ms))
        }
    }

    fn lease(name: &str, id: &str) -> BindingLease {
        BindingLease::issue(
            BindingLeaseId::new(id),
            BindingName::parse(name).unwrap(),
            SecretValue::new(format!("secret-for-{name}")),
            1_000,
            60_000,
        )
    }

    #[test]
    fn gate_opens_when_all_required_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        let leases = vec![lease("db_url", "l1"), lease("api_key", "l2")];
        establish_bindings(&mut ch, &leases, 3).expect("gate should open");
        // both secrets are on tmpfs.
        assert_eq!(std::fs::read_to_string(dir.path().join("db_url")).unwrap(), "secret-for-db_url");
        assert!(dir.path().join("api_key").exists());
    }

    #[test]
    fn gate_fails_closed_when_a_required_binding_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        // only deliver one of the two required.
        let leases = vec![lease("db_url", "l1")];
        let err = establish_bindings(&mut ch, &leases, 3).unwrap_err().to_string();
        assert!(err.contains("api_key"), "should report the missing binding: {err}");
        assert!(err.contains("pending"), "should fail closed: {err}");
    }

    #[test]
    fn revoke_scrubs_the_binding_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(vec![BindingName::parse("db_url").unwrap()], TmpfsBindingSink::new(dir.path())),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("db_url", "l1")], 2).unwrap();
        assert!(dir.path().join("db_url").exists());
        revoke_binding(&mut ch, BindingLeaseId::new("l1")).unwrap();
        assert!(!dir.path().join("db_url").exists(), "revoke scrubbed the tmpfs file");
    }

    #[test]
    fn stop_scrub_wipes_all_bindings_before_teardown() {
        let dir = tempfile::tempdir().unwrap();
        let mut ch = MockChannel {
            session: BindingSession::new(
                vec![BindingName::parse("db_url").unwrap(), BindingName::parse("api_key").unwrap()],
                TmpfsBindingSink::new(dir.path()),
            ),
            now_ms: 1_000,
        };
        establish_bindings(&mut ch, &[lease("db_url", "l1"), lease("api_key", "l2")], 2).unwrap();
        assert!(dir.path().join("db_url").exists() && dir.path().join("api_key").exists());
        stop_scrub(&mut ch).unwrap();
        assert!(!dir.path().join("db_url").exists(), "stop-scrub wiped db_url");
        assert!(!dir.path().join("api_key").exists(), "stop-scrub wiped api_key");
    }

    /// PR 7: the **no-secret** end-to-end invariant proof (in-process). A full bound
    /// lifecycle must leave the host artifacts secret-free and the tmpfs scrubbed:
    /// the value reaches only the guest tmpfs (never a host receipt / lease dump / the
    /// non-serializable lease), a bound session cannot be re-sealed, and stop-scrub
    /// wipes tmpfs. (The live Firecracker+vsock hardware E2E is the remaining step.)
    #[test]
    fn no_secret_e2e_host_artifacts_stay_clean_and_tmpfs_is_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "TOP-SECRET-DB-PASSWORD-42";
        let l = BindingLease::issue(
            BindingLeaseId::new("l1"),
            BindingName::parse("db_url").unwrap(),
            SecretValue::new(secret),
            1_000,
            60_000,
        );
        let mut ch = MockChannel {
            session: BindingSession::new(vec![BindingName::parse("db_url").unwrap()], TmpfsBindingSink::new(dir.path())),
            now_ms: 1_000,
        };

        // 1. Bind → secret lands ONLY on the guest tmpfs.
        establish_bindings(&mut ch, std::slice::from_ref(&l), 2).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("db_url")).unwrap(), secret);

        // 2. No host-side artifact carries the secret: the loggable receipt is
        //    value-free, the lease Debug redacts, and the lease is not even Serialize.
        let receipt = l.receipt(1_000);
        assert!(!serde_json::to_string(&receipt).unwrap().contains(secret), "host receipt leaked the secret");
        assert!(!format!("{l:?}").contains(secret), "lease Debug leaked the secret");
        assert!(!format!("{receipt:?}").contains(secret));

        // 3. A bound session may never be re-sealed.
        assert!(ensure_pre_bind_before_seal(true).is_err());

        // 4. stop-scrub wipes tmpfs.
        stop_scrub(&mut ch).unwrap();
        assert!(!dir.path().join("db_url").exists(), "tmpfs scrubbed after stop");
    }

    #[test]
    fn sealing_a_bound_session_is_refused() {
        // Pre-bind boot ⇒ allowed (this is how every seal is produced).
        assert!(ensure_pre_bind_before_seal(false).is_ok());
        // Bound session ⇒ refused (post-bind state is dirty, never re-seal).
        let err = ensure_pre_bind_before_seal(true).unwrap_err().to_string();
        assert!(err.contains("post-bind state is dirty"), "{err}");
        assert!(err.to_lowercase().contains("never re-seal"), "{err}");
    }
}
