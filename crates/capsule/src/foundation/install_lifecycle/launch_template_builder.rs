//! Conservative [`LaunchTemplate`] generation from validated install inputs
//! (#581 wave 4C).
//!
//! This is the safe boundary that turns a [`ValidatedInstallReusableInputs`]
//! (the #581 wave 4B read-validation layer) into a reusable,
//! session-independent [`LaunchTemplate`] — but **only** when the inputs are
//! [`LaunchTemplateReadiness::Ready`]. It does *not* implement launch reuse, a
//! [`LaunchMaterializationRecord`](super::materialization::LaunchMaterializationRecord),
//! a session `execution_id`, runner placement, or any runtime wiring.
//!
//! # Why standard install produces no template
//!
//! The standard install path loads successfully but is reported
//! [`LaunchTemplateReadiness::NotReady`] (its requirement graph and compatibility
//! index are `Partial` and it has no resolved bindings). [`build_launch_template`]
//! refuses such inputs with [`LaunchTemplateBuildError::InputsNotReady`] and
//! persists nothing. Only an explicitly-complete fixture (complete graph,
//! complete compatibility index with a supported non-denied runner class, and a
//! non-empty binding set) reaches the `Ready` gate and yields a template.
//!
//! # Identity discipline
//!
//! The template identity is a [`LaunchTemplateKey`], constructed via
//! [`LaunchTemplateKey::from_inputs`] under
//! [`RequirementGraphCompletenessPolicy::RequireComplete`] — so a `Partial`
//! requirement graph can never mint a template. The key folds in only stable
//! install-time inputs (install revision id, profile hash, requirement-graph
//! snapshot identity, binding-set hash, network/capability/state-contract policy
//! hashes, runner compatibility class). No session id, dynamic port, pid,
//! container id, live route, log cursor, observed status, timestamp-as-identity,
//! or secret value enters the key, the template, or the persisted record. The
//! exclusion is asserted by
//! [`tests::template_identity_excludes_session_and_observed_fields`].

use thiserror::Error;

use super::launch_inputs::{LaunchTemplateReadiness, ValidatedInstallReusableInputs};
use super::launch_template::{
    LaunchTemplate, LaunchTemplateKey, LaunchTemplateKeyInputs, RunnerCompatibilityClass,
    RunnerCompatibilityClassParseError,
};
use super::records::{
    RequirementGraphCompletenessPolicy, RequirementGraphSnapshotIdentityError,
    combined_state_contract_hash,
};

/// Inputs to [`build_launch_template`] (#581 wave 4C).
///
/// Carries the validated reusable records plus the install-time policy hashes
/// and runner compatibility class the key keys on. The requirement-graph
/// snapshot identity, binding-set hash, install revision id, and combined
/// state-contract hash are taken from the validated records, not re-supplied by
/// the caller, so they cannot drift from what was persisted.
pub struct LaunchTemplateBuildInput {
    /// Stable identifier to assign the generated template.
    pub template_id: String,
    /// The #581 wave 4B validated input boundary.
    pub reusable_inputs: ValidatedInstallReusableInputs,
    /// Hash of the launch profile (`blake3:<hex>`). A stable install-time input.
    pub profile_hash: String,
    /// Hash of the resolved network policy (`blake3:<hex>`).
    pub network_policy_hash: String,
    /// Hash of the resolved capability policy (`blake3:<hex>`).
    pub capability_policy_hash: String,
    /// The coarse runner class the template is built for. A launch-template
    /// input (it keys the template), never a concrete `runner_id`.
    pub runner_compatibility_class: RunnerCompatibilityClass,
}

/// A generated template together with its cache identity (#581 wave 4C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTemplateBuildOutput {
    pub launch_template: LaunchTemplate,
    pub key: LaunchTemplateKey,
}

/// Typed failure when building a [`LaunchTemplate`] from validated inputs.
///
/// Every variant is a structured, auditable reason — never an in-band
/// `"unknown"`/`"unset"` sentinel. The error carries content hashes / typed
/// readiness only, never a secret or observed value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchTemplateBuildError {
    /// The validated inputs are not yet sufficient for a real template. Carries
    /// the typed [`LaunchTemplateReadiness`] (with reasons) so a caller can log
    /// precisely why — this is the variant standard install always hits.
    #[error("install inputs are not ready for a launch template: {readiness:?}")]
    InputsNotReady { readiness: LaunchTemplateReadiness },
    /// Defensive guard: the inputs reported `Ready` but the compatibility index
    /// proves no runner class is actually usable (supported and not denied).
    /// `Ready` already implies a compatible class, so this is unreachable in
    /// practice; it exists so the builder can never emit a template without one.
    #[error("no compatible runner class for the launch template")]
    NoCompatibleRunnerClass,
    /// The *requested* `runner_compatibility_class` could not be parsed into a
    /// coarse [`RunnerClass`](super::launch_template::RunnerClass), so it cannot
    /// be checked against the compatibility index.
    #[error("requested runner compatibility class is unparsable: {0}")]
    RunnerCompatibilityClassUnparsable(#[source] RunnerCompatibilityClassParseError),
    /// The *requested* `runner_compatibility_class` resolves to a runner class
    /// that the [`CompatibilityIndex`](super::launch_template::CompatibilityIndex)
    /// does not support (absent from the supported set, or denied — deny wins). A
    /// template must be built only for a class the revision is proven to support;
    /// `has_compatible_runner_class()` (some class is usable) is not sufficient.
    #[error(
        "requested runner compatibility class '{runner_compatibility_class}' is not supported by the compatibility index"
    )]
    RunnerCompatibilityClassNotSupported { runner_compatibility_class: String },
    /// The requirement-graph snapshot identity failed the
    /// `RequireComplete` recompute-and-compare check during key construction
    /// (e.g. a `Partial` graph, or an empty/raw/stale snapshot hash).
    #[error("requirement-graph snapshot is invalid for a launch template: {0}")]
    InvalidRequirementGraphSnapshot(#[source] RequirementGraphSnapshotIdentityError),
    /// Constructing the [`LaunchTemplateKey`] failed for a reason other than a
    /// snapshot-identity rejection (e.g. a canonicalization error). Distinct from
    /// [`Self::InvalidRequirementGraphSnapshot`] so a rare internal failure is
    /// never mislabelled as a tampered snapshot.
    #[error("launch-template key could not be constructed: {detail}")]
    KeyConstructionFailed { detail: String },
    /// Computing the combined state-contract hash or the template payload hash
    /// failed (canonicalization error).
    #[error("launch-template payload could not be hashed: {detail}")]
    TemplateConstructionFailed { detail: String },
}

/// Build a reusable, session-independent [`LaunchTemplate`] from validated
/// install inputs — only when they are [`LaunchTemplateReadiness::Ready`].
///
/// Refuses standard-install (`NotReady`) inputs with
/// [`LaunchTemplateBuildError::InputsNotReady`] and produces nothing. The
/// returned template's identity is constructed under
/// [`RequirementGraphCompletenessPolicy::RequireComplete`], so a `Partial`
/// requirement graph is rejected even if every other gate somehow passed.
pub fn build_launch_template(
    input: LaunchTemplateBuildInput,
) -> Result<LaunchTemplateBuildOutput, LaunchTemplateBuildError> {
    let LaunchTemplateBuildInput {
        template_id,
        reusable_inputs,
        profile_hash,
        network_policy_hash,
        capability_policy_hash,
        runner_compatibility_class,
    } = input;

    // 1. Readiness gate. Standard install is `NotReady` (Partial graph + Partial
    //    compatibility index + no bindings) and stops here — nothing is built or
    //    persisted. `Ready` guarantees: complete graph, complete compatibility
    //    index with a compatible runner class, and at least one resolved binding.
    let readiness = reusable_inputs.launch_template_readiness();
    if !readiness.is_ready() {
        return Err(LaunchTemplateBuildError::InputsNotReady { readiness });
    }

    // 2. Defensive: `Ready` already implies *some* compatible runner class, but
    //    assert it explicitly so the builder can never emit a template without one
    //    (deny-wins logic lives in `CompatibilityIndex::has_compatible_runner_class`).
    if !reusable_inputs
        .compatibility_index
        .has_compatible_runner_class()
    {
        return Err(LaunchTemplateBuildError::NoCompatibleRunnerClass);
    }

    // 3. The *requested* class must itself be supported — not merely that some
    //    class is. Otherwise a `browser_runner/...` template could be minted for a
    //    revision whose index only supports `managed_runner`. Parse the requested
    //    compatibility class to its coarse `RunnerClass` and check it against the
    //    index (deny wins).
    let requested_runner_class = runner_compatibility_class
        .runner_class()
        .map_err(LaunchTemplateBuildError::RunnerCompatibilityClassUnparsable)?;
    if !reusable_inputs
        .compatibility_index
        .is_supported(&requested_runner_class)
    {
        return Err(
            LaunchTemplateBuildError::RunnerCompatibilityClassNotSupported {
                runner_compatibility_class: runner_compatibility_class.as_str().to_owned(),
            },
        );
    }

    // 4. Derive the stable identity inputs from the validated records (not from
    //    caller-supplied copies that could drift from what was persisted).
    let install_revision_id = reusable_inputs.install_revision.install_revision_id.clone();
    let binding_set_hash = reusable_inputs
        .binding_assignment_set
        .binding_set_hash
        .clone();
    let state_contract_hash = combined_state_contract_hash(&reusable_inputs.state_contracts)
        .map_err(|e| LaunchTemplateBuildError::TemplateConstructionFailed {
            detail: format!("{e:#}"),
        })?;

    // 5. Build the key. `RequireComplete` is enforced here regardless of how the
    //    inputs were loaded (4B loads under AllowPartial): a Partial graph that
    //    slipped past readiness would still be rejected at key construction.
    let key = LaunchTemplateKey::from_inputs(LaunchTemplateKeyInputs {
        install_revision_id,
        profile_hash,
        requirement_graph: reusable_inputs.requirement_graph.clone(),
        binding_set_hash,
        network_policy_hash: network_policy_hash.clone(),
        capability_policy_hash: capability_policy_hash.clone(),
        state_contract_hash,
        runner_compatibility_class,
        completeness_policy: RequirementGraphCompletenessPolicy::RequireComplete,
    })
    .map_err(
        |e| match e.downcast::<RequirementGraphSnapshotIdentityError>() {
            Ok(identity) => LaunchTemplateBuildError::InvalidRequirementGraphSnapshot(identity),
            Err(other) => LaunchTemplateBuildError::KeyConstructionFailed {
                detail: format!("{other:#}"),
            },
        },
    )?;

    // 6. Project the template payload from stable, content-addressed install
    //    facts. The filesystem-view template hash is derived from the frozen
    //    artifact build id + persisted output hashes — content identity, never a
    //    materialized per-session view. Policy-template hashes mirror the
    //    install-time policy hashes. None of these are session/observed facts.
    let filesystem_view_template_hash = derive_filesystem_view_template_hash(&reusable_inputs)
        .map_err(|e| LaunchTemplateBuildError::TemplateConstructionFailed {
            detail: format!("{e:#}"),
        })?;

    let template = LaunchTemplate::new(
        template_id,
        key.clone(),
        reusable_inputs
            .install_revision
            .install_profile_key
            .as_str(),
        reusable_inputs.install_revision.artifact_build_id.as_str(),
        reusable_inputs.requirement_graph.snapshot_id.as_str(),
        reusable_inputs
            .binding_assignment_set
            .binding_set_id
            .as_str(),
        filesystem_view_template_hash,
        network_policy_hash,
        capability_policy_hash,
    )
    .map_err(|e| LaunchTemplateBuildError::TemplateConstructionFailed {
        detail: format!("{e:#}"),
    })?;

    Ok(LaunchTemplateBuildOutput {
        launch_template: template,
        key,
    })
}

/// Content identity of the frozen install output, used as the launch template's
/// `filesystem_view_template_hash`. Derived from the artifact build id plus the
/// install receipt's recorded output hashes (sorted for order-independence) —
/// purely content-addressed install facts, never a per-session materialized view.
fn derive_filesystem_view_template_hash(
    inputs: &ValidatedInstallReusableInputs,
) -> anyhow::Result<String> {
    let mut output_hashes: Vec<&str> = inputs
        .install_receipt
        .output_hashes
        .iter()
        .map(String::as_str)
        .collect();
    output_hashes.sort_unstable();
    super::hashing::canonical_hash(&(
        "ato.launch_template.fs_view.v1",
        inputs.install_revision.artifact_build_id.as_str(),
        &output_hashes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::finalizer::{
        FinalizerInput, FinalizerOutput, InstallBuildFacts, InstallRevisionFinalizer,
    };
    use crate::foundation::install_lifecycle::ids::{
        ArtifactBuildId, InstallRevisionId, InstalledAppId, ProfileId,
    };
    use crate::foundation::install_lifecycle::launch_inputs::LaunchTemplateReadinessReason;
    use crate::foundation::install_lifecycle::launch_template::{
        CompatibilityIndex, CompatibilityIndexCompleteness, RequirementBinding,
        RequirementBindingKind, RunnerClass,
    };
    use crate::foundation::install_lifecycle::records::RequirementGraphCompleteness;
    use crate::foundation::install_lifecycle::store::{
        AppRecord, InstallInstanceStore, LaunchProfile,
    };
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── Harness (mirrors launch_inputs.rs / finalizer.rs test helpers) ───────

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_launch_template_builder_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "launch-template-builder".into(),
                capsule_handle: "test/launch-template-builder".into(),
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

    fn load_standard(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        out_base: &std::path::Path,
        build_suffix: &str,
    ) -> (InstallRevisionId, ValidatedInstallReusableInputs) {
        let out = finalize_standard(store, app, profile, out_base, build_suffix);
        let rev = out.install_revision_id.clone();
        let v = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        (rev, v)
    }

    /// A compatibility index that is `Complete` and has one supported,
    /// non-denied runner class.
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

    /// Mutate a loaded standard install into a Ready fixture: Complete graph,
    /// Complete compatibility index with a supported runner class, and one
    /// resolved binding. (In-memory mutation of the validated view — exercises
    /// the builder gate, not the load path.)
    fn make_ready(v: &mut ValidatedInstallReusableInputs) {
        v.requirement_graph = v
            .requirement_graph
            .clone()
            .with_completeness(RequirementGraphCompleteness::Complete)
            .unwrap();
        v.compatibility_index = complete_compat_index();
        v.binding_assignment_set.assignments.push(one_binding());
    }

    fn build_input(
        template_id: &str,
        v: ValidatedInstallReusableInputs,
    ) -> LaunchTemplateBuildInput {
        LaunchTemplateBuildInput {
            template_id: template_id.into(),
            reusable_inputs: v,
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        }
    }

    // ── Standard install never builds a template ─────────────────────────────

    #[test]
    fn standard_install_inputs_do_not_build_launch_template() {
        let (dir, store, app, profile) = setup();
        let (_rev, v) = load_standard(&store, &app, &profile, dir.path(), "ba01");

        let err = build_launch_template(build_input("ltmpl_std", v)).unwrap_err();
        match err {
            LaunchTemplateBuildError::InputsNotReady { readiness } => {
                assert!(!readiness.is_ready());
            }
            other => panic!("expected InputsNotReady, got {other:?}"),
        }
    }

    #[test]
    fn standard_install_does_not_persist_fake_template() {
        let (dir, store, app, profile) = setup();
        let (rev, v) = load_standard(&store, &app, &profile, dir.path(), "ba02");

        // Building from standard install fails before any persistence.
        assert!(build_launch_template(build_input("ltmpl_std", v)).is_err());

        // The revision authority embeds no launch template, and nothing was
        // written to the standalone launch-templates dir.
        let revision = store.read_install_revision(&app, &profile, &rev).unwrap();
        assert!(
            revision.launch_templates.is_empty(),
            "standard install must embed no LaunchTemplate"
        );
        assert!(
            store
                .read_launch_templates(&app, &profile, &rev)
                .unwrap()
                .is_empty(),
            "standard install must persist no standalone LaunchTemplate"
        );
    }

    // ── Each readiness blocker is surfaced ───────────────────────────────────

    #[test]
    fn build_template_rejects_partial_requirement_graph() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba03");
        make_ready(&mut v);
        // Force the requirement graph back to Partial — the only blocker.
        v.requirement_graph = v
            .requirement_graph
            .clone()
            .with_completeness(RequirementGraphCompleteness::Partial {
                reasons: vec![
                    crate::foundation::install_lifecycle::records::RequirementGraphCompletenessReason::ManifestFactsUnavailable,
                ],
            })
            .unwrap();

        let err = build_launch_template(build_input("ltmpl", v)).unwrap_err();
        match err {
            LaunchTemplateBuildError::InputsNotReady { readiness } => match readiness {
                LaunchTemplateReadiness::NotReady { reasons } => assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::RequirementGraphPartial),
                    "got {reasons:?}"
                ),
                LaunchTemplateReadiness::Ready => unreachable!(),
            },
            other => panic!("expected InputsNotReady, got {other:?}"),
        }
    }

    #[test]
    fn build_template_rejects_partial_compatibility_index() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba04");
        make_ready(&mut v);
        // Force the compatibility index back to Partial.
        v.compatibility_index = CompatibilityIndex::new(
            "cidx:partial",
            vec![RunnerClass::ManagedRunner],
            vec![],
            vec![],
            vec![],
        )
        .unwrap(); // default completeness is Partial

        let err = build_launch_template(build_input("ltmpl", v)).unwrap_err();
        match err {
            LaunchTemplateBuildError::InputsNotReady { readiness } => match readiness {
                LaunchTemplateReadiness::NotReady { reasons } => assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::CompatibilityIndexPartial),
                    "got {reasons:?}"
                ),
                LaunchTemplateReadiness::Ready => unreachable!(),
            },
            other => panic!("expected InputsNotReady, got {other:?}"),
        }
    }

    // ── Requested runner compatibility class must itself be supported ─────────

    #[test]
    fn build_template_rejects_runner_compatibility_class_not_supported_by_index() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba13");
        make_ready(&mut v); // index supports ManagedRunner only

        // Request a browser_runner template — the index does not support it, even
        // though it *does* have a compatible class (ManagedRunner). The build must
        // refuse, not silently mint a BrowserRunner template.
        let mut input = build_input("ltmpl", v);
        input.runner_compatibility_class = RunnerCompatibilityClass::new("browser_runner/wasm");

        let err = build_launch_template(input).unwrap_err();
        match err {
            LaunchTemplateBuildError::RunnerCompatibilityClassNotSupported {
                runner_compatibility_class,
            } => assert_eq!(runner_compatibility_class, "browser_runner/wasm"),
            other => panic!("expected RunnerCompatibilityClassNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn build_template_rejects_runner_compatibility_class_denied_by_index() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba14");
        make_ready(&mut v);
        // Index supports ManagedRunner + BrowserRunner, but denies BrowserRunner
        // (deny wins). `has_compatible_runner_class()` is still true (ManagedRunner),
        // so the build reaches the requested-class check.
        v.compatibility_index = CompatibilityIndex::new(
            "cidx:deny_browser",
            vec![RunnerClass::ManagedRunner, RunnerClass::BrowserRunner],
            vec![RunnerClass::BrowserRunner],
            vec![],
            vec![],
        )
        .unwrap()
        .with_completeness(CompatibilityIndexCompleteness::Complete)
        .unwrap();

        let mut input = build_input("ltmpl", v);
        input.runner_compatibility_class = RunnerCompatibilityClass::new("browser_runner/wasm");

        let err = build_launch_template(input).unwrap_err();
        assert!(
            matches!(
                err,
                LaunchTemplateBuildError::RunnerCompatibilityClassNotSupported { .. }
            ),
            "a denied runner class must be rejected, got {err:?}"
        );
    }

    #[test]
    fn build_template_accepts_runner_compatibility_class_matching_supported_runner() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba15");
        make_ready(&mut v); // index supports ManagedRunner

        let mut input = build_input("ltmpl", v);
        input.runner_compatibility_class =
            RunnerCompatibilityClass::new("managed_runner/linux-x86_64");

        let out = build_launch_template(input).expect("requested class is supported → builds");
        assert_eq!(
            out.launch_template.runner_compatibility_class.as_str(),
            "managed_runner/linux-x86_64"
        );
    }

    #[test]
    fn build_template_rejects_no_compatible_runner_class() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba05");
        make_ready(&mut v);
        // Complete analysis, but nothing supported → no compatible runner class.
        v.compatibility_index =
            CompatibilityIndex::new("cidx:empty", vec![], vec![], vec![], vec![])
                .unwrap()
                .with_completeness(CompatibilityIndexCompleteness::Complete)
                .unwrap();

        let err = build_launch_template(build_input("ltmpl", v)).unwrap_err();
        match err {
            LaunchTemplateBuildError::InputsNotReady { readiness } => match readiness {
                LaunchTemplateReadiness::NotReady { reasons } => assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::NoCompatibleRunnerClass),
                    "got {reasons:?}"
                ),
                LaunchTemplateReadiness::Ready => unreachable!(),
            },
            other => panic!("expected InputsNotReady, got {other:?}"),
        }
    }

    #[test]
    fn build_template_rejects_empty_bindings() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba06");
        make_ready(&mut v);
        // Remove the resolved bindings again — the only blocker.
        v.binding_assignment_set.assignments.clear();

        let err = build_launch_template(build_input("ltmpl", v)).unwrap_err();
        match err {
            LaunchTemplateBuildError::InputsNotReady { readiness } => match readiness {
                LaunchTemplateReadiness::NotReady { reasons } => assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::NoResolvedBindings),
                    "got {reasons:?}"
                ),
                LaunchTemplateReadiness::Ready => unreachable!(),
            },
            other => panic!("expected InputsNotReady, got {other:?}"),
        }
    }

    // ── Complete fixture builds a template ───────────────────────────────────

    #[test]
    fn complete_inputs_build_launch_template() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba07");
        make_ready(&mut v);

        let out =
            build_launch_template(build_input("ltmpl_ok", v)).expect("ready inputs must build");
        assert_eq!(out.launch_template.template_id, "ltmpl_ok");
        assert!(out.launch_template.template_hash.starts_with("blake3:"));
        // The template's key matches the returned key.
        assert_eq!(out.launch_template.key, out.key);
        // The key folds in the snapshot identity (a real blake3 hash), not the
        // content-only graph_hash.
        assert!(
            out.key
                .requirement_graph_snapshot_hash
                .as_str()
                .starts_with("blake3:")
        );
    }

    // ── Key stability / sensitivity ──────────────────────────────────────────

    #[test]
    fn launch_template_key_is_stable_for_same_inputs() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba08");
        make_ready(&mut v);

        let a = build_launch_template(build_input("ltmpl_a", v.clone())).unwrap();
        let b = build_launch_template(build_input("ltmpl_b", v)).unwrap();
        assert_eq!(
            a.key.key_hash().unwrap(),
            b.key.key_hash().unwrap(),
            "same stable inputs must produce the same key hash (template_id is not an identity input)"
        );
        // template_id differs but does not affect the key hash.
        assert_eq!(
            a.launch_template.template_hash,
            b.launch_template.template_hash
        );
    }

    #[test]
    fn launch_template_key_changes_when_binding_set_changes() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba09");
        make_ready(&mut v);
        let base = build_launch_template(build_input("ltmpl", v.clone()))
            .unwrap()
            .key
            .key_hash()
            .unwrap();

        // Add a second resolved binding → the binding-set hash changes → the key
        // hash changes.
        let mut v2 = v;
        v2.binding_assignment_set = crate::foundation::install_lifecycle::launch_template::BindingAssignmentSet::new(
            "bset_changed",
            v2.install_revision.install_profile_key.clone(),
            v2.requirement_graph.graph_hash.clone(),
            vec![
                one_binding(),
                RequirementBinding {
                    requirement_id: "req-storage".into(),
                    binding_kind: RequirementBindingKind::Resource,
                    resolved_resource_ref: Some("/ns/example/storage".into()),
                    resolved_resource_refs: vec![],
                    affects_execution_identity: true,
                },
            ],
            crate::foundation::install_lifecycle::launch_template::BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();

        let changed = build_launch_template(build_input("ltmpl", v2))
            .unwrap()
            .key
            .key_hash()
            .unwrap();
        assert_ne!(base, changed, "binding set hash must affect the key");
    }

    #[test]
    fn launch_template_key_changes_when_policy_hash_changes() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba10");
        make_ready(&mut v);
        let base = build_launch_template(build_input("ltmpl", v.clone()))
            .unwrap()
            .key
            .key_hash()
            .unwrap();

        let mut changed_input = build_input("ltmpl", v);
        changed_input.network_policy_hash = "blake3:net_other".into();
        let changed = build_launch_template(changed_input)
            .unwrap()
            .key
            .key_hash()
            .unwrap();
        assert_ne!(base, changed, "network policy hash must affect the key");
    }

    // ── Persistence round-trip ───────────────────────────────────────────────

    #[test]
    fn persisted_launch_template_round_trips() {
        let (dir, store, app, profile) = setup();
        let (rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba11");
        make_ready(&mut v);
        let out = build_launch_template(build_input("ltmpl_rt", v)).unwrap();
        let key_hash = out.key.key_hash().unwrap();

        store
            .write_launch_template(&app, &profile, &rev, &out.launch_template)
            .unwrap();

        let read_back = store
            .read_launch_template(&app, &profile, &rev, &key_hash)
            .unwrap();
        assert_eq!(read_back, out.launch_template);

        let all = store.read_launch_templates(&app, &profile, &rev).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], out.launch_template);

        // Rebuilding from the same inputs reproduces the same key hash → the
        // same on-disk path → an idempotent overwrite, not a duplicate.
        let (_, mut v2) = load_standard(&store, &app, &profile, dir.path(), "ba11");
        make_ready(&mut v2);
        let out2 = build_launch_template(build_input("ltmpl_rt2", v2)).unwrap();
        assert_eq!(out2.key.key_hash().unwrap(), key_hash);
        store
            .write_launch_template(&app, &profile, &rev, &out2.launch_template)
            .unwrap();
        assert_eq!(
            store
                .read_launch_templates(&app, &profile, &rev)
                .unwrap()
                .len(),
            1,
            "same key hash must not create a second persisted template"
        );
    }

    // ── Identity excludes session / observed / secret facts ──────────────────

    #[test]
    fn template_identity_excludes_session_and_observed_fields() {
        let (dir, store, app, profile) = setup();
        let (_rev, mut v) = load_standard(&store, &app, &profile, dir.path(), "ba12");
        make_ready(&mut v);
        let base = build_launch_template(build_input("ltmpl", v.clone())).unwrap();
        let base_key_hash = base.key.key_hash().unwrap();
        let base_template_hash = base.launch_template.template_hash.clone();

        // A launch happens, producing observed/session/secret facts. None of them
        // are inputs to the builder, the key, or the template — so rebuilding from
        // the same stable inputs is byte-identical. The struct exists so a future
        // refactor that tried to thread these into identity surfaces here.
        struct ObservedSessionFacts {
            session_id: &'static str,
            dynamic_port: u16,
            process_id: u32,
            container_id: &'static str,
            live_route: &'static str,
            log_cursor: &'static str,
            observed_status: &'static str,
            timestamp: &'static str,
            secret_value: &'static str,
        }
        let launch = ObservedSessionFacts {
            session_id: "ses_a",
            dynamic_port: 40001,
            process_id: 111,
            container_id: "ctr_a",
            live_route: "https://a.live",
            log_cursor: "cursor:1",
            observed_status: "running",
            timestamp: "2026-06-08T00:00:00Z",
            secret_value: "hunter2",
        };
        let _ = (
            launch.session_id,
            launch.dynamic_port,
            launch.process_id,
            launch.container_id,
            launch.live_route,
            launch.log_cursor,
            launch.observed_status,
            launch.timestamp,
            launch.secret_value,
        );

        let rebuilt = build_launch_template(build_input("ltmpl", v)).unwrap();
        assert_eq!(base_key_hash, rebuilt.key.key_hash().unwrap());
        assert_eq!(base_template_hash, rebuilt.launch_template.template_hash);

        // Spot-check: the serialized template contains none of the forbidden
        // values, so they cannot have leaked into any field.
        let json = serde_json::to_string(&base.launch_template).unwrap();
        for forbidden in [
            launch.session_id,
            launch.container_id,
            launch.live_route,
            launch.log_cursor,
            launch.observed_status,
            launch.secret_value,
        ] {
            assert!(
                !json.contains(forbidden),
                "launch template must not contain observed/session/secret value {forbidden:?}"
            );
        }
    }
}
