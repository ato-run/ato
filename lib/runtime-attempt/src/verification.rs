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

use ato_formation::authoring::{
    BoundContract, BoundDerivation, HTTP_CONTRACT_VERIFIER, TLS_CONTRACT_VERIFIER, TlsStart,
};
use ato_formation::scenario::ScenarioObservation;
use ato_formation::verify::{
    CONTRACT_VERIFICATION_RECEIPT_SCHEMA, CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2,
    CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V3, CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V4,
    ContractVerification, ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation,
    RuntimeTlsObservation, VerificationExecutionEvidence, VerificationTargetKind, verify_runtime,
};

use crate::spec::AttemptSpec;

/// Immutable verification input retained at Formation for later Run attempts.
/// The value is evidence input, not itself a receipt or an authorization to run.
pub const FROZEN_VERIFICATION_SCHEMA: &str = "ato.frozen-verification-input/1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenVerificationInput {
    pub schema: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub contract: BoundContract,
    pub derivation: BoundDerivation,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrozenVerificationInputError {
    #[error("unsupported frozen verification input schema")]
    UnsupportedSchema,
    #[error("invalid canonical Contract or Derivation")]
    InvalidCanonicalValue,
    #[error("Contract does not match the assigned ContractRef")]
    ContractMismatch,
    #[error("Derivation does not match the assigned DerivationRef")]
    DerivationMismatch,
}

impl FrozenVerificationInput {
    /// Check both the transported values and their assignment. These checks
    /// deliberately do not observe a workload or claim K is satisfied.
    pub fn validate_assignment(
        &self,
        contract_ref: &str,
        derivation_ref: &str,
    ) -> Result<(), FrozenVerificationInputError> {
        use FrozenVerificationInputError::*;
        if self.schema != FROZEN_VERIFICATION_SCHEMA {
            return Err(UnsupportedSchema);
        }
        let actual_contract = self
            .contract
            .contract_ref()
            .map_err(|_| InvalidCanonicalValue)?;
        if self.contract_ref != contract_ref || actual_contract != contract_ref {
            return Err(ContractMismatch);
        }
        let actual_derivation = self
            .derivation
            .derivation_ref()
            .map_err(|_| InvalidCanonicalValue)?;
        if self.derivation_ref != derivation_ref || actual_derivation != derivation_ref {
            return Err(DerivationMismatch);
        }
        Ok(())
    }
}

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

/// A TLS observation the Contract needs, by logical port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredTlsObservation {
    pub requirement_id: String,
    pub port_id: String,
    /// The name the certificate must cover, exactly as K states it.
    pub server_name: String,
    pub start: TlsStart,
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

/// The Contract's TLS observations the Derivation exports a port for.
///
/// A requirement on a port the Derivation never exports is not observed — the
/// verifier fails it as unobserved instead of connecting to something nobody
/// claimed.
pub fn required_tls_observations(spec: &AttemptSpec<'_>) -> Vec<RequiredTlsObservation> {
    spec.contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == TLS_CONTRACT_VERIFIER)
        .filter_map(|requirement| {
            let port_id = requirement.port.clone()?;
            spec.derivation
                .ports
                .iter()
                .any(|port| port.id == port_id)
                .then(|| RequiredTlsObservation {
                    requirement_id: requirement.id.clone(),
                    port_id,
                    server_name: requirement.server_name.clone().unwrap_or_default(),
                    start: requirement.start.unwrap_or(TlsStart::Implicit),
                })
        })
        .collect()
}

/// The probe's start mode for a Contract's TLS start mode.
///
/// The probe is a leaf crate that may not depend on the Contract layer, so the
/// two enums exist separately and this is their one conversion.
pub fn probe_tls_start(start: TlsStart) -> ato_tls_observe::TlsStart {
    match start {
        TlsStart::Implicit => ato_tls_observe::TlsStart::Implicit,
        TlsStart::SmtpStarttls => ato_tls_observe::TlsStart::SmtpStarttls,
    }
}

fn contract_tls_start(start: ato_tls_observe::TlsStart) -> TlsStart {
    match start {
        ato_tls_observe::TlsStart::Implicit => TlsStart::Implicit,
        ato_tls_observe::TlsStart::SmtpStarttls => TlsStart::SmtpStarttls,
    }
}

/// What the probe measured, in the Runtime's own observation shape. The
/// requirement only supplies the ids this attempt answers.
pub fn runtime_tls_observation(
    requirement: &RequiredTlsObservation,
    facts: ato_tls_observe::TlsFacts,
) -> RuntimeTlsObservation {
    RuntimeTlsObservation {
        requirement_id: requirement.requirement_id.clone(),
        port: requirement.port_id.clone(),
        server_name: facts.server_name,
        start: contract_tls_start(facts.start),
        presented_serial: facts.presented_serial,
        san: facts.san,
        not_before: facts.not_before,
        not_after: facts.not_after,
        days_remaining: facts.days_remaining,
        chain_length: facts.chain_length,
        trust_anchor: facts.trust_anchor,
        trusted: facts.trusted,
        handshake_completed: facts.handshake_completed,
        server_name_matches: facts.server_name_matches,
        within_validity: facts.within_validity,
    }
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
    scenarios: Vec<ScenarioObservation>,
    tls: Vec<RuntimeTlsObservation>,
    mut execution: VerificationExecutionEvidence,
    context: &ReceiptContext<'_>,
) -> VerificationPoint {
    let observation = RuntimeObservation {
        input_refs: spec.input_refs.clone(),
        http,
        instance_snapshot_ref: spec.instance_snapshot_ref.clone(),
        scenarios,
        tls,
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
        execution.portability_profile = Some(profile.to_owned());
    }
    // The schema is the highest thing the receipt actually carries, and one
    // decision rather than an order of overwrites: connector evidence (/4),
    // then certificate evidence (/3), then a portability profile (/2), then
    // the base schema. A /3 must never carry connector fields.
    let carries_tls = receipt.observations.iter().any(|observation| {
        observation
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.tls.is_some())
    });
    receipt.schema = if execution.connector.is_some() {
        CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V4.to_owned()
    } else if carries_tls {
        CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V3.to_owned()
    } else if context.portability_profile.is_some() {
        CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2.to_owned()
    } else {
        CONTRACT_VERIFICATION_RECEIPT_SCHEMA.to_owned()
    };
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
    use ato_formation::verify::{
        CandidateObservation, ObservationOutcome, ReceiptOutcome, VerificationEvidence, verify,
    };

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

            server_name: None,
            start: None,
        }
    }

    fn http(id: &str, path: &str, body_digest: Option<String>) -> BoundRequirement {
        BoundRequirement {
            port: Some("app.http".to_owned()),
            method: Some("GET".to_owned()),
            path: Some(path.to_owned()),
            status: Some(200),
            body_digest,
            server_name: None,
            start: None,
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
                    server_name: None,
                    start: None,
                    ..requirement("saved-state", INSTANCE_SNAPSHOT_CONTRACT_VERIFIER)
                },
                BoundRequirement {
                    input: Some("workspace".to_owned()),
                    digest: Some(TREE.to_owned()),
                    server_name: None,
                    start: None,
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

    #[test]
    fn frozen_input_round_trip_checks_both_canonical_values_and_assignment() {
        let contract = contract();
        let derivation = derivation();
        let contract_ref = contract.contract_ref().unwrap();
        let derivation_ref = derivation.derivation_ref().unwrap();
        let frozen = FrozenVerificationInput {
            schema: FROZEN_VERIFICATION_SCHEMA.to_owned(),
            contract_ref: contract_ref.clone(),
            derivation_ref: derivation_ref.clone(),
            contract,
            derivation,
        };
        let mut decoded: FrozenVerificationInput =
            serde_json::from_slice(&serde_json::to_vec(&frozen).unwrap()).unwrap();
        assert_eq!(
            decoded.validate_assignment(&contract_ref, &derivation_ref),
            Ok(())
        );
        assert_eq!(
            decoded.validate_assignment("sha256:another-contract", &derivation_ref),
            Err(FrozenVerificationInputError::ContractMismatch),
        );
        decoded.contract.requirements[0].status = Some(404);
        assert_eq!(
            decoded.validate_assignment(&contract_ref, &derivation_ref),
            Err(FrozenVerificationInputError::ContractMismatch),
        );
        decoded = frozen.clone();
        decoded.derivation.ports[0].from = "other".into();
        assert_eq!(
            decoded.validate_assignment(&contract_ref, &derivation_ref),
            Err(FrozenVerificationInputError::DerivationMismatch),
        );
        decoded = frozen.clone();
        decoded.schema = "ato.frozen-verification-input/2".into();
        assert_eq!(
            decoded.validate_assignment(&contract_ref, &derivation_ref),
            Err(FrozenVerificationInputError::UnsupportedSchema),
        );
        let mut unknown = serde_json::to_value(frozen).unwrap();
        unknown["unrecognized"] = serde_json::json!(true);
        assert!(serde_json::from_value::<FrozenVerificationInput>(unknown).is_err());
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
            scenarios: Vec::new(),
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

    /// A receipt is accepted only when its schema and its fields agree.
    fn accepts(receipt: &ContractVerificationReceipt) {
        let (contract, _) = (contract(), derivation());
        ato_formation::receipt::accept_receipt(
            &ato_formation::receipt::ReceiptAssignment {
                contract: &contract,
                contract_ref: "sha256:k",
                derivation_ref: "sha256:d",
                attempt_id: "attempt-1",
                request_id: None,
            },
            receipt,
        )
        .expect("the receipt is internally consistent");
    }

    fn relayed_execution() -> VerificationExecutionEvidence {
        let mut execution = execution();
        execution.connector = Some("relay".to_owned());
        execution.relay_image_digest = Some(format!("sha256:{}", "a".repeat(64)));
        execution.relay_container_id = Some("b".repeat(64));
        execution.platform_emulated = Some(true);
        execution
    }

    /// A direct Run's receipt must stay the receipt stg produced: same schema,
    /// same fields, accepted by the same `accept_receipt`. The connector
    /// evidence is for the relay materialization only.
    #[test]
    fn a_direct_service_group_receipt_keeps_the_stg_schema() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, None);
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
        assert_eq!(point.receipt.schema, CONTRACT_VERIFICATION_RECEIPT_SCHEMA);
        let evidence = point.receipt.execution.as_ref().unwrap();
        assert!(evidence.connector.is_none());
        assert!(evidence.relay_image_digest.is_none());
        assert!(evidence.platform_emulated.is_none());
        accepts(&point.receipt);

        let profile = ReceiptContext {
            portability_profile: Some("offline"),
            ..hosted()
        };
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &profile,
        );
        assert_eq!(
            point.receipt.schema,
            CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2
        );
        accepts(&point.receipt);
    }

    #[test]
    fn a_relayed_receipt_is_v4_and_certificate_evidence_does_not_lower_it() {
        let (contract, derivation) = (contract(), derivation());
        let route = spec(&contract, &derivation, TREE, None);
        let relayed = verify_observed_candidate(
            &route,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            relayed_execution(),
            &hosted(),
        );
        assert_eq!(
            relayed.receipt.schema,
            CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V4
        );
        assert_eq!(
            relayed
                .receipt
                .execution
                .as_ref()
                .unwrap()
                .connector
                .as_deref(),
            Some("relay")
        );
        accepts(&relayed.receipt);

        // A relayed Run that also observed a certificate is still /4: the
        // higher statement wins, and one schema carries both field sets.
        let tls_contract = tls_contract(tls_requirement(
            "imaps",
            "app.http",
            "mail.example",
            TlsStart::Implicit,
        ));
        let tls_spec = spec(&tls_contract, &derivation, TREE, None);
        let relayed_tls = verify_observed_candidate(
            &tls_spec,
            Vec::new(),
            Vec::new(),
            vec![tls_facts("imaps", "app.http", "mail.example")],
            relayed_execution(),
            &hosted(),
        );
        assert_eq!(
            relayed_tls.receipt.schema,
            CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V4
        );
        assert!(
            relayed_tls.receipt.observations[0]
                .evidence
                .as_ref()
                .unwrap()
                .tls
                .is_some()
        );
        let assignment = ato_formation::receipt::ReceiptAssignment {
            contract: &tls_contract,
            contract_ref: "sha256:k",
            derivation_ref: "sha256:d",
            attempt_id: "attempt-1",
            request_id: None,
        };
        ato_formation::receipt::accept_receipt(&assignment, &relayed_tls.receipt)
            .expect("the relayed TLS receipt is internally consistent");
    }

    #[test]
    fn every_observation_satisfied_is_a_fully_satisfied_receipt() {
        let (contract, derivation) = (contract(), derivation());
        let spec = spec(&contract, &derivation, TREE, Some(SNAPSHOT));
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
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

                tls: None,
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
        let point = verify_observed_candidate(
            &spec,
            served(503, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
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
        let point =
            verify_observed_candidate(&spec, http, Vec::new(), Vec::new(), execution(), &hosted());
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
            Vec::new(),
            Vec::new(),
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
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
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
        let point = verify_observed_candidate(
            &restored_other,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
        assert_consistent(&point, &contract);
        assert_eq!(
            failure(&point, "saved-state").as_deref(),
            Some("instance_snapshot_mismatch")
        );
        let restored_none = spec(&contract, &derivation, TREE, None);
        let point = verify_observed_candidate(
            &restored_none,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
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
            Vec::new(),
            Vec::new(),
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
            Vec::new(),
            Vec::new(),
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
            server_name: None,
            start: None,
            ..http("posted", "/submit", None)
        });
        contract.requirements.push(BoundRequirement {
            port: Some("admin.http".to_owned()),
            server_name: None,
            start: None,
            ..http("unexported", "/admin", None)
        });
        contract.requirements.push(BoundRequirement {
            path: None,
            server_name: None,
            start: None,
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
        let point = verify_observed_candidate(
            &spec,
            served(200, PROOF),
            Vec::new(),
            Vec::new(),
            execution(),
            &hosted(),
        );
        assert_eq!(
            failure(&point, "posted").as_deref(),
            Some("runtime_observation_missing")
        );
        assert_eq!(
            failure(&point, "unexported").as_deref(),
            Some("runtime_observation_missing")
        );
    }

    fn tls_requirement(
        id: &str,
        port: &str,
        server_name: &str,
        start: TlsStart,
    ) -> BoundRequirement {
        BoundRequirement {
            port: Some(port.to_owned()),
            server_name: Some(server_name.to_owned()),
            start: Some(start),
            ..requirement(id, TLS_CONTRACT_VERIFIER)
        }
    }

    fn tls_facts(id: &str, port: &str, server_name: &str) -> RuntimeTlsObservation {
        RuntimeTlsObservation {
            requirement_id: id.to_owned(),
            port: port.to_owned(),
            server_name: server_name.to_owned(),
            start: TlsStart::Implicit,
            presented_serial: "1234abcd".to_owned(),
            san: vec![server_name.to_owned()],
            not_before: "2026-01-01T00:00:00Z".to_owned(),
            not_after: "2046-01-01T00:00:00Z".to_owned(),
            days_remaining: 7000,
            chain_length: 2,
            trust_anchor: "public-roots".to_owned(),
            trusted: true,
            handshake_completed: true,
            server_name_matches: true,
            within_validity: true,
        }
    }

    fn tls_contract(requirement: BoundRequirement) -> BoundContract {
        BoundContract {
            schema: BOUND_CONTRACT_SCHEMA.to_owned(),
            requirements: vec![requirement],
        }
    }

    #[test]
    fn formation_defers_a_tls_observation_to_the_runtime() {
        let contract = tls_contract(tls_requirement(
            "imaps",
            "app.http",
            "mail.example",
            TlsStart::Implicit,
        ));
        let candidate = CandidateObservation {
            exported_ports: ["app.http".to_owned()].into(),
            ..Default::default()
        };
        let verification = verify(&contract, &candidate);
        assert_eq!(
            verification.verdicts[0].outcome,
            ObservationOutcome::Deferred {
                by: "runtime tls verifier imaps".to_owned()
            }
        );
        // A deferred gate is not satisfaction, but it does not block sealing.
        assert!(!verification.fully_satisfied());
        assert!(verification.seal_admissible());
    }

    #[test]
    fn a_valid_certificate_satisfies_tls_and_the_receipt_records_its_facts() {
        let contract = tls_contract(tls_requirement(
            "imaps",
            "app.http",
            "mail.example",
            TlsStart::Implicit,
        ));
        let derivation = derivation();
        let spec = spec(&contract, &derivation, TREE, None);
        let point = verify_observed_candidate(
            &spec,
            Vec::new(),
            Vec::new(),
            vec![tls_facts("imaps", "app.http", "mail.example")],
            execution(),
            &hosted(),
        );
        assert!(point.receipt.fully_satisfied);
        assert_eq!(
            point.receipt.schema,
            ato_formation::verify::CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V3
        );
        let evidence = point.receipt.observations[0]
            .evidence
            .as_ref()
            .expect("a satisfied TLS verdict carries evidence");
        let tls = evidence.tls.as_ref().expect("the certificate was recorded");
        assert_eq!(tls.server_name, "mail.example");
        assert_eq!(tls.start, TlsStart::Implicit);
        assert_eq!(tls.presented_serial, "1234abcd");
        assert_eq!(tls.san, vec!["mail.example".to_owned()]);
        assert_eq!(tls.not_before, "2026-01-01T00:00:00Z");
        assert_eq!(tls.not_after, "2046-01-01T00:00:00Z");
        assert_eq!(tls.chain_length, 2);
        assert_eq!(tls.trust_anchor, "public-roots");
        // A TLS observation states where it connected to; nothing else.
        assert!(evidence.method.is_none());
        assert!(evidence.path.is_none());
        assert!(evidence.status.is_none());
        assert!(evidence.body_sha256.is_none());
        assert!(evidence.input.is_none());
        assert!(evidence.digest.is_none());
        // The receipt is accepted only because its own evidence agrees with K.
        let assignment = ato_formation::receipt::ReceiptAssignment {
            contract: &contract,
            contract_ref: "sha256:k",
            derivation_ref: "sha256:d",
            attempt_id: "attempt-1",
            request_id: None,
        };
        ato_formation::receipt::accept_receipt(&assignment, &point.receipt)
            .expect("the TLS receipt is internally consistent");
    }

    fn tls_failure_code(contract: &BoundContract, tls: RuntimeTlsObservation) -> &'static str {
        let verification = verify_runtime(
            contract,
            &RuntimeObservation {
                tls: vec![tls],
                ..Default::default()
            },
        );
        match &verification.verdicts[0].outcome {
            ObservationOutcome::Failed { code, .. } => code,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn every_tls_failure_names_the_check_that_failed() {
        let contract = tls_contract(tls_requirement(
            "imaps",
            "app.http",
            "mail.example",
            TlsStart::Implicit,
        ));
        let assertion = |facts: RuntimeTlsObservation, code: &str| {
            assert_eq!(tls_failure_code(&contract, facts), code);
        };
        // No certificate was observed at all.
        assertion(
            RuntimeTlsObservation {
                requirement_id: "other".to_owned(),
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_observation_missing",
        );
        // The certificate covers another name.
        assertion(
            RuntimeTlsObservation {
                server_name_matches: false,
                trusted: false,
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_server_name_mismatch",
        );
        // The certificate is outside its validity window.
        assertion(
            RuntimeTlsObservation {
                within_validity: false,
                trusted: false,
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_certificate_outside_validity",
        );
        // The chain does not validate to a public root.
        assertion(
            RuntimeTlsObservation {
                trusted: false,
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_chain_untrusted",
        );
        // The observed port or start mode is not the one K names.
        assertion(
            RuntimeTlsObservation {
                start: TlsStart::SmtpStarttls,
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_observation_mismatch",
        );
        // A certificate can validate while the handshake then fails. Nothing
        // completed on that connection, so the observation cannot satisfy K.
        assertion(
            RuntimeTlsObservation {
                handshake_completed: false,
                ..tls_facts("imaps", "app.http", "mail.example")
            },
            "tls_handshake_incomplete",
        );
    }

    #[test]
    fn a_tls_receipt_is_accepted_only_as_v3_with_public_root_evidence() {
        let contract = tls_contract(tls_requirement(
            "imaps",
            "app.http",
            "mail.example",
            TlsStart::Implicit,
        ));
        let derivation = derivation();
        let spec = spec(&contract, &derivation, TREE, None);
        let point = verify_observed_candidate(
            &spec,
            Vec::new(),
            Vec::new(),
            vec![tls_facts("imaps", "app.http", "mail.example")],
            execution(),
            &hosted(),
        );
        let assignment = ato_formation::receipt::ReceiptAssignment {
            contract: &contract,
            contract_ref: "sha256:k",
            derivation_ref: "sha256:d",
            attempt_id: "attempt-1",
            request_id: None,
        };
        ato_formation::receipt::accept_receipt(&assignment, &point.receipt)
            .expect("the baseline TLS receipt is consistent");

        let code = |receipt: &ato_formation::verify::ContractVerificationReceipt| {
            ato_formation::receipt::accept_receipt(&assignment, receipt)
                .expect_err("the receipt must be refused")
                .code
        };

        let mut downgraded = point.receipt.clone();
        downgraded.schema =
            ato_formation::verify::CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2.to_owned();
        assert_eq!(code(&downgraded), "receipt_tls_evidence_schema");

        let mut test_anchor = point.receipt.clone();
        test_anchor.observations[0]
            .evidence
            .as_mut()
            .unwrap()
            .tls
            .as_mut()
            .unwrap()
            .trust_anchor = "test-only".to_owned();
        assert_eq!(code(&test_anchor), "receipt_evidence_inconsistent");

        let mut incomplete = point.receipt.clone();
        incomplete.observations[0]
            .evidence
            .as_mut()
            .unwrap()
            .tls
            .as_mut()
            .unwrap()
            .handshake_completed = false;
        assert_eq!(code(&incomplete), "receipt_evidence_inconsistent");
    }
}
