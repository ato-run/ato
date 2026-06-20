//! ExecutionPlan consent wire types — the single source of truth for the
//! consent-required payload that `ato run` emits and two independent
//! consumers parse.
//!
//! # Why this lives here
//!
//! When `ato run` hits the ExecutionPlan consent gate (E302) in a non-TTY
//! shell it announces the requirement on **two** channels:
//!
//! - **stdout**, as a `CONSENT-REQUIRED: <json>` line that the Connected
//!   Runner (`ato-cli`'s `runner_agent`) parses to park its lease
//!   `needs_consent`; carries the `schema` tag and the derived `consent_ref`
//!   so an owner's approval binds the exact policy.
//! - **stderr**, as the `details` block of the typed `--json` error envelope
//!   that a UI shell (today: `ato-desktop`) reads to render an approval
//!   modal; carries the `reason` discriminator instead of `consent_ref`.
//!
//! Both channels share the same **identity 5-tuple + human summary**. That
//! shared core, and its validation, used to be hand-mirrored in three
//! places (`consent_store::ConsentRequiredLine`, `runner_agent::
//! ConsentRequest`, `ato-desktop::cli_envelope::ConsentRequiredDetailsDto`),
//! so a schema bump compiled everywhere but made the validators silently
//! reject — no consent prompt would appear. Single-sourcing the type and
//! the validation here makes that drift impossible (the same move M4/M5
//! made for `ccp` and `ConfigField`).
//!
//! Serialization contract (DO NOT change without coordinating both
//! producers + both consumers + spec docs): the identity tuple is flattened
//! into each channel envelope via `#[serde(flatten)]`, so on the wire the
//! five identity fields and `summary` appear as siblings of the
//! channel-specific fields (`schema`/`consent_ref` on stdout; `reason` on
//! stderr). The flattened fields are required: an envelope missing any of
//! them fails to deserialize, which the consumers treat as "not a consent
//! signal" (fail closed).

use serde::{Deserialize, Serialize};

/// Schema tag baked into the `consent_ref` hash input and emitted on the
/// `CONSENT-REQUIRED:` line. Single-sourced here so the runner's validation,
/// the CLI's emitted line, and `capsule-core`'s `consent_ref` hash all agree
/// on one literal. `capsule-core` re-exports this as `CONSENT_REF_SCHEMA`.
pub const CONSENT_REQUIRED_SCHEMA: &str = "execution_plan_consent_v1";

/// Discriminator written into the stderr `details.reason` field so a
/// consumer can route an E302 (`ATO_ERR_EXECUTION_CONTRACT_INVALID`)
/// envelope to the consent-modal flow specifically. Any other E302 lacks
/// this reason and falls through to the generic fatal-toast path.
pub const CONSENT_REQUIRED_REASON: &str = "execution_plan_consent_required";

/// The shared core of every consent-required payload: the identity 5-tuple
/// that uniquely names the policy decision (`scoped_id`, `version`,
/// `target_label`, `policy_segment_hash`, `provisioning_policy_hash`) plus a
/// pre-rendered human `summary`. Flattened into both channel envelopes.
///
/// All fields are required on the wire (the producer emits them
/// unconditionally). `summary` must be PRESENT but may be empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentIdentity {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
    /// Human policy summary. Must be PRESENT (may be empty).
    pub summary: String,
}

impl ConsentIdentity {
    /// The identity is actionable only if it can round-trip back through
    /// `ato internal consent approve-execution-plan`: a non-empty scoped_id /
    /// version / target_label and `blake3:`-prefixed policy hashes. `summary`
    /// is intentionally NOT validated here — an empty summary is allowed.
    pub fn is_valid(&self) -> bool {
        !self.scoped_id.is_empty()
            && !self.version.is_empty()
            && !self.target_label.is_empty()
            && self.policy_segment_hash.starts_with("blake3:")
            && self.provisioning_policy_hash.starts_with("blake3:")
    }
}

/// stdout `CONSENT-REQUIRED: <json>` payload. The runner parses this and,
/// only after the owner approves this exact `consent_ref`, calls the local
/// `approve-execution-plan` primitive and retries.
///
/// All fields are required (no serde defaults): a line missing any field
/// fails to deserialize and is NOT treated as a consent signal — an
/// incomplete signal must never reach the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRequiredLine {
    pub schema: String,
    /// `blake3(JCS(schema + 5-tuple))` — the hash an owner's approval binds.
    pub consent_ref: String,
    #[serde(flatten)]
    pub identity: ConsentIdentity,
}

impl ConsentRequiredLine {
    /// A consent signal is honored only if it is complete AND well-formed:
    /// the exact schema, a `blake3:` consent_ref, and a valid identity.
    /// Anything less is not a valid consent gate.
    pub fn is_valid(&self) -> bool {
        self.schema == CONSENT_REQUIRED_SCHEMA
            && self.consent_ref.starts_with("blake3:")
            && self.identity.is_valid()
    }
}

/// stderr `details` payload for the E302 consent sub-shape. Carries the same
/// identity tuple plus a `reason` discriminator (no `consent_ref` on this
/// channel).
///
/// The identity fields are required: an unrelated E302 (e.g. a generic
/// `ExecutionContractInvalid` whose `details` carries `field`/`service`)
/// fails to deserialize into this shape, which the consumer maps to "fall
/// through to the fatal-toast path" — never to a half-populated modal.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConsentRequiredDetails {
    /// Must equal [`CONSENT_REQUIRED_REASON`]. Older E302 envelopes (without
    /// this field) yield `None` from [`consent_required`](Self::consent_required)
    /// so the caller falls through to the generic fatal-toast path.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub identity: ConsentIdentity,
}

impl ConsentRequiredDetails {
    /// Return the validated identity ONLY when this envelope is a genuine,
    /// actionable consent-required signal: the `reason` discriminator matches
    /// AND every identity field is well-formed. Both gates protect the caller
    /// from routing an unrelated or unsatisfiable E302 to the consent modal.
    pub fn consent_required(&self) -> Option<&ConsentIdentity> {
        if self.reason.as_deref() != Some(CONSENT_REQUIRED_REASON) {
            return None;
        }
        if !self.identity.is_valid() {
            return None;
        }
        Some(&self.identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_line() -> ConsentRequiredLine {
        ConsentRequiredLine {
            schema: CONSENT_REQUIRED_SCHEMA.to_string(),
            consent_ref: "blake3:ref".to_string(),
            identity: ConsentIdentity {
                scoped_id: "community/hello-capsule".to_string(),
                version: "0.3.0".to_string(),
                target_label: "main".to_string(),
                policy_segment_hash: "blake3:p".to_string(),
                provisioning_policy_hash: "blake3:q".to_string(),
                summary: "network: api.example.com".to_string(),
            },
        }
    }

    #[test]
    fn line_round_trips_with_flattened_identity() {
        let line = valid_line();
        let json = serde_json::to_string(&line).unwrap();
        // The identity fields must be siblings of schema/consent_ref, not nested.
        assert!(json.contains("\"scoped_id\":\"community/hello-capsule\""));
        assert!(!json.contains("\"identity\""));
        let back: ConsentRequiredLine = serde_json::from_str(&json).unwrap();
        assert_eq!(back, line);
        assert!(back.is_valid());
    }

    #[test]
    fn line_rejects_incomplete_or_malformed() {
        // Missing a field → fails to deserialize (no defaults).
        let missing_consent_ref = r#"{"schema":"execution_plan_consent_v1","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#;
        assert!(serde_json::from_str::<ConsentRequiredLine>(missing_consent_ref).is_err());

        // Present but invalid → parses, but is_valid() rejects.
        let mut wrong_schema = valid_line();
        wrong_schema.schema = "WRONG".to_string();
        assert!(!wrong_schema.is_valid());

        let mut non_blake3_ref = valid_line();
        non_blake3_ref.consent_ref = "sha256:r".to_string();
        assert!(!non_blake3_ref.is_valid());

        let mut empty_scoped = valid_line();
        empty_scoped.identity.scoped_id = String::new();
        assert!(!empty_scoped.is_valid());

        let mut empty_hash = valid_line();
        empty_hash.identity.policy_segment_hash = String::new();
        assert!(!empty_hash.is_valid());
    }

    #[test]
    fn line_allows_empty_summary() {
        let mut line = valid_line();
        line.identity.summary = String::new();
        assert!(line.is_valid());
    }

    #[test]
    fn details_routes_only_complete_consent_reason() {
        let line = r#"{"reason":"execution_plan_consent_required","scoped_id":"wasedap2p-backend","version":"0.1.0","target_label":"app","policy_segment_hash":"blake3:aaa","provisioning_policy_hash":"blake3:bbb","summary":"Capsule: x"}"#;
        let details: ConsentRequiredDetails = serde_json::from_str(line).unwrap();
        let identity = details.consent_required().expect("complete consent signal");
        assert_eq!(identity.scoped_id, "wasedap2p-backend");
        assert_eq!(identity.target_label, "app");
        assert!(!identity.summary.is_empty());
    }

    #[test]
    fn details_unrelated_e302_fails_shape() {
        // No identity fields → the strict flatten fails to deserialize,
        // which the desktop maps (via `.ok()?`) to "fall through".
        let line = r#"{"field":"some.other.path","service":null}"#;
        assert!(serde_json::from_str::<ConsentRequiredDetails>(line).is_err());
    }

    #[test]
    fn details_rejects_empty_identity() {
        let line = r#"{"reason":"execution_plan_consent_required","scoped_id":"","version":"","target_label":"","policy_segment_hash":"","provisioning_policy_hash":"","summary":""}"#;
        let details: ConsentRequiredDetails = serde_json::from_str(line).unwrap();
        assert!(details.consent_required().is_none());
    }

    #[test]
    fn details_without_reason_returns_none() {
        let line = r#"{"scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#;
        let details: ConsentRequiredDetails = serde_json::from_str(line).unwrap();
        assert!(details.consent_required().is_none());
    }
}
