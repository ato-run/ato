//! Ready-State **guest-agent** — Phase 8a binding-lease control plane (#863; contract:
//! `docs/ready-state/binding-lease.md`).
//!
//! The pure [`BindingSession`] state machine consumes the host↔agent control messages
//! ([`protocol::binding_control`]), computes the **bound-ready** gate, and — via a
//! [`BindingSink`] — materializes the secret **inside the guest**:
//!
//! - PR 2 (skeleton): control flow + bound-ready, value dropped.
//! - **PR 3 (this): real tmpfs delivery.** On `Deliver` the value is written to a
//!   `0600` file at `/run/ato/bindings/<name>` ([`TmpfsBindingSink`]); on
//!   `Revoke`/`Stop`/expiry the file is scrubbed. **The value is only ever on the
//!   sink (tmpfs) — never retained in the agent's memory** (the session drops its
//!   in-memory copy after handing it to the sink).
//!
//! No vsock transport here (the guest binary drives that); this is the logic + the
//! delivery sink.

use std::collections::HashMap;

use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::{BindingLeaseId, BindingName};

pub mod supervisor;
pub mod tmpfs;
pub mod volume_mount;
pub mod vsock;
pub use tmpfs::TmpfsBindingSink;

/// Where the guest-agent materializes a binding **inside the guest**. Implementations
/// must place the value on tmpfs (nothing on a persistent/overlay disk) and scrub it
/// on revoke/stop. The value is passed through by reference and never returned/logged.
pub trait BindingSink {
    /// Materialize `value` for `name` (atomic, mode `0600`). Renew = deliver again.
    fn deliver(&self, name: &BindingName, value: &str) -> std::io::Result<()>;
    /// Scrub the binding for `name` (wipe + remove). Idempotent — absent ⇒ Ok.
    fn scrub(&self, name: &BindingName) -> std::io::Result<()>;
}

/// A binding the session is tracking as *present*. Metadata only — **no secret value
/// is retained** (it lives only on the sink/tmpfs).
#[derive(Debug, Clone)]
struct Present {
    id: BindingLeaseId,
    expires_at_ms: u64,
}

/// The guest-agent session: the bindings this session *requires*, which are present,
/// and the [`BindingSink`] the secret is materialized through.
pub struct BindingSession<S: BindingSink> {
    required: Vec<BindingName>,
    present: HashMap<BindingName, Present>,
    sink: S,
}

impl<S: BindingSink> BindingSession<S> {
    /// A session that requires `required` bindings before it is bound-ready, delivering
    /// through `sink`. A no-binding capsule (`required` empty) is trivially bound-ready.
    pub fn new(required: Vec<BindingName>, sink: S) -> Self {
        BindingSession {
            required,
            present: HashMap::new(),
            sink,
        }
    }

    /// Handle one host control message as of `now_ms`. Delivery/scrub go through the
    /// sink; the secret value is never retained in memory. A sink error becomes an
    /// `AgentToHost::Error` (fail-closed: the binding is NOT marked present).
    pub fn handle(&mut self, msg: HostToAgent, now_ms: u64) -> AgentToHost {
        match msg {
            HostToAgent::Deliver(d) => {
                // Write the value to the sink (tmpfs), then drop it — the agent keeps
                // only presence metadata. On sink failure, fail closed: do not mark
                // present, do not ack.
                if let Err(e) = self.sink.deliver(&d.name, d.value.expose()) {
                    return AgentToHost::Error {
                        message: format!("deliver {}: {e}", d.name.as_str()),
                    };
                }
                self.present.insert(
                    d.name.clone(),
                    Present {
                        id: d.id.clone(),
                        expires_at_ms: d.expires_at_ms,
                    },
                );
                AgentToHost::Ack {
                    id: d.id,
                    name: d.name,
                }
            }
            HostToAgent::Revoke { id } => {
                self.scrub_by_id(&id);
                AgentToHost::Scrubbed { id }
            }
            HostToAgent::QueryBoundReady => {
                // Expired bindings are scrubbed lazily so tmpfs never outlives the TTL.
                self.scrub_expired(now_ms);
                self.bound_ready_response(now_ms)
            }
            HostToAgent::Stop => {
                self.scrub_all();
                self.bound_ready_response(now_ms)
            }
            // Supervisor control (v1.2): not a binding-session concern — the guest
            // binary routes it to the workload supervisor before reaching here. If it
            // ever arrives (a session with no supervisor), report the current
            // bound-ready state rather than error.
            HostToAgent::StopWorkload => self.bound_ready_response(now_ms),
            // v1.6 (ato#983) Slice 3 revision: durable-state mounting, same
            // treatment as StopWorkload above — the guest binary's dispatch()
            // intercepts and answers `MountVolumes` before it ever reaches
            // here (this session doesn't own the mounter). Defensive
            // fallback only.
            HostToAgent::MountVolumes => self.bound_ready_response(now_ms),
        }
    }

    /// Whether every required binding is present and unexpired as of `now_ms`.
    pub fn bound_ready(&self, now_ms: u64) -> bool {
        self.pending(now_ms).is_empty()
    }

    /// Required bindings not currently present (missing or expired).
    pub fn pending(&self, now_ms: u64) -> Vec<BindingName> {
        self.required
            .iter()
            .filter(|n| !self.is_present(n, now_ms))
            .cloned()
            .collect()
    }

    /// Scrub every binding whose TTL has elapsed as of `now_ms` (tmpfs wipe).
    pub fn scrub_expired(&mut self, now_ms: u64) {
        let expired: Vec<BindingName> = self
            .present
            .iter()
            .filter(|(_, p)| now_ms >= p.expires_at_ms)
            .map(|(n, _)| n.clone())
            .collect();
        for n in expired {
            let _ = self.sink.scrub(&n);
            self.present.remove(&n);
        }
    }

    /// Scrub all bindings (stop-scrub). Best-effort per binding, always clears state.
    pub fn scrub_all(&mut self) {
        for n in self.present.keys() {
            let _ = self.sink.scrub(n);
        }
        self.present.clear();
    }

    fn is_present(&self, name: &BindingName, now_ms: u64) -> bool {
        self.present
            .get(name)
            .map(|p| now_ms < p.expires_at_ms)
            .unwrap_or(false)
    }

    fn bound_ready_response(&self, now_ms: u64) -> AgentToHost {
        let pending = self.pending(now_ms);
        AgentToHost::BoundReady {
            ready: pending.is_empty(),
            pending,
        }
    }

    fn scrub_by_id(&mut self, id: &BindingLeaseId) {
        let names: Vec<BindingName> = self
            .present
            .iter()
            .filter(|(_, p)| &p.id == id)
            .map(|(n, _)| n.clone())
            .collect();
        for n in names {
            let _ = self.sink.scrub(&n);
            self.present.remove(&n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};
    use std::cell::RefCell;
    use std::collections::HashMap as Map;

    /// A recording sink for the pure-logic tests (no filesystem).
    #[derive(Default)]
    struct MemSink {
        live: RefCell<Map<String, String>>,
    }
    impl BindingSink for MemSink {
        fn deliver(&self, name: &BindingName, value: &str) -> std::io::Result<()> {
            self.live
                .borrow_mut()
                .insert(name.as_str().to_string(), value.to_string());
            Ok(())
        }
        fn scrub(&self, name: &BindingName) -> std::io::Result<()> {
            self.live.borrow_mut().remove(name.as_str());
            Ok(())
        }
    }

    fn deliver(name: &str, id: &str, issued: u64, ttl: u64, secret: &str) -> HostToAgent {
        let lease = BindingLease::issue(
            BindingLeaseId::new(id),
            BindingName::parse(name).unwrap(),
            SecretValue::new(secret),
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
        let s = BindingSession::new(vec![], MemSink::default());
        assert!(s.bound_ready(0));
    }

    #[test]
    fn bound_ready_only_when_all_required_delivered_and_value_hits_the_sink() {
        let mut s = BindingSession::new(vec![name("db_url"), name("api_key")], MemSink::default());
        assert!(!s.bound_ready(1000));

        let r = s.handle(deliver("db_url", "l1", 1000, 5000, "PGPASS"), 1000);
        assert_eq!(
            r,
            AgentToHost::Ack {
                id: BindingLeaseId::new("l1"),
                name: name("db_url")
            }
        );
        assert_eq!(
            s.sink.live.borrow().get("db_url").map(String::as_str),
            Some("PGPASS"),
            "value delivered to sink"
        );
        assert!(!s.bound_ready(1000), "still missing api_key");

        s.handle(deliver("api_key", "l2", 1000, 5000, "KEY"), 1000);
        assert!(s.bound_ready(1000));
    }

    #[test]
    fn revoke_expiry_stop_scrub_the_sink_and_drop_ready() {
        let mut s = BindingSession::new(vec![name("db_url")], MemSink::default());
        s.handle(deliver("db_url", "l1", 1000, 5000, "S1"), 1000); // expires 6000
        assert!(s.bound_ready(1000));

        // expiry scrubs the sink on the next query.
        s.handle(HostToAgent::QueryBoundReady, 6000);
        assert!(
            s.sink.live.borrow().get("db_url").is_none(),
            "expired binding scrubbed from sink"
        );
        assert!(!s.bound_ready(6000));

        // revoke scrubs.
        s.handle(deliver("db_url", "l2", 6000, 5000, "S2"), 6000);
        assert!(s.sink.live.borrow().contains_key("db_url"));
        s.handle(
            HostToAgent::Revoke {
                id: BindingLeaseId::new("l2"),
            },
            6000,
        );
        assert!(
            s.sink.live.borrow().get("db_url").is_none(),
            "revoked binding scrubbed"
        );
        assert!(!s.bound_ready(6000));

        // stop scrubs all.
        s.handle(deliver("db_url", "l3", 6000, 5000, "S3"), 6000);
        s.handle(HostToAgent::Stop, 6000);
        assert!(
            s.sink.live.borrow().is_empty(),
            "stop scrubbed all bindings from sink"
        );
        assert!(!s.bound_ready(6000));
    }

    #[test]
    fn sink_failure_fails_closed_no_ack_no_presence() {
        struct FailSink;
        impl BindingSink for FailSink {
            fn deliver(&self, _n: &BindingName, _v: &str) -> std::io::Result<()> {
                Err(std::io::Error::other("disk full"))
            }
            fn scrub(&self, _n: &BindingName) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut s = BindingSession::new(vec![name("db_url")], FailSink);
        let r = s.handle(deliver("db_url", "l1", 1000, 5000, "S"), 1000);
        assert!(
            matches!(r, AgentToHost::Error { .. }),
            "sink failure ⇒ Error, not Ack"
        );
        assert!(
            !s.bound_ready(1000),
            "failed delivery must not be marked present"
        );
    }
}
