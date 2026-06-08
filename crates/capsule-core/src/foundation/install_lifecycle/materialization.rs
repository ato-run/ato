//! Per-session launch materialization record
//! (RFC: Ato Resource Namespace §"Install Outputs and Launch Reuse",
//! §"Relationship to Execution Identity"; #581 Stage 4).
//!
//! # The three-layer separation
//!
//! ```text
//! InstallRevision            immutable build/output identity   — never changes per launch
//! LaunchTemplate             reusable launch envelope template — reused across sessions
//! LaunchMaterializationRecord per-session projection record    — frozen per session, never reused
//! ```
//!
//! A [`LaunchMaterializationRecord`] is the *only* record that pins a launch to
//! a concrete session: it records which install revision / template inputs were
//! used, which runner was selected, the projection digests produced at prepare
//! time, the `execution_id`, and the derived [`CapsuleInstanceKey`]. Every new
//! session gets a new record; records are never reused for a later launch.
//!
//! Identity vs diagnostics: the record stores *digests* of projections (artifact,
//! storage, network policy, secret) — never the secret values themselves — and
//! the stable identity inputs. Runtime-observed diagnostics (logs, routes,
//! bound ports, readiness, PIDs, container ids, timestamps other than
//! `materialized_at`) are deliberately absent: they belong on the session's
//! observed receipt, not on this identity record, and must not be treated as
//! execution drift by default.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ids::{
    CapsuleInstanceKey, ExecutionId, InstallProfileKey, InstallRevisionId,
    derive_capsule_instance_key,
};
use super::launch_template::RunnerClass;

/// A digest of one projection produced while preparing a launch.
///
/// `digest` is a content hash (`blake3:<hex>`) of the projected input — for a
/// secret projection this is a digest of the projection *shape/target*, never
/// the secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionDigest {
    /// Reference to the source resource that was projected (a namespace ref).
    pub source_ref: String,
    /// Projection kind: `"artifact"`, `"storage"`, `"network_policy"`,
    /// `"secret"`, `"launch_envelope"`, …
    pub projection_kind: String,
    /// Content digest of the projection (`blake3:<hex>`). Never a secret value.
    pub digest: String,
}

/// Why a [`ProjectionDigest`] is not a valid materialization input.
///
/// Typed (never an in-band sentinel). The `digest` must be a `blake3:<hex>`
/// content hash — requiring that prefix is what structurally prevents a raw
/// secret value from being accepted as a "digest".
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionDigestInvalidReason {
    /// `source_ref` is empty.
    #[error("projection digest source_ref is empty")]
    EmptySourceRef,
    /// `projection_kind` is empty.
    #[error("projection digest projection_kind is empty")]
    EmptyKind,
    /// `digest` is empty.
    #[error("projection digest digest is empty")]
    EmptyDigest,
    /// `digest` is not a `blake3:<hex>` content hash — it could be a raw value.
    #[error("projection digest '{digest}' is not a blake3:<hex> content hash")]
    DigestNotContentHash { digest: String },
}

impl ProjectionDigest {
    /// Validate that this is a well-formed projection digest safe to fold into a
    /// materialization identity: non-empty fields and a `blake3:<hex>` content
    /// hash (so a raw secret value can never masquerade as a digest).
    pub fn validate(&self) -> Result<(), ProjectionDigestInvalidReason> {
        if self.source_ref.is_empty() {
            return Err(ProjectionDigestInvalidReason::EmptySourceRef);
        }
        if self.projection_kind.is_empty() {
            return Err(ProjectionDigestInvalidReason::EmptyKind);
        }
        if self.digest.is_empty() {
            return Err(ProjectionDigestInvalidReason::EmptyDigest);
        }
        if !is_blake3_content_hash(&self.digest) {
            return Err(ProjectionDigestInvalidReason::DigestNotContentHash {
                digest: self.digest.clone(),
            });
        }
        Ok(())
    }
}

/// True if `s` is a `blake3:<hex>` content hash (non-empty lowercase hex body).
fn is_blake3_content_hash(s: &str) -> bool {
    match s.strip_prefix("blake3:") {
        Some(hex) => !hex.is_empty() && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        None => false,
    }
}

/// A per-session record of what was actually projected onto a concrete session
/// / runner. Frozen per session; never reused for a subsequent launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchMaterializationRecord {
    /// Control-plane-global session id (not a runner-local process id).
    pub session_ref: String,
    pub install_profile_key: InstallProfileKey,
    pub install_revision_id: InstallRevisionId,
    /// Stable identity inputs carried for receipt/diff correlation.
    pub profile_hash: String,
    pub requirement_graph_hash: String,
    pub binding_set_hash: String,
    /// The runner selected for this session, if placement has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_runner_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_runner_class: Option<RunnerClass>,
    /// References to the inputs that fed the launch envelope (artifact, bindings…).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_refs: Vec<String>,
    /// Projection digests captured at prepare time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection_digests: Vec<ProjectionDigest>,
    /// Launch-envelope / execution identity for this session.
    pub execution_id: ExecutionId,
    /// `install_profile_key + install_revision_id + execution_id` — the exact
    /// replay / session key. Derived, never supplied directly.
    pub capsule_instance_key: CapsuleInstanceKey,
    /// RFC 3339 timestamp this record was frozen. This is the *only* timestamp
    /// on the record and is not an identity input.
    pub materialized_at: String,
}

impl LaunchMaterializationRecord {
    /// Freeze a new per-session materialization record.
    ///
    /// Derives the [`CapsuleInstanceKey`] from
    /// `install_profile_key + install_revision_id + execution_id` so callers
    /// cannot supply an inconsistent key.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_ref: impl Into<String>,
        install_profile_key: InstallProfileKey,
        install_revision_id: InstallRevisionId,
        profile_hash: impl Into<String>,
        requirement_graph_hash: impl Into<String>,
        binding_set_hash: impl Into<String>,
        selected_runner_ref: Option<String>,
        selected_runner_class: Option<RunnerClass>,
        input_refs: Vec<String>,
        projection_digests: Vec<ProjectionDigest>,
        execution_id: ExecutionId,
        materialized_at: impl Into<String>,
    ) -> Self {
        let capsule_instance_key =
            derive_capsule_instance_key(&install_profile_key, &install_revision_id, &execution_id);
        Self {
            session_ref: session_ref.into(),
            install_profile_key,
            install_revision_id,
            profile_hash: profile_hash.into(),
            requirement_graph_hash: requirement_graph_hash.into(),
            binding_set_hash: binding_set_hash.into(),
            selected_runner_ref,
            selected_runner_class,
            input_refs,
            projection_digests,
            execution_id,
            capsule_instance_key,
            materialized_at: materialized_at.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::ids::{
        InstalledAppId, ProfileId, derive_install_profile_key,
    };

    fn ipk() -> InstallProfileKey {
        derive_install_profile_key(
            &InstalledAppId::new("app_pgweb"),
            &ProfileId::new("default"),
        )
    }

    fn record_for_session(session: &str, exec: ExecutionId) -> LaunchMaterializationRecord {
        LaunchMaterializationRecord::new(
            session,
            ipk(),
            InstallRevisionId::new("rev_aaaa"),
            "blake3:prof",
            "blake3:graph",
            "blake3:bind",
            Some("/runners/run_managed_1".into()),
            Some(RunnerClass::ManagedRunner),
            vec!["/artifacts/blake3/3333".into()],
            vec![ProjectionDigest {
                source_ref: "/secrets/sec_db".into(),
                projection_kind: "secret".into(),
                digest: "blake3:projdigest".into(),
            }],
            exec,
            "2026-06-08T00:00:00Z",
        )
    }

    // ── Acceptance: every session gets a new, non-reused record ───────────────

    #[test]
    fn each_session_gets_a_distinct_record() {
        let exec_a = ExecutionId::generate();
        let exec_b = ExecutionId::generate();
        let rec_a = record_for_session("ses_a", exec_a.clone());
        let rec_b = record_for_session("ses_b", exec_b.clone());

        assert_ne!(rec_a.session_ref, rec_b.session_ref);
        assert_ne!(rec_a.execution_id, rec_b.execution_id);
        assert_ne!(
            rec_a.capsule_instance_key, rec_b.capsule_instance_key,
            "distinct sessions (distinct execution ids) must have distinct instance keys"
        );
    }

    // ── Acceptance: capsule_instance_key derived from the canonical triple ────

    #[test]
    fn capsule_instance_key_is_derived_from_ipk_revision_execution() {
        let exec = ExecutionId::generate();
        let rec = record_for_session("ses_x", exec.clone());
        let expected =
            derive_capsule_instance_key(&ipk(), &InstallRevisionId::new("rev_aaaa"), &exec);
        assert_eq!(rec.capsule_instance_key, expected);
        assert!(rec.capsule_instance_key.as_str().starts_with("cik_"));
    }

    #[test]
    fn same_triple_same_instance_key_across_record_rebuilds() {
        // Same ipk + revision + execution_id ⇒ same instance key, even though
        // the record is a fresh object each time (per-session freeze, but the
        // key is a pure function of the triple).
        let exec = ExecutionId::generate();
        let a = record_for_session("ses_1", exec.clone());
        let b = record_for_session("ses_1", exec);
        assert_eq!(a.capsule_instance_key, b.capsule_instance_key);
    }

    // ── Acceptance: diagnostics separate from identity inputs ─────────────────

    #[test]
    fn record_carries_projection_digests_not_secret_values() {
        let rec = record_for_session("ses_secret", ExecutionId::generate());
        let secret_proj = rec
            .projection_digests
            .iter()
            .find(|p| p.projection_kind == "secret")
            .unwrap();
        // The digest is a hash, not the value.
        assert!(secret_proj.digest.starts_with("blake3:"));
        // Serialized record must not contain anything that looks like a raw value.
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("blake3:projdigest"));
        assert!(
            !json.contains("hunter2") && !json.contains("password"),
            "materialization record must never carry secret values"
        );
    }

    #[test]
    fn record_includes_selected_runner_and_projection_digests() {
        let rec = record_for_session("ses_runner", ExecutionId::generate());
        assert_eq!(rec.selected_runner_class, Some(RunnerClass::ManagedRunner));
        assert!(rec.selected_runner_ref.is_some());
        assert!(!rec.projection_digests.is_empty());
    }

    #[test]
    fn materialization_record_roundtrips() {
        let rec = record_for_session("ses_rt", ExecutionId::generate());
        let json = serde_json::to_string(&rec).unwrap();
        let back: LaunchMaterializationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }
}
