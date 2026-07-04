//! Phase 8a guest-agent **control plane** wire messages (vsock) — #863; contract:
//! `docs/ready-state/binding-lease.md`.
//!
//! PR 2 (skeleton): the host↔guest-agent message types. The only value-bearing message
//! is [`HostToAgent::Deliver`], which carries the explicit
//! [`BindingLeaseDelivery`](crate::binding_lease::BindingLeaseDelivery); every response
//! is value-free. No real tmpfs delivery yet — the pure session state machine that
//! consumes these lives in the `guest-agent` crate.

use serde::{Deserialize, Serialize};

use crate::binding_lease::{BindingLeaseDelivery, BindingLeaseId, BindingName};

/// Schema version for the control-plane messages. Bump on any breaking change.
pub const BINDING_CONTROL_SCHEMA_VERSION: u32 = 1;

/// Host → guest-agent control messages (over vsock).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostToAgent {
    /// Deliver (or renew) a lease. The **only** value-bearing message — carries the
    /// explicit wire payload.
    Deliver(BindingLeaseDelivery),
    /// Revoke a lease by id; the agent scrubs its binding immediately.
    Revoke { id: BindingLeaseId },
    /// Ask whether every required binding is present (the bound-ready gate).
    QueryBoundReady,
    /// v1.2 (supervisor mode): stop the supervised workload process — **stop only,
    /// it does NOT scrub bindings**. Used at BUILD time before the pre-bind snapshot;
    /// the app must not be running when the memory image is captured. The build
    /// sequence is `StopWorkload` **then `Revoke` (all leases)** to also scrub the
    /// tmpfs before the seal (contract §7.2). A capsule without a supervisor answers
    /// `WorkloadStopped { was_running: false }`.
    StopWorkload,
    /// Session teardown: revoke + scrub all bindings, then the host tears the VM down.
    Stop,
}

/// Guest-agent → host responses. **Never** contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentToHost {
    /// A lease was accepted (delivered/renewed). No value echoed back.
    Ack { id: BindingLeaseId, name: BindingName },
    /// Bound-ready state: whether all required bindings are present, and which names
    /// are still pending.
    BoundReady { ready: bool, pending: Vec<BindingName> },
    /// A binding was scrubbed (revoke / expiry / stop).
    Scrubbed { id: BindingLeaseId },
    /// v1.2 (supervisor mode): the supervised workload was stopped (or was not
    /// running). Never carries a value.
    WorkloadStopped { was_running: bool },
    /// An error — must never carry a secret.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

    #[test]
    fn deliver_is_the_only_value_bearing_message_and_round_trips() {
        let secret = "pw-should-only-be-in-deliver";
        let lease = BindingLease::issue(
            BindingLeaseId::new("l1"),
            BindingName::parse("db_url").unwrap(),
            SecretValue::new(secret),
            1000,
            5000,
        );
        let deliver = HostToAgent::Deliver(lease.to_delivery());
        let json = serde_json::to_string(&deliver).unwrap();
        assert!(json.contains(secret), "Deliver carries the value for wire");
        // Its Debug still redacts (WireSecret Debug).
        assert!(!format!("{deliver:?}").contains(secret), "Deliver Debug leaked the secret");

        // Responses never carry the value.
        let resp = AgentToHost::BoundReady { ready: false, pending: vec![BindingName::parse("db_url").unwrap()] };
        assert!(!serde_json::to_string(&resp).unwrap().contains(secret));
        // Round-trip a response.
        let back: AgentToHost = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
    }
    #[test]
    fn supervisor_control_messages_round_trip_and_are_value_free() {
        let stop = HostToAgent::StopWorkload;
        let j = serde_json::to_string(&stop).unwrap();
        assert_eq!(j, r#"{"kind":"stop_workload"}"#);
        let resp = AgentToHost::WorkloadStopped { was_running: true };
        let rj = serde_json::to_string(&resp).unwrap();
        assert_eq!(rj, r#"{"kind":"workload_stopped","was_running":true}"#);
        let back: AgentToHost = serde_json::from_str(&rj).unwrap();
        assert_eq!(back, resp);
    }
}
