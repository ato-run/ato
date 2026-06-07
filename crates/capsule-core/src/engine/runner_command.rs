//! Typed runner command payloads for the prepare/start session split
//! (RFC: Ato Resource Namespace §"Relationship to Runner API", Step 10
//! "runner prepare / start"; #581 Stage 5).
//!
//! # Why prepare and start are separate
//!
//! The control plane never sends `StartSession` blind. It first sends
//! `PrepareSession`, which asks the runner to project the artifact, storage
//! bindings, network policy, and (where allowed) secrets, and to report launch
//! envelope readiness. Only after the projection digests are captured on the
//! per-session
//! [`crate::foundation::install_lifecycle::materialization::LaunchMaterializationRecord`]
//! and the `execution_id` is fixed does the control plane send `StartSession`
//! with an already-fixed launch-envelope reference. This keeps requirement
//! resolution, runner placement, launch-envelope identity, and runner execution
//! as separate responsibilities, so receipt diff and observed drift stay
//! attributable.
//!
//! # Provider → Runner bridge
//!
//! The RFC's vocabulary is "Runner". The current codebase does not yet have a
//! `Provider`/`Engine` command enum (this is the first typed command payload),
//! so there is nothing to rename here; future provider-side code should adopt
//! this `RunnerCommandPayload` rather than introducing an untyped
//! `Record<string, unknown>` / `Payload interface{}` boundary.
//!
//! This module defines the **typed command shape**. Wiring it to an actual
//! command queue, lease/idempotency lifecycle, and runner adapter is later work
//! (RFC PR 4 / PR 16); nothing here dispatches or executes a command.

use serde::{Deserialize, Serialize};

use crate::foundation::install_lifecycle::materialization::ProjectionDigest;

/// Reference to a prepared materialization plan (e.g. a
/// `LaunchMaterializationRecord` id / namespace path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaterializationPlanRef(String);

impl MaterializationPlanRef {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reference to an assembled launch envelope, fixed at prepare completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaunchEnvelopeRef(String);

impl LaunchEnvelopeRef {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A typed runner command payload.
///
/// A tagged enum (never an untyped JSON blob) so command compatibility,
/// authorization, audit, and migration stay tractable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum RunnerCommandPayload {
    /// Ask the runner to project inputs and report launch-envelope readiness.
    /// Carries the session ref and the materialization plan to project.
    PrepareSession {
        session: String,
        materialization_plan: MaterializationPlanRef,
    },
    /// Start the session using an already-fixed launch envelope. Sent only
    /// after `PrepareSession` succeeded and `execution_id` is fixed.
    StartSession {
        session: String,
        launch_envelope_ref: LaunchEnvelopeRef,
    },
    /// Stop a session with a reason.
    StopSession { session: String, reason: String },
}

impl RunnerCommandPayload {
    /// The session ref this command targets.
    pub fn session(&self) -> &str {
        match self {
            RunnerCommandPayload::PrepareSession { session, .. }
            | RunnerCommandPayload::StartSession { session, .. }
            | RunnerCommandPayload::StopSession { session, .. } => session,
        }
    }
}

/// What a runner reports back after handling a `PrepareSession`.
///
/// The projection digests and readiness flow onto the session's
/// materialization record; they fix the launch envelope before any
/// `StartSession` is issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareSessionOutcome {
    pub session: String,
    /// Digests of the artifact / storage / network / secret projections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection_digests: Vec<ProjectionDigest>,
    /// Whether the launch envelope is assembled and ready to start.
    pub launch_envelope_ready: bool,
    /// The launch envelope reference, present once readiness is achieved. A
    /// `StartSession` must reference this exact value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_envelope_ref: Option<LaunchEnvelopeRef>,
}

impl PrepareSessionOutcome {
    /// Produce the `StartSession` command this prepare outcome authorizes, if
    /// the launch envelope is ready. Returns `None` if the envelope is not yet
    /// ready — start must not be issued before prepare fixes the envelope.
    pub fn into_start_command(self) -> Option<RunnerCommandPayload> {
        match (self.launch_envelope_ready, self.launch_envelope_ref) {
            (true, Some(launch_envelope_ref)) => Some(RunnerCommandPayload::StartSession {
                session: self.session,
                launch_envelope_ref,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_then_start_uses_fixed_envelope_ref() {
        let prepare = RunnerCommandPayload::PrepareSession {
            session: "ses_1".into(),
            materialization_plan: MaterializationPlanRef::new("/sessions/ses_1/execution_identity"),
        };
        assert_eq!(prepare.session(), "ses_1");

        // Prepare completes: projection digests captured, envelope ready.
        let outcome = PrepareSessionOutcome {
            session: "ses_1".into(),
            projection_digests: vec![ProjectionDigest {
                source_ref: "/artifacts/blake3/3333".into(),
                projection_kind: "artifact".into(),
                digest: "blake3:art".into(),
            }],
            launch_envelope_ready: true,
            launch_envelope_ref: Some(LaunchEnvelopeRef::new("env_abc")),
        };
        assert!(!outcome.projection_digests.is_empty());

        // Start uses the already-fixed launch envelope reference.
        let start = outcome.into_start_command().unwrap();
        match start {
            RunnerCommandPayload::StartSession {
                session,
                launch_envelope_ref,
            } => {
                assert_eq!(session, "ses_1");
                assert_eq!(launch_envelope_ref.as_str(), "env_abc");
            }
            other => panic!("expected StartSession, got {other:?}"),
        }
    }

    #[test]
    fn start_not_issued_before_envelope_ready() {
        let outcome = PrepareSessionOutcome {
            session: "ses_2".into(),
            projection_digests: vec![],
            launch_envelope_ready: false,
            launch_envelope_ref: None,
        };
        assert!(
            outcome.into_start_command().is_none(),
            "start must not be issued before prepare fixes the launch envelope"
        );
    }

    #[test]
    fn payload_is_tagged_not_untyped() {
        // A tagged enum serializes with a discriminant — the type boundary the
        // RFC requires (never an untyped payload).
        let json = serde_json::to_string(&RunnerCommandPayload::StopSession {
            session: "ses_3".into(),
            reason: "user requested".into(),
        })
        .unwrap();
        assert!(json.contains("\"command\":\"stop_session\""));

        let back: RunnerCommandPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session(), "ses_3");
    }

    #[test]
    fn prepare_command_roundtrips() {
        let cmd = RunnerCommandPayload::PrepareSession {
            session: "ses_4".into(),
            materialization_plan: MaterializationPlanRef::new("plan_1"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: RunnerCommandPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }
}
