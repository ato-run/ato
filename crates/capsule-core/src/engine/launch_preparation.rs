//! Launch-preparation planning facade (#581 wave 5D).
//!
//! This composes the existing #581 pieces into one safe, typed planning
//! boundary that answers a single question:
//!
//! > Given an installed `(app, profile, revision)` and a selected runner
//! > context, can Ato *prepare* a launch from persisted install outputs?
//!
//! It is a **composition boundary only** — it does not launch, dispatch a runner
//! command, implement a command queue / lease, call managed cloud provisioning,
//! or touch ato-api / PWA / Store. It wires together, in order:
//!
//! ```text
//! 1. ValidatedInstallReusableInputs::load        (#592)  — load + validate inputs
//! 2. evaluate_persisted_launch_template_reuse     (#595)  — reuse + volatile gate
//! 3. build_launch_materialization                 (#596)  — per-session record
//! 4. store.write_launch_materialization_record    (#596)  — persist the plan
//! 5. build_prepare_session_command                (#598)  — typed PrepareSession
//! ```
//!
//! and returns either a [`LaunchPreparationDecision::Prepared`] plan (carrying
//! the reused template, the materialization record, the `PrepareSession`
//! command, and the identity refs a future dispatch / API layer needs) or a
//! [`LaunchPreparationDecision::NotPrepared`] with typed blockers.
//!
//! # It lives in `engine/`, not `foundation/`
//!
//! The facade needs [`crate::engine::runner_command_builder`], and the layer
//! rule is `engine -> foundation` (foundation must not import engine). So the
//! facade sits in `engine/`, one level above the records it composes.
//!
//! # Identity discipline
//!
//! The plan preserves the #588/#596 distinction between the requirement-graph
//! **content** hash (`requirement_graph_hash`) and the **snapshot** identity
//! (`requirement_graph_snapshot_hash`) — they are never collapsed. No raw secret
//! value, dynamic port, pid, container id, live route, log cursor, readiness /
//! observed status, or timestamp-as-identity enters the plan: the plan carries
//! only references, content hashes, and the reference-only `PrepareSession`
//! payload. Volatile revalidation gates the decision (a failed or skipped check
//! blocks, never silently succeeds, via the #595 reuse evaluator) but is not a
//! plan identity input.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::foundation::install_lifecycle::ids::{
    CapsuleInstanceKey, ExecutionId, InstallRevisionId, InstalledAppId, ProfileId,
};
use crate::foundation::install_lifecycle::launch_inputs::{
    InstallReusableInputValidationError, ValidatedInstallReusableInputs,
};
use crate::foundation::install_lifecycle::launch_materialization_builder::{
    LaunchMaterializationBuildError, LaunchMaterializationBuildInput, build_launch_materialization,
};
use crate::foundation::install_lifecycle::launch_reuse::VolatileRevalidation;
use crate::foundation::install_lifecycle::launch_template::{
    LaunchTemplate, RunnerClass, RunnerCompatibilityClass,
};
use crate::foundation::install_lifecycle::launch_template_reuse::{
    LaunchTemplateReuseInput, PersistedLaunchTemplateReuseBlocker,
    PersistedLaunchTemplateReuseDecision, evaluate_persisted_launch_template_reuse,
};
use crate::foundation::install_lifecycle::materialization::{
    LaunchMaterializationRecord, ProjectionDigest,
};
use crate::foundation::install_lifecycle::store::InstallInstanceStore;

use super::runner_command::RunnerCommandPayload;
use super::runner_command_builder::{
    PrepareSessionCommandBuildError, PrepareSessionCommandBuildInput, build_prepare_session_command,
};

/// Inputs to [`prepare_launch`] (#581 wave 5D).
///
/// Mirrors the stable install-time inputs the reuse path keys on, plus the
/// launch-time facts (selected runner, session ref, projection digests) the
/// materialization pins. `volatile_revalidation` is the gate, not an identity
/// input; `materialized_at` is metadata only.
pub struct LaunchPreparationInput {
    pub app: InstalledAppId,
    pub profile: ProfileId,
    pub revision: InstallRevisionId,

    /// Hash of the launch profile (`blake3:<hex>`).
    pub profile_hash: String,
    /// Hash of the resolved network policy (`blake3:<hex>`).
    pub network_policy_hash: String,
    /// Hash of the resolved capability policy (`blake3:<hex>`).
    pub capability_policy_hash: String,
    /// The coarse compatibility class the persisted template was built for.
    pub runner_compatibility_class: RunnerCompatibilityClass,

    /// The concrete runner class chosen by placement (must match the template's
    /// compatibility class).
    pub selected_runner_class: RunnerClass,
    /// A reference to the selected runner instance (a control-plane ref).
    pub selected_runner_ref: String,

    /// Control-plane-global session reference (not a runner-local pid).
    pub session_ref: String,
    /// A stable command-request id (idempotency / correlation key for the later
    /// dispatch layer).
    pub command_request_id: String,

    /// Already-collected volatile revalidation outcomes (#595). This facade does
    /// no real probing; callers supply typed outcomes.
    pub volatile_revalidation: VolatileRevalidation,
    /// Projection digests captured at prepare time (#596). Each must be a
    /// `blake3:<64 hex>` content digest, never a raw value.
    pub projection_digests: Vec<ProjectionDigest>,
    /// RFC 3339 timestamp the materialization is frozen. Metadata only.
    pub materialized_at: String,
}

/// The outcome of a launch-preparation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPreparationDecision {
    /// A launch can be prepared: the plan carries everything a future dispatch /
    /// API layer needs (but nothing has been dispatched or launched).
    Prepared(LaunchPreparationPlan),
    /// A launch cannot be prepared; `blockers` lists why (typically one, in the
    /// order the pipeline evaluates).
    NotPrepared {
        blockers: Vec<LaunchPreparationBlocker>,
    },
}

impl LaunchPreparationDecision {
    /// True only for [`Self::Prepared`].
    pub fn is_prepared(&self) -> bool {
        matches!(self, Self::Prepared(_))
    }

    fn not_prepared(blocker: LaunchPreparationBlocker) -> Self {
        Self::NotPrepared {
            blockers: vec![blocker],
        }
    }
}

/// A prepared launch plan (#581 wave 5D).
///
/// Session-scoped and reference-only: it composes the reused template, the
/// per-session materialization record, and the `PrepareSession` command, plus
/// the identity refs the dispatch / API layer correlates on. `Serialize` /
/// `Deserialize` so a future ato-api boundary can carry it; no field is a raw
/// secret or observed/runtime fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPreparationPlan {
    /// The reused, persisted launch template.
    pub launch_template: LaunchTemplate,
    /// The frozen per-session materialization record (also persisted).
    pub materialization: LaunchMaterializationRecord,
    /// The reference-only `PrepareSession` command (never dispatched here).
    pub prepare_command: RunnerCommandPayload,

    // ── Identity refs preserved across the pipeline ──────────────────────────
    pub install_revision_id: InstallRevisionId,
    pub capsule_instance_key: CapsuleInstanceKey,
    pub execution_id: ExecutionId,
    /// Requirement graph **content** hash (`graph_hash`) — never the snapshot.
    pub requirement_graph_hash: String,
    /// Requirement graph **snapshot** identity (graph + profile defaults +
    /// completeness) — kept distinct from the content hash (#588/#596).
    pub requirement_graph_snapshot_hash: String,
    /// The reused launch template's key hash.
    pub launch_template_key_hash: String,
    /// The command-request id, echoed for the dispatch layer.
    pub command_request_id: String,
    pub selected_runner_class: RunnerClass,
    pub selected_runner_ref: String,
}

/// A typed reason a launch could not be prepared (#581 wave 5D).
///
/// Each variant wraps the typed error of the pipeline stage that blocked, in
/// evaluation order. Never an in-band `"unknown"` / `"unset"` sentinel; carries
/// only content hashes / typed reasons, never a secret or observed value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchPreparationBlocker {
    /// The reusable install inputs could not be loaded / validated.
    #[error("reusable install inputs are invalid: {0}")]
    ReusableInputsInvalid(#[source] InstallReusableInputValidationError),
    /// No persisted launch template could be reused (not ready, missing,
    /// integrity/volatile-revalidation failure, …). Carries the #595 blocker.
    #[error("launch template is not reusable: {0}")]
    LaunchTemplateNotReusable(#[source] PersistedLaunchTemplateReuseBlocker),
    /// The materialization record could not be built (runner-class mismatch,
    /// invalid projection digest, missing runner ref, …). Carries the #596 error.
    #[error("launch materialization failed: {0}")]
    LaunchMaterializationFailed(#[source] LaunchMaterializationBuildError),
    /// The materialization record could not be persisted. The `PrepareSession`
    /// command references the persisted plan, so a non-persisted plan is not
    /// preparable.
    #[error("launch materialization could not be persisted: {detail}")]
    MaterializationPersistFailed { detail: String },
    /// The `PrepareSession` command could not be built (unpinned runner, missing
    /// command id, …). Carries the #598 error.
    #[error("prepare-session command failed: {0}")]
    PrepareSessionCommandFailed(#[source] PrepareSessionCommandBuildError),
}

/// Compose a [`LaunchPreparationPlan`] from persisted install outputs (#581
/// wave 5D).
///
/// Runs the load → reuse → materialize → persist → prepare pipeline in order,
/// returning the first stage's typed blocker on failure. Does not launch,
/// dispatch, queue, lease, or probe anything.
pub fn prepare_launch(
    store: &InstallInstanceStore,
    input: LaunchPreparationInput,
) -> LaunchPreparationDecision {
    use LaunchPreparationBlocker as B;

    let LaunchPreparationInput {
        app,
        profile,
        revision,
        profile_hash,
        network_policy_hash,
        capability_policy_hash,
        runner_compatibility_class,
        selected_runner_class,
        selected_runner_ref,
        session_ref,
        command_request_id,
        volatile_revalidation,
        projection_digests,
        materialized_at,
    } = input;

    // 1. Load + validate the reusable install inputs. We need the requirement
    //    graph **content** hash and the install-profile key for materialization,
    //    neither of which the reuse decision surfaces. A genuine load/validation
    //    failure is a distinct blocker from a successful-but-not-reusable load.
    let inputs = match ValidatedInstallReusableInputs::load(store, &app, &profile, &revision) {
        Ok(v) => v,
        Err(e) => return LaunchPreparationDecision::not_prepared(B::ReusableInputsInvalid(e)),
    };
    let requirement_graph_hash = inputs.requirement_graph.graph_hash.clone();
    let install_profile_key = inputs.install_revision.install_profile_key.clone();
    // The session materialization is persisted under `instances/<app>/sessions/`,
    // so keep the app id (the reuse input below takes ownership of `app`).
    let persist_app = app.clone();

    // 2. Evaluate persisted launch-template reuse. This re-runs load + readiness,
    //    reads the persisted template, validates its integrity, and applies the
    //    volatile-revalidation gate (a failed OR skipped check blocks here, never
    //    a silent success). Standard install is NotReady → NotReusable.
    let reuse = evaluate_persisted_launch_template_reuse(
        store,
        LaunchTemplateReuseInput {
            app,
            profile,
            revision: revision.clone(),
            profile_hash,
            network_policy_hash,
            capability_policy_hash,
            runner_compatibility_class,
            volatile_revalidation,
        },
    );
    let (launch_template, launch_template_key_hash) = match reuse {
        PersistedLaunchTemplateReuseDecision::Reusable {
            launch_template,
            key_hash,
            ..
        } => (launch_template, key_hash),
        PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => {
            return LaunchPreparationDecision::NotPrepared {
                blockers: reasons
                    .into_iter()
                    .map(B::LaunchTemplateNotReusable)
                    .collect(),
            };
        }
    };

    // 3. Build the per-session materialization from the reused template. This
    //    re-checks template integrity, runner-class compatibility, validates the
    //    projection digests, and pins the selected runner.
    let materialization = match build_launch_materialization(LaunchMaterializationBuildInput {
        launch_template: launch_template.clone(),
        install_profile_key,
        requirement_graph_hash: requirement_graph_hash.clone(),
        session_ref,
        selected_runner_class,
        selected_runner_ref: selected_runner_ref.clone(),
        projection_digests,
        materialized_at,
    }) {
        Ok(out) => out,
        Err(e) => {
            return LaunchPreparationDecision::not_prepared(B::LaunchMaterializationFailed(e));
        }
    };
    let record = materialization.record;
    let execution_id = materialization.execution_id;

    // 4. Persist the materialization. The PrepareSession command references the
    //    plan by `sessions/<capsule_instance_key>/materialization`, so it must be
    //    on disk for the command to be dereferenceable.
    if let Err(e) = store.write_launch_materialization_record(&persist_app, &record) {
        return LaunchPreparationDecision::not_prepared(B::MaterializationPersistFailed {
            detail: format!("{e:#}"),
        });
    }

    // 5. Build the reference-only PrepareSession command from the materialization.
    let prepared = match build_prepare_session_command(PrepareSessionCommandBuildInput {
        materialization: record.clone(),
        runner_ref: selected_runner_ref.clone(),
        command_request_id: command_request_id.clone(),
    }) {
        Ok(out) => out,
        Err(e) => {
            return LaunchPreparationDecision::not_prepared(B::PrepareSessionCommandFailed(e));
        }
    };

    // 6. Assemble the prepared plan, preserving every identity ref distinctly.
    LaunchPreparationDecision::Prepared(LaunchPreparationPlan {
        requirement_graph_snapshot_hash: record.requirement_graph_snapshot_hash.clone(),
        capsule_instance_key: record.capsule_instance_key.clone(),
        materialization: record,
        prepare_command: prepared.payload,
        launch_template,
        install_revision_id: revision,
        execution_id,
        requirement_graph_hash,
        launch_template_key_hash,
        command_request_id,
        selected_runner_class,
        selected_runner_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::finalizer::{
        FinalizerInput, FinalizerOutput, InstallBuildFacts, InstallRevisionFinalizer,
    };
    use crate::foundation::install_lifecycle::ids::ArtifactBuildId;
    use crate::foundation::install_lifecycle::launch_reuse::{
        RevalidationFailure, RevalidationFailureKind, RevalidationOutcome,
    };
    use crate::foundation::install_lifecycle::launch_template::{
        BindingAssignmentSet, BindingAssignmentSource, CompatibilityIndex,
        CompatibilityIndexCompleteness, RequirementBinding, RequirementBindingKind,
    };
    use crate::foundation::install_lifecycle::launch_template_builder::{
        LaunchTemplateBuildInput, build_launch_template,
    };
    use crate::foundation::install_lifecycle::records::RequirementGraphCompleteness;
    use crate::foundation::install_lifecycle::store::{AppRecord, LaunchProfile};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── Harness (mirrors #594–#598 test helpers) ─────────────────────────────

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_launch_prep_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "launch-prep".into(),
                capsule_handle: "test/launch-prep".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        (dir, store, app, profile_id)
    }

    fn valid_build_id(suffix: &str) -> ArtifactBuildId {
        let hex: String = suffix.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let padded = format!("{:0<64}", hex);
        ArtifactBuildId::new(format!("build_{}", &padded[..64]))
    }

    fn make_output_dir(base: &std::path::Path) -> PathBuf {
        let out = base.join("output");
        fs::create_dir_all(&out).unwrap();
        let mut f = fs::File::create(out.join("index.js")).unwrap();
        f.write_all(b"console.log('hello')").unwrap();
        out
    }

    fn sample_facts() -> InstallBuildFacts {
        InstallBuildFacts {
            capsule_ref: Some("acme/pgweb@1.2.3".into()),
            source_provenance_ref: Some("blake3:cafef00d".into()),
            output_content_hash: Some("blake3:cafef00d".into()),
            dependency_output_hash: None,
            platform: Some("linux/x86_64".into()),
            requirement_graph: None,
            state_contracts: vec![],
        }
    }

    fn finalize_standard(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        out_base: &std::path::Path,
        build_suffix: &str,
    ) -> FinalizerOutput {
        InstallRevisionFinalizer::new(store)
            .finalize(FinalizerInput {
                installed_app_id: app.clone(),
                profile_id: profile.clone(),
                artifact_build_id: valid_build_id(build_suffix),
                output_dir: make_output_dir(out_base),
                artifact_manifest_json: None,
                source_provenance_json: None,
                oci_lock_json: None,
                build_facts: Some(sample_facts()),
            })
            .unwrap()
    }

    fn one_binding() -> RequirementBinding {
        RequirementBinding {
            requirement_id: "req-runtime".into(),
            binding_kind: RequirementBindingKind::Resource,
            resolved_resource_ref: Some("/ns/example/resource".into()),
            resolved_resource_refs: vec![],
            affects_execution_identity: false,
        }
    }

    fn complete_compat_index() -> CompatibilityIndex {
        CompatibilityIndex::new(
            "cidx:ok",
            vec![RunnerClass::ManagedRunner],
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_completeness(CompatibilityIndexCompleteness::Complete)
        .unwrap()
    }

    fn make_ready(v: &mut ValidatedInstallReusableInputs) {
        v.requirement_graph = v
            .requirement_graph
            .clone()
            .with_completeness(RequirementGraphCompleteness::Complete)
            .unwrap();
        v.compatibility_index = complete_compat_index();
        v.binding_assignment_set.assignments.push(one_binding());
    }

    /// Persist a `(app, profile, revision)` whose on-disk records load as Ready,
    /// plus the matching reusable launch template (mirrors the #595 helper).
    fn persist_ready_on_disk(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        out_base: &std::path::Path,
        build_suffix: &str,
    ) -> InstallRevisionId {
        let out = finalize_standard(store, app, profile, out_base, build_suffix);
        let rev = out.install_revision_id.clone();

        let mut v = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        make_ready(&mut v);

        let bset = BindingAssignmentSet::new(
            "bset_ready",
            v.install_revision.install_profile_key.clone(),
            v.requirement_graph.graph_hash.clone(),
            vec![one_binding()],
            BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();
        v.binding_assignment_set = bset.clone();

        let mut receipt = v.install_receipt.clone();
        receipt.binding_set_hash = Some(bset.binding_set_hash.clone());
        receipt.compatibility_precheck_hash = Some(v.compatibility_index.precheck_hash.clone());
        v.install_receipt = receipt.clone();

        store
            .write_requirement_graph_snapshot(app, profile, &rev, &v.requirement_graph)
            .unwrap();
        store
            .write_binding_assignment_set(app, profile, &rev, &bset)
            .unwrap();
        store
            .write_compatibility_index(app, profile, &rev, &v.compatibility_index)
            .unwrap();
        store
            .write_install_receipt(app, profile, &rev, &receipt)
            .unwrap();

        let mut revision = store.read_install_revision(app, profile, &rev).unwrap();
        revision.requirement_graph = v.requirement_graph.clone();
        revision.binding_assignment_set = Some(bset);
        revision.compatibility_index = Some(v.compatibility_index.clone());
        revision.install_receipt = receipt;
        store
            .write_install_revision(app, profile, &revision)
            .unwrap();

        let reloaded = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        assert!(reloaded.launch_template_readiness().is_ready());

        let built = build_launch_template(LaunchTemplateBuildInput {
            template_id: "ltmpl_persisted".into(),
            reusable_inputs: reloaded,
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        })
        .expect("ready inputs build a template");
        store
            .write_launch_template(app, profile, &rev, &built.launch_template)
            .unwrap();
        rev
    }

    // ── digest / input helpers ───────────────────────────────────────────────

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

    fn all_ok() -> VolatileRevalidation {
        VolatileRevalidation {
            runner_health: RevalidationOutcome::Passed,
            runner_capability: RevalidationOutcome::Passed,
            consent: RevalidationOutcome::Passed,
            auth: RevalidationOutcome::Passed,
            secret_refs: RevalidationOutcome::Passed,
            storage_credentials: RevalidationOutcome::Passed,
            network_policy: RevalidationOutcome::Passed,
            state_lock: RevalidationOutcome::Passed,
        }
    }

    const RUNNER_REF: &str = "/runners/run_managed_1";

    fn prep_input(
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        revalidation: VolatileRevalidation,
        digests: Vec<ProjectionDigest>,
    ) -> LaunchPreparationInput {
        LaunchPreparationInput {
            app: app.clone(),
            profile: profile.clone(),
            revision: rev.clone(),
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
            selected_runner_class: RunnerClass::ManagedRunner,
            selected_runner_ref: RUNNER_REF.into(),
            session_ref: "ses_prep".into(),
            command_request_id: "cmdreq_1".into(),
            volatile_revalidation: revalidation,
            projection_digests: digests,
            materialized_at: "2026-06-09T00:00:00Z".into(),
        }
    }

    fn blockers(decision: &LaunchPreparationDecision) -> &[LaunchPreparationBlocker] {
        match decision {
            LaunchPreparationDecision::NotPrepared { blockers } => blockers,
            LaunchPreparationDecision::Prepared(_) => &[],
        }
    }

    // ── 1/2. Standard install is not prepared ────────────────────────────────

    #[test]
    fn launch_preparation_standard_install_is_not_prepared() {
        let (dir, store, app, profile) = setup();
        // Standard install: finalized but NotReady, no persisted template.
        let out = finalize_standard(&store, &app, &profile, dir.path(), "ea01");
        let rev = out.install_revision_id.clone();

        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        assert!(
            !decision.is_prepared(),
            "standard install must not be prepared"
        );
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchTemplateNotReusable(
                    PersistedLaunchTemplateReuseBlocker::InputsNotReady { .. }
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    // ── 3. Missing persisted template ────────────────────────────────────────

    #[test]
    fn launch_preparation_missing_template_is_not_prepared() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea02");
        // Delete the persisted template, keeping Ready inputs on disk.
        let templates = store.read_launch_templates(&app, &profile, &rev).unwrap();
        let key_hash = templates[0].key.key_hash().unwrap();
        fs::remove_file(store.revision_launch_template_path(&app, &profile, &rev, &key_hash))
            .unwrap();

        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchTemplateNotReusable(
                    PersistedLaunchTemplateReuseBlocker::TemplateMissing { .. }
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    // ── 4. Ready template builds a Prepared plan ─────────────────────────────

    #[test]
    fn launch_preparation_ready_template_builds_prepared_plan() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea03");

        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        let plan = match decision {
            LaunchPreparationDecision::Prepared(p) => p,
            LaunchPreparationDecision::NotPrepared { blockers } => {
                panic!("expected Prepared, got {blockers:?}")
            }
        };
        assert_eq!(plan.install_revision_id, rev);
        assert_eq!(plan.command_request_id, "cmdreq_1");
        assert_eq!(plan.selected_runner_class, RunnerClass::ManagedRunner);
        assert_eq!(plan.selected_runner_ref, RUNNER_REF);
        // The materialization was persisted under the session's instance key.
        let persisted = store
            .read_launch_materialization_record(&app, &plan.capsule_instance_key)
            .expect("materialization persisted");
        assert_eq!(persisted, plan.materialization);
    }

    // ── 5/6. Volatile revalidation gates ─────────────────────────────────────

    #[test]
    fn launch_preparation_failed_revalidation_blocks() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea04");
        let mut reval = all_ok();
        reval.consent = RevalidationOutcome::Failed(RevalidationFailure::new(
            RevalidationFailureKind::ConsentRevoked,
            "user revoked consent",
        ));
        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, reval, sample_digests()),
        );
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchTemplateNotReusable(
                    PersistedLaunchTemplateReuseBlocker::VolatileRevalidationBlocked { .. }
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    #[test]
    fn launch_preparation_skipped_revalidation_blocks() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea05");
        let mut reval = all_ok();
        reval.secret_refs = RevalidationOutcome::Skipped {
            reason: "secret manager probe not implemented".into(),
        };
        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, reval, sample_digests()),
        );
        assert!(
            !decision.is_prepared(),
            "skipped revalidation must block, not silently succeed"
        );
        assert!(blockers(&decision).iter().any(|b| matches!(
            b,
            LaunchPreparationBlocker::LaunchTemplateNotReusable(
                PersistedLaunchTemplateReuseBlocker::VolatileRevalidationBlocked { .. }
            )
        )));
    }

    // ── 7. Invalid projection digest ─────────────────────────────────────────

    #[test]
    fn launch_preparation_invalid_projection_digest_blocks() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea06");
        let bad = vec![projection_digest("secret", "/secrets/sec_db", "hunter2")];
        let decision = prepare_launch(&store, prep_input(&app, &profile, &rev, all_ok(), bad));
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchMaterializationFailed(
                    LaunchMaterializationBuildError::ProjectionDigestInvalid { .. }
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    // ── 8. Runner class mismatch ─────────────────────────────────────────────

    #[test]
    fn launch_preparation_runner_class_mismatch_blocks() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea07");
        let mut input = prep_input(&app, &profile, &rev, all_ok(), sample_digests());
        // The template targets managed_runner; select a desktop_runner.
        input.selected_runner_class = RunnerClass::DesktopRunner;
        let decision = prepare_launch(&store, input);
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchMaterializationFailed(
                    LaunchMaterializationBuildError::RunnerClassMismatch { .. }
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    // ── 9. Missing selected runner ref ───────────────────────────────────────

    #[test]
    fn launch_preparation_missing_runner_ref_blocks() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea08");
        let mut input = prep_input(&app, &profile, &rev, all_ok(), sample_digests());
        input.selected_runner_ref = String::new();
        let decision = prepare_launch(&store, input);
        assert!(
            blockers(&decision).iter().any(|b| matches!(
                b,
                LaunchPreparationBlocker::LaunchMaterializationFailed(
                    LaunchMaterializationBuildError::SelectedRunnerRefEmpty
                )
            )),
            "got {:?}",
            blockers(&decision)
        );
    }

    // ── 10. PrepareSession command only; no dispatch ─────────────────────────

    #[test]
    fn launch_preparation_outputs_prepare_session_only() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea09");
        let plan = match prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        ) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        assert!(
            matches!(
                plan.prepare_command,
                RunnerCommandPayload::PrepareSession { .. }
            ),
            "the plan must carry a PrepareSession command, not Start/Stop"
        );
    }

    // ── 11. Preserves identity refs ──────────────────────────────────────────

    #[test]
    fn launch_preparation_preserves_graph_hash_and_snapshot_hash() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea10");
        // The content graph hash the facade should preserve.
        let inputs = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap();
        let content_hash = inputs.requirement_graph.graph_hash.clone();
        let snapshot_hash = inputs
            .requirement_graph
            .requirement_graph_snapshot_hash
            .clone();

        let plan = match prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        ) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        assert_eq!(plan.requirement_graph_hash, content_hash);
        assert_eq!(plan.requirement_graph_snapshot_hash, snapshot_hash);
        assert_ne!(
            plan.requirement_graph_hash, plan.requirement_graph_snapshot_hash,
            "content hash and snapshot hash must stay distinct (#588/#596)"
        );
        // And the materialization carries the same distinct pair.
        assert_eq!(plan.materialization.requirement_graph_hash, content_hash);
        assert_eq!(
            plan.materialization.requirement_graph_snapshot_hash,
            snapshot_hash
        );
    }

    #[test]
    fn launch_preparation_preserves_capsule_instance_key_and_execution_id() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea11");
        let plan = match prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        ) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        assert_eq!(plan.execution_id, plan.materialization.execution_id);
        assert_eq!(
            plan.capsule_instance_key,
            plan.materialization.capsule_instance_key
        );
        assert!(plan.execution_id.is_valid());
        assert!(plan.capsule_instance_key.as_str().starts_with("cik_"));
    }

    // ── 12. No secret values / observed diagnostics in the plan ──────────────

    #[test]
    fn launch_preparation_does_not_include_secret_values() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea12");
        let plan = match prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        ) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        let json = serde_json::to_string(&plan).unwrap();
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(
                !json.contains(forbidden),
                "plan must never carry a raw secret value ({forbidden:?})"
            );
        }
    }

    #[test]
    fn launch_preparation_excludes_observed_diagnostics() {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ea13");

        // The plan identity must not depend on the metadata-only materialized_at.
        let mut early = prep_input(&app, &profile, &rev, all_ok(), sample_digests());
        early.materialized_at = "2026-06-09T00:00:00Z".into();
        let mut late = prep_input(&app, &profile, &rev, all_ok(), sample_digests());
        late.materialized_at = "2026-12-31T23:59:59Z".into();

        let plan_a = match prepare_launch(&store, early) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        let plan_b = match prepare_launch(&store, late) {
            LaunchPreparationDecision::Prepared(p) => p,
            other => panic!("expected Prepared, got {other:?}"),
        };
        assert_eq!(
            plan_a.execution_id, plan_b.execution_id,
            "materialized_at must not change the plan identity"
        );
        assert_eq!(plan_a.capsule_instance_key, plan_b.capsule_instance_key);
        assert_eq!(plan_a.prepare_command, plan_b.prepare_command);

        // No observed/runtime field names appear in the serialized plan.
        let json = serde_json::to_string(&plan_a).unwrap();
        for forbidden in [
            "observed_status",
            "readiness_status",
            "dynamic_port",
            "process_id",
            "container_id",
            "log_cursor",
            "live_route",
        ] {
            assert!(
                !json.contains(forbidden),
                "plan must not contain observed/runtime field {forbidden:?}"
            );
        }
    }

    // ── 13. Bridge contract (#581 ↔ #593) ────────────────────────────────────
    //
    // These tests cover `engine::launch_preparation_bridge`, the
    // control-plane-facing JSON projection. They reuse this module's harness and
    // are named `launch_preparation_bridge_*` so the `--lib launch_preparation`
    // filter picks them up alongside the plan tests.

    use crate::engine::launch_preparation_bridge::{
        LaunchPreparationBridgeResult, bridge_blocker_code,
    };

    /// A deterministic *prepared* bridge result for fixtures/assertions.
    ///
    /// `build_suffix` is fixed so the derived `rev_`/`exec_`/`cik_` ids are stable
    /// across runs — the golden fixture is byte-stable.
    fn prepared_bridge_result() -> LaunchPreparationBridgeResult {
        let (dir, store, app, profile) = setup();
        let rev = persist_ready_on_disk(&store, &app, &profile, dir.path(), "b41d9e");
        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        assert!(decision.is_prepared(), "fixture must be prepared");
        LaunchPreparationBridgeResult::from_decision(&decision)
    }

    /// A deterministic *not_prepared* bridge result: a standard (NotReady) install
    /// with no persisted launch template.
    fn not_prepared_bridge_result() -> LaunchPreparationBridgeResult {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "b42a01");
        let rev = out.install_revision_id.clone();
        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        assert!(!decision.is_prepared(), "fixture must not be prepared");
        LaunchPreparationBridgeResult::from_decision(&decision)
    }

    fn bridge_fixture_path(name: &str) -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/launch_preparation")
            .join(format!("{name}.json"))
    }

    #[test]
    fn launch_preparation_bridge_prepared_invariants() {
        let result = prepared_bridge_result();
        let plan = match &result {
            LaunchPreparationBridgeResult::Prepared { plan } => plan,
            other => panic!("expected prepared, got {other:?}"),
        };

        // selected_runner_class is managed_runner for the fixture.
        assert_eq!(plan.selected_runner_class, RunnerClass::ManagedRunner);
        // requirement_graph_hash and snapshot_hash are distinct (#588/#596).
        assert_ne!(
            plan.requirement_graph_hash, plan.requirement_graph_snapshot_hash,
            "content hash and snapshot hash must stay distinct"
        );
        assert!(plan.requirement_graph_hash.starts_with("blake3:"));
        assert!(plan.requirement_graph_snapshot_hash.starts_with("blake3:"));
        // prepare_command is PrepareSession only.
        assert!(
            matches!(
                plan.prepare_command,
                crate::engine::runner_command::RunnerCommandPayload::PrepareSession { .. }
            ),
            "bridge plan must carry only PrepareSession, got {:?}",
            plan.prepare_command
        );
        // Flat ids keep their typed shape.
        assert!(plan.install_revision_id.starts_with("rev_"));
        assert!(plan.capsule_instance_key.starts_with("cik_"));
        assert!(plan.execution_id.starts_with("exec_"));

        // No secrets / observed diagnostics in the serialized bridge result.
        let json = serde_json::to_string(&result).unwrap();
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(
                !json.contains(forbidden),
                "bridge result must never carry a raw secret value ({forbidden:?})"
            );
        }
        for forbidden in [
            "observed_status",
            "readiness_status",
            "dynamic_port",
            "process_id",
            "container_id",
            "log_cursor",
            "live_route",
        ] {
            assert!(
                !json.contains(forbidden),
                "bridge result must not contain observed/runtime field {forbidden:?}"
            );
        }
        // The bridge intentionally drops the nested template / materialization
        // records (checked precisely on object keys, not substrings — the
        // PrepareSession ref legitimately ends in ".../materialization").
        let value = serde_json::to_value(&result).unwrap();
        let plan_obj = value
            .get("plan")
            .and_then(|p| p.as_object())
            .expect("prepared result has a plan object");
        assert!(
            !plan_obj.contains_key("launch_template"),
            "bridge plan must not export the nested launch_template record"
        );
        assert!(
            !plan_obj.contains_key("materialization"),
            "bridge plan must not export the nested materialization record"
        );
    }

    #[test]
    fn launch_preparation_bridge_not_prepared_has_stable_codes() {
        let result = not_prepared_bridge_result();
        let blockers = match &result {
            LaunchPreparationBridgeResult::NotPrepared { blockers } => blockers,
            other => panic!("expected not_prepared, got {other:?}"),
        };
        assert!(
            blockers
                .iter()
                .any(|b| b.code == "launch_template_not_reusable"),
            "expected launch_template_not_reusable, got {blockers:?}"
        );
        // No raw secret leaks through the detail text.
        let json = serde_json::to_string(&result).unwrap();
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn launch_preparation_bridge_blocker_codes_are_stable() {
        // Lock the #581 → control-plane vocabulary so a rename is a conscious,
        // contract-breaking change.
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "b43c02");
        let rev = out.install_revision_id.clone();
        let decision = prepare_launch(
            &store,
            prep_input(&app, &profile, &rev, all_ok(), sample_digests()),
        );
        match decision {
            LaunchPreparationDecision::NotPrepared { blockers } => {
                for b in &blockers {
                    assert!(
                        matches!(
                            bridge_blocker_code(b),
                            "reusable_inputs_invalid"
                                | "launch_template_not_reusable"
                                | "launch_materialization_failed"
                                | "prepare_session_command_failed"
                                | "launch_preparation_unavailable"
                        ),
                        "unexpected bridge code for {b:?}"
                    );
                }
            }
            other => panic!("expected not_prepared, got {other:?}"),
        }
    }

    #[test]
    fn launch_preparation_bridge_roundtrips() {
        let result = prepared_bridge_result();
        let json = serde_json::to_string(&result).unwrap();
        let back: LaunchPreparationBridgeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back, "bridge result must survive serde round-trip");
    }

    #[test]
    fn launch_preparation_bridge_prepared_matches_golden() {
        let fresh = serde_json::to_value(prepared_bridge_result()).unwrap();
        let path = bridge_fixture_path("prepared_managed_runner");
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "read golden {} ({e}); regenerate with `cargo test -p capsule-core --lib \
                 regenerate_launch_preparation_bridge_golden_fixtures -- --ignored`",
                path.display()
            )
        });
        let golden: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            fresh,
            golden,
            "prepared bridge JSON drifted from golden {}; regenerate if intended",
            path.display()
        );
    }

    #[test]
    fn launch_preparation_bridge_not_prepared_matches_golden() {
        let fresh = serde_json::to_value(not_prepared_bridge_result()).unwrap();
        let path = bridge_fixture_path("not_prepared_standard_install");
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("read golden {} ({e})", path.display())
        });
        let golden: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            fresh,
            golden,
            "not_prepared bridge JSON drifted from golden {}; regenerate if intended",
            path.display()
        );
    }

    /// Regenerate the committed golden bridge fixtures. Ignored by default; run
    /// explicitly after an intentional contract change:
    /// `cargo test -p capsule-core --lib \
    ///  regenerate_launch_preparation_bridge_golden_fixtures -- --ignored`.
    #[test]
    #[ignore = "writes golden fixtures into the source tree; run explicitly"]
    fn regenerate_launch_preparation_bridge_golden_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/launch_preparation");
        fs::create_dir_all(&dir).unwrap();
        for (name, result) in [
            ("prepared_managed_runner", prepared_bridge_result()),
            ("not_prepared_standard_install", not_prepared_bridge_result()),
        ] {
            let json = serde_json::to_string_pretty(&result).unwrap();
            fs::write(dir.join(format!("{name}.json")), format!("{json}\n")).unwrap();
        }
    }
}
