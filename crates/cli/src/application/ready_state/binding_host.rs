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
}
