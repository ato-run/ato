//! Persisted launch-template **reuse read path** (#581 wave 5A).
//!
//! This is the safe read/validation boundary that answers one question:
//!
//! > Given an installed `(app, profile, revision)` and the stable
//! > launch-template inputs, is there a *persisted* [`LaunchTemplate`] that can
//! > be reused right now?
//!
//! It is deliberately **read + validate only**. It does not launch anything, it
//! does not create a [`LaunchMaterializationRecord`](super::materialization::LaunchMaterializationRecord),
//! it does not compute a session `execution_id`, and — crucially — it never
//! *auto-creates* a missing template. A missing template is a typed
//! [`PersistedLaunchTemplateReuseBlocker::TemplateMissing`], not a silent build.
//!
//! ## What it validates (in order)
//!
//! 1. The reusable install inputs load
//!    ([`ValidatedInstallReusableInputs::load`]).
//! 2. The inputs are [`LaunchTemplateReadiness::Ready`] *and* an expected
//!    [`LaunchTemplateKey`] can be derived — both are decided by reusing the
//!    #594 builder ([`build_launch_template`]) in memory. The builder refuses
//!    standard install (`NotReady`) with
//!    [`LaunchTemplateBuildError::InputsNotReady`], which maps to
//!    [`PersistedLaunchTemplateReuseBlocker::InputsNotReady`]. The built template
//!    is **not** persisted — it only yields the expected key.
//! 3. A persisted template exists at the expected key hash (missing vs. corrupt
//!    are distinguished cleanly: the path is probed for existence before read).
//! 4. The loaded template's `key.key_hash()` equals the requested key hash
//!    (the filename alone is never trusted — #594 follow-up).
//! 5. The loaded template's `key` equals the expected key.
//! 6. The loaded template is internally consistent
//!    ([`LaunchTemplate::validate_integrity`]): `template_hash` recomputes and
//!    the `runner_compatibility_class` field agrees with the key.
//! 7. Volatile revalidation gates reuse via the existing
//!    [`evaluate_launch_reuse`] model — a skipped or failed check blocks reuse
//!    explicitly and never silently succeeds.
//!
//! No session id, dynamic port, pid, container id, live route, log cursor,
//! observed status, timestamp-as-identity, or secret value participates in the
//! reuse identity. Volatile revalidation outcomes gate *whether* the cached
//! template may be used; they are not cache-key inputs.

use thiserror::Error;

use super::ids::{InstallRevisionId, InstalledAppId, ProfileId};
use super::launch_inputs::{
    InstallReusableInputValidationError, LaunchTemplateReadiness, ValidatedInstallReusableInputs,
};
use super::launch_reuse::{
    LaunchReuseDecision, LaunchReuseInputs, VolatileRevalidation, evaluate_launch_reuse,
};
use super::launch_template::{
    LaunchTemplate, LaunchTemplateIntegrityError, RunnerCompatibilityClass,
    RunnerCompatibilityClassParseError,
};
use super::launch_template_builder::{
    LaunchTemplateBuildError, LaunchTemplateBuildInput, build_launch_template,
};
use super::store::InstallInstanceStore;

/// Stable inputs to a persisted-launch-template reuse lookup (#581 wave 5A).
///
/// Mirrors the install-time inputs the #594 builder keys on — no session or
/// observed field appears here. `volatile_revalidation` is the *gate*, not an
/// identity input.
pub struct LaunchTemplateReuseInput {
    pub app: InstalledAppId,
    pub profile: ProfileId,
    pub revision: InstallRevisionId,
    /// Hash of the launch profile (`blake3:<hex>`). A stable install-time input.
    pub profile_hash: String,
    /// Hash of the resolved network policy (`blake3:<hex>`).
    pub network_policy_hash: String,
    /// Hash of the resolved capability policy (`blake3:<hex>`).
    pub capability_policy_hash: String,
    /// The coarse runner class the launch targets; must match the persisted
    /// template's class (via the key) and be supported by the compatibility
    /// index (enforced when deriving the expected key through the builder).
    pub runner_compatibility_class: RunnerCompatibilityClass,
    /// The already-collected volatile revalidation outcomes. This module does no
    /// real probing; callers supply typed outcomes (see [`VolatileRevalidation`]).
    pub volatile_revalidation: VolatileRevalidation,
}

/// The outcome of a persisted-launch-template reuse lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedLaunchTemplateReuseDecision {
    /// A persisted template matched every stable input, validated internally, and
    /// passed volatile revalidation: it may be reused.
    Reusable {
        // Boxed: LaunchTemplate dwarfs the NotReusable variant
        // (clippy::large_enum_variant).
        launch_template: Box<LaunchTemplate>,
        key_hash: String,
        reuse_decision: LaunchReuseDecision,
    },
    /// Reuse is not possible; `reasons` lists why (typically one, in the order
    /// the gates are evaluated).
    NotReusable {
        reasons: Vec<PersistedLaunchTemplateReuseBlocker>,
    },
}

impl PersistedLaunchTemplateReuseDecision {
    /// True only for [`Self::Reusable`].
    pub fn is_reusable(&self) -> bool {
        matches!(self, Self::Reusable { .. })
    }

    fn not_reusable(blocker: PersistedLaunchTemplateReuseBlocker) -> Self {
        Self::NotReusable {
            reasons: vec![blocker],
        }
    }
}

/// A typed reason a persisted template cannot be reused (#581 wave 5A).
///
/// Every variant is structured and auditable — never an in-band `"unknown"` /
/// `"unset"` sentinel. Carries only content hashes / typed reasons, never a
/// secret or observed value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PersistedLaunchTemplateReuseBlocker {
    /// The reusable install inputs could not be loaded / validated (missing or
    /// inconsistent records). Distinct from a successful load that is merely
    /// `NotReady`.
    #[error("reusable install inputs could not be loaded: {0}")]
    InputsLoadFailed(#[source] InstallReusableInputValidationError),
    /// The inputs loaded but are not ready for a launch template (the standard
    /// install path). Carries the typed readiness with its reasons.
    #[error("install inputs are not ready for launch-template reuse: {readiness:?}")]
    InputsNotReady { readiness: LaunchTemplateReadiness },
    /// The requested `runner_compatibility_class` could not be parsed to a coarse
    /// runner class for the reuse decision's compatibility check.
    #[error("requested runner compatibility class is unparsable: {0}")]
    RunnerCompatibilityClassUnparsable(#[source] RunnerCompatibilityClassParseError),
    /// The inputs are ready, but the *expected* launch template could not be
    /// derived (e.g. a runner class the index does not support, or a snapshot
    /// identity failure). Carries the builder's typed error.
    #[error("expected launch template cannot be built: {reason}")]
    ExpectedTemplateCannotBeBuilt { reason: LaunchTemplateBuildError },
    /// No persisted template exists at the expected key hash. The reuse path does
    /// **not** create one — that is a separate, explicit build/persist step.
    #[error("no persisted launch template at key hash {key_hash}")]
    TemplateMissing { key_hash: String },
    /// A template file exists at the expected key hash but could not be read or
    /// parsed (corruption) — distinct from [`Self::TemplateMissing`].
    #[error("persisted launch template at {key_hash} could not be read: {detail}")]
    TemplateReadFailed { key_hash: String, detail: String },
    /// The loaded template's `key.key_hash()` does not equal the requested key
    /// hash — the filename is not trusted on its own.
    #[error("loaded template key hash {actual} != requested {expected}")]
    TemplateKeyHashMismatch { expected: String, actual: String },
    /// The loaded template's `key` is not equal to the expected key (some stable
    /// input — binding/policy/state hash, runner class, … — differs even though
    /// the hash collided or the file was misfiled).
    #[error("loaded template key does not match the expected key")]
    TemplateKeyMismatch,
    /// The loaded template failed its internal self-consistency check (tampered /
    /// stale `template_hash`, or a `runner_compatibility_class` field that drifted
    /// from the key).
    #[error("loaded template failed integrity validation: {0}")]
    TemplateIntegrityInvalid(#[source] LaunchTemplateIntegrityError),
    /// Volatile revalidation did not pass (a check failed, or was skipped / not
    /// wired). Reuse is blocked explicitly — never a silent success. Carries a
    /// human-readable detail of the first blocking check.
    #[error("volatile revalidation blocked reuse: {detail}")]
    VolatileRevalidationBlocked { detail: String },
}

/// Validate a loaded [`LaunchTemplate`] against the expected key + key hash for
/// reuse (#581 wave 5A). Returns the matched key hash on success.
///
/// Checks, in order: the loaded template's own `key.key_hash()` equals
/// `expected_key_hash` (the filename is never trusted); the loaded `key` equals
/// `expected_key`; and the template is internally self-consistent
/// ([`LaunchTemplate::validate_integrity`]). Any failure is a typed
/// [`PersistedLaunchTemplateReuseBlocker`].
pub fn validate_launch_template_for_reuse(
    template: &LaunchTemplate,
    expected_key: &super::launch_template::LaunchTemplateKey,
    expected_key_hash: &str,
) -> Result<(), PersistedLaunchTemplateReuseBlocker> {
    // The loaded template must hash to the requested key hash. We recompute from
    // the loaded key, never trusting the filename it was read from.
    let actual_key_hash = template.key.key_hash().map_err(|e| {
        PersistedLaunchTemplateReuseBlocker::TemplateReadFailed {
            key_hash: expected_key_hash.to_owned(),
            detail: format!("recompute loaded key hash: {e:#}"),
        }
    })?;
    if actual_key_hash != expected_key_hash {
        return Err(
            PersistedLaunchTemplateReuseBlocker::TemplateKeyHashMismatch {
                expected: expected_key_hash.to_owned(),
                actual: actual_key_hash,
            },
        );
    }
    // Equal hashes are not enough: compare the full key structurally too.
    if &template.key != expected_key {
        return Err(PersistedLaunchTemplateReuseBlocker::TemplateKeyMismatch);
    }
    // Self-consistency: template_hash recomputes and runner class agrees with key.
    template
        .validate_integrity()
        .map_err(PersistedLaunchTemplateReuseBlocker::TemplateIntegrityInvalid)?;
    Ok(())
}

/// Evaluate whether a persisted launch template can be reused for `(app,
/// profile, revision)` under the given stable inputs (#581 wave 5A).
///
/// Read + validate only: never launches, never auto-creates a missing template.
/// See the [module docs](self) for the full validation order.
pub fn evaluate_persisted_launch_template_reuse(
    store: &InstallInstanceStore,
    input: LaunchTemplateReuseInput,
) -> PersistedLaunchTemplateReuseDecision {
    use PersistedLaunchTemplateReuseBlocker as B;

    let LaunchTemplateReuseInput {
        app,
        profile,
        revision,
        profile_hash,
        network_policy_hash,
        capability_policy_hash,
        runner_compatibility_class,
        volatile_revalidation,
    } = input;

    // 1. Load the reusable install inputs. A load/validation failure is a typed
    //    blocker distinct from a successful-but-NotReady load.
    let reusable_inputs =
        match ValidatedInstallReusableInputs::load(store, &app, &profile, &revision) {
            Ok(v) => v,
            Err(e) => {
                return PersistedLaunchTemplateReuseDecision::not_reusable(B::InputsLoadFailed(e));
            }
        };

    // Parse the requested runner class now (before consuming inputs) for the
    // volatile-revalidation compatibility check later.
    let selected_runner_class = match runner_compatibility_class.runner_class() {
        Ok(c) => c,
        Err(e) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(
                B::RunnerCompatibilityClassUnparsable(e),
            );
        }
    };

    // 2. Derive the expected template (and thus the expected key) by reusing the
    //    #594 builder in memory. This enforces readiness + RequireComplete +
    //    runner-class-supported. The built template is NOT persisted here.
    let expected = match build_launch_template(LaunchTemplateBuildInput {
        template_id: "reuse-probe".to_owned(),
        reusable_inputs,
        profile_hash,
        network_policy_hash,
        capability_policy_hash,
        runner_compatibility_class,
    }) {
        Ok(out) => out,
        Err(LaunchTemplateBuildError::InputsNotReady { readiness }) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(B::InputsNotReady {
                readiness,
            });
        }
        Err(reason) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(
                B::ExpectedTemplateCannotBeBuilt { reason },
            );
        }
    };
    let expected_key = expected.key;
    let expected_key_hash = match expected_key.key_hash() {
        Ok(h) => h,
        Err(e) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(
                B::ExpectedTemplateCannotBeBuilt {
                    reason: LaunchTemplateBuildError::KeyConstructionFailed {
                        detail: format!("hash expected key: {e:#}"),
                    },
                },
            );
        }
    };

    // 3. A persisted template must already exist at the expected key hash.
    //    Probe existence first so "missing" and "corrupt" are cleanly distinct.
    let path = store.revision_launch_template_path(&app, &profile, &revision, &expected_key_hash);
    if !path.exists() {
        return PersistedLaunchTemplateReuseDecision::not_reusable(B::TemplateMissing {
            key_hash: expected_key_hash,
        });
    }
    let loaded = match store.read_launch_template(&app, &profile, &revision, &expected_key_hash) {
        Ok(t) => t,
        Err(e) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(B::TemplateReadFailed {
                key_hash: expected_key_hash,
                detail: format!("{e:#}"),
            });
        }
    };

    // 4-6. Key hash match, key match, internal integrity.
    if let Err(blocker) =
        validate_launch_template_for_reuse(&loaded, &expected_key, &expected_key_hash)
    {
        return PersistedLaunchTemplateReuseDecision::not_reusable(blocker);
    }

    // 7. Volatile revalidation gates reuse. The cached template is the one we just
    //    validated; its key already matches, so `evaluate_launch_reuse` returns
    //    `Reuse` iff every volatile check passed, and `Blocked` otherwise.
    let reuse_inputs = LaunchReuseInputs {
        key: expected_key,
        selected_runner_class,
    };
    let decision = match evaluate_launch_reuse(&reuse_inputs, Some(&loaded), &volatile_revalidation)
    {
        Ok(d) => d,
        Err(e) => {
            return PersistedLaunchTemplateReuseDecision::not_reusable(B::TemplateReadFailed {
                key_hash: expected_key_hash,
                detail: format!("evaluate launch reuse: {e:#}"),
            });
        }
    };

    match decision {
        LaunchReuseDecision::Reuse { template_key_hash } => {
            PersistedLaunchTemplateReuseDecision::Reusable {
                launch_template: Box::new(loaded),
                key_hash: template_key_hash.clone(),
                reuse_decision: LaunchReuseDecision::Reuse { template_key_hash },
            }
        }
        LaunchReuseDecision::Blocked(failure) => {
            PersistedLaunchTemplateReuseDecision::not_reusable(B::VolatileRevalidationBlocked {
                detail: format!("{:?}: {}", failure.kind, failure.detail),
            })
        }
        // Unreachable: we validated the loaded key == expected key, so the cache
        // hashes match and `evaluate_launch_reuse` cannot ask to rebuild. Treat a
        // surprise rebuild as a key-hash mismatch rather than panicking.
        LaunchReuseDecision::RebuildTemplate { .. } => {
            PersistedLaunchTemplateReuseDecision::not_reusable(B::TemplateKeyHashMismatch {
                expected: expected_key_hash.clone(),
                actual: loaded
                    .key
                    .key_hash()
                    .unwrap_or_else(|_| expected_key_hash.clone()),
            })
        }
    }
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
        CompatibilityIndex, CompatibilityIndexCompleteness, LaunchTemplate, RequirementBinding,
        RequirementBindingKind, RunnerClass,
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

    // ── Harness (mirrors launch_template_builder.rs test helpers) ────────────

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_launch_template_reuse_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "launch-template-reuse".into(),
                capsule_handle: "test/launch-template-reuse".into(),
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

    fn reuse_input(
        app: &InstalledAppId,
        profile: &ProfileId,
        rev: &InstallRevisionId,
        revalidation: VolatileRevalidation,
    ) -> LaunchTemplateReuseInput {
        LaunchTemplateReuseInput {
            app: app.clone(),
            profile: profile.clone(),
            revision: rev.clone(),
            profile_hash: "blake3:prof".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
            volatile_revalidation: revalidation,
        }
    }

    // NOTE: The reuse path forces the requirement graph + compatibility index to
    // `Ready` in-memory before building the *expected* key. So on the standard
    // install path (graph + index Partial on disk), `evaluate_persisted...` sees
    // `NotReady` and never reaches the persisted template. The "persisted" tests
    // below therefore stub a Ready-on-disk world by *also* persisting a finalized
    // revision whose on-disk records are forced Ready — see
    // `persist_ready_on_disk`.

    /// Persist a `(app, profile, revision)` whose on-disk reusable records are
    /// forced to a Ready shape (Complete graph, Complete compat index with a
    /// supported runner class, one resolved binding) AND persist the matching
    /// launch template. This makes `evaluate_persisted_launch_template_reuse`
    /// load Ready inputs from disk so the full read path is exercised.
    fn persist_ready_on_disk(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        out_base: &std::path::Path,
        build_suffix: &str,
    ) -> (InstallRevisionId, LaunchTemplate) {
        let out = finalize_standard(store, app, profile, out_base, build_suffix);
        let rev = out.install_revision_id.clone();

        // Load, force Ready, and rewrite the standalone records + the embedded
        // copies in revision.json so a fresh load() returns Ready and passes every
        // cross-check.
        let mut v = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        make_ready(&mut v);

        // Rebuild the binding set against the (unchanged) graph content hash so the
        // binding↔graph cross-check holds, then refresh the receipt's audit hashes.
        let bset = crate::foundation::install_lifecycle::launch_template::BindingAssignmentSet::new(
            "bset_ready",
            v.install_revision.install_profile_key.clone(),
            v.requirement_graph.graph_hash.clone(),
            vec![one_binding()],
            crate::foundation::install_lifecycle::launch_template::BindingAssignmentSource::ProfileExplicit,
        )
        .unwrap();
        v.binding_assignment_set = bset.clone();

        let mut receipt = v.install_receipt.clone();
        receipt.binding_set_hash = Some(bset.binding_set_hash.clone());
        receipt.compatibility_precheck_hash = Some(v.compatibility_index.precheck_hash.clone());
        v.install_receipt = receipt.clone();

        // Write standalone records.
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

        // Update the embedded copies in revision.json (the authority).
        let mut revision = store.read_install_revision(app, profile, &rev).unwrap();
        revision.requirement_graph = v.requirement_graph.clone();
        revision.binding_assignment_set = Some(bset);
        revision.compatibility_index = Some(v.compatibility_index.clone());
        revision.install_receipt = receipt;
        store
            .write_install_revision(app, profile, &revision)
            .unwrap();

        // Sanity: a fresh load is now Ready.
        let reloaded = ValidatedInstallReusableInputs::load(store, app, profile, &rev).unwrap();
        assert!(
            reloaded.launch_template_readiness().is_ready(),
            "on-disk records must load as Ready for the reuse read path"
        );

        // Build + persist the matching template.
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
        (rev, built.launch_template)
    }

    // ── 1. Standard install is not reusable, creates nothing ─────────────────

    #[test]
    fn standard_install_is_not_reusable() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "ca01");
        let rev = out.install_revision_id.clone();

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => {
                assert!(
                    reasons.iter().any(|r| matches!(
                        r,
                        PersistedLaunchTemplateReuseBlocker::InputsNotReady { .. }
                    )),
                    "got {reasons:?}"
                );
            }
            PersistedLaunchTemplateReuseDecision::Reusable { .. } => {
                panic!("standard install must not be reusable")
            }
        }
    }

    #[test]
    fn reuse_lookup_does_not_persist_missing_template() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "ca02");
        let rev = out.install_revision_id.clone();

        let _ = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );

        // Nothing was written to the standalone launch-templates dir.
        assert!(
            store
                .read_launch_templates(&app, &profile, &rev)
                .unwrap()
                .is_empty(),
            "reuse lookup must never create a template"
        );
    }

    // ── 3. Complete fixture with persisted template is reusable ──────────────

    #[test]
    fn complete_fixture_with_persisted_template_is_reusable_after_revalidation() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca03");

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::Reusable {
                launch_template,
                key_hash,
                reuse_decision,
            } => {
                assert_eq!(*launch_template, template);
                assert_eq!(key_hash, template.key.key_hash().unwrap());
                assert!(matches!(reuse_decision, LaunchReuseDecision::Reuse { .. }));
            }
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => {
                panic!("expected Reusable, got {reasons:?}")
            }
        }
    }

    // ── 4. Missing template returns typed NotReusable, no rebuild ─────────────

    #[test]
    fn reuse_lookup_reports_missing_template() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca04");

        // Delete the persisted template, leaving Ready inputs on disk.
        let key_hash = template.key.key_hash().unwrap();
        let path = store.revision_launch_template_path(&app, &profile, &rev, &key_hash);
        fs::remove_file(&path).unwrap();

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => {
                assert!(
                    reasons.iter().any(|r| matches!(
                        r,
                        PersistedLaunchTemplateReuseBlocker::TemplateMissing { .. }
                    )),
                    "got {reasons:?}"
                );
            }
            PersistedLaunchTemplateReuseDecision::Reusable { .. } => {
                panic!("missing template must not be reusable")
            }
        }
        // And it was not silently recreated.
        assert!(
            !path.exists(),
            "reuse lookup must not recreate the template"
        );
    }

    // ── 5. Wrong key hash in the file (filename not trusted) ─────────────────

    #[test]
    fn reuse_lookup_rejects_template_file_with_wrong_key_hash() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca05");
        let key_hash = template.key.key_hash().unwrap();

        // Write a DIFFERENT template's bytes into the expected-key-hash filename.
        // Its own key hashes differently, so the loaded key hash won't match the
        // path it was read from.
        let mut wrong = template.clone();
        wrong.key.binding_set_hash = "blake3:DIFFERENT".into();
        let wrong = LaunchTemplate::new(
            "ltmpl_wrong",
            wrong.key,
            wrong.profile_ref,
            wrong.artifact_ref,
            wrong.requirement_graph_ref,
            wrong.binding_assignment_set_ref,
            wrong.filesystem_view_template_hash,
            wrong.network_policy_template_hash,
            wrong.capability_policy_template_hash,
        )
        .unwrap();
        // Overwrite the file at the EXPECTED key-hash path with the wrong template.
        let path = store.revision_launch_template_path(&app, &profile, &rev, &key_hash);
        fs::write(&path, serde_json::to_vec_pretty(&wrong).unwrap()).unwrap();

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    PersistedLaunchTemplateReuseBlocker::TemplateKeyHashMismatch { .. }
                )),
                "got {reasons:?}"
            ),
            other => panic!("expected NotReusable key-hash mismatch, got {other:?}"),
        }
    }

    // ── 6. Wrong key (same hash slot, structurally different) ────────────────
    //
    // A structural key mismatch that still collides on key_hash is not
    // realistically constructible (blake3), so we exercise the key-comparison
    // guard directly via the validate helper with a deliberately mismatched
    // expected key whose hash we pass as the "expected" hash.

    #[test]
    fn reuse_lookup_rejects_template_file_with_wrong_key() {
        let (dir, store, app, profile) = setup();
        let (_rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca06");

        // Build an expected key that differs from the loaded template's key, but
        // pass the LOADED template's own key hash as the "expected" hash. The
        // key-hash check passes (we feed the loaded hash), so the structural
        // key-comparison guard is what must fire.
        let loaded_hash = template.key.key_hash().unwrap();
        let mut expected_key = template.key.clone();
        expected_key.network_policy_hash = "blake3:OTHER".into();

        let err =
            validate_launch_template_for_reuse(&template, &expected_key, &loaded_hash).unwrap_err();
        assert!(
            matches!(
                err,
                PersistedLaunchTemplateReuseBlocker::TemplateKeyMismatch
            ),
            "got {err:?}"
        );
    }

    // ── 7. Tampered template_hash ────────────────────────────────────────────

    #[test]
    fn reuse_lookup_rejects_tampered_template_hash() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca07");
        let key_hash = template.key.key_hash().unwrap();

        // Tamper only the template_hash on disk (key untouched → same filename).
        let mut tampered = template.clone();
        tampered.template_hash = "blake3:TAMPERED".into();
        let path = store.revision_launch_template_path(&app, &profile, &rev, &key_hash);
        fs::write(&path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    PersistedLaunchTemplateReuseBlocker::TemplateIntegrityInvalid(
                        LaunchTemplateIntegrityError::TemplateHashMismatch { .. }
                    )
                )),
                "got {reasons:?}"
            ),
            other => panic!("expected NotReusable integrity mismatch, got {other:?}"),
        }
    }

    // ── 8. Wrong runner compatibility class ──────────────────────────────────

    #[test]
    fn reuse_lookup_rejects_wrong_runner_compatibility_class() {
        let (dir, store, app, profile) = setup();
        let (rev, _template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca08");

        // Request a different runner compatibility class. The compatibility index
        // only supports ManagedRunner, so the builder refuses to derive an
        // expected key for browser_runner → ExpectedTemplateCannotBeBuilt
        // (RunnerCompatibilityClassNotSupported).
        let mut input = reuse_input(&app, &profile, &rev, all_ok());
        input.runner_compatibility_class = RunnerCompatibilityClass::new("browser_runner/wasm");

        let decision = evaluate_persisted_launch_template_reuse(&store, input);
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    PersistedLaunchTemplateReuseBlocker::ExpectedTemplateCannotBeBuilt {
                        reason: LaunchTemplateBuildError::RunnerCompatibilityClassNotSupported { .. }
                    }
                )),
                "got {reasons:?}"
            ),
            other => panic!("expected NotReusable wrong-runner-class, got {other:?}"),
        }
    }

    /// A persisted template whose `key.runner_compatibility_class` differs from
    /// the request is also rejected — via key mismatch — when the index *does*
    /// support both classes. Exercises the key path rather than the build gate.
    #[test]
    fn reuse_lookup_rejects_template_with_mismatched_runner_class_via_key() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca09");

        // Validate-helper level: an expected key for a different runner class must
        // not match the loaded (managed) template.
        let loaded_hash = template.key.key_hash().unwrap();
        let mut expected_key = template.key.clone();
        expected_key.runner_compatibility_class =
            RunnerCompatibilityClass::new("browser_runner/wasm");
        // Feed the loaded hash so the key-hash check passes and the key compare fires.
        let err =
            validate_launch_template_for_reuse(&template, &expected_key, &loaded_hash).unwrap_err();
        assert!(matches!(
            err,
            PersistedLaunchTemplateReuseBlocker::TemplateKeyMismatch
        ));
        // Belt-and-suspenders: the persisted file is still intact.
        let _ = store
            .read_launch_template(&app, &profile, &rev, &loaded_hash)
            .unwrap();
    }

    // ── 9-11. Volatile revalidation gates ────────────────────────────────────

    #[test]
    fn reuse_lookup_blocks_skipped_revalidation() {
        let (dir, store, app, profile) = setup();
        let (rev, _template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca10");

        let mut reval = all_ok();
        reval.secret_refs = RevalidationOutcome::Skipped {
            reason: "secret manager probe not implemented".into(),
        };
        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, reval),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    PersistedLaunchTemplateReuseBlocker::VolatileRevalidationBlocked { .. }
                )),
                "skipped revalidation must block; got {reasons:?}"
            ),
            other => panic!("expected NotReusable, got {other:?}"),
        }
    }

    #[test]
    fn reuse_lookup_blocks_failed_revalidation() {
        let (dir, store, app, profile) = setup();
        let (rev, _template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca11");

        let mut reval = all_ok();
        reval.consent = RevalidationOutcome::Failed(RevalidationFailure::new(
            RevalidationFailureKind::ConsentRevoked,
            "user revoked consent",
        ));
        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, reval),
        );
        match decision {
            PersistedLaunchTemplateReuseDecision::NotReusable { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    PersistedLaunchTemplateReuseBlocker::VolatileRevalidationBlocked { .. }
                )),
                "failed revalidation must block; got {reasons:?}"
            ),
            other => panic!("expected NotReusable, got {other:?}"),
        }
    }

    #[test]
    fn reuse_lookup_allows_passed_revalidation() {
        let (dir, store, app, profile) = setup();
        let (rev, _template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca12");

        let decision = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        assert!(
            decision.is_reusable(),
            "all-passed revalidation must allow reuse; got {decision:?}"
        );
    }

    // ── 12. Reuse identity excludes session / observed facts ─────────────────

    #[test]
    fn reuse_identity_excludes_session_and_observed_fields() {
        let (dir, store, app, profile) = setup();
        let (rev, template) = persist_ready_on_disk(&store, &app, &profile, dir.path(), "ca13");

        // Two lookups whose only difference is observed/session facts (which are
        // not inputs to this API at all) must produce identical reuse identity.
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
        let facts = ObservedSessionFacts {
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
            facts.session_id,
            facts.dynamic_port,
            facts.process_id,
            facts.container_id,
            facts.live_route,
            facts.log_cursor,
            facts.observed_status,
            facts.timestamp,
            facts.secret_value,
        );

        let d1 = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        let d2 = evaluate_persisted_launch_template_reuse(
            &store,
            reuse_input(&app, &profile, &rev, all_ok()),
        );
        assert_eq!(d1, d2, "reuse identity must not depend on observed facts");
        assert!(d1.is_reusable());

        // The persisted template carries none of the forbidden values.
        let json = serde_json::to_string(&template).unwrap();
        for forbidden in [
            facts.session_id,
            facts.container_id,
            facts.live_route,
            facts.log_cursor,
            facts.observed_status,
            facts.secret_value,
        ] {
            assert!(
                !json.contains(forbidden),
                "persisted template must not contain observed/session/secret value {forbidden:?}"
            );
        }
    }
}
