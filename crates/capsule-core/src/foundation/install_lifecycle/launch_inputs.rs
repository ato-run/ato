//! Read-validation layer for install-time reusable records (#581 wave 4B).
//!
//! This module loads the per-`(app, profile, revision)` records an install
//! finalized — [`InstallRevision`] (the authority), the standalone
//! [`RequirementGraphSnapshot`], [`BindingAssignmentSet`],
//! [`CompatibilityIndex`], [`StateContractSnapshot`]s, and [`InstallReceipt`] —
//! and validates them into a [`ValidatedInstallReusableInputs`] that a *future*
//! launch-template wave can consume as a safe input boundary.
//!
//! It is deliberately a **read + validate** layer only. It does **not** implement
//! launch reuse, it does **not** persist a [`LaunchTemplate`](super::launch_template::LaunchTemplate),
//! and it never fabricates readiness. The standard install path loads
//! successfully but is reported [`LaunchTemplateReadiness::NotReady`] because its
//! requirement graph and compatibility index are `Partial` and it has no resolved
//! bindings.
//!
//! ## What is validated
//!
//! 1. The revision is finalized (`revision.json` exists — it is the marker).
//! 2. `revision.json` reads successfully (the embedded authority).
//! 3. Each standalone record file exists and reads successfully.
//! 4. Each standalone record equals the copy embedded in `revision.json`
//!    (the finalizer writes both from one in-memory value, so any divergence is
//!    corruption / tampering and is rejected).
//! 5. `binding_assignment_set.requirement_graph_hash == requirement_graph.graph_hash`
//!    (the binding set is bound to the requirement-graph **content** hash).
//! 6. `install_receipt.binding_set_hash == binding_assignment_set.binding_set_hash`.
//! 7. `install_receipt.compatibility_precheck_hash == compatibility_index.precheck_hash`.
//! 8. The requirement-graph snapshot identity validates through the explicit
//!    recompute-and-compare helper
//!    ([`RequirementGraphSnapshot::validate_for_launch_template`]) under
//!    [`RequirementGraphCompletenessPolicy::AllowPartial`] — so a `Partial` graph
//!    loads, but a raw/empty/stale snapshot hash is rejected.
//!
//! A `Partial` requirement graph or compatibility index is a typed *readiness*
//! state ([`LaunchTemplateReadiness::NotReady`]), never silently treated as
//! "compatible" or "complete". Nothing computed here is a hash input or a
//! persisted identity: no session id, port, pid, container id, route, log cursor,
//! observed runtime status, timestamp-as-identity, or secret value enters any
//! field of these types.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ids::{InstallRevisionId, InstalledAppId, ProfileId};
use super::launch_template::{BindingAssignmentSet, CompatibilityIndex};
use super::records::{
    InstallReceipt, InstallRevision, RequirementGraphCompletenessPolicy, RequirementGraphSnapshot,
    RequirementGraphSnapshotIdentityError, StateContractSnapshot,
};
use super::store::InstallInstanceStore;

/// Typed failure when loading/validating reusable install inputs (#581 wave 4B).
///
/// Every variant is a structured, auditable reason — there is no opaque
/// `anyhow!("invalid")` path. The `*Mismatch` variants carry the conflicting
/// **content hashes** (never secrets) so a caller can log precisely what diverged.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstallReusableInputValidationError {
    /// `revision.json` is absent: the revision was never finalized (the marker is
    /// written last, after all sub-records succeed).
    #[error("revision is not finalized: revision.json is missing")]
    RevisionNotFinalized,
    /// `revision.json` is present (revision finalized) but could not be read or
    /// parsed — corruption, not an un-finalized revision.
    #[error("revision.json is present but unreadable: {detail}")]
    RevisionUnreadable { detail: String },
    /// `requirement-graph.json` is missing or unreadable.
    #[error("requirement-graph.json is missing or unreadable: {detail}")]
    MissingRequirementGraph { detail: String },
    /// `binding-assignments.json` is missing or unreadable.
    #[error("binding-assignments.json is missing or unreadable: {detail}")]
    MissingBindingAssignmentSet { detail: String },
    /// `compatibility-index.json` is missing or unreadable.
    #[error("compatibility-index.json is missing or unreadable: {detail}")]
    MissingCompatibilityIndex { detail: String },
    /// `state-contracts.json` is missing or unreadable.
    #[error("state-contracts.json is missing or unreadable: {detail}")]
    MissingStateContracts { detail: String },
    /// `install-receipt.json` is missing or unreadable.
    #[error("install-receipt.json is missing or unreadable: {detail}")]
    MissingInstallReceipt { detail: String },
    /// Standalone `requirement-graph.json` ≠ the copy embedded in `revision.json`.
    #[error("standalone requirement-graph.json does not match the copy embedded in revision.json")]
    EmbeddedRequirementGraphMismatch,
    /// Standalone `binding-assignments.json` ≠ the copy embedded in `revision.json`
    /// (including the case where `revision.json` embeds no binding set at all).
    #[error(
        "standalone binding-assignments.json does not match the copy embedded in revision.json"
    )]
    EmbeddedBindingAssignmentMismatch,
    /// Standalone `compatibility-index.json` ≠ the copy embedded in `revision.json`
    /// (including the case where `revision.json` embeds no compatibility index).
    #[error(
        "standalone compatibility-index.json does not match the copy embedded in revision.json"
    )]
    EmbeddedCompatibilityIndexMismatch,
    /// Standalone `state-contracts.json` ≠ the copy embedded in `revision.json`.
    #[error("standalone state-contracts.json does not match the copy embedded in revision.json")]
    EmbeddedStateContractsMismatch,
    /// Standalone `install-receipt.json` ≠ the copy embedded in `revision.json`.
    #[error("standalone install-receipt.json does not match the copy embedded in revision.json")]
    EmbeddedInstallReceiptMismatch,
    /// `binding_assignment_set.requirement_graph_hash` ≠ `requirement_graph.graph_hash`:
    /// the binding set was resolved against a different requirement-graph content.
    #[error(
        "binding set requirement_graph_hash ({binding_graph_hash}) != requirement graph graph_hash ({snapshot_graph_hash})"
    )]
    BindingRequirementGraphMismatch {
        binding_graph_hash: String,
        snapshot_graph_hash: String,
    },
    /// `install_receipt.binding_set_hash` ≠ `binding_assignment_set.binding_set_hash`
    /// (or the receipt recorded no binding-set hash).
    #[error(
        "install receipt binding_set_hash ({receipt:?}) != binding set binding_set_hash ({binding_set})"
    )]
    ReceiptBindingHashMismatch {
        receipt: Option<String>,
        binding_set: String,
    },
    /// `install_receipt.compatibility_precheck_hash` ≠ `compatibility_index.precheck_hash`
    /// (or the receipt recorded no precheck hash).
    #[error(
        "install receipt compatibility_precheck_hash ({receipt:?}) != compatibility index precheck_hash ({index})"
    )]
    ReceiptCompatibilityHashMismatch {
        receipt: Option<String>,
        index: String,
    },
    /// The requirement-graph snapshot identity failed the recompute-and-compare
    /// check (missing/empty hash, non-`blake3:` format, or a stored hash that does
    /// not match a fresh recompute — e.g. a raw content-only `graph_hash`).
    #[error("requirement-graph snapshot identity is invalid: {0}")]
    RequirementGraphSnapshotInvalid(#[source] RequirementGraphSnapshotIdentityError),
    /// Recomputing the snapshot identity itself failed (e.g. serialization error).
    /// Distinct from [`Self::RequirementGraphSnapshotInvalid`] so a rare internal
    /// failure is never mislabelled as a tampered identity.
    #[error("requirement-graph snapshot identity could not be recomputed: {detail}")]
    RequirementGraphSnapshotRecomputeFailed { detail: String },
}

/// Why a validated set of inputs is not yet sufficient for a *real*
/// [`LaunchTemplate`](super::launch_template::LaunchTemplate) (#581 wave 4B).
///
/// Typed unit reasons — never an in-band `unknown`/`unset` sentinel string. Not a
/// hash input and not persisted by this layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchTemplateReadinessReason {
    /// The compiled requirement graph is still `Partial`.
    RequirementGraphPartial,
    /// The compatibility index is still `Partial` — no runner class is proven
    /// supported.
    CompatibilityIndexPartial,
    /// The binding-assignment set has no resolved bindings.
    NoResolvedBindings,
}

/// Whether validated inputs are sufficient to build a real launch template.
///
/// This is a *derived view* of the loaded records, recomputed on demand. It does
/// not fabricate readiness: the standard install path is `NotReady`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LaunchTemplateReadiness {
    /// Requirement graph and compatibility index are `Complete` and at least one
    /// binding is resolved. (Unreachable on the standard install path today.)
    Ready,
    /// Not yet usable for a real launch template; `reasons` lists why.
    NotReady {
        reasons: Vec<LaunchTemplateReadinessReason>,
    },
}

impl LaunchTemplateReadiness {
    /// True only for [`LaunchTemplateReadiness::Ready`].
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Validated, internally-consistent install-time reusable records for one
/// `(app, profile, revision)` (#581 wave 4B).
///
/// Holding a value of this type means every check in the [module docs](self)
/// passed. It is the safe input boundary a future launch-template wave consumes;
/// it is **not** itself a launch template and is not persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedInstallReusableInputs {
    /// The finalized revision authority (`revision.json`).
    pub install_revision: InstallRevision,
    /// The compiled requirement-graph snapshot (validated identity).
    pub requirement_graph: RequirementGraphSnapshot,
    /// The normalized binding-assignment set (may have empty `assignments`).
    pub binding_assignment_set: BindingAssignmentSet,
    /// The runner-class / capability precheck (may be `Partial`).
    pub compatibility_index: CompatibilityIndex,
    /// State-contract snapshots (may be empty).
    pub state_contracts: Vec<StateContractSnapshot>,
    /// The install-time audit receipt.
    pub install_receipt: InstallReceipt,
}

impl ValidatedInstallReusableInputs {
    /// Load + validate the reusable records for `(app, profile, revision)`.
    ///
    /// Returns a typed [`InstallReusableInputValidationError`] on any missing /
    /// malformed / inconsistent record. A `Partial` requirement graph or
    /// compatibility index is **not** an error here — it loads, and the
    /// Partial-ness is reported by [`Self::launch_template_readiness`].
    pub fn load(
        store: &InstallInstanceStore,
        app: &InstalledAppId,
        profile: &ProfileId,
        revision: &InstallRevisionId,
    ) -> Result<Self, InstallReusableInputValidationError> {
        use InstallReusableInputValidationError as E;

        // 1. `revision.json` is the finalization marker. A scaffolded-but-
        //    incomplete revision (sub-records partially written, marker absent) is
        //    explicitly not finalized.
        if !store.is_revision_finalized(app, profile, revision) {
            return Err(E::RevisionNotFinalized);
        }

        // 2. Read the embedded authority. Marker present but unreadable = corrupt
        //    revision.json, distinct from "not finalized".
        let install_revision = store
            .read_install_revision(app, profile, revision)
            .map_err(|e| E::RevisionUnreadable {
                detail: format!("{e:#}"),
            })?;

        // 3. Read each standalone record. Missing or malformed → typed error.
        let requirement_graph = store
            .read_requirement_graph_snapshot(app, profile, revision)
            .map_err(|e| E::MissingRequirementGraph {
                detail: format!("{e:#}"),
            })?;
        let binding_assignment_set = store
            .read_binding_assignment_set(app, profile, revision)
            .map_err(|e| E::MissingBindingAssignmentSet {
                detail: format!("{e:#}"),
            })?;
        let compatibility_index = store
            .read_compatibility_index(app, profile, revision)
            .map_err(|e| E::MissingCompatibilityIndex {
                detail: format!("{e:#}"),
            })?;
        let state_contracts = store
            .read_state_contracts(app, profile, revision)
            .map_err(|e| E::MissingStateContracts {
                detail: format!("{e:#}"),
            })?;
        let install_receipt = store
            .read_install_receipt(app, profile, revision)
            .map_err(|e| E::MissingInstallReceipt {
                detail: format!("{e:#}"),
            })?;

        // 4. Standalone records must equal the copies embedded in revision.json
        //    (the authority). The finalizer writes both sides from one in-memory
        //    value, so any divergence is corruption / tampering.
        if install_revision.requirement_graph != requirement_graph {
            return Err(E::EmbeddedRequirementGraphMismatch);
        }
        match install_revision.binding_assignment_set.as_ref() {
            Some(embedded) if *embedded == binding_assignment_set => {}
            _ => return Err(E::EmbeddedBindingAssignmentMismatch),
        }
        match install_revision.compatibility_index.as_ref() {
            Some(embedded) if *embedded == compatibility_index => {}
            _ => return Err(E::EmbeddedCompatibilityIndexMismatch),
        }
        if install_revision.state_contracts != state_contracts {
            return Err(E::EmbeddedStateContractsMismatch);
        }
        if install_revision.install_receipt != install_receipt {
            return Err(E::EmbeddedInstallReceiptMismatch);
        }

        // 5. The binding set is bound to the requirement-graph CONTENT hash
        //    (`graph_hash`), not the snapshot identity.
        if binding_assignment_set.requirement_graph_hash != requirement_graph.graph_hash {
            return Err(E::BindingRequirementGraphMismatch {
                binding_graph_hash: binding_assignment_set.requirement_graph_hash.clone(),
                snapshot_graph_hash: requirement_graph.graph_hash.clone(),
            });
        }

        // 6. The receipt's audit hash must match the binding set it describes.
        if install_receipt.binding_set_hash.as_deref()
            != Some(binding_assignment_set.binding_set_hash.as_str())
        {
            return Err(E::ReceiptBindingHashMismatch {
                receipt: install_receipt.binding_set_hash.clone(),
                binding_set: binding_assignment_set.binding_set_hash.clone(),
            });
        }

        // 7. The receipt's audit hash must match the compatibility index it
        //    describes.
        if install_receipt.compatibility_precheck_hash.as_deref()
            != Some(compatibility_index.precheck_hash.as_str())
        {
            return Err(E::ReceiptCompatibilityHashMismatch {
                receipt: install_receipt.compatibility_precheck_hash.clone(),
                index: compatibility_index.precheck_hash.clone(),
            });
        }

        // 8. Snapshot identity, via the explicit recompute-and-compare helper.
        //    AllowPartial: the standard install is `Partial` and must still load;
        //    its Partial-ness surfaces in readiness, not as a load error. The
        //    helper still rejects an empty/raw/stale snapshot hash.
        if let Err(e) = requirement_graph
            .validate_for_launch_template(RequirementGraphCompletenessPolicy::AllowPartial)
        {
            return Err(
                match e.downcast::<RequirementGraphSnapshotIdentityError>() {
                    Ok(identity) => E::RequirementGraphSnapshotInvalid(identity),
                    Err(other) => E::RequirementGraphSnapshotRecomputeFailed {
                        detail: format!("{other:#}"),
                    },
                },
            );
        }

        Ok(Self {
            install_revision,
            requirement_graph,
            binding_assignment_set,
            compatibility_index,
            state_contracts,
            install_receipt,
        })
    }

    /// Whether these validated inputs are sufficient to build a *real*
    /// [`LaunchTemplate`](super::launch_template::LaunchTemplate).
    ///
    /// Never fabricates readiness. A standard install is `NotReady` with reasons
    /// [`RequirementGraphPartial`](LaunchTemplateReadinessReason::RequirementGraphPartial),
    /// [`CompatibilityIndexPartial`](LaunchTemplateReadinessReason::CompatibilityIndexPartial),
    /// and [`NoResolvedBindings`](LaunchTemplateReadinessReason::NoResolvedBindings).
    /// Reasons are emitted in a fixed order, so the result is deterministic.
    pub fn launch_template_readiness(&self) -> LaunchTemplateReadiness {
        let mut reasons = Vec::new();
        if !self.requirement_graph.completeness.is_complete() {
            reasons.push(LaunchTemplateReadinessReason::RequirementGraphPartial);
        }
        if !self.compatibility_index.completeness.is_complete() {
            reasons.push(LaunchTemplateReadinessReason::CompatibilityIndexPartial);
        }
        if self.binding_assignment_set.assignments.is_empty() {
            reasons.push(LaunchTemplateReadinessReason::NoResolvedBindings);
        }
        if reasons.is_empty() {
            LaunchTemplateReadiness::Ready
        } else {
            LaunchTemplateReadiness::NotReady { reasons }
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
    use crate::foundation::install_lifecycle::store::{AppRecord, LaunchProfile};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── Harness (mirrors the finalizer's own test helpers) ──────────────────

    fn setup() -> (TempDir, InstallInstanceStore, InstalledAppId, ProfileId) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app = InstalledAppId::new("app_launch_inputs_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app.clone(),
                publisher: "test".into(),
                slug: "launch-inputs".into(),
                capsule_handle: "test/launch-inputs".into(),
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

    /// Finalize one standard install and return everything needed to load it.
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

    // ── Required test 1 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_load_standard_install_records() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa01");
        let rev = out.install_revision_id.clone();

        let validated = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev)
            .expect("standard install must load");

        // The loaded records equal the finalizer's authority + embedded copies.
        assert_eq!(validated.install_revision, out.install_revision);
        assert_eq!(
            validated.requirement_graph,
            out.install_revision.requirement_graph
        );
        assert_eq!(
            Some(&validated.binding_assignment_set),
            out.install_revision.binding_assignment_set.as_ref()
        );
        assert_eq!(
            Some(&validated.compatibility_index),
            out.install_revision.compatibility_index.as_ref()
        );
        assert_eq!(
            validated.state_contracts,
            out.install_revision.state_contracts
        );
        assert_eq!(
            validated.install_receipt,
            out.install_revision.install_receipt
        );
    }

    // ── Required test 2 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_report_standard_install_not_ready() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa02");
        let validated =
            ValidatedInstallReusableInputs::load(&store, &app, &profile, &out.install_revision_id)
                .unwrap();

        let readiness = validated.launch_template_readiness();
        assert!(!readiness.is_ready(), "standard install must not be Ready");
        match readiness {
            LaunchTemplateReadiness::NotReady { reasons } => {
                assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::RequirementGraphPartial),
                    "expected RequirementGraphPartial, got {reasons:?}"
                );
                assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::CompatibilityIndexPartial),
                    "expected CompatibilityIndexPartial, got {reasons:?}"
                );
                assert!(
                    reasons.contains(&LaunchTemplateReadinessReason::NoResolvedBindings),
                    "expected NoResolvedBindings, got {reasons:?}"
                );
            }
            LaunchTemplateReadiness::Ready => unreachable!(),
        }
    }

    // ── Required test 3 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_missing_binding_assignment_file() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa03");
        let rev = out.install_revision_id.clone();

        fs::remove_file(store.revision_binding_assignment_set_path(&app, &profile, &rev)).unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert!(
            matches!(
                err,
                InstallReusableInputValidationError::MissingBindingAssignmentSet { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Required test 4 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_missing_compatibility_index_file() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa04");
        let rev = out.install_revision_id.clone();

        fs::remove_file(store.revision_compatibility_index_path(&app, &profile, &rev)).unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert!(
            matches!(
                err,
                InstallReusableInputValidationError::MissingCompatibilityIndex { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Required test 5 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_embedded_binding_mismatch() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa05");
        let rev = out.install_revision_id.clone();

        // Tamper ONLY the standalone binding-assignments.json so it diverges from
        // the copy embedded in revision.json. `binding_set_id` is not part of any
        // cross-check hash, so the embedded-equality check is what fires.
        let mut tampered = store
            .read_binding_assignment_set(&app, &profile, &rev)
            .unwrap();
        tampered.binding_set_id = "bset:tampered".into();
        store
            .write_binding_assignment_set(&app, &profile, &rev, &tampered)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert_eq!(
            err,
            InstallReusableInputValidationError::EmbeddedBindingAssignmentMismatch
        );
    }

    // ── Required test 6 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_embedded_compatibility_mismatch() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa06");
        let rev = out.install_revision_id.clone();

        // Tamper ONLY the standalone compatibility-index.json. `index_id` is not a
        // precheck-hash input, so the embedded-equality check is what fires.
        let mut tampered = store
            .read_compatibility_index(&app, &profile, &rev)
            .unwrap();
        tampered.index_id = "cidx:tampered".into();
        store
            .write_compatibility_index(&app, &profile, &rev, &tampered)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert_eq!(
            err,
            InstallReusableInputValidationError::EmbeddedCompatibilityIndexMismatch
        );
    }

    // ── Required test 7 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_binding_requirement_graph_hash_mismatch() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa07");
        let rev = out.install_revision_id.clone();

        // Repoint the binding set's requirement_graph_hash at a bogus content hash
        // in BOTH the standalone file and the embedded copy, so the embedded-equality
        // check passes and the binding↔graph cross-check is what fires.
        let mut binding = store
            .read_binding_assignment_set(&app, &profile, &rev)
            .unwrap();
        binding.requirement_graph_hash = "blake3:deadbeefdeadbeef".into();
        store
            .write_binding_assignment_set(&app, &profile, &rev, &binding)
            .unwrap();
        let mut revision = store.read_install_revision(&app, &profile, &rev).unwrap();
        revision.binding_assignment_set = Some(binding);
        store
            .write_install_revision(&app, &profile, &revision)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert!(
            matches!(
                err,
                InstallReusableInputValidationError::BindingRequirementGraphMismatch { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Required test 8 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_receipt_binding_hash_mismatch() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa08");
        let rev = out.install_revision_id.clone();

        // Repoint the receipt's binding_set_hash at a wrong hash in BOTH the
        // standalone file and the embedded copy, so the embedded-receipt check
        // passes and the receipt↔binding cross-check is what fires.
        let mut receipt = store.read_install_receipt(&app, &profile, &rev).unwrap();
        receipt.binding_set_hash = Some("blake3:wrongbindinghash".into());
        store
            .write_install_receipt(&app, &profile, &rev, &receipt)
            .unwrap();
        let mut revision = store.read_install_revision(&app, &profile, &rev).unwrap();
        revision.install_receipt = receipt;
        store
            .write_install_revision(&app, &profile, &revision)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert!(
            matches!(
                err,
                InstallReusableInputValidationError::ReceiptBindingHashMismatch { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Required test 9 ─────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_reject_receipt_compat_hash_mismatch() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa09");
        let rev = out.install_revision_id.clone();

        // Repoint the receipt's compatibility_precheck_hash at a wrong hash in BOTH
        // places. The receipt↔binding check still passes (binding hash untouched),
        // so the receipt↔compat cross-check is what fires.
        let mut receipt = store.read_install_receipt(&app, &profile, &rev).unwrap();
        receipt.compatibility_precheck_hash = Some("blake3:wrongcompathash".into());
        store
            .write_install_receipt(&app, &profile, &rev, &receipt)
            .unwrap();
        let mut revision = store.read_install_revision(&app, &profile, &rev).unwrap();
        revision.install_receipt = receipt;
        store
            .write_install_revision(&app, &profile, &revision)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert!(
            matches!(
                err,
                InstallReusableInputValidationError::ReceiptCompatibilityHashMismatch { .. }
            ),
            "got {err:?}"
        );
    }

    // ── Required test 10 ────────────────────────────────────────────────────

    #[test]
    fn validated_inputs_do_not_fabricate_launch_template() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa10");
        let validated =
            ValidatedInstallReusableInputs::load(&store, &app, &profile, &out.install_revision_id)
                .unwrap();

        // No LaunchTemplate is persisted by install, and this layer does not mint
        // one. Readiness is NotReady — a real template is a later wave.
        assert!(
            validated.install_revision.launch_templates.is_empty(),
            "install must not persist any LaunchTemplate"
        );
        assert!(!validated.launch_template_readiness().is_ready());
    }

    // ── Extra: an un-finalized revision is rejected (marker authority) ──────

    #[test]
    fn validated_inputs_reject_unfinalized_revision() {
        let (dir, store, app, profile) = setup();
        let out = finalize_standard(&store, &app, &profile, dir.path(), "aa11");
        let rev = out.install_revision_id.clone();

        // Drop the revision.json finalization marker only; sub-records remain.
        store
            .remove_install_revision_marker(&app, &profile, &rev)
            .unwrap();

        let err = ValidatedInstallReusableInputs::load(&store, &app, &profile, &rev).unwrap_err();
        assert_eq!(
            err,
            InstallReusableInputValidationError::RevisionNotFinalized
        );
    }
}
