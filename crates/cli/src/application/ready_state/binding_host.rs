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
#[allow(dead_code)] // wired into the run gate in PR 6
pub(crate) trait AgentChannel {
    fn request(&mut self, msg: HostToAgent) -> Result<AgentToHost>;
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
}
