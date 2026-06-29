//! `PrepareSession` command construction from a launch materialization
//! (RFC: Ato Resource Namespace §"Relationship to Runner API", Step 10; #581
//! wave 5C).
//!
//! This is the next boundary in the launch pipeline:
//!
//! ```text
//! LaunchTemplate
//!   -> LaunchMaterializationRecord   (frozen per session, #581 wave 5B)
//!   -> PrepareSession command payload (this module)
//!   -> PrepareSessionOutcome          (runner reports projection digests)
//!   -> StartSession command payload   (PrepareSessionOutcome::into_start_command)
//! ```
//!
//! It turns a validated [`LaunchMaterializationRecord`] plus a selected runner
//! reference and a stable command-request id into a typed
//! [`RunnerCommandPayload::PrepareSession`]. It **constructs and validates the
//! typed payload only** — it does not dispatch it, does not implement a command
//! queue or lease/idempotency store, does not launch a process/container/browser,
//! and computes no live/observed fact.
//!
//! # Reference-only payload
//!
//! The existing [`RunnerCommandPayload::PrepareSession`] is deliberately
//! reference-shaped: it carries the session ref and a
//! [`MaterializationPlanRef`] pointing at the per-session materialization plan
//! (the persisted [`LaunchMaterializationRecord`], stored under
//! `sessions/<capsule_instance_key>/materialization.json`). The runner
//! dereferences the plan to obtain the projection targets, requirement-graph
//! hashes, and digests — none of those are inlined into the wire payload, and
//! no payload-shape migration is introduced here.
//!
//! The plan ref is built from the materialization's `capsule_instance_key`,
//! which (via the #581 wave 5B identity derivation) folds in the projection
//! digests: a changed projection digest yields a different `execution_id`, hence
//! a different `capsule_instance_key`, hence a different plan ref — so the
//! command's content changes when the materialization changes, while the payload
//! still carries only references, never raw secret/state values or observed
//! diagnostics (status, port, pid, container id, route, log cursor).

use thiserror::Error;

use crate::foundation::install_lifecycle::ids::{CapsuleInstanceKey, ExecutionId};
use crate::foundation::install_lifecycle::materialization::{
    LaunchMaterializationRecord, MaterializationRecordInvalidReason,
};

use super::runner_command::{MaterializationPlanRef, RunnerCommandPayload};

/// Inputs to [`build_prepare_session_command`] (#581 wave 5C).
pub struct PrepareSessionCommandBuildInput {
    /// The validated, per-session materialization record to prepare.
    pub materialization: LaunchMaterializationRecord,
    /// The runner selected for this session (a control-plane ref, not a pid /
    /// container id / live route). Must match the record's `selected_runner_ref`
    /// when the record pins one.
    pub runner_ref: String,
    /// A stable, caller-supplied command-request id (idempotency / correlation
    /// key for the later dispatch layer). Validated as non-empty; not folded into
    /// the wire payload here (the dispatch/lease layer is a later wave).
    pub command_request_id: String,
}

/// The built `PrepareSession` command plus the identity refs it preserves
/// (#581 wave 5C).
///
/// The wire `payload` is reference-only; the echoed identity refs (preserved
/// verbatim from the validated materialization) let the dispatch layer correlate
/// without re-reading the plan. None of them are observed/runtime facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareSessionCommandBuildOutput {
    /// The typed command payload (reference-only `PrepareSession`).
    pub payload: RunnerCommandPayload,
    /// The materialization plan reference embedded in the payload.
    pub materialization_plan: MaterializationPlanRef,
    /// The command-request id, echoed for the dispatch layer.
    pub command_request_id: String,
    /// The session ref the command targets.
    pub session: String,
    /// The runner ref the command was built for.
    pub runner_ref: String,
    // ── Preserved identity refs (verbatim from the materialization) ──────────
    pub execution_id: ExecutionId,
    pub capsule_instance_key: CapsuleInstanceKey,
    pub requirement_graph_hash: String,
    pub requirement_graph_snapshot_hash: String,
}

/// Typed failure when building a `PrepareSession` command (#581 wave 5C).
///
/// Every variant is structured and auditable — never an in-band `"unknown"` /
/// `"unset"` sentinel. Carries only refs / content hashes / typed reasons, never
/// a secret or observed value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrepareSessionCommandBuildError {
    /// The materialization record failed structural validation.
    #[error("materialization record is invalid: {0}")]
    InvalidMaterialization(#[source] MaterializationRecordInvalidReason),
    /// The selected runner reference is empty.
    #[error("runner reference is empty")]
    RunnerRefMissing,
    /// The command-request id is empty.
    #[error("command request id is empty")]
    CommandRequestIdMissing,
    /// The materialization has no pinned runner (both `selected_runner_class` and
    /// `selected_runner_ref` are absent). `PrepareSession` is the post-placement
    /// boundary, so a pre-placement materialization cannot be prepared.
    #[error("materialization has no pinned runner (placement has not selected one)")]
    MaterializationRunnerNotPinned,
    /// The requested runner ref does not match the runner the materialization
    /// already pinned at placement time.
    #[error(
        "requested runner ref '{requested}' does not match materialization runner ref '{record}'"
    )]
    RunnerRefMismatch { record: String, requested: String },
}

/// Build a [`RunnerCommandPayload::PrepareSession`] from a validated launch
/// materialization (#581 wave 5C).
///
/// Validates the materialization structurally, checks the runner ref / command
/// id are present and that the runner ref agrees with the record's pinned
/// runner, and assembles a reference-only payload. Does not dispatch, queue, or
/// launch anything.
pub fn build_prepare_session_command(
    input: PrepareSessionCommandBuildInput,
) -> Result<PrepareSessionCommandBuildOutput, PrepareSessionCommandBuildError> {
    let PrepareSessionCommandBuildInput {
        materialization,
        runner_ref,
        command_request_id,
    } = input;

    // 1. The materialization must be structurally valid before we reference it.
    materialization
        .validate()
        .map_err(PrepareSessionCommandBuildError::InvalidMaterialization)?;

    // 2. The materialization must come from completed placement: both the runner
    //    class and ref must be pinned. `validate()` already rejected the
    //    one-present-one-absent case (RunnerSelectionInconsistent), so the only
    //    remaining unpinned shape here is both-absent — a pre-placement record
    //    that cannot be prepared.
    let record_runner_ref = match (
        materialization.selected_runner_ref.as_deref(),
        materialization.selected_runner_class,
    ) {
        (Some(record_runner), Some(_class)) => record_runner,
        _ => return Err(PrepareSessionCommandBuildError::MaterializationRunnerNotPinned),
    };

    // 3. Required command metadata.
    if runner_ref.is_empty() {
        return Err(PrepareSessionCommandBuildError::RunnerRefMissing);
    }
    if command_request_id.is_empty() {
        return Err(PrepareSessionCommandBuildError::CommandRequestIdMissing);
    }

    // 4. The requested runner must match the runner the record pinned.
    if record_runner_ref != runner_ref {
        return Err(PrepareSessionCommandBuildError::RunnerRefMismatch {
            record: record_runner_ref.to_owned(),
            requested: runner_ref,
        });
    }

    // 5. Build the reference-only payload. The plan ref is content-addressed by
    //    `capsule_instance_key`, so it changes whenever the materialization
    //    identity (incl. projection digests) changes.
    let materialization_plan = MaterializationPlanRef::new(format!(
        "/sessions/{}/materialization",
        materialization.capsule_instance_key.as_str()
    ));
    let session = materialization.session_ref.clone();
    let payload = RunnerCommandPayload::PrepareSession {
        session: session.clone(),
        materialization_plan: materialization_plan.clone(),
    };

    Ok(PrepareSessionCommandBuildOutput {
        payload,
        materialization_plan,
        command_request_id,
        session,
        runner_ref,
        execution_id: materialization.execution_id.clone(),
        capsule_instance_key: materialization.capsule_instance_key.clone(),
        requirement_graph_hash: materialization.requirement_graph_hash.clone(),
        requirement_graph_snapshot_hash: materialization.requirement_graph_snapshot_hash.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::runner_command::{LaunchEnvelopeRef, PrepareSessionOutcome};
    use crate::foundation::install_lifecycle::ids::{
        InstallRevisionId, InstalledAppId, ProfileId, derive_install_profile_key,
    };
    use crate::foundation::install_lifecycle::launch_template::RunnerClass;
    use crate::foundation::install_lifecycle::materialization::ProjectionDigest;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// A valid `blake3:<64 hex>` digest from a short hex seed (right-padded).
    fn digest(seed_hex: &str) -> String {
        let body = format!("{seed_hex:0<64}");
        format!("blake3:{}", &body[..64])
    }

    fn projection_digest(kind: &str, source: &str, digest: &str) -> ProjectionDigest {
        ProjectionDigest {
            source_ref: source.into(),
            projection_kind: kind.into(),
            digest: digest.into(),
        }
    }

    fn sample_digests() -> Vec<ProjectionDigest> {
        vec![
            projection_digest("artifact", "/artifacts/blake3/3333", &digest("a47d16")),
            projection_digest("secret", "/secrets/sec_db", &digest("5ecd16")),
        ]
    }

    const RUNNER_REF: &str = "/runners/run_managed_1";

    /// A valid materialization record with the given session ref + digests.
    fn materialization(
        session: &str,
        digests: Vec<ProjectionDigest>,
    ) -> LaunchMaterializationRecord {
        let ipk = derive_install_profile_key(
            &InstalledAppId::new("app_prepare_cmd_test"),
            &ProfileId::new("default"),
        );
        LaunchMaterializationRecord::new(
            session,
            ipk,
            InstallRevisionId::new("rev_aaaa"),
            "blake3:prof",
            // content graph hash vs snapshot hash — deliberately distinct.
            digest("c0a7e7"),
            digest("57a45e"),
            "blake3:bind",
            Some(RUNNER_REF.to_owned()),
            Some(RunnerClass::ManagedRunner),
            vec!["/artifacts/blake3/3333".into()],
            digests,
            ExecutionId::new(format!("exec_{}", "a".repeat(32))),
            "2026-06-08T00:00:00Z",
        )
    }

    fn build_input(rec: LaunchMaterializationRecord) -> PrepareSessionCommandBuildInput {
        PrepareSessionCommandBuildInput {
            materialization: rec,
            runner_ref: RUNNER_REF.to_owned(),
            command_request_id: "cmdreq_1".to_owned(),
        }
    }

    // ── 1/2. Accepts a valid materialization ─────────────────────────────────

    #[test]
    fn prepare_session_builder_accepts_valid_materialization() {
        let rec = materialization("ses_a", sample_digests());
        let out = build_prepare_session_command(build_input(rec.clone())).unwrap();
        match &out.payload {
            RunnerCommandPayload::PrepareSession {
                session,
                materialization_plan,
            } => {
                assert_eq!(session, "ses_a");
                assert_eq!(
                    materialization_plan.as_str(),
                    format!(
                        "/sessions/{}/materialization",
                        rec.capsule_instance_key.as_str()
                    )
                );
            }
            other => panic!("expected PrepareSession, got {other:?}"),
        }
        assert_eq!(out.session, "ses_a");
        assert_eq!(out.command_request_id, "cmdreq_1");
    }

    // ── 3. Rejects missing runner ref ────────────────────────────────────────

    #[test]
    fn prepare_session_builder_rejects_missing_runner_ref() {
        let rec = materialization("ses_b", sample_digests());
        let mut input = build_input(rec);
        input.runner_ref = String::new();
        let err = build_prepare_session_command(input).unwrap_err();
        assert!(matches!(
            err,
            PrepareSessionCommandBuildError::RunnerRefMissing
        ));
    }

    #[test]
    fn prepare_session_builder_rejects_missing_command_request_id() {
        let rec = materialization("ses_b2", sample_digests());
        let mut input = build_input(rec);
        input.command_request_id = String::new();
        let err = build_prepare_session_command(input).unwrap_err();
        assert!(matches!(
            err,
            PrepareSessionCommandBuildError::CommandRequestIdMissing
        ));
    }

    #[test]
    fn prepare_session_builder_rejects_runner_ref_mismatch() {
        let rec = materialization("ses_b3", sample_digests());
        let mut input = build_input(rec);
        input.runner_ref = "/runners/run_other".into();
        let err = build_prepare_session_command(input).unwrap_err();
        assert!(
            matches!(
                err,
                PrepareSessionCommandBuildError::RunnerRefMismatch { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Rejects a materialization whose runner was not pinned by placement ───

    #[test]
    fn prepare_session_builder_rejects_materialization_without_selected_runner() {
        // Both runner ref and class absent: pre-placement record. validate()
        // accepts both-absent as consistent, so the builder's stronger pinned
        // check is what must fire.
        let mut rec = materialization("ses_unpinned", sample_digests());
        rec.selected_runner_ref = None;
        rec.selected_runner_class = None;
        assert!(rec.validate().is_ok(), "both-absent is structurally valid");

        let err = build_prepare_session_command(build_input(rec)).unwrap_err();
        assert!(
            matches!(
                err,
                PrepareSessionCommandBuildError::MaterializationRunnerNotPinned
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn prepare_session_builder_rejects_materialization_without_selected_runner_class() {
        // Ref present, class absent: a half-pinned record. validate() rejects this
        // as RunnerSelectionInconsistent before the pinned check.
        let mut rec = materialization("ses_noclass", sample_digests());
        rec.selected_runner_class = None;

        let err = build_prepare_session_command(build_input(rec)).unwrap_err();
        assert!(
            matches!(
                err,
                PrepareSessionCommandBuildError::InvalidMaterialization(
                    MaterializationRecordInvalidReason::RunnerSelectionInconsistent
                )
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn prepare_session_builder_rejects_materialization_without_selected_runner_ref() {
        // Class present, ref absent: the other half-pinned shape, also rejected by
        // validate() as RunnerSelectionInconsistent.
        let mut rec = materialization("ses_noref", sample_digests());
        rec.selected_runner_ref = None;

        let err = build_prepare_session_command(build_input(rec)).unwrap_err();
        assert!(
            matches!(
                err,
                PrepareSessionCommandBuildError::InvalidMaterialization(
                    MaterializationRecordInvalidReason::RunnerSelectionInconsistent
                )
            ),
            "got {err:?}"
        );
    }

    // ── 4. Rejects invalid projection digest (via materialization validation) ─

    #[test]
    fn prepare_session_builder_rejects_invalid_projection_digest() {
        // A digest that is not a blake3:<64 hex> content hash → validation fails.
        let bad = vec![projection_digest("secret", "/secrets/sec_db", "hunter2")];
        let rec = materialization("ses_c", bad);
        let err = build_prepare_session_command(build_input(rec)).unwrap_err();
        assert!(
            matches!(
                err,
                PrepareSessionCommandBuildError::InvalidMaterialization(
                    MaterializationRecordInvalidReason::ProjectionDigestInvalid { .. }
                )
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn prepare_session_builder_rejects_structurally_invalid_materialization() {
        // Empty graph snapshot hash → InvalidMaterialization.
        let mut rec = materialization("ses_c2", sample_digests());
        rec.requirement_graph_snapshot_hash = String::new();
        let err = build_prepare_session_command(build_input(rec)).unwrap_err();
        assert!(matches!(
            err,
            PrepareSessionCommandBuildError::InvalidMaterialization(
                MaterializationRecordInvalidReason::RequirementGraphSnapshotHashEmpty
            )
        ));
    }

    // ── 5/6. Preserves identity refs (incl. distinct graph/snapshot hashes) ──

    #[test]
    fn prepare_session_payload_preserves_capsule_instance_key() {
        let rec = materialization("ses_d", sample_digests());
        let out = build_prepare_session_command(build_input(rec.clone())).unwrap();
        assert_eq!(out.capsule_instance_key, rec.capsule_instance_key);
        // And the plan ref embeds it.
        assert!(
            out.materialization_plan
                .as_str()
                .contains(rec.capsule_instance_key.as_str())
        );
    }

    #[test]
    fn prepare_session_payload_preserves_execution_id() {
        let rec = materialization("ses_e", sample_digests());
        let out = build_prepare_session_command(build_input(rec.clone())).unwrap();
        assert_eq!(out.execution_id, rec.execution_id);
    }

    #[test]
    fn prepare_session_payload_preserves_graph_hash_and_snapshot_hash() {
        let rec = materialization("ses_f", sample_digests());
        let out = build_prepare_session_command(build_input(rec.clone())).unwrap();
        assert_eq!(out.requirement_graph_hash, rec.requirement_graph_hash);
        assert_eq!(
            out.requirement_graph_snapshot_hash,
            rec.requirement_graph_snapshot_hash
        );
        // The two are genuinely distinct (the #596 invariant is preserved).
        assert_ne!(
            out.requirement_graph_hash,
            out.requirement_graph_snapshot_hash
        );
    }

    // ── 5. No secret values in the payload ───────────────────────────────────

    #[test]
    fn prepare_session_payload_does_not_store_secret_values() {
        let rec = materialization("ses_g", sample_digests());
        let out = build_prepare_session_command(build_input(rec)).unwrap();
        let json = serde_json::to_string(&out.payload).unwrap();
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(
                !json.contains(forbidden),
                "PrepareSession payload must not carry a raw secret value ({forbidden:?})"
            );
        }
        // The payload is reference-only: session ref + plan ref, nothing else.
        assert!(json.contains("prepare_session"));
        assert!(json.contains("materialization_plan"));
    }

    // ── 7. Observed diagnostics excluded; deterministic for same inputs ──────

    #[test]
    fn prepare_session_payload_excludes_observed_diagnostics() {
        // Two records identical in stable inputs but differing only in the
        // metadata-only materialized_at timestamp produce the same payload.
        let mut a = materialization("ses_h", sample_digests());
        a.materialized_at = "2026-06-08T00:00:00Z".into();
        let mut b = materialization("ses_h", sample_digests());
        b.materialized_at = "2026-12-31T23:59:59Z".into();

        let out_a = build_prepare_session_command(build_input(a)).unwrap();
        let out_b = build_prepare_session_command(build_input(b)).unwrap();
        assert_eq!(
            out_a.payload, out_b.payload,
            "materialized_at (metadata) must not change the command payload"
        );
        // No observed/runtime field names appear in the serialized payload.
        let json = serde_json::to_string(&out_a.payload).unwrap();
        for forbidden in [
            "observed_status",
            "readiness",
            "dynamic_port",
            "process_id",
            "container_id",
            "log_cursor",
            "live_route",
        ] {
            assert!(
                !json.contains(forbidden),
                "payload must not contain observed/runtime field {forbidden:?}"
            );
        }
    }

    #[test]
    fn prepare_session_payload_is_stable_for_same_inputs() {
        let a =
            build_prepare_session_command(build_input(materialization("ses_i", sample_digests())))
                .unwrap();
        let b = build_prepare_session_command(build_input(materialization(
            "ses_i",
            // same digests in a different order — identity is order-independent
            // on the record, so the command is stable.
            sample_digests().into_iter().rev().collect(),
        )))
        .unwrap();
        assert_eq!(a.payload, b.payload);
        assert_eq!(a.capsule_instance_key, b.capsule_instance_key);
    }

    // ── 8. Changing a projection digest changes payload content ──────────────

    #[test]
    fn prepare_session_payload_changes_when_projection_digest_changes() {
        // The materialization identity (capsule_instance_key) folds in the
        // projection digests (#581 wave 5B). Build two records that differ ONLY
        // in a projection digest but otherwise share stable inputs, by routing
        // through the wave-5B builder so the execution id is re-derived.
        use crate::foundation::install_lifecycle::launch_materialization_builder::{
            LaunchMaterializationBuildInput, build_launch_materialization,
        };
        use crate::foundation::install_lifecycle::launch_template::{
            LaunchTemplate, LaunchTemplateKey, RunnerCompatibilityClass,
        };
        use crate::foundation::install_lifecycle::records::RequirementGraphSnapshotHash;

        fn template() -> LaunchTemplate {
            let key = LaunchTemplateKey {
                install_revision_id: InstallRevisionId::new("rev_pcmd"),
                profile_hash: "blake3:prof".into(),
                requirement_graph_snapshot_hash: RequirementGraphSnapshotHash::parse(
                    "blake3:graphsnap",
                )
                .unwrap(),
                binding_set_hash: "blake3:bind".into(),
                network_policy_hash: "blake3:net".into(),
                capability_policy_hash: "blake3:cap".into(),
                state_contract_hash: "blake3:state".into(),
                runner_compatibility_class: RunnerCompatibilityClass::new(
                    "managed_runner/linux-x86_64",
                ),
                ready_state_runner_class: None,
            };
            LaunchTemplate::new(
                "ltmpl_pcmd",
                key,
                "/profile",
                "/artifact",
                "snap",
                "bset",
                "blake3:fs",
                "blake3:net",
                "blake3:cap",
            )
            .unwrap()
        }

        fn materialize(digests: Vec<ProjectionDigest>) -> LaunchMaterializationRecord {
            let ipk = derive_install_profile_key(
                &InstalledAppId::new("app_pcmd"),
                &ProfileId::new("default"),
            );
            build_launch_materialization(LaunchMaterializationBuildInput {
                launch_template: template(),
                install_profile_key: ipk,
                requirement_graph_hash: digest("c0a7e7"),
                session_ref: "ses_change".into(),
                selected_runner_class: RunnerClass::ManagedRunner,
                selected_runner_ref: RUNNER_REF.into(),
                projection_digests: digests,
                materialized_at: "2026-06-08T00:00:00Z".into(),
            })
            .unwrap()
            .record
        }

        let base =
            build_prepare_session_command(build_input(materialize(sample_digests()))).unwrap();
        let mut changed_digests = sample_digests();
        changed_digests[0].digest = digest("d1ffe7");
        let changed =
            build_prepare_session_command(build_input(materialize(changed_digests))).unwrap();

        assert_ne!(
            base.payload, changed.payload,
            "a changed projection digest must change the command payload (via the plan ref)"
        );
        assert_ne!(base.capsule_instance_key, changed.capsule_instance_key);
    }

    // ── 9/StartSession transition: into_start_command preserves identity refs ─

    #[test]
    fn prepare_outcome_into_start_command_preserves_identity_refs() {
        let rec = materialization("ses_start", sample_digests());
        let prepared = build_prepare_session_command(build_input(rec)).unwrap();
        let session = prepared.session.clone();

        // The runner reports prepare success, fixing the launch envelope.
        let outcome = PrepareSessionOutcome {
            session: session.clone(),
            projection_digests: sample_digests(),
            launch_envelope_ready: true,
            launch_envelope_ref: Some(LaunchEnvelopeRef::new("env_fixed_1")),
        };
        let start = outcome
            .into_start_command()
            .expect("ready envelope → start");
        match start {
            RunnerCommandPayload::StartSession {
                session: start_session,
                launch_envelope_ref,
            } => {
                // The session ref is preserved across prepare → start.
                assert_eq!(start_session, session);
                assert_eq!(launch_envelope_ref.as_str(), "env_fixed_1");
            }
            other => panic!("expected StartSession, got {other:?}"),
        }
    }

    #[test]
    fn prepare_outcome_into_start_command_does_not_invent_observed_facts() {
        // A not-ready outcome must NOT yield a start command — start is never
        // issued before prepare fixes the envelope, and the transition invents no
        // readiness/observed fact of its own.
        let not_ready = PrepareSessionOutcome {
            session: "ses_notready".into(),
            projection_digests: sample_digests(),
            launch_envelope_ready: false,
            launch_envelope_ref: None,
        };
        assert!(not_ready.into_start_command().is_none());

        // A ready outcome yields a StartSession that carries only the session +
        // the fixed envelope ref — no observed diagnostics.
        let ready = PrepareSessionOutcome {
            session: "ses_ready".into(),
            projection_digests: sample_digests(),
            launch_envelope_ready: true,
            launch_envelope_ref: Some(LaunchEnvelopeRef::new("env_2")),
        };
        let start = ready.into_start_command().unwrap();
        let json = serde_json::to_string(&start).unwrap();
        for forbidden in [
            "observed_status",
            "readiness",
            "dynamic_port",
            "process_id",
            "container_id",
            "log_cursor",
            "live_route",
        ] {
            assert!(
                !json.contains(forbidden),
                "StartSession must not invent observed fact {forbidden:?}"
            );
        }
    }
}
