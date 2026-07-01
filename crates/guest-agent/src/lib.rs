//! Ready-State **guest-agent** — Phase 8a binding-lease control plane (#863; contract:
//! `docs/ready-state/binding-lease.md`).
//!
//! PR 2 (skeleton): the pure [`BindingSession`] state machine that consumes the
//! host↔agent control messages ([`protocol::binding_control`]) and computes the
//! **bound-ready** gate. It does **no real secret delivery**: it tracks binding
//! *presence* (name → id + expiry) so the bound-ready gate can be computed, but it
//! **never stores or writes the secret value** — that arrives in PR 3 (tmpfs delivery).
//! No vsock transport and no `/run/ato/bindings` I/O here; this is the logic core the
//! guest binary will drive.

use std::collections::HashMap;

use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::{BindingLeaseId, BindingName};

/// A binding the session is currently tracking as *present*. Skeleton: **metadata only
/// — no secret value is retained** (the `Deliver` message's value is intentionally
/// dropped until PR 3 writes it to tmpfs).
#[derive(Debug, Clone)]
struct Present {
    id: BindingLeaseId,
    expires_at_ms: u64,
}

/// The guest-agent session: the set of bindings this session *requires*, and which are
/// currently present. Drives the bound-ready gate.
#[derive(Debug)]
pub struct BindingSession {
    required: Vec<BindingName>,
    present: HashMap<BindingName, Present>,
}

impl BindingSession {
    /// A session that requires `required` bindings before it is bound-ready. A
    /// no-binding capsule (`required` empty) is trivially bound-ready.
    pub fn new(required: Vec<BindingName>) -> Self {
        BindingSession { required, present: HashMap::new() }
    }

    /// Handle one host control message as of `now_ms`, returning the agent's response.
    /// The skeleton performs **no** tmpfs I/O and never retains the secret value.
    pub fn handle(&mut self, msg: HostToAgent, now_ms: u64) -> AgentToHost {
        match msg {
            HostToAgent::Deliver(d) => {
                // Skeleton: record presence for the bound-ready gate; DROP the value
                // (no real delivery yet). Renew = re-Deliver with a later expiry.
                let name = d.name.clone();
                self.present.insert(name.clone(), Present { id: d.id.clone(), expires_at_ms: d.expires_at_ms });
                AgentToHost::Ack { id: d.id, name }
            }
            HostToAgent::Revoke { id } => {
                self.remove_by_id(&id);
                AgentToHost::Scrubbed { id }
            }
            HostToAgent::QueryBoundReady => self.bound_ready_response(now_ms),
            HostToAgent::Stop => {
                // Stop-scrub: drop all bindings; the session is no longer ready.
                self.present.clear();
                self.bound_ready_response(now_ms)
            }
        }
    }

    /// Whether every required binding is present and unexpired as of `now_ms`.
    pub fn bound_ready(&self, now_ms: u64) -> bool {
        self.pending(now_ms).is_empty()
    }

    /// Required bindings that are not currently present (missing or expired).
    pub fn pending(&self, now_ms: u64) -> Vec<BindingName> {
        self.required.iter().filter(|n| !self.is_present(n, now_ms)).cloned().collect()
    }

    fn is_present(&self, name: &BindingName, now_ms: u64) -> bool {
        self.present.get(name).map(|p| now_ms < p.expires_at_ms).unwrap_or(false)
    }

    fn bound_ready_response(&self, now_ms: u64) -> AgentToHost {
        let pending = self.pending(now_ms);
        AgentToHost::BoundReady { ready: pending.is_empty(), pending }
    }

    fn remove_by_id(&mut self, id: &BindingLeaseId) {
        self.present.retain(|_, p| &p.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

    fn deliver(name: &str, id: &str, issued: u64, ttl: u64) -> HostToAgent {
        let lease = BindingLease::issue(
            BindingLeaseId::new(id),
            BindingName::parse(name).unwrap(),
            SecretValue::new("secret-VALUE-must-never-be-stored-by-skeleton"),
            issued,
            ttl,
        );
        HostToAgent::Deliver(lease.to_delivery())
    }

    fn name(n: &str) -> BindingName {
        BindingName::parse(n).unwrap()
    }

    #[test]
    fn no_binding_session_is_trivially_ready() {
        let s = BindingSession::new(vec![]);
        assert!(s.bound_ready(0));
    }

    #[test]
    fn bound_ready_only_when_all_required_delivered() {
        let mut s = BindingSession::new(vec![name("db_url"), name("api_key")]);
        assert!(!s.bound_ready(1000));
        assert_eq!(s.pending(1000), vec![name("db_url"), name("api_key")]);

        let r = s.handle(deliver("db_url", "l1", 1000, 5000), 1000);
        assert_eq!(r, AgentToHost::Ack { id: BindingLeaseId::new("l1"), name: name("db_url") });
        assert!(!s.bound_ready(1000), "still missing api_key");

        s.handle(deliver("api_key", "l2", 1000, 5000), 1000);
        assert!(s.bound_ready(1000), "both delivered ⇒ ready");
        assert!(matches!(s.handle(HostToAgent::QueryBoundReady, 1000), AgentToHost::BoundReady { ready: true, .. }));
    }

    #[test]
    fn revoke_and_expiry_and_stop_drop_bound_ready() {
        let mut s = BindingSession::new(vec![name("db_url")]);
        s.handle(deliver("db_url", "l1", 1000, 5000), 1000); // expires 6000
        assert!(s.bound_ready(1000));

        // expiry.
        assert!(!s.bound_ready(6000), "expired ⇒ not ready");

        // re-deliver, then revoke.
        s.handle(deliver("db_url", "l2", 6000, 5000), 6000);
        assert!(s.bound_ready(6000));
        let r = s.handle(HostToAgent::Revoke { id: BindingLeaseId::new("l2") }, 6000);
        assert_eq!(r, AgentToHost::Scrubbed { id: BindingLeaseId::new("l2") });
        assert!(!s.bound_ready(6000), "revoked ⇒ not ready");

        // stop-scrub.
        s.handle(deliver("db_url", "l3", 6000, 5000), 6000);
        assert!(s.bound_ready(6000));
        s.handle(HostToAgent::Stop, 6000);
        assert!(!s.bound_ready(6000), "stop scrubs all ⇒ not ready");
    }
}
