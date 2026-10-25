//! Contract-preserving Derivation transformation (v0).
//!
//! This module answers one question and nothing more: given an existing
//! portable `.capsule` whose canonical Contract K is frozen, and a NEW source
//! directory, can a candidate Derivation be built from that source **under the
//! same K**?
//!
//! It does not make transformation a semantic concept. There is no new
//! identity, no new Contract kind, no K evaluator and no receipt schema
//! change. A candidate is an ordinary portable bundle (`build_authored_bundle_v2`
//! output) whose Contract must be byte-identical to the frozen one; verifying
//! it is the existing runtime verifier's job (`ato run` / `run_attempt`).
//!
//! The invariants are:
//!
//! - **K is frozen, never recaptured.** The old bundle's canonical
//!   `BoundContract` body is read from the bundle and compared to the
//!   candidate's canonical JCS bytes AND `contract_ref`. A candidate that
//!   derives a different K is refused with [`CANDIDATE_CONTRACT_MISMATCH`]
//!   before anything runs.
//! - **A source-bound K cannot admit a different source.** If K carries an
//!   `ato.contract.workspace@1` observation, its digest pins one workspace
//!   closure; a new source is a different closure, so the transformation is
//!   refused up front with [`FROZEN_CONTRACT_IS_SOURCE_BOUND`]. That check is
//!   not weakened here.
//! - **No implicit selection.** A candidate bundle with zero or several
//!   derivations does not get a guessed one; the caller names a declared
//!   `DerivationRef` or receives the list.
//! - **Nothing is published.** The returned bytes are a local candidate; the
//!   old bundle and its receipts are never touched.

use std::collections::BTreeSet;
use std::path::Path;

use ato_formation::authoring::{BoundContract, WORKSPACE_CONTRACT_VERIFIER};
use ato_objects::{
    CapsuleBundleDocument, PortableApplicationBundle, decode_capsule_bundle_document,
};

use crate::{
    PortableApplicationError, build_authored_bundle_v2, bundle_sha256, validate_all_derivations,
    validate_bundle_for_derivation, workspace_input,
};

/// The report schema written by the CLI beside the candidate bundle. It is a
/// sidecar: the receipt schema (`ato.contract-verification-receipt/1`) is
/// unchanged and carries only K/D/observations.
pub const TRANSFORMATION_REPORT_SCHEMA: &str = "ato.transformation-candidate/1";

/// The frozen Contract pins one workspace closure through
/// `ato.contract.workspace@1`; a different source is not admitted.
pub const FROZEN_CONTRACT_IS_SOURCE_BOUND: &str = "frozen_contract_is_source_bound";
/// The candidate source derived a different Contract body or ref than the
/// frozen bundle. The old K is not rewritten to fit it.
pub const CANDIDATE_CONTRACT_MISMATCH: &str = "candidate_contract_mismatch";
/// The frozen input is not a portable-application (v3/v4) bundle.
pub const FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION: &str = "frozen_bundle_not_portable_application";
/// The candidate bundle has no derivations, or several and none was named.
pub const TRANSFORMATION_DERIVATION_SELECTION_REQUIRED: &str =
    "transformation_derivation_selection_required";
/// The named candidate DerivationRef is not declared by the candidate bundle.
pub const TRANSFORMATION_DERIVATION_NOT_DECLARED: &str = "transformation_derivation_not_declared";
/// The candidate derivation omitted its `ato.workspace@1` input.
pub const CANDIDATE_SOURCE_CLOSURE_MISSING: &str = "candidate_source_closure_missing";

fn refusal(code: &'static str, detail: impl Into<String>) -> PortableApplicationError {
    PortableApplicationError::Transformation {
        code,
        detail: detail.into(),
    }
}

/// What the frozen bundle contributes to the transformation: its canonical K
/// body, its old DerivationRefs, and the source closures those old derivations
/// pinned. All of it is read-only evidence; none of it is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenTransformTarget {
    /// SHA-256 of the frozen `.capsule` transport bytes.
    pub bundle_sha256: String,
    /// The canonical Contract body read from the bundle (the frozen K).
    pub contract: BoundContract,
    /// The ContractRef the frozen K digests to.
    pub contract_ref: String,
    /// Every DerivationRef the frozen bundle declared, sorted.
    pub derivation_refs: Vec<String>,
    /// Every distinct `ato.workspace@1` content_ref among those old
    /// derivations, sorted. These are the old source closures.
    pub source_closure_refs: Vec<String>,
}

/// A candidate bundle built from a new source under the frozen K, before any
/// execution. `k_preserved` is true only when the candidate's canonical K body
/// and ref equal the frozen ones.
#[derive(Debug, Clone)]
pub struct TransformationCandidate {
    /// Candidate transport bytes (`ato.portable-application/1`, v3).
    pub bytes: Vec<u8>,
    /// The decoded candidate bundle, for the existing execution/verification
    /// path.
    pub bundle: PortableApplicationBundle,
    /// The frozen Contract body the candidate was built under.
    pub contract: BoundContract,
    /// The frozen ContractRef.
    pub contract_ref: String,
    /// The selected candidate DerivationRef.
    pub derivation_ref: String,
    /// The candidate source closure: the selected D's `ato.workspace@1`
    /// input content_ref.
    pub source_closure_ref: String,
    /// Old DerivationRefs from the frozen bundle, sorted.
    pub old_derivation_refs: Vec<String>,
    /// Old source closures from the frozen bundle, sorted and deduplicated.
    pub old_source_closure_refs: Vec<String>,
    /// SHA-256 of the frozen transport.
    pub frozen_bundle_sha256: String,
    /// True when the candidate K is byte-identical to the frozen K.
    pub k_preserved: bool,
}

/// Is this Contract source-bound?
///
/// True iff any requirement is decided by `ato.contract.workspace@1`: such a
/// requirement's digest pins one workspace closure, so the Contract admits
/// exactly the source it captured. This is the single implementation used by
/// [`build_transformation_candidate`].
pub fn frozen_contract_is_source_bound(contract: &BoundContract) -> bool {
    contract
        .requirements
        .iter()
        .any(|requirement| requirement.verifier == WORKSPACE_CONTRACT_VERIFIER)
}

fn decode_frozen_bundle(
    frozen_bundle_bytes: &[u8],
) -> Result<PortableApplicationBundle, PortableApplicationError> {
    match decode_capsule_bundle_document(frozen_bundle_bytes).map_err(|error| {
        refusal(
            FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION,
            format!("the frozen input is not a decodable Capsule bundle: {error}"),
        )
    })? {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => Ok(bundle),
        CapsuleBundleDocument::ComputationV2(_) => Err(refusal(
            FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION,
            "the frozen input is a computation-root v2 bundle, not a portable application \
             (ato.portable-application/1) whose canonical Contract can be preserved",
        )),
    }
}

fn contract_ref(contract: &BoundContract) -> Result<String, PortableApplicationError> {
    contract.contract_ref().map_err(|error| {
        refusal(
            CANDIDATE_CONTRACT_MISMATCH,
            format!("the Contract body does not digest to one ref: {error}"),
        )
    })
}

fn canonical_contract(contract: &BoundContract) -> Result<Vec<u8>, PortableApplicationError> {
    serde_jcs::to_vec(contract).map_err(|error| {
        refusal(
            CANDIDATE_CONTRACT_MISMATCH,
            format!("the Contract body is not canonicalizable: {error}"),
        )
    })
}

/// Read the frozen bundle without building anything.
pub fn inspect_frozen_transform_target(
    frozen_bundle_bytes: &[u8],
) -> Result<FrozenTransformTarget, PortableApplicationError> {
    let bundle = decode_frozen_bundle(frozen_bundle_bytes)?;
    let routes = validate_all_derivations(&bundle)?;
    let contract = routes
        .first()
        .map(|route| route.contract.clone())
        .ok_or_else(|| {
            refusal(
                FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION,
                "the frozen bundle declares no Derivation",
            )
        })?;
    // The bundle has one canonical K object as its root, and every route binds
    // to it, so one body is the frozen K for all of them.
    let contract_ref = contract_ref(&contract)?;
    if contract_ref != bundle.index.root_contract_ref {
        return Err(refusal(
            FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION,
            format!(
                "the frozen Contract body digests to {contract_ref}, not its declared {}",
                bundle.index.root_contract_ref
            ),
        ));
    }
    let mut derivation_refs = bundle.index.derivations.clone();
    derivation_refs.sort();
    let mut source_closure_refs = BTreeSet::new();
    for route in &routes {
        if let Some(input) = workspace_input(&route.derivation) {
            source_closure_refs.insert(input.content_ref.clone());
        }
    }
    Ok(FrozenTransformTarget {
        bundle_sha256: bundle_sha256(frozen_bundle_bytes),
        contract,
        contract_ref,
        derivation_refs,
        source_closure_refs: source_closure_refs.into_iter().collect(),
    })
}

/// Build a candidate Derivation bundle from `source_root` under the frozen K.
///
/// The frozen Contract is read from `frozen_bundle_bytes`; the candidate is
/// built with the existing `ato.capsule/2` authoring builder. Nothing is
/// executed here, and nothing on disk is written.
pub fn build_transformation_candidate(
    frozen_bundle_bytes: &[u8],
    source_root: &Path,
    derivation_hint: Option<&str>,
) -> Result<TransformationCandidate, PortableApplicationError> {
    let frozen = inspect_frozen_transform_target(frozen_bundle_bytes)?;
    if frozen_contract_is_source_bound(&frozen.contract) {
        return Err(refusal(
            FROZEN_CONTRACT_IS_SOURCE_BOUND,
            format!(
                "the frozen Contract {} captures a workspace closure \
                 (ato.contract.workspace@1), so it only admits its original source; a different \
                 source cannot be transformed under it",
                frozen.contract_ref
            ),
        ));
    }

    let (candidate_bytes, candidate_bundle) = build_authored_bundle_v2(source_root)?;
    let candidate_routes = validate_all_derivations(&candidate_bundle)?;
    let candidate_contract = candidate_routes
        .first()
        .map(|route| route.contract.clone())
        .ok_or_else(|| {
            refusal(
                TRANSFORMATION_DERIVATION_SELECTION_REQUIRED,
                "the candidate bundle declares no Derivation",
            )
        })?;
    let candidate_contract_ref = contract_ref(&candidate_contract)?;

    // K is compared as canonical bytes AND as the ref that names those bytes.
    // Both, because either alone would let a different body claim the old K.
    if candidate_contract_ref != frozen.contract_ref
        || canonical_contract(&candidate_contract)? != canonical_contract(&frozen.contract)?
        || candidate_bundle.index.root_contract_ref != frozen.contract_ref
    {
        return Err(refusal(
            CANDIDATE_CONTRACT_MISMATCH,
            format!(
                "the new source derives Contract {candidate_contract_ref}, but the frozen Contract \
                 is {}; a transformation must preserve K, so the old K is not rewritten",
                frozen.contract_ref
            ),
        ));
    }

    let selected = match derivation_hint {
        Some(hint) => {
            if !candidate_bundle
                .index
                .derivations
                .iter()
                .any(|declared| declared == hint)
            {
                return Err(refusal(
                    TRANSFORMATION_DERIVATION_NOT_DECLARED,
                    format!(
                        "the candidate bundle does not declare derivation `{hint}`; declared: {}",
                        candidate_bundle.index.derivations.join(", ")
                    ),
                ));
            }
            hint.to_owned()
        }
        None if candidate_bundle.index.derivations.len() == 1 => {
            candidate_bundle.index.derivations[0].clone()
        }
        None => {
            return Err(refusal(
                TRANSFORMATION_DERIVATION_SELECTION_REQUIRED,
                format!(
                    "the candidate bundle declares {} derivations; select one with --derivation \
                     <sha256:...>: {}",
                    candidate_bundle.index.derivations.len(),
                    candidate_bundle.index.derivations.join(", ")
                ),
            ));
        }
    };
    let validated = validate_bundle_for_derivation(&candidate_bundle, &selected)?;
    let source_closure_ref = workspace_input(&validated.derivation)
        .map(|input| input.content_ref.clone())
        .ok_or_else(|| {
            refusal(
                CANDIDATE_SOURCE_CLOSURE_MISSING,
                format!("candidate derivation {selected} declares no ato.workspace@1 input"),
            )
        })?;

    Ok(TransformationCandidate {
        bytes: candidate_bytes,
        bundle: candidate_bundle,
        contract: frozen.contract,
        contract_ref: frozen.contract_ref,
        derivation_ref: selected,
        source_closure_ref,
        old_derivation_refs: frozen.derivation_refs,
        old_source_closure_refs: frozen.source_closure_refs,
        frozen_bundle_sha256: frozen.bundle_sha256,
        k_preserved: true,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ato_computation::ContentRef;
    use ato_formation::authoring::{
        BOUND_CONTRACT_SCHEMA, BoundDerivation, BoundRequirement, WORKSPACE_CONTRACT_VERIFIER,
        WORKSPACE_PROTOCOL,
    };
    use ato_objects::{
        PortableBundleObjectDescriptor, PortableBundleObjectKind, PortableBundlePayload,
        encode_portable_application_bundle,
    };
    use base64::Engine;

    use super::*;
    use crate::build_authored_bundle_v2;

    /// A `ato.capsule/2` process route with one HTTP observation. The Contract
    /// this derives is behavioral (no workspace identity), which is exactly
    /// the shape a transformation may preserve across sources.
    fn written_source(status: u16, app: &str, route_id: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = format!(
            r#"
schema = "ato.capsule/2"

[application]
title = "Transformation fixture"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
status = {status}

[[derivation]]
id = "{route_id}"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = {{ python = "3.12" }}

[effects]
default = "pure"
"#
        );
        fs::write(dir.path().join("capsule.toml"), manifest).expect("capsule.toml");
        fs::write(dir.path().join("app.py"), app).expect("app.py");
        dir
    }

    fn two_route_source() -> tempfile::TempDir {
        let base = written_source(200, "print('a')\n", "python-a");
        let manifest = fs::read_to_string(base.path().join("capsule.toml")).unwrap();
        let second = r#"
[[derivation]]
id = "python-b"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }
env = { ROUTE_VARIANT = "b" }
"#;
        let manifest = manifest.replacen("[effects]", &format!("{second}\n[effects]"), 1);
        fs::write(base.path().join("capsule.toml"), manifest).unwrap();
        base
    }

    fn frozen_bytes(source: &Path) -> Vec<u8> {
        build_authored_bundle_v2(source).expect("frozen bundle").0
    }

    #[test]
    fn a_workspace_requirement_makes_the_contract_source_bound() {
        let body: BoundContract = serde_json::from_value(serde_json::json!({
            "schema": "ato.contract/1",
            "requirements": [{
                "id": "http",
                "verifier": "ato.contract.http@1",
                "port": "app.http",
                "method": "GET",
                "path": "/",
                "status": 200
            }]
        }))
        .unwrap();
        assert!(!frozen_contract_is_source_bound(&body));

        let mut source_bound = body.clone();
        source_bound.requirements.push(BoundRequirement {
            id: "source-identity".to_owned(),
            verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
            port: None,
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: Some("workspace".to_owned()),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        });
        assert!(frozen_contract_is_source_bound(&source_bound));
    }

    /// Turn a behavioral portable bundle into one whose K captures the single
    /// derivation's workspace closure. Every object stays canonical and the
    /// bundle closure stays complete, so the result validates as a real
    /// source-bound bundle — the shape an existing `.capsule` may already have.
    fn make_source_bound(bundle: &mut PortableApplicationBundle) -> Vec<u8> {
        let old_ref = bundle.index.root_contract_ref.clone();
        let old = ContentRef::parse(old_ref.clone()).unwrap();
        let mut contract: BoundContract =
            serde_json::from_slice(&bundle.payload_bytes(&old).expect("frozen contract payload"))
                .expect("frozen contract");
        assert_eq!(contract.schema, BOUND_CONTRACT_SCHEMA);

        let derivation_ref = ContentRef::parse(bundle.index.derivations[0].clone()).unwrap();
        let derivation: BoundDerivation = serde_json::from_slice(
            &bundle
                .payload_bytes(&derivation_ref)
                .expect("derivation payload"),
        )
        .expect("derivation");
        let closure = derivation
            .inputs
            .iter()
            .find(|input| input.protocol == WORKSPACE_PROTOCOL)
            .expect("workspace input")
            .content_ref
            .clone();
        contract.requirements.push(BoundRequirement {
            id: "source-identity".to_owned(),
            verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
            port: None,
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: Some("workspace".to_owned()),
            digest: Some(closure),
        });
        contract
            .requirements
            .sort_by(|left, right| left.id.cmp(&right.id));

        let bytes = serde_jcs::to_vec(&contract).unwrap();
        let new_ref = bundle_sha256(&bytes);
        bundle
            .index
            .objects
            .retain(|object| object.reference != old_ref);
        bundle.index.objects.push(PortableBundleObjectDescriptor {
            reference: new_ref.clone(),
            kind: PortableBundleObjectKind::Structured,
            schema: Some(BOUND_CONTRACT_SCHEMA.to_owned()),
            size: bytes.len() as u64,
        });
        bundle
            .index
            .objects
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        bundle
            .payloads
            .retain(|payload| payload.reference != old_ref);
        bundle.payloads.push(PortableBundlePayload {
            reference: new_ref.clone(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        bundle
            .payloads
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        bundle.index.root_contract_ref = new_ref;
        encode_portable_application_bundle(bundle).expect("re-encoded bundle")
    }

    #[test]
    fn a_source_bound_frozen_contract_is_refused_before_any_build() {
        let frozen_source = written_source(200, "print('old')\n", "python-a");
        let (_, mut bundle) = build_authored_bundle_v2(frozen_source.path()).unwrap();
        let frozen = make_source_bound(&mut bundle);
        assert!(
            inspect_frozen_transform_target(&frozen)
                .expect("the source-bound bundle still validates")
                .contract
                .requirements
                .iter()
                .any(|requirement| requirement.verifier == WORKSPACE_CONTRACT_VERIFIER)
        );

        let new_source = written_source(200, "print('new')\n", "python-a");
        let error = build_transformation_candidate(&frozen, new_source.path(), None)
            .expect_err("source-bound K cannot admit a new source");
        assert_eq!(
            error.transformation_code(),
            Some(FROZEN_CONTRACT_IS_SOURCE_BOUND)
        );
    }

    #[test]
    fn a_candidate_that_derives_a_different_k_is_refused_with_both_refs() {
        let frozen_source = written_source(200, "print('old')\n", "python-a");
        let frozen = frozen_bytes(frozen_source.path());
        let mismatch = written_source(201, "print('new')\n", "python-a");
        let error = build_transformation_candidate(&frozen, mismatch.path(), None)
            .expect_err("a different K is not preserved");
        assert_eq!(
            error.transformation_code(),
            Some(CANDIDATE_CONTRACT_MISMATCH)
        );
        let message = error.to_string();
        let frozen_contract = inspect_frozen_transform_target(&frozen)
            .unwrap()
            .contract_ref;
        assert!(message.contains(&frozen_contract), "{message}");
        assert!(message.contains("sha256:"), "{message}");
    }

    #[test]
    fn a_different_source_keeps_k_and_moves_the_derivation() {
        let frozen_source = written_source(200, "print('old')\n", "python-a");
        let frozen = frozen_bytes(frozen_source.path());
        let new_source = written_source(200, "print('new source')\n", "python-a");

        let candidate =
            build_transformation_candidate(&frozen, new_source.path(), None).expect("candidate");
        let frozen_target = inspect_frozen_transform_target(&frozen).expect("frozen target");
        assert!(candidate.k_preserved);
        assert_eq!(candidate.contract_ref, frozen_target.contract_ref);
        assert_eq!(candidate.frozen_bundle_sha256, bundle_sha256(&frozen));
        assert_eq!(candidate.old_derivation_refs, frozen_target.derivation_refs);
        assert_eq!(
            candidate.old_source_closure_refs,
            frozen_target.source_closure_refs
        );
        assert_eq!(
            candidate.contract_ref, candidate.bundle.index.root_contract_ref,
            "the candidate bundle roots at the frozen K"
        );
        assert_ne!(
            candidate.source_closure_ref, candidate.old_source_closure_refs[0],
            "a different source is a different closure"
        );
        let frozen_derivation = inspect_frozen_transform_target(&frozen)
            .unwrap()
            .derivation_refs[0]
            .clone();
        assert_ne!(
            candidate.derivation_ref, frozen_derivation,
            "a different source is a different DerivationRef"
        );
        assert!(
            !candidate.old_source_closure_refs.is_empty(),
            "the old closure is reported"
        );
    }

    #[test]
    fn several_candidate_derivations_require_an_explicit_selection() {
        let frozen_source = written_source(200, "print('old')\n", "python-a");
        let frozen = frozen_bytes(frozen_source.path());
        let two_routes = two_route_source();

        let error = build_transformation_candidate(&frozen, two_routes.path(), None)
            .expect_err("no implicit selection");
        assert_eq!(
            error.transformation_code(),
            Some(TRANSFORMATION_DERIVATION_SELECTION_REQUIRED)
        );
        assert!(error.to_string().contains("sha256:"), "{error}");

        let invalid = build_transformation_candidate(
            &frozen,
            two_routes.path(),
            Some(&format!("sha256:{}", "0".repeat(64))),
        )
        .expect_err("undeclared hint");
        assert_eq!(
            invalid.transformation_code(),
            Some(TRANSFORMATION_DERIVATION_NOT_DECLARED)
        );
    }

    #[test]
    fn a_computation_bundle_is_not_a_transformation_input() {
        let error = build_transformation_candidate(b"{}", Path::new("."), None)
            .expect_err("not a portable application");
        assert_eq!(
            error.transformation_code(),
            Some(FROZEN_BUNDLE_NOT_PORTABLE_APPLICATION)
        );
    }
}
