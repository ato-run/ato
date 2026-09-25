//! The verification point: K's verdicts over what a Runtime observed of a
//! running candidate, and the receipt that states them.
//!
//! ```text
//! run_attempt        realize ─▶ observe over HTTP ───────┐
//!                                                        ├─▶ verify_observed_candidate
//! Hosted validator   observe the Hosted Run (API relay) ─┘      C ⊨ K, receipt
//! ```
//!
//! Every caller that decides whether a candidate satisfied K decides it here,
//! whoever owns the candidate and however it was reached. Nothing here starts
//! a candidate, records a start, stops, cleans up or publishes anything: it
//! is given what was observed and returns what that establishes. What happens
//! to the candidate afterwards is its owner's and is not in the receipt.

use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
use ato_formation::verify::{
    CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2, ContractVerification, ContractVerificationReceipt,
    RuntimeHttpObservation, RuntimeObservation, VerificationExecutionEvidence,
    VerificationTargetKind, verify_runtime,
};

use crate::spec::AttemptSpec;

/// The only HTTP method a Contract observation is made with.
pub const HTTP_OBSERVATION_METHOD: &str = "GET";

/// What the receipt records about where the verified bytes came from.
#[derive(Debug, Clone, Copy)]
pub struct ReceiptContext<'a> {
    pub target: VerificationTargetKind,
    /// The `.capsule` the candidate was unpacked from, when there is one. A
    /// Formation candidate has none and names none.
    pub bundle_sha256: Option<&'a str>,
    /// The transport's dependency profile, when it declares one (receipt
    /// schema /2).
    pub portability_profile: Option<&'a str>,
    /// The Run the candidate belongs to, when there is one: the Run an
    /// attempt starts for a caller that keeps it, or the Hosted Run observed.
    pub run_id: Option<&'a str>,
}

impl ReceiptContext<'_> {
    /// A Formation attempt: no transport, no Run.
    pub fn formation() -> Self {
        Self {
            target: VerificationTargetKind::FormationRuntime,
            bundle_sha256: None,
            portability_profile: None,
            run_id: None,
        }
    }
}

/// An HTTP observation the Contract needs, by logical port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredObservation {
    pub port_id: String,
    pub path: String,
}

/// The Contract's HTTP observations the Derivation exports a port for, each
/// made with [`HTTP_OBSERVATION_METHOD`].
///
/// A requirement on a port the Derivation never exports is not probed — it
/// would measure a port nobody claimed — and the verifier fails it as
/// unobserved. Only GET is observed; anything else is left for the verifier
/// to refuse rather than be probed with the wrong method.
pub fn required_http_observations(spec: &AttemptSpec<'_>) -> Vec<RequiredObservation> {
    spec.contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER)
        .filter(|requirement| {
            requirement
                .method
                .as_deref()
                .is_none_or(|method| method == HTTP_OBSERVATION_METHOD)
        })
        .filter_map(|requirement| {
            let port_id = requirement.port.clone()?;
            spec.derivation
                .ports
                .iter()
                .any(|port| port.id == port_id)
                .then(|| RequiredObservation {
                    port_id,
                    path: requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
                })
        })
        .collect()
}

/// What one verification point established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPoint {
    /// K's verdict on every observation.
    pub verification: ContractVerification,
    /// The same verdicts with their evidence, as this Runtime states them.
    pub receipt: ContractVerificationReceipt,
}

/// Decide K against what was observed of the running candidate, and state it
/// in a receipt.
///
/// K, D and the facts besides HTTP — the resolved input identities and the
/// restored Instance snapshot — are the spec's; the HTTP responses are what
/// the caller read from the candidate. The verdicts and the receipt's
/// evidence come from the one observation built from them, so a receipt can
/// only state what was decided. `execution` is the caller's evidence of the
/// candidate (realization, pid, endpoint, attempt and lease); the Run and the
/// dependency profile are named by `context`.
pub fn verify_observed_candidate(
    spec: &AttemptSpec<'_>,
    http: Vec<RuntimeHttpObservation>,
    mut execution: VerificationExecutionEvidence,
    context: &ReceiptContext<'_>,
) -> VerificationPoint {
    let observation = RuntimeObservation {
        input_refs: spec.input_refs.clone(),
        http,
        instance_snapshot_ref: spec.instance_snapshot_ref.clone(),
    };
    let verification = verify_runtime(spec.contract, &observation);
    let mut receipt = ContractVerificationReceipt::for_attempt(
        context.target,
        context.bundle_sha256.map(str::to_owned),
        spec.contract_ref,
        spec.derivation_ref,
        spec.contract,
        &observation,
        verification.clone(),
    );
    execution.run_id = context.run_id.map(str::to_owned);
    if let Some(profile) = context.portability_profile {
        receipt.schema = CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2.to_owned();
        execution.portability_profile = Some(profile.to_owned());
    }
    receipt.execution = Some(execution);
    VerificationPoint {
        verification,
        receipt,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ato_formation::authoring::{
        BOUND_CONTRACT_SCHEMA, BoundContract, BoundDerivation, BoundRequirement,
        INSTANCE_SNAPSHOT_CONTRACT_VERIFIER, WORKSPACE_CONTRACT_VERIFIER,
    };
    use ato_formation::verify::{ReceiptOutcome, VerificationEvidence};

    use super::*;
    use crate::spec::CandidateShape;

    const TREE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const SNAPSHOT: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const PROOF: &[u8] = b"ato-k-interop-v1\n";

    fn requirement(id: &str, verifier: &str) -> BoundRequirement {
        BoundRequirement {
            id: id.to_owned(),
            verifier: verifier.to_owned(),
            port: None,
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: None,
            digest: None,
        }
    }

    fn http(id: &str, path: &str, body_digest: Option<String>) -> BoundRequirement {
        BoundRequirement {
            port: Some("app.http".to_owned()),
            method: Some("GET".to_owned()),
            path: Some(path.to_owned()),
            status: Some(200),
            body_digest,
            ..requirement(id, HTTP_CONTRACT_VERIFIER)
        }
    }

    /// K: `/` answers 200, `/proof.txt` answers 200 with PROOF, the workspace
    /// is TREE and the restored Instance snapshot is SNAPSHOT.
    fn contract() -> BoundContract {
        BoundContract {
            schema: BOUND_CONTRACT_SCHEMA.to_owned(),
            requirements: vec![
                http("app-proof", "/proof.txt", Some(digest(PROOF))),
                http("app-root", "/", None),
                BoundRequirement {
                    digest: Some(SNAPSHOT.to_owned()),
                    ..requirement("saved-state", INSTANCE_SNAPSHOT_CONTRACT_VERIFIER)
                },
                BoundRequirement {
                    input: Some("workspace".to_owned()),
                    digest: Some(TREE.to_owned()),
                    ..requirement("source-identity", WORKSPACE_CONTRACT_VERIFIER)
                },
            ],
        }
    }

    fn derivation() -> BoundDerivation {
        serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": {}, "steps": [],
            "ports": [{ "id": "app.http", "protocol": "http", "from": "process" }],
            "state": [],
            "effects": "pure"
        }))
        .unwrap()
    }

    fn digest(body: &[u8]) -> String {
        RuntimeHttpObservation::from_response("", "", "", 0, body).body_digest
    }

    fn spec<'a>(
        contract: &'a BoundContract,
        derivation: &'a BoundDerivation,
        tree: &str,
        snapshot: Option<&str>,
    ) -> AttemptSpec<'a> {
        AttemptSpec {
            contract,
            contract_ref: "sha256:k",
            derivation,
            derivation_ref: "sha256:d",
            shape: CandidateShape::Process,
            input_refs: BTreeMap::from([("workspace".to_owned(), tree.to_owned())]),
            instance_snapshot_ref: snapshot.map(str::to_owned),
        }
    }

    fn served(root_status: u16, proof: &[u8]) -> Vec<RuntimeHttpObservation> {
        vec![
            RuntimeHttpObservation::from_response("app.http", "GET", "/proof.txt", 200, proof),
            RuntimeHttpObservation::from_response("app.http", "GET", "/", root_status, b"<html>"),
        ]
    }

    fn execution() -> VerificationExecutionEvidence {
        serde_json::from_value(serde_json::json!({
            "realization": "local_process",
            "endpoint": "http://127.0.0.1:8000/",
            "lease_id": "lease-1",
            "attempt_id": "attempt-1"
        }))
        .unwrap()
    }

    fn hosted() -> ReceiptContext<'static> {
        ReceiptContext {
            target: VerificationTargetKind::AtoRunHosted,
            bundle_sha256: Some("sha256:bundle"),
            portability_profile: None,
            run_id: Some("run-1"),
        }
    }

    /// The verdict code of observation `id`, or `None` when it was satisfied.
    fn failure(point: &VerificationPoint, id: &str) -> Option<String> {
        let observation = point
            .receipt
            .observations
            .iter()
            .find(|observation| observation.id == id)
            .unwrap();
        match observation.outcome {
            ReceiptOutcome::Satisfied => None,
            _ => Some(
                observation
                    .failure
                    .as_deref()
                    .unwrap()
                    .split(':')
                    .next()
                    .unwrap()
                    .to_owned(),
            ),
        }
    }

    /// The receipt states exactly what was decided: one observation per
    /// requirement of K, in K's order, satisfied where the verdict is.
    fn assert_consistent(point: &VerificationPoint, contract: &BoundContract) {
        assert_eq!(
            point
                .receipt
                .observations
                .iter()
                .map(|observation| observation.id.as_str())
                .collect::<Vec<_>>(),
            contract
                .requirements
                .iter()
                .map(|requirement| requirement.id.as_str())
                .collect::<Vec<_>>()
        );
        for (verdict, observation) in point
            .verification
            .verdicts
            .iter()
            .zip(&point.receipt.observations)
        {
            assert_eq!(verdict.id, observation.id);
            assert_eq!(
                matches!(
                    verdict.outcome,
                    ato_formation::verify::ObservationOutcome::Satisfied
                ),
                observation.outcome == ReceiptOutcome::Satisfied
            );
        }
        assert_eq!(
            point.receipt.fully_satisfied,
            point.verification.fully_satisfied()
        );
    }

    #[test]
    fn every_observation_satisfied_is_a_fully_satisfied_receipt() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let point = verify_observed_candidate(&spec, served(200, PROOF), execution(), &hosted());
        assert_consistent(&point, &contract);
        assert!(point.receipt.fully_satisfied);
        let receipt = &point.receipt;
        assert_eq!(receipt.schema, "ato.contract-verification-receipt/1");
        assert_eq!(receipt.target.kind, VerificationTargetKind::AtoRunHosted);
        assert_eq!(receipt.bundle_sha256.as_deref(), Some("sha256:bundle"));
        assert_eq!(receipt.contract_ref, "sha256:k");
        assert_eq!(receipt.derivation_ref, "sha256:d");
        // Each observation carries the evidence it was decided on.
        assert_eq!(
            receipt.observations[0].evidence,
            Some(VerificationEvidence {
                method: Some("GET".to_owned()),
                path: Some("/proof.txt".to_owned()),
                status: Some(200),
                body_sha256: Some(digest(PROOF)),
                input: None,
                digest: None,
            })
        );
        assert_eq!(
            receipt.observations[2].evidence.as_ref().unwrap().digest,
            Some(SNAPSHOT.to_owned())
        );
        assert_eq!(
            receipt.observations[3].evidence.as_ref().unwrap().digest,
            Some(TREE.to_owned())
        );
        // The caller's execution evidence, and the Run the context names.
        let execution = receipt.execution.as_ref().unwrap();
        assert_eq!(execution.realization, "local_process");
        assert_eq!(
            execution.endpoint.as_deref(),
            Some("http://127.0.0.1:8000/")
        );
        assert_eq!(execution.lease_id.as_deref(), Some("lease-1"));
        assert_eq!(execution.attempt_id.as_deref(), Some("attempt-1"));
        assert_eq!(execution.run_id.as_deref(), Some("run-1"));
        assert_eq!(execution.request_id, None);
        assert_eq!(execution.portability_profile, None);
    }

    #[test]
    fn a_failed_http_status_fails_that_observation_only() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let point = verify_observed_candidate(&spec, served(503, PROOF), execution(), &hosted());
        assert_consistent(&point, &contract);
        assert!(!point.receipt.fully_satisfied);
        assert_eq!(
            failure(&point, "app-root").as_deref(),
            Some("http_status_mismatch")
        );
        assert_eq!(failure(&point, "app-proof"), None);
        assert_eq!(
            point.receipt.observations[1]
                .evidence
                .as_ref()
                .unwrap()
                .status,
            Some(503)
        );
    }

    #[test]
    fn an_observation_that_was_not_made_fails_closed() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let mut http = served(200, PROOF);
        http.remove(0);
        let point = verify_observed_candidate(&spec, http, execution(), &hosted());
        assert_consistent(&point, &contract);
        assert!(!point.receipt.fully_satisfied);
        assert_eq!(
            failure(&point, "app-proof").as_deref(),
            Some("runtime_observation_missing")
        );
        // Nothing was observed, so nothing is claimed as its evidence.
        assert_eq!(point.receipt.observations[0].evidence, None);
    }

    #[test]
    fn a_wrong_body_fails_its_digest() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let point = verify_observed_candidate(
            &spec,
            served(200, b"not the proof\n"),
            execution(),
            &hosted(),
        );
        assert_consistent(&point, &contract);
        assert_eq!(
            failure(&point, "app-proof").as_deref(),
            Some("http_body_digest_mismatch")
        );
        assert_eq!(
            point.receipt.observations[0]
                .evidence
                .as_ref()
                .unwrap()
                .body_sha256,
            Some(digest(b"not the proof\n"))
        );
    }

    #[test]
    fn a_different_workspace_fails_input_identity() {
        let (contract, derivation) = (contract(), derivation());
        let other = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
        let spec = spec(&contract, &derivation, other, Some(SNAPSHOT));
        let point = verify_observed_candidate(&spec, served(200, PROOF), execution(), &hosted());
        assert_consistent(&point, &contract);
        assert_eq!(
            failure(&point, "source-identity").as_deref(),
            Some("input_identity_mismatch")
        );
        assert_eq!(
            point.receipt.observations[3]
                .evidence
                .as_ref()
                .unwrap()
                .digest
                .as_deref(),
            Some(other)
        );
    }

    #[test]
    fn a_different_or_absent_snapshot_fails_the_snapshot_observation() {
        let (contract, derivation) = (contract(), derivation());
        let other = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
        let restored_other = spec(&contract, &derivation, TREE, Some(other));
        let point =
            verify_observed_candidate(&restored_other, served(200, PROOF), execution(), &hosted());
        assert_consistent(&point, &contract);
        assert_eq!(
            failure(&point, "saved-state").as_deref(),
            Some("instance_snapshot_mismatch")
        );
        let restored_none = spec(&contract, &derivation, TREE, None);
        let point =
            verify_observed_candidate(&restored_none, served(200, PROOF), execution(), &hosted());
        assert_eq!(
            failure(&point, "saved-state").as_deref(),
            Some("instance_snapshot_missing")
        );
        assert_eq!(point.receipt.observations[2].evidence, None);
    }

    #[test]
    fn the_context_names_the_run_and_the_dependency_profile() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        // Whatever Run the evidence claimed, the receipt names the context's.
        let mut evidence = execution();
        evidence.run_id = Some("another-run".to_owned());
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            evidence.clone(),
            &ReceiptContext {
                target: VerificationTargetKind::CliLocal,
                bundle_sha256: Some("sha256:bundle"),
                portability_profile: Some("cached"),
                run_id: None,
            },
        );
        assert_eq!(
            point.receipt.schema,
            CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2
        );
        let execution = point.receipt.execution.as_ref().unwrap();
        assert_eq!(execution.run_id, None);
        assert_eq!(execution.portability_profile.as_deref(), Some("cached"));
        // A Formation attempt names no transport and no Run.
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            evidence,
            &ReceiptContext::formation(),
        );
        assert_eq!(point.receipt.bundle_sha256, None);
        assert_eq!(
            point.receipt.target.kind,
            VerificationTargetKind::FormationRuntime
        );
        assert_eq!(point.receipt.schema, "ato.contract-verification-receipt/1");
    }

    #[test]
    fn only_get_observations_on_exported_ports_are_required() {
        let mut contract = contract();
        contract.requirements.push(BoundRequirement {
            method: Some("POST".to_owned()),
            ..http("posted", "/submit", None)
        });
        contract.requirements.push(BoundRequirement {
            port: Some("admin.http".to_owned()),
            ..http("unexported", "/admin", None)
        });
        contract.requirements.push(BoundRequirement {
            path: None,
            ..http("default-path", "/", None)
        });
        let derivation = derivation();
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let required = |path: &str| RequiredObservation {
            port_id: "app.http".to_owned(),
            path: path.to_owned(),
        };
        assert_eq!(
            required_http_observations(&spec),
            vec![required("/proof.txt"), required("/"), required("/")]
        );
        // What is not required is not observed, and fails as unobserved.
        let point = verify_observed_candidate(&spec, served(200, PROOF), execution(), &hosted());
        assert_eq!(
            failure(&point, "posted").as_deref(),
            Some("runtime_observation_missing")
        );
        assert_eq!(
            failure(&point, "unexported").as_deref(),
            Some("runtime_observation_missing")
        );
    }
}
