//! May a receipt that arrived from somewhere else be accepted as the result
//! of the attempt it claims to be?
//!
//! A receipt is evidence about one attempt of one request: this K, this D,
//! this attempt. Whoever receives it — a coordinator, a requester — checks
//! that it is that, and that it is internally honest, before counting it.
//! Nothing here re-observes the candidate; it checks the claim against the
//! frozen K and the assignment.
//!
//! What is NOT here: who sent it. Issuer authentication, the lease and the
//! fence belong to the channel the receipt arrived on and are checked there.
//! A receipt that passes this is well-formed for its assignment, not
//! authentic.

use std::collections::BTreeSet;

use crate::authoring::{
    BoundContract, HTTP_CONTRACT_VERIFIER, INSTANCE_SNAPSHOT_CONTRACT_VERIFIER,
    WORKSPACE_CONTRACT_VERIFIER,
};
use crate::verify::{
    CONTRACT_VERIFICATION_RECEIPT_SCHEMA, CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2,
    ContractVerificationReceipt, ReceiptObservation, ReceiptOutcome,
};

/// What the receiver assigned: the attempt a receipt must be about.
pub struct ReceiptAssignment<'a> {
    /// The frozen K. Its requirement ids are the exact observation set.
    pub contract: &'a BoundContract,
    pub contract_ref: &'a str,
    pub derivation_ref: &'a str,
    pub attempt_id: &'a str,
    /// The request the attempt spent budget for, when the receiver knows it.
    pub request_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRejection {
    pub code: &'static str,
    pub detail: String,
}

fn reject(code: &'static str, detail: impl Into<String>) -> ReceiptRejection {
    ReceiptRejection {
        code,
        detail: detail.into(),
    }
}

/// Parse and check a receipt received as JSON.
pub fn accept_receipt_json(
    assignment: &ReceiptAssignment<'_>,
    receipt: &serde_json::Value,
) -> Result<ContractVerificationReceipt, ReceiptRejection> {
    let receipt: ContractVerificationReceipt = serde_json::from_value(receipt.clone())
        .map_err(|error| reject("receipt_malformed", error.to_string()))?;
    accept_receipt(assignment, &receipt)?;
    Ok(receipt)
}

/// `Ok` when the receipt is about exactly this assignment and its verdicts
/// are consistent with its own evidence and the frozen K.
pub fn accept_receipt(
    assignment: &ReceiptAssignment<'_>,
    receipt: &ContractVerificationReceipt,
) -> Result<(), ReceiptRejection> {
    if receipt.schema != CONTRACT_VERIFICATION_RECEIPT_SCHEMA
        && receipt.schema != CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2
    {
        return Err(reject("receipt_schema_unknown", receipt.schema.clone()));
    }
    if receipt.contract_ref != assignment.contract_ref {
        return Err(reject(
            "receipt_contract_mismatch",
            format!(
                "{} is not {}",
                receipt.contract_ref, assignment.contract_ref
            ),
        ));
    }
    if receipt.derivation_ref != assignment.derivation_ref {
        return Err(reject(
            "receipt_derivation_mismatch",
            format!(
                "{} is not {}",
                receipt.derivation_ref, assignment.derivation_ref
            ),
        ));
    }
    let execution = receipt
        .execution
        .as_ref()
        .ok_or_else(|| reject("receipt_attempt_missing", "the receipt names no attempt"))?;
    if execution.attempt_id.as_deref() != Some(assignment.attempt_id) {
        return Err(reject(
            "receipt_attempt_mismatch",
            format!(
                "the receipt is about attempt {:?}, not {}",
                execution.attempt_id, assignment.attempt_id
            ),
        ));
    }
    if let Some(request_id) = assignment.request_id
        && execution.request_id.as_deref() != Some(request_id)
    {
        return Err(reject(
            "receipt_request_mismatch",
            format!(
                "the receipt is about request {:?}, not {request_id}",
                execution.request_id
            ),
        ));
    }

    // The observation set is exactly K's requirement set: nothing missing,
    // nothing extra, nothing twice.
    let mut seen = BTreeSet::new();
    for observation in &receipt.observations {
        if !seen.insert(observation.id.as_str()) {
            return Err(reject(
                "receipt_observation_duplicate",
                observation.id.clone(),
            ));
        }
    }
    let required: BTreeSet<&str> = assignment
        .contract
        .requirements
        .iter()
        .map(|requirement| requirement.id.as_str())
        .collect();
    if let Some(missing) = required.difference(&seen).next() {
        return Err(reject("receipt_observation_missing", (*missing).to_owned()));
    }
    if let Some(extra) = seen.difference(&required).next() {
        return Err(reject("receipt_observation_unknown", (*extra).to_owned()));
    }

    let all_satisfied = receipt
        .observations
        .iter()
        .all(|observation| observation.outcome == ReceiptOutcome::Satisfied);
    if receipt.fully_satisfied != all_satisfied {
        return Err(reject(
            "receipt_summary_inconsistent",
            "fully_satisfied does not follow from the observations",
        ));
    }
    for observation in &receipt.observations {
        check_observation(assignment.contract, observation)?;
    }
    Ok(())
}

/// A verdict must be what its own evidence and K say it is.
fn check_observation(
    contract: &BoundContract,
    observation: &ReceiptObservation,
) -> Result<(), ReceiptRejection> {
    let requirement = contract
        .requirements
        .iter()
        .find(|requirement| requirement.id == observation.id)
        .expect("the observation set was checked against K");
    let inconsistent = |detail: &str| {
        Err(reject(
            "receipt_evidence_inconsistent",
            format!("{}: {detail}", observation.id),
        ))
    };
    match observation.outcome {
        // A runtime receipt decides; it does not promise a later gate.
        ReceiptOutcome::Deferred => {
            return Err(reject(
                "receipt_observation_deferred",
                format!(
                    "{} was deferred in a receipt of an actual run",
                    observation.id
                ),
            ));
        }
        ReceiptOutcome::Failed => {
            if observation.failure.as_deref().is_none_or(str::is_empty) {
                return inconsistent("a failed verdict states no failure");
            }
            return Ok(());
        }
        ReceiptOutcome::Satisfied => {}
    }
    let Some(evidence) = observation.evidence.as_ref() else {
        return inconsistent("a satisfied verdict carries no evidence");
    };
    match requirement.verifier.as_str() {
        HTTP_CONTRACT_VERIFIER => {
            let method = requirement.method.as_deref().unwrap_or("GET");
            let path = requirement.path.as_deref().unwrap_or("/");
            if evidence.method.as_deref() != Some(method) || evidence.path.as_deref() != Some(path)
            {
                return inconsistent("the evidence is of a different request than K observes");
            }
            let Some(status) = evidence.status else {
                return inconsistent("no status was observed");
            };
            if requirement
                .status
                .is_some_and(|expected| expected != status)
            {
                return inconsistent("the observed status is not the one K requires");
            }
            let Some(body) = evidence.body_sha256.as_deref() else {
                return inconsistent("no body digest was observed");
            };
            if requirement
                .body_digest
                .as_deref()
                .is_some_and(|expected| expected != body)
            {
                return inconsistent("the observed body is not the one K requires");
            }
        }
        WORKSPACE_CONTRACT_VERIFIER => {
            if evidence.input != requirement.input || evidence.digest != requirement.digest {
                return inconsistent("the observed input identity is not the one K requires");
            }
        }
        INSTANCE_SNAPSHOT_CONTRACT_VERIFIER => {
            if evidence.digest != requirement.digest {
                return inconsistent("the restored snapshot is not the one K requires");
            }
        }
        other => {
            return Err(reject(
                "receipt_verifier_unknown",
                format!(
                    "{}: no verifier named {other} can have satisfied it",
                    observation.id
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::BoundRequirement;
    use crate::verify::{RuntimeHttpObservation, RuntimeObservation, verify_runtime};

    fn contract() -> BoundContract {
        BoundContract {
            schema: "ato.contract/1".to_owned(),
            requirements: vec![
                BoundRequirement {
                    id: "root".to_owned(),
                    verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
                    port: Some("app.http".to_owned()),
                    method: Some("GET".to_owned()),
                    path: Some("/".to_owned()),
                    status: Some(200),
                    body_digest: None,
                    input: None,
                    digest: None,
                },
                BoundRequirement {
                    id: "source".to_owned(),
                    verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
                    port: None,
                    method: None,
                    path: None,
                    status: None,
                    body_digest: None,
                    input: Some("workspace".to_owned()),
                    digest: Some("sha256:tree".to_owned()),
                },
            ],
        }
    }

    fn issued(status: u16) -> ContractVerificationReceipt {
        let contract = contract();
        let runtime = RuntimeObservation {
            input_refs: [("workspace".to_owned(), "sha256:tree".to_owned())].into(),
            http: vec![RuntimeHttpObservation::from_response(
                "app.http", "GET", "/", status, b"hi",
            )],
            instance_snapshot_ref: None,
        };
        let mut receipt = ContractVerificationReceipt::from_attempt(
            "sha256:k",
            "sha256:d",
            &contract,
            &runtime,
            verify_runtime(&contract, &runtime),
        );
        receipt.execution = Some(crate::verify::VerificationExecutionEvidence {
            realization: "process".to_owned(),
            runtime_executable: None,
            runtime_version: None,
            pid: None,
            container_id: None,
            image: None,
            platform: None,
            endpoint: None,
            run_id: None,
            lease_id: None,
            attempt_id: Some("att".to_owned()),
            request_id: Some("req".to_owned()),
            dependency_fetches: Vec::new(),
            portability_profile: None,
            embedded_oci_image_loaded: None,
            services: Vec::new(),
        });
        receipt
    }

    fn check(receipt: &ContractVerificationReceipt) -> Result<(), ReceiptRejection> {
        let contract = contract();
        accept_receipt(
            &ReceiptAssignment {
                contract: &contract,
                contract_ref: "sha256:k",
                derivation_ref: "sha256:d",
                attempt_id: "att",
                request_id: Some("req"),
            },
            receipt,
        )
    }

    fn code(receipt: &ContractVerificationReceipt) -> &'static str {
        check(receipt).unwrap_err().code
    }

    #[test]
    fn an_issued_receipt_is_accepted_passing_or_failing() {
        assert_eq!(check(&issued(200)), Ok(()));
        let failing = issued(500);
        assert!(!failing.fully_satisfied);
        assert_eq!(check(&failing), Ok(()));
    }

    #[test]
    fn a_receipt_about_another_assignment_is_refused() {
        let mut other = issued(200);
        other.contract_ref = "sha256:other".to_owned();
        assert_eq!(code(&other), "receipt_contract_mismatch");
        let mut other = issued(200);
        other.derivation_ref = "sha256:other".to_owned();
        assert_eq!(code(&other), "receipt_derivation_mismatch");
        let mut other = issued(200);
        other.execution.as_mut().unwrap().attempt_id = Some("another".to_owned());
        assert_eq!(code(&other), "receipt_attempt_mismatch");
        let mut other = issued(200);
        other.execution.as_mut().unwrap().request_id = Some("another".to_owned());
        assert_eq!(code(&other), "receipt_request_mismatch");
        let mut other = issued(200);
        other.execution = None;
        assert_eq!(code(&other), "receipt_attempt_missing");
    }

    #[test]
    fn the_observation_set_is_exactly_k() {
        let mut missing = issued(200);
        missing.observations.pop();
        assert_eq!(code(&missing), "receipt_observation_missing");
        let mut duplicate = issued(200);
        duplicate
            .observations
            .push(duplicate.observations[0].clone());
        assert_eq!(code(&duplicate), "receipt_observation_duplicate");
        let mut extra = issued(200);
        let mut invented = extra.observations[0].clone();
        invented.id = "invented".to_owned();
        extra.observations.push(invented);
        assert_eq!(code(&extra), "receipt_observation_unknown");
    }

    #[test]
    fn a_verdict_must_follow_from_its_evidence() {
        // A failing observation relabelled satisfied.
        let mut forged = issued(500);
        for observation in &mut forged.observations {
            observation.outcome = ReceiptOutcome::Satisfied;
            observation.failure = None;
        }
        forged.fully_satisfied = true;
        assert_eq!(code(&forged), "receipt_evidence_inconsistent");

        // A summary that does not follow from the verdicts.
        let mut summary = issued(500);
        summary.fully_satisfied = true;
        assert_eq!(code(&summary), "receipt_summary_inconsistent");

        // Satisfied with no evidence at all.
        let mut bare = issued(200);
        bare.observations[0].evidence = None;
        assert_eq!(code(&bare), "receipt_evidence_inconsistent");

        // Evidence of a different request than K observes.
        let mut elsewhere = issued(200);
        let root = elsewhere
            .observations
            .iter_mut()
            .find(|observation| observation.id == "root")
            .unwrap();
        root.evidence.as_mut().unwrap().path = Some("/other".to_owned());
        assert_eq!(code(&elsewhere), "receipt_evidence_inconsistent");

        // Deferred has no place in a receipt of an actual run.
        let mut deferred = issued(200);
        deferred.observations[0].outcome = ReceiptOutcome::Deferred;
        deferred.fully_satisfied = false;
        assert_eq!(code(&deferred), "receipt_observation_deferred");
    }

    #[test]
    fn a_receipt_round_trips_through_json() {
        let receipt = issued(200);
        let value = serde_json::to_value(&receipt).unwrap();
        let contract = contract();
        let accepted = accept_receipt_json(
            &ReceiptAssignment {
                contract: &contract,
                contract_ref: "sha256:k",
                derivation_ref: "sha256:d",
                attempt_id: "att",
                request_id: Some("req"),
            },
            &value,
        )
        .unwrap();
        assert_eq!(accepted, receipt);
        let mut unknown = value;
        unknown["surprise"] = serde_json::json!(true);
        assert!(
            accept_receipt_json(
                &ReceiptAssignment {
                    contract: &contract,
                    contract_ref: "sha256:k",
                    derivation_ref: "sha256:d",
                    attempt_id: "att",
                    request_id: Some("req"),
                },
                &unknown,
            )
            .is_err()
        );
    }
}
