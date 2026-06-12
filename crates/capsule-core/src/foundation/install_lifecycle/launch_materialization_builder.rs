//! Per-session [`LaunchMaterializationRecord`] builder (#581 wave 5B).
//!
//! This is the boundary between the **reusable, install/revision-scoped**
//! [`LaunchTemplate`] and a **per-session** [`LaunchMaterializationRecord`]:
//!
//! ```text
//! install-time reusable template   (LaunchTemplate — reused across sessions)
//!   -> launch-time materialization  (LaunchMaterializationRecord — frozen per session)
//!   -> later PrepareSession/StartSession command payload (a later wave)
//! ```
//!
//! The builder consumes a *validated, reusable* [`LaunchTemplate`] (the #594/#595
//! output) plus the launch-time facts that pin it to one session — the selected
//! runner, a session reference, and the projection digests captured at prepare
//! time — and freezes them into one materialization record. It does **not**
//! start a process, dispatch a runner command, build a command payload, or run
//! any real runner/secret/storage/network probe. Those are later waves.
//!
//! # Identity discipline
//!
//! The record is session-scoped and must never be reused as a launch template.
//! Its identity is a **preliminary, deterministic materialization
//! `execution_id`** derived purely from stable launch-time inputs — the template
//! key hash + template hash, the (order-independent) projection digests, the
//! selected runner class/ref, and the session reference. The runtime
//! `PrepareSession` step fixes the *final* execution identity in a later wave;
//! deriving it deterministically here is what makes the materialization identity
//! reproducible for the same inputs and sensitive to a changed projection digest
//! (acceptance #8/#9) while staying within the existing record shape.
//!
//! No raw secret value, dynamic port, pid, container id, live route, log cursor,
//! readiness/observed status, or timestamp-as-identity participates in the
//! derived identity. `materialized_at` is carried as metadata only and is never
//! a derivation input. Projection *digests* are allowed precisely because they
//! are `blake3:<hex>` content hashes of the projection shape, never the value
//! (enforced by [`ProjectionDigest::validate`]).

use thiserror::Error;

use super::hashing::canonical_hash;
use super::ids::{ExecutionId, InstallProfileKey};
use super::launch_template::{
    LaunchTemplate, LaunchTemplateIntegrityError, RunnerClass, RunnerCompatibilityClassParseError,
};
use super::materialization::{
    LaunchMaterializationRecord, ProjectionDigest, ProjectionDigestInvalidReason,
};

/// Inputs to [`build_launch_materialization`] (#581 wave 5B).
///
/// Carries the validated reusable template plus the launch-time facts that pin
/// it to one session. Stable identity hashes (profile / requirement-graph /
/// binding-set) are read from the template's key, not re-supplied.
pub struct LaunchMaterializationBuildInput {
    /// The validated, reusable launch template (its integrity is re-checked).
    pub launch_template: LaunchTemplate,
    /// The install-profile key for this `(app, profile)`. Not present on the
    /// template key, so it is supplied by the caller (which has the
    /// `(app, profile)` context) — it is part of the canonical instance-key
    /// triple, never a session/observed fact.
    pub install_profile_key: InstallProfileKey,
    /// The requirement graph **content** hash (`graph_hash`) for receipt/diff
    /// correlation. Not present on the template key (which carries only the
    /// snapshot identity), so it is supplied by the caller from the validated
    /// install records. Kept distinct from the snapshot hash (#581 wave 3A/3B):
    /// it is recorded as-is for correlation and is not an identity input here.
    pub requirement_graph_hash: String,
    /// Control-plane-global session reference (not a runner-local pid). The
    /// session-scoped component of the materialization identity.
    pub session_ref: String,
    /// The runner class chosen by placement for this session. Must match the
    /// template's runner compatibility class.
    pub selected_runner_class: RunnerClass,
    /// A reference to the selected runner instance (a control-plane ref, not a
    /// pid / container id / live route).
    pub selected_runner_ref: String,
    /// Projection digests captured at prepare time. Each must be a `blake3:<hex>`
    /// content digest (never a raw secret/state value). Order-independent.
    pub projection_digests: Vec<ProjectionDigest>,
    /// RFC 3339 timestamp this record is frozen. Metadata only — never an
    /// identity input.
    pub materialized_at: String,
}

/// A freshly-built per-session materialization record (#581 wave 5B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchMaterializationBuildOutput {
    pub record: LaunchMaterializationRecord,
    /// The derived preliminary materialization execution id (also on the record).
    pub execution_id: ExecutionId,
}

/// Typed failure when building a materialization record (#581 wave 5B).
///
/// Every variant is structured and auditable — never an in-band `"unknown"` /
/// `"unset"` sentinel. Carries only content hashes / class names / typed
/// reasons, never a secret or observed value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchMaterializationBuildError {
    /// The input launch template failed its self-consistency check (tampered /
    /// stale `template_hash`, or a `runner_compatibility_class` field that
    /// drifted from the key).
    #[error("launch template failed integrity validation: {0}")]
    TemplateIntegrityInvalid(#[source] LaunchTemplateIntegrityError),
    /// The template's `runner_compatibility_class` could not be parsed to a
    /// coarse runner class for the compatibility check.
    #[error("template runner compatibility class is unparsable: {0}")]
    RunnerCompatibilityClassUnparsable(#[source] RunnerCompatibilityClassParseError),
    /// The selected runner class is not the one the template was built for.
    #[error("selected runner class {selected:?} does not match template runner class {template:?}")]
    RunnerClassMismatch {
        template: RunnerClass,
        selected: RunnerClass,
    },
    /// No projection digests were supplied. A materialization records what was
    /// actually projected; an empty set is not a valid materialization.
    #[error("no projection digests supplied for materialization")]
    NoProjectionDigests,
    /// A projection digest is malformed (empty field or not a content hash).
    #[error("projection digest at index {index} is invalid: {reason}")]
    ProjectionDigestInvalid {
        index: usize,
        reason: ProjectionDigestInvalidReason,
    },
    /// The session reference is empty.
    #[error("session reference is empty")]
    SessionRefEmpty,
    /// The selected runner reference is empty.
    #[error("selected runner reference is empty")]
    SelectedRunnerRefEmpty,
    /// Deriving the preliminary materialization identity failed (e.g. a
    /// canonicalization error). Distinct from a malformed input.
    #[error("materialization identity could not be derived: {detail}")]
    IdentityDerivationFailed { detail: String },
}

/// Build a per-session [`LaunchMaterializationRecord`] from a validated reusable
/// launch template (#581 wave 5B).
///
/// Validates template integrity and runner-class compatibility, validates and
/// normalizes the projection digests, derives a deterministic preliminary
/// materialization `execution_id`, and freezes the record (whose
/// `capsule_instance_key` is derived from the canonical
/// `install_profile_key + install_revision_id + execution_id` triple).
///
/// Does not launch, dispatch, or probe anything.
pub fn build_launch_materialization(
    input: LaunchMaterializationBuildInput,
) -> Result<LaunchMaterializationBuildOutput, LaunchMaterializationBuildError> {
    let LaunchMaterializationBuildInput {
        launch_template,
        install_profile_key,
        requirement_graph_hash,
        session_ref,
        selected_runner_class,
        selected_runner_ref,
        projection_digests,
        materialized_at,
    } = input;

    // 1. The template must be internally consistent before we materialize from it.
    launch_template
        .validate_integrity()
        .map_err(LaunchMaterializationBuildError::TemplateIntegrityInvalid)?;

    // 2. The selected runner class must match the class the template was built
    //    for (e.g. a managed_runner template rejects a browser_runner selection).
    let template_class = launch_template
        .key
        .runner_compatibility_class
        .runner_class()
        .map_err(LaunchMaterializationBuildError::RunnerCompatibilityClassUnparsable)?;
    if template_class != selected_runner_class {
        return Err(LaunchMaterializationBuildError::RunnerClassMismatch {
            template: template_class,
            selected: selected_runner_class,
        });
    }

    // 3. Session / runner references must be present.
    if session_ref.is_empty() {
        return Err(LaunchMaterializationBuildError::SessionRefEmpty);
    }
    if selected_runner_ref.is_empty() {
        return Err(LaunchMaterializationBuildError::SelectedRunnerRefEmpty);
    }

    // 4. Validate every projection digest (typed, content-hash only — no raw
    //    secret/state values), then normalize to a deterministic order so the
    //    materialization identity is independent of the order they were captured.
    if projection_digests.is_empty() {
        return Err(LaunchMaterializationBuildError::NoProjectionDigests);
    }
    for (index, digest) in projection_digests.iter().enumerate() {
        digest.validate().map_err(|reason| {
            LaunchMaterializationBuildError::ProjectionDigestInvalid { index, reason }
        })?;
    }
    let mut normalized_digests = projection_digests;
    normalized_digests.sort_by(|a, b| {
        (&a.projection_kind, &a.source_ref, &a.digest).cmp(&(
            &b.projection_kind,
            &b.source_ref,
            &b.digest,
        ))
    });

    // 5. Derive the preliminary, deterministic materialization execution id from
    //    stable launch-time inputs only.
    let template_key_hash = launch_template.key.key_hash().map_err(|e| {
        LaunchMaterializationBuildError::IdentityDerivationFailed {
            detail: format!("hash template key: {e:#}"),
        }
    })?;
    let execution_id = derive_materialization_execution_id(
        &template_key_hash,
        &launch_template.template_hash,
        &normalized_digests,
        selected_runner_class,
        &selected_runner_ref,
        &session_ref,
    )
    .map_err(
        |e| LaunchMaterializationBuildError::IdentityDerivationFailed {
            detail: format!("{e:#}"),
        },
    )?;

    // 6. Freeze the record. `input_refs` references the install outputs that fed
    //    the launch envelope (all template refs — stable, never secret/observed).
    let input_refs = vec![
        launch_template.artifact_ref.clone(),
        launch_template.requirement_graph_ref.clone(),
        launch_template.binding_assignment_set_ref.clone(),
    ];
    let record = LaunchMaterializationRecord::new(
        session_ref,
        install_profile_key,
        launch_template.key.install_revision_id.clone(),
        launch_template.key.profile_hash.clone(),
        // Content `graph_hash` (correlation) vs snapshot identity — kept distinct
        // (#581 wave 3A/3B): the content hash is the caller-supplied value, the
        // snapshot hash comes from the template key.
        requirement_graph_hash,
        launch_template
            .key
            .requirement_graph_snapshot_hash
            .as_str()
            .to_owned(),
        launch_template.key.binding_set_hash.clone(),
        Some(selected_runner_ref),
        Some(selected_runner_class),
        input_refs,
        normalized_digests,
        execution_id.clone(),
        materialized_at,
    );

    Ok(LaunchMaterializationBuildOutput {
        record,
        execution_id,
    })
}

/// Derive the preliminary, deterministic materialization [`ExecutionId`] from
/// stable launch-time inputs only (#581 wave 5B).
///
/// The id is `exec_<blake3-hex>` over a tagged tuple of the template key hash +
/// template hash, the (already order-normalized) projection digests, the
/// selected runner class/ref, and the session reference. It excludes every
/// observed/runtime fact (pid, port, container id, route, log cursor, readiness,
/// timestamp, secret value): none of those are arguments here, so they cannot
/// enter the identity.
fn derive_materialization_execution_id(
    template_key_hash: &str,
    template_hash: &str,
    normalized_projection_digests: &[ProjectionDigest],
    selected_runner_class: RunnerClass,
    selected_runner_ref: &str,
    session_ref: &str,
) -> anyhow::Result<ExecutionId> {
    let content_hash = canonical_hash(&(
        "ato.launch_materialization.execution_id.v1",
        template_key_hash,
        template_hash,
        normalized_projection_digests,
        selected_runner_class,
        selected_runner_ref,
        session_ref,
    ))?;
    // `canonical_hash` returns `blake3:<64 hex>`; the hex body is a valid
    // ExecutionId tail (≥32 lowercase hex).
    let hex = content_hash
        .strip_prefix("blake3:")
        .unwrap_or(&content_hash);
    Ok(ExecutionId::new(format!("exec_{hex}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::finalizer::{
        FinalizerInput, FinalizerOutput, InstallBuildFacts, InstallRevisionFinalizer,
    };
    use crate::foundation::install_lifecycle::ids::{
        ArtifactBuildId, InstalledAppId, ProfileId, derive_install_profile_key,
    };
    use crate::foundation::install_lifecycle::launch_inputs::ValidatedInstallReusableInputs;
    use crate::foundation::install_lifecycle::launch_template::{
        CompatibilityIndex, CompatibilityIndexCompleteness, RequirementBinding,
        RequirementBindingKind, RunnerCompatibilityClass,
    };
    use crate::foundation::install_lifecycle::launch_template_builder::{
        LaunchTemplateBuildInput, build_launch_template,
    };
    use crate::foundation::install_lifecycle::records::RequirementGraphCompleteness;
    use crate::foundation::install_lifecycle::store::{
        AppRecord, InstallInstanceStore, LaunchProfile,
    };
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── Harness (mirrors launch_template_builder.rs / launch_template_reuse.rs) ─

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_launch_materialization_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "launch-materialization".into(),
                capsule_handle: "test/launch-materialization".into(),
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

    fn one_binding() -> RequirementBinding {
        RequirementBinding {
            requirement_id: "req-runtime".into(),
            binding_kind: RequirementBindingKind::Resource,
            resolved_resource_ref: Some("/ns/example/resource".into()),
            resolved_resource_refs: vec![],
            affects_execution_identity: false,
        }
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

    /// Finalize a standard install, force Ready, and build a reusable template.
    fn ready_template(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        out_base: &std::path::Path,
        build_suffix: &str,
    ) -> LaunchTemplate {
        let out = finalize_standard(store, app, profile, out_base, build_suffix);
        let rev = out.install_revision_id.clone();
        let mut v = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        make_ready(&mut v);
        build_launch_template(LaunchTemplateBuildInput {
            template_id: "ltmpl_mat".into(),
            reusable_inputs: v,
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        })
        .expect("ready inputs build a template")
        .launch_template
    }

    fn projection_digest(kind: &str, source: &str, digest: &str) -> ProjectionDigest {
        ProjectionDigest {
            source_ref: source.into(),
            projection_kind: kind.into(),
            digest: digest.into(),
        }
    }

    /// A valid `blake3:<64 hex>` digest from a short hex seed (right-padded).
    fn digest(seed_hex: &str) -> String {
        let body = format!("{seed_hex:0<64}");
        format!("blake3:{}", &body[..64])
    }

    fn sample_digests() -> Vec<ProjectionDigest> {
        vec![
            projection_digest("artifact", "/artifacts/blake3/3333", &digest("a47d16")),
            projection_digest("secret", "/secrets/sec_db", &digest("5ecd16")),
            projection_digest("network_policy", "/policies/net", &digest("0e7d16")),
        ]
    }

    /// A fixed, valid `blake3:<64 hex>` standing in for the requirement graph
    /// **content** hash (`graph_hash`). The builder records it as-is (correlation
    /// only); tests assert it is kept distinct from the snapshot identity.
    const GRAPH_CONTENT_HASH: &str =
        "blake3:1111111111111111111111111111111111111111111111111111111111111111";

    fn build_input(
        template: LaunchTemplate,
        ipk_app: &InstalledAppId,
        ipk_profile: &ProfileId,
        session_ref: &str,
        digests: Vec<ProjectionDigest>,
    ) -> LaunchMaterializationBuildInput {
        LaunchMaterializationBuildInput {
            launch_template: template,
            install_profile_key: derive_install_profile_key(ipk_app, ipk_profile),
            requirement_graph_hash: GRAPH_CONTENT_HASH.into(),
            session_ref: session_ref.into(),
            selected_runner_class: RunnerClass::ManagedRunner,
            selected_runner_ref: "/runners/run_managed_1".into(),
            projection_digests: digests,
            materialized_at: "2026-06-08T00:00:00Z".into(),
        }
    }

    // ── 1. Accepts a valid reusable template ─────────────────────────────────

    #[test]
    fn materialization_builder_accepts_valid_reusable_template() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da01");
        let rev = template.key.install_revision_id.clone();

        let out = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_a",
            sample_digests(),
        ))
        .expect("valid reusable template materializes");

        assert_eq!(out.record.session_ref, "ses_a");
        assert_eq!(out.record.install_revision_id, rev);
        assert_eq!(out.record.execution_id, out.execution_id);
        assert!(out.record.execution_id.is_valid());
        assert!(
            out.record.capsule_instance_key.as_str().starts_with("cik_"),
            "instance key must be derived"
        );
        assert_eq!(
            out.record.selected_runner_class,
            Some(RunnerClass::ManagedRunner)
        );
    }

    // ── Graph content hash vs snapshot hash are distinct fields (#588) ───────

    #[test]
    fn materialization_record_distinguishes_graph_hash_from_snapshot_hash() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da01b");
        let snapshot_hash = template
            .key
            .requirement_graph_snapshot_hash
            .as_str()
            .to_owned();

        let out = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_distinct",
            sample_digests(),
        ))
        .unwrap();

        // The content `graph_hash` field holds the caller-supplied content hash…
        assert_eq!(out.record.requirement_graph_hash, GRAPH_CONTENT_HASH);
        // …and the snapshot identity is recorded in its own field, from the key.
        assert_eq!(out.record.requirement_graph_snapshot_hash, snapshot_hash);
        // They are genuinely different values (the snapshot folds in profile
        // defaults + completeness on top of the content hash, #581 wave 3B).
        assert_ne!(
            out.record.requirement_graph_hash,
            out.record.requirement_graph_snapshot_hash
        );
    }

    #[test]
    fn materialization_builder_does_not_store_snapshot_hash_in_requirement_graph_hash() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da01c");
        let snapshot_hash = template
            .key
            .requirement_graph_snapshot_hash
            .as_str()
            .to_owned();

        let out = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_nostore",
            sample_digests(),
        ))
        .unwrap();

        // The `requirement_graph_hash` field must NOT be the snapshot hash — that
        // would make a reader mistake the snapshot identity for the content hash.
        assert_ne!(
            out.record.requirement_graph_hash, snapshot_hash,
            "requirement_graph_hash must not carry the snapshot hash"
        );
        assert_eq!(out.record.requirement_graph_hash, GRAPH_CONTENT_HASH);
    }

    // ── 2. Validates template integrity ──────────────────────────────────────

    #[test]
    fn materialization_builder_validates_template_integrity() {
        let (dir, store, app, profile) = setup();
        let mut template = ready_template(&store, &app, &profile, dir.path(), "da02");
        // Tamper the template_hash so integrity validation fails.
        template.template_hash = "blake3:TAMPERED".into();

        let err = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_b",
            sample_digests(),
        ))
        .unwrap_err();
        assert!(
            matches!(
                err,
                LaunchMaterializationBuildError::TemplateIntegrityInvalid(
                    LaunchTemplateIntegrityError::TemplateHashMismatch { .. }
                )
            ),
            "got {err:?}"
        );
    }

    // ── 3. Rejects runner class mismatch ─────────────────────────────────────

    #[test]
    fn materialization_builder_rejects_runner_class_mismatch() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da03");
        // The template targets managed_runner; select a browser_runner.
        let mut input = build_input(template, &app, &profile, "ses_c", sample_digests());
        input.selected_runner_class = RunnerClass::BrowserRunner;

        let err = build_launch_materialization(input).unwrap_err();
        match err {
            LaunchMaterializationBuildError::RunnerClassMismatch { template, selected } => {
                assert_eq!(template, RunnerClass::ManagedRunner);
                assert_eq!(selected, RunnerClass::BrowserRunner);
            }
            other => panic!("expected RunnerClassMismatch, got {other:?}"),
        }
    }

    // ── 4. Rejects invalid projection digest ─────────────────────────────────

    #[test]
    fn materialization_builder_rejects_invalid_projection_digest() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da04");

        // A digest that is not a content hash (could be a raw value).
        let bad = vec![projection_digest("secret", "/secrets/sec_db", "hunter2")];
        let err = build_launch_materialization(build_input(template, &app, &profile, "ses_d", bad))
            .unwrap_err();
        assert!(
            matches!(
                err,
                LaunchMaterializationBuildError::ProjectionDigestInvalid {
                    reason: ProjectionDigestInvalidReason::DigestNotContentHash { .. },
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn materialization_builder_rejects_empty_projection_digests() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da05");
        let err =
            build_launch_materialization(build_input(template, &app, &profile, "ses_e", vec![]))
                .unwrap_err();
        assert!(matches!(
            err,
            LaunchMaterializationBuildError::NoProjectionDigests
        ));
    }

    // ── 6. Never accepts / stores raw secret values ──────────────────────────

    #[test]
    fn materialization_builder_does_not_store_secret_values() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da06");

        // A well-formed secret *projection digest* (a hash of the projection
        // shape, NOT the secret value).
        let secret_digest = digest("5ec5e7");
        let digests = vec![projection_digest(
            "secret",
            "/secrets/sec_db",
            &secret_digest,
        )];
        let out =
            build_launch_materialization(build_input(template, &app, &profile, "ses_f", digests))
                .unwrap();

        let json = serde_json::to_string(&out.record).unwrap();
        assert!(json.contains(&secret_digest));
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(
                !json.contains(forbidden),
                "materialization record must never carry a raw secret value ({forbidden:?})"
            );
        }
    }

    // ── 8. Identity is stable for same inputs ────────────────────────────────

    #[test]
    fn materialization_identity_is_stable_for_same_inputs() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da07");

        let a = build_launch_materialization(build_input(
            template.clone(),
            &app,
            &profile,
            "ses_stable",
            sample_digests(),
        ))
        .unwrap();
        let b = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_stable",
            // Same digests in a different order — identity must be order-independent.
            sample_digests().into_iter().rev().collect(),
        ))
        .unwrap();

        assert_eq!(
            a.execution_id, b.execution_id,
            "same stable inputs must produce a stable materialization execution id"
        );
        assert_eq!(a.record.capsule_instance_key, b.record.capsule_instance_key);
    }

    // ── 9. Identity changes when a projection digest changes ─────────────────

    #[test]
    fn materialization_identity_changes_when_projection_digest_changes() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da08");

        let base = build_launch_materialization(build_input(
            template.clone(),
            &app,
            &profile,
            "ses_x",
            sample_digests(),
        ))
        .unwrap();

        let mut changed = sample_digests();
        changed[0].digest = digest("d1ffe7ed");
        let other =
            build_launch_materialization(build_input(template, &app, &profile, "ses_x", changed))
                .unwrap();

        assert_ne!(
            base.execution_id, other.execution_id,
            "a changed projection digest must change the materialization identity"
        );
        assert_ne!(
            base.record.capsule_instance_key,
            other.record.capsule_instance_key
        );
    }

    #[test]
    fn materialization_identity_changes_per_session() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da09");
        let a = build_launch_materialization(build_input(
            template.clone(),
            &app,
            &profile,
            "ses_one",
            sample_digests(),
        ))
        .unwrap();
        let b = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_two",
            sample_digests(),
        ))
        .unwrap();
        assert_ne!(
            a.execution_id, b.execution_id,
            "distinct sessions must yield distinct materialization identities"
        );
    }

    // ── 7. Observed diagnostics do not enter identity ────────────────────────

    #[test]
    fn materialization_identity_excludes_observed_diagnostics() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da10");

        let base = build_launch_materialization(build_input(
            template.clone(),
            &app,
            &profile,
            "ses_obs",
            sample_digests(),
        ))
        .unwrap();

        // Two launches whose only difference is observed/runtime facts (which are
        // not inputs to the builder at all) and the metadata-only materialized_at.
        struct ObservedFacts {
            dynamic_port: u16,
            process_id: u32,
            container_id: &'static str,
            live_route: &'static str,
            log_cursor: &'static str,
            observed_status: &'static str,
        }
        let obs = ObservedFacts {
            dynamic_port: 40001,
            process_id: 111,
            container_id: "ctr_a",
            live_route: "https://a.live",
            log_cursor: "cursor:1",
            observed_status: "running",
        };
        let _ = (
            obs.dynamic_port,
            obs.process_id,
            obs.container_id,
            obs.live_route,
            obs.log_cursor,
            obs.observed_status,
        );

        let mut later = build_input(template, &app, &profile, "ses_obs", sample_digests());
        // A different freeze timestamp must NOT change identity (metadata only).
        later.materialized_at = "2026-12-31T23:59:59Z".into();
        let rebuilt = build_launch_materialization(later).unwrap();

        assert_eq!(
            base.execution_id, rebuilt.execution_id,
            "observed facts and materialized_at must not change identity"
        );
        assert_eq!(
            base.record.capsule_instance_key,
            rebuilt.record.capsule_instance_key
        );
    }

    // ── 10. Standard install cannot reach materialization ────────────────────

    #[test]
    fn standard_install_cannot_materialize_without_reusable_template() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "da11");
        let rev = out.install_revision_id.clone();
        let v = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap();

        // Standard install is NotReady, so the #594 builder refuses to produce a
        // reusable template — there is no LaunchTemplate to feed materialization.
        let built = build_launch_template(LaunchTemplateBuildInput {
            template_id: "ltmpl_std".into(),
            reusable_inputs: v,
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        });
        assert!(
            built.is_err(),
            "standard install must not yield a reusable template to materialize from"
        );
    }

    // ── 11/persistence. Round-trips through the store ────────────────────────

    #[test]
    fn persisted_materialization_round_trips_if_store_methods_added() {
        let (dir, store, app, profile) = setup();
        let template = ready_template(&store, &app, &profile, dir.path(), "da12");
        let out = build_launch_materialization(build_input(
            template,
            &app,
            &profile,
            "ses_rt",
            sample_digests(),
        ))
        .unwrap();

        store
            .write_launch_materialization_record(&app, &out.record)
            .unwrap();
        let back = store
            .read_launch_materialization_record(&app, &out.record.capsule_instance_key)
            .unwrap();
        assert_eq!(back, out.record);
    }
}
