//! Port claims and logical-endpoint admission (#508).
//!
//! Each installed capsule can claim a preferred port for a stable *logical
//! endpoint* (e.g. `ato://app/<id>/http`). At relaunch the preferred port may
//! be taken — by another installed app's claim or by an unrelated OS process —
//! so the claim's conflict policy decides whether Ato remaps to an available
//! alternative (the logical endpoint stays stable), prompts, or fails.
//!
//! This module holds the typed claim/decision and the **pure** evaluator; the
//! DB methods that compose it with recorded claims live in `db.rs`. A port
//! claim is a relaunch *ledger entry*, not exclusive OS ownership.

use std::net::TcpListener;

/// What to do when an installed capsule's preferred port is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Pick an available alternative port; the logical endpoint stays stable.
    Remap,
    /// Surface the conflict for the user to resolve.
    Prompt,
    /// Refuse to launch.
    Fail,
}

impl ConflictPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConflictPolicy::Remap => "remap",
            ConflictPolicy::Prompt => "prompt",
            ConflictPolicy::Fail => "fail",
        }
    }

    /// Parse the stored string form; `None` for unrecognized values.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "remap" => Some(ConflictPolicy::Remap),
            "prompt" => Some(ConflictPolicy::Prompt),
            "fail" => Some(ConflictPolicy::Fail),
            _ => None,
        }
    }
}

/// A port reservation an installed capsule holds for a logical endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortClaim {
    pub install_profile_key: String,
    pub logical_endpoint: String,
    pub preferred_port: u16,
    pub last_actual_port: Option<u16>,
    pub protocol: String,
    pub conflict_policy: ConflictPolicy,
}

/// Result of a port-admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortAdmission {
    /// The preferred port is available; use it.
    Admitted { port: u16 },
    /// The preferred port was taken; remapped to an available alternative. The
    /// logical endpoint stays stable (remap policy).
    Remapped { preferred: u16, port: u16 },
    /// The preferred port was taken and the policy disallows remapping.
    Rejected {
        preferred: u16,
        policy: ConflictPolicy,
    },
}

impl PortAdmission {
    /// The port to bind, if the launch may proceed (`None` when rejected).
    pub fn resolved_port(&self) -> Option<u16> {
        match self {
            PortAdmission::Admitted { port } | PortAdmission::Remapped { port, .. } => Some(*port),
            PortAdmission::Rejected { .. } => None,
        }
    }

    /// Whether the launch may proceed (admitted or remapped).
    pub fn is_admitted(&self) -> bool {
        !matches!(self, PortAdmission::Rejected { .. })
    }
}

/// Inclusive range scanned for a remap alternative (IANA dynamic / ephemeral).
const REMAP_RANGE: std::ops::RangeInclusive<u16> = 49152..=65535;

/// Pure port-admission decision. `is_available(port)` returns whether a port is
/// free (not claimed by another app and free on the OS). Deterministic for a
/// given `is_available`.
pub fn evaluate_port_admission(
    preferred: u16,
    policy: ConflictPolicy,
    is_available: impl Fn(u16) -> bool,
) -> PortAdmission {
    if is_available(preferred) {
        return PortAdmission::Admitted { port: preferred };
    }
    match policy {
        ConflictPolicy::Remap => {
            match REMAP_RANGE
                .clone()
                .find(|candidate| *candidate != preferred && is_available(*candidate))
            {
                Some(port) => PortAdmission::Remapped { preferred, port },
                None => PortAdmission::Rejected { preferred, policy },
            }
        }
        ConflictPolicy::Prompt | ConflictPolicy::Fail => {
            PortAdmission::Rejected { preferred, policy }
        }
    }
}

/// Whether `port` can currently be bound on loopback (a cheap OS availability
/// probe, not a reservation). Port `0` ("any port") is treated as unavailable
/// since it is not a concrete claim.
pub fn os_port_is_free(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_when_preferred_is_available() {
        let decision = evaluate_port_admission(3000, ConflictPolicy::Fail, |_| true);
        assert_eq!(decision, PortAdmission::Admitted { port: 3000 });
        assert_eq!(decision.resolved_port(), Some(3000));
    }

    #[test]
    fn remap_policy_returns_an_available_alternative() {
        // Preferred 3000 is taken; everything else is free → remap to the first
        // port in the remap range.
        let decision = evaluate_port_admission(3000, ConflictPolicy::Remap, |p| p != 3000);
        assert_eq!(
            decision,
            PortAdmission::Remapped {
                preferred: 3000,
                port: 49152
            }
        );
        assert!(decision.is_admitted());
    }

    #[test]
    fn fail_policy_rejects_when_preferred_is_taken() {
        let decision = evaluate_port_admission(3000, ConflictPolicy::Fail, |_| false);
        assert_eq!(
            decision,
            PortAdmission::Rejected {
                preferred: 3000,
                policy: ConflictPolicy::Fail
            }
        );
        assert!(!decision.is_admitted());
        assert_eq!(decision.resolved_port(), None);
    }

    #[test]
    fn prompt_policy_rejects_without_remapping() {
        let decision = evaluate_port_admission(3000, ConflictPolicy::Prompt, |p| p != 3000);
        assert!(matches!(
            decision,
            PortAdmission::Rejected {
                policy: ConflictPolicy::Prompt,
                ..
            }
        ));
    }

    #[test]
    fn remap_rejects_when_no_alternative_is_available() {
        let decision = evaluate_port_admission(3000, ConflictPolicy::Remap, |_| false);
        assert!(matches!(decision, PortAdmission::Rejected { .. }));
    }

    #[test]
    fn conflict_policy_string_roundtrip() {
        for policy in [
            ConflictPolicy::Remap,
            ConflictPolicy::Prompt,
            ConflictPolicy::Fail,
        ] {
            assert_eq!(ConflictPolicy::from_str_opt(policy.as_str()), Some(policy));
        }
        assert_eq!(ConflictPolicy::from_str_opt("nonsense"), None);
    }
}
