//! `C' ⊨ K` — does the candidate this Derivation produced actually satisfy the
//! Contract?
//!
//! ## Why this exists at all
//!
//! Formation used to succeed when execution succeeded. That is a claim about
//! our build machinery, not about the author's App: a build can finish, publish
//! an artifact and report success while producing something that does not
//! satisfy a single thing the author said had to be true. Under a model where
//! the Capsule's identity IS the Contract, sealing on "the build worked" mints
//! an identity that nobody checked.
//!
//! So Formation succeeds when the candidate satisfies `K`, and not before.
//!
//! ## Honest about when each condition is decided
//!
//! Formation forms an artifact. It does not start the author's process — that
//! happens later, on a Runner — so some observations cannot be decided here.
//! Rather than pass them silently, each condition resolves to one of three
//! outcomes:
//!
//! - **Satisfied** — decided here, from the candidate the build produced.
//! - **Deferred** — decided at run time by a gate that provably covers this
//!   exact observation, and named so a reader knows which gate.
//! - **Failed** — including the case nobody would think to test for: an
//!   observation that NOTHING will ever check. An authored contract observing
//!   `/health` while readiness probes `/` is not "probably fine"; it is a
//!   Capsule identity claiming an observation no gate performs, and it fails
//!   closed.
//!
//! A `Deferred` outcome is a promise about a gate that exists. It is produced
//! only when the candidate's own run-time readiness names the same port and the
//! same path — which the projection guarantees by DERIVING readiness from the
//! Contract rather than beside it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authoring::{BoundContract, HTTP_CONTRACT_VERIFIER, WORKSPACE_CONTRACT_VERIFIER};

pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA: &str = "ato.contract-verification-receipt/1";
pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2: &str = "ato.contract-verification-receipt/2";

/// What the executed Derivation actually produced, in the terms `K` observes.
///
/// Everything here is something the worker holds by the time it has an
/// artifact. Nothing here requires a running process: an observation that would
/// is `Deferred` or `Failed`, never assumed.
#[derive(Debug, Clone, Default)]
pub struct CandidateObservation {
    /// The resolved identity of each input the Derivation consumed.
    pub input_refs: BTreeMap<String, String>,
    /// Ports the candidate exports, by port id.
    pub exported_ports: BTreeSet<String>,
    /// Request paths the candidate serves from its own artifact, decided
    /// without running anything — a static surface knows its own files.
    pub statically_served_paths: BTreeSet<String>,
    /// The run-time readiness gate this candidate will be admitted by:
    /// `(port id, request path)`. An HTTP observation matching it is deferred
    /// to that gate; one that does not match is checked by nothing.
    pub runtime_readiness: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ObservationOutcome {
    Satisfied,
    /// Decided later, by the named gate.
    Deferred {
        by: String,
    },
    Failed {
        code: &'static str,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationVerdict {
    pub id: String,
    pub outcome: ObservationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVerification {
    pub verdicts: Vec<ObservationVerdict>,
}

impl ContractVerification {
    /// May this candidate be sealed under the Contract?
    ///
    /// Every condition is either decided satisfied here, or handed to a gate
    /// that will decide it. None is skipped.
    pub fn passed(&self) -> bool {
        self.seal_admissible()
    }

    /// May Formation seal this candidate while naming every remaining gate?
    pub fn seal_admissible(&self) -> bool {
        self.verdicts
            .iter()
            .all(|verdict| !matches!(verdict.outcome, ObservationOutcome::Failed { .. }))
    }

    /// Has an actual run decided every condition successfully?
    ///
    /// Interoperability receipts require this stronger result. A deferred
    /// Formation verdict is deliberately not launch success.
    pub fn fully_satisfied(&self) -> bool {
        self.verdicts
            .iter()
            .all(|verdict| verdict.outcome == ObservationOutcome::Satisfied)
    }

    /// The first reason this candidate is not the Capsule it claims to be.
    pub fn failure(&self) -> Option<(&str, &str, &str)> {
        self.verdicts
            .iter()
            .find_map(|verdict| match &verdict.outcome {
                ObservationOutcome::Failed { code, detail } => {
                    Some((verdict.id.as_str(), *code, detail.as_str()))
                }
                _ => None,
            })
    }

    /// A one-line summary for a receipt.
    pub fn summary(&self) -> String {
        let satisfied = self
            .verdicts
            .iter()
            .filter(|v| v.outcome == ObservationOutcome::Satisfied)
            .count();
        let deferred = self
            .verdicts
            .iter()
            .filter(|v| matches!(v.outcome, ObservationOutcome::Deferred { .. }))
            .count();
        let failed = self.verdicts.len() - satisfied - deferred;
        format!("{satisfied} satisfied, {deferred} deferred, {failed} failed")
    }
}

/// An HTTP response read from the actual running candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHttpObservation {
    pub port: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub body_digest: String,
}

impl RuntimeHttpObservation {
    pub fn from_response(
        port: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
        status: u16,
        body: &[u8],
    ) -> Self {
        Self {
            port: port.into(),
            method: method.into(),
            path: path.into(),
            status,
            body_digest: format!("sha256:{:x}", Sha256::digest(body)),
        }
    }
}

/// Evidence gathered from an actual running candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeObservation {
    pub input_refs: BTreeMap<String, String>,
    pub http: Vec<RuntimeHttpObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationTargetKind {
    CliLocal,
    AtoRunHosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationTarget {
    pub kind: VerificationTargetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Satisfied,
    Deferred,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptObservation {
    pub id: String,
    pub outcome: ReceiptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<VerificationEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Receipt-safe facts about the concrete realization selected for this Run.
/// These fields are evidence, never Contract identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationExecutionEvidence {
    pub realization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// Immutable objects fetched from outside the bundle for this attempt.
    /// Only emitted with receipt schema /2; never part of K or D identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_fetches: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portability_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_oci_image_loaded: Option<String>,
}

/// Shared proof emitted by both local and hosted execution paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVerificationReceipt {
    pub schema: String,
    pub bundle_sha256: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub target: VerificationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<VerificationExecutionEvidence>,
    pub observations: Vec<ReceiptObservation>,
    pub fully_satisfied: bool,
}

impl ContractVerificationReceipt {
    pub fn new(
        bundle_sha256: impl Into<String>,
        contract_ref: impl Into<String>,
        derivation_ref: impl Into<String>,
        target: VerificationTargetKind,
        verification: ContractVerification,
    ) -> Self {
        let fully_satisfied = verification.fully_satisfied();
        let observations = verification
            .verdicts
            .into_iter()
            .map(|verdict| receipt_observation(verdict, None))
            .collect();
        Self {
            schema: CONTRACT_VERIFICATION_RECEIPT_SCHEMA.to_owned(),
            bundle_sha256: bundle_sha256.into(),
            contract_ref: contract_ref.into(),
            derivation_ref: derivation_ref.into(),
            target: VerificationTarget { kind: target },
            execution: None,
            observations,
            fully_satisfied,
        }
    }

    pub fn from_runtime(
        bundle_sha256: impl Into<String>,
        contract_ref: impl Into<String>,
        derivation_ref: impl Into<String>,
        target: VerificationTargetKind,
        contract: &BoundContract,
        runtime: &RuntimeObservation,
        verification: ContractVerification,
    ) -> Self {
        let fully_satisfied = verification.fully_satisfied();
        let observations = verification
            .verdicts
            .into_iter()
            .map(|verdict| {
                let requirement = contract
                    .requirements
                    .iter()
                    .find(|requirement| requirement.id == verdict.id);
                let evidence =
                    requirement.and_then(|requirement| match requirement.verifier.as_str() {
                        HTTP_CONTRACT_VERIFIER => runtime
                            .http
                            .iter()
                            .find(|observation| {
                                requirement.port.as_deref() == Some(observation.port.as_str())
                                    && requirement.method.as_deref()
                                        == Some(observation.method.as_str())
                                    && requirement.path.as_deref()
                                        == Some(observation.path.as_str())
                            })
                            .map(|observation| VerificationEvidence {
                                method: Some(observation.method.clone()),
                                path: Some(observation.path.clone()),
                                status: Some(observation.status),
                                body_sha256: Some(observation.body_digest.clone()),
                                input: None,
                                digest: None,
                            }),
                        WORKSPACE_CONTRACT_VERIFIER => {
                            requirement
                                .input
                                .as_ref()
                                .map(|input| VerificationEvidence {
                                    method: None,
                                    path: None,
                                    status: None,
                                    body_sha256: None,
                                    input: Some(input.clone()),
                                    digest: runtime.input_refs.get(input).cloned(),
                                })
                        }
                        _ => None,
                    });
                receipt_observation(verdict, evidence)
            })
            .collect();
        Self {
            schema: CONTRACT_VERIFICATION_RECEIPT_SCHEMA.to_owned(),
            bundle_sha256: bundle_sha256.into(),
            contract_ref: contract_ref.into(),
            derivation_ref: derivation_ref.into(),
            target: VerificationTarget { kind: target },
            execution: None,
            observations,
            fully_satisfied,
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_jcs::to_vec(self)
    }
}

fn receipt_observation(
    verdict: ObservationVerdict,
    evidence: Option<VerificationEvidence>,
) -> ReceiptObservation {
    let (outcome, deferred_by, failure) = match verdict.outcome {
        ObservationOutcome::Satisfied => (ReceiptOutcome::Satisfied, None, None),
        ObservationOutcome::Deferred { by } => (ReceiptOutcome::Deferred, Some(by), None),
        ObservationOutcome::Failed { code, detail } => (
            ReceiptOutcome::Failed,
            None,
            Some(format!("{code}: {detail}")),
        ),
    };
    ReceiptObservation {
        id: verdict.id,
        outcome,
        evidence,
        deferred_by,
        failure,
    }
}

/// Decide every condition in the Contract against the candidate.
pub fn verify(contract: &BoundContract, candidate: &CandidateObservation) -> ContractVerification {
    let verdicts = contract
        .requirements
        .iter()
        .map(|requirement| {
            let outcome = match requirement.verifier.as_str() {
                WORKSPACE_CONTRACT_VERIFIER => {
                    let input = requirement.input.as_deref().unwrap_or_default();
                    let expected = requirement.digest.as_deref().unwrap_or_default();
                    match candidate.input_refs.get(input) {
                        Some(actual) if actual == expected => ObservationOutcome::Satisfied,
                        Some(actual) => ObservationOutcome::Failed {
                            code: "input_identity_mismatch",
                            detail: format!(
                                "{input} resolved to {actual}, and this Capsule is the one \
                                 whose {input} is {expected}"
                            ),
                        },
                        None => ObservationOutcome::Failed {
                            code: "input_not_resolved",
                            detail: format!("the candidate resolved no input named {input}"),
                        },
                    }
                }
                HTTP_CONTRACT_VERIFIER => {
                    let port = requirement.port.as_deref().unwrap_or_default();
                    let path = requirement.path.as_deref().unwrap_or("/");
                    if !candidate.exported_ports.contains(port) {
                        ObservationOutcome::Failed {
                            code: "port_not_exported",
                            detail: format!(
                                "the Contract observes port {port}, which this candidate does \
                                 not export"
                            ),
                        }
                    } else if requirement.body_digest.is_some()
                        && (candidate.statically_served_paths.contains(path)
                            || candidate.runtime_readiness.as_ref().is_some_and(
                                |(gate_port, gate_path)| gate_port == port && gate_path == path,
                            ))
                    {
                        // Formation can prove the exact endpoint exists, but
                        // only the runtime verifier reads its response body.
                        ObservationOutcome::Deferred {
                            by: format!("runtime contract verifier {port} GET {path}"),
                        }
                    } else if requirement.status == Some(200)
                        && candidate.statically_served_paths.contains(path)
                    {
                        // A static surface answers this from its own files.
                        // Decided now, from the artifact that was just built.
                        ObservationOutcome::Satisfied
                    } else if candidate.runtime_readiness.as_ref().is_some_and(
                        |(gate_port, gate_path)| gate_port == port && gate_path == path,
                    ) {
                        ObservationOutcome::Deferred {
                            by: format!("runtime readiness {port} {path}"),
                        }
                    } else {
                        ObservationOutcome::Failed {
                            code: "observation_unverifiable",
                            detail: format!(
                                "nothing checks GET {path} on {port}: the candidate does not \
                                 serve it from its artifact and its readiness gate does not \
                                 probe it"
                            ),
                        }
                    }
                }
                other => ObservationOutcome::Failed {
                    code: "verifier_unknown",
                    detail: format!("no verifier named {other} is available to decide this"),
                },
            };
            ObservationVerdict {
                id: requirement.id.clone(),
                outcome,
            }
        })
        .collect();
    ContractVerification { verdicts }
}

/// Decide every condition against evidence read from the actual run.
///
/// This path has no deferred result: a missing observation fails closed.
pub fn verify_runtime(
    contract: &BoundContract,
    candidate: &RuntimeObservation,
) -> ContractVerification {
    let verdicts = contract
        .requirements
        .iter()
        .map(|requirement| {
            let outcome = match requirement.verifier.as_str() {
                WORKSPACE_CONTRACT_VERIFIER => {
                    let input = requirement.input.as_deref().unwrap_or_default();
                    let expected = requirement.digest.as_deref().unwrap_or_default();
                    match candidate.input_refs.get(input) {
                        Some(actual) if actual == expected => ObservationOutcome::Satisfied,
                        Some(actual) => ObservationOutcome::Failed {
                            code: "input_identity_mismatch",
                            detail: format!("{input} resolved to {actual}; expected {expected}"),
                        },
                        None => ObservationOutcome::Failed {
                            code: "input_not_resolved",
                            detail: format!("the runtime resolved no input named {input}"),
                        },
                    }
                }
                HTTP_CONTRACT_VERIFIER => {
                    let port = requirement.port.as_deref().unwrap_or_default();
                    let method = requirement.method.as_deref().unwrap_or("GET");
                    let path = requirement.path.as_deref().unwrap_or("/");
                    match candidate.http.iter().find(|observation| {
                        observation.port == port
                            && observation.method == method
                            && observation.path == path
                    }) {
                        None => ObservationOutcome::Failed {
                            code: "runtime_observation_missing",
                            detail: format!(
                                "the runtime did not observe {method} {path} on {port}"
                            ),
                        },
                        Some(observation)
                            if requirement
                                .status
                                .is_some_and(|status| observation.status != status) =>
                        {
                            ObservationOutcome::Failed {
                                code: "http_status_mismatch",
                                detail: format!(
                                    "{method} {path} on {port} returned {}; expected {}",
                                    observation.status,
                                    requirement.status.expect("checked as some")
                                ),
                            }
                        }
                        Some(observation)
                            if requirement
                                .body_digest
                                .as_ref()
                                .is_some_and(|digest| observation.body_digest != *digest) =>
                        {
                            ObservationOutcome::Failed {
                                code: "http_body_digest_mismatch",
                                detail: format!(
                                    "{method} {path} on {port} produced {}; expected {}",
                                    observation.body_digest,
                                    requirement.body_digest.as_deref().expect("checked as some")
                                ),
                            }
                        }
                        Some(_) => ObservationOutcome::Satisfied,
                    }
                }
                other => ObservationOutcome::Failed {
                    code: "verifier_unknown",
                    detail: format!("no verifier named {other} is available to decide this"),
                },
            };
            ObservationVerdict {
                id: requirement.id.clone(),
                outcome,
            }
        })
        .collect();
    ContractVerification { verdicts }
}

#[cfg(test)]
mod tests {
    use sha2::Digest;

    use super::*;
    use crate::authoring::{BOUND_CONTRACT_SCHEMA, BoundRequirement};

    fn http(id: &str, port: &str, path: &str) -> BoundRequirement {
        BoundRequirement {
            id: id.to_owned(),
            verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
            port: Some(port.to_owned()),
            method: Some("GET".to_owned()),
            path: Some(path.to_owned()),
            status: Some(200),
            body_digest: None,
            input: None,
            digest: None,
        }
    }

    fn identity(id: &str, input: &str, digest: &str) -> BoundRequirement {
        BoundRequirement {
            id: id.to_owned(),
            verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
            port: None,
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: Some(input.to_owned()),
            digest: Some(digest.to_owned()),
        }
    }

    fn contract(requirements: Vec<BoundRequirement>) -> BoundContract {
        BoundContract {
            schema: BOUND_CONTRACT_SCHEMA.to_owned(),
            requirements,
        }
    }

    #[test]
    fn a_static_surface_decides_its_own_root_observation_now() {
        let k = contract(vec![
            http("root", "app.http", "/"),
            identity("source", "workspace", "sha256:aa"),
        ]);
        let candidate = CandidateObservation {
            input_refs: [("workspace".to_owned(), "sha256:aa".to_owned())].into(),
            exported_ports: ["app.http".to_owned()].into(),
            statically_served_paths: ["/".to_owned()].into(),
            runtime_readiness: None,
        };
        let verification = verify(&k, &candidate);
        assert!(verification.passed(), "{verification:?}");
        assert_eq!(verification.summary(), "2 satisfied, 0 deferred, 0 failed");
    }

    #[test]
    fn a_source_that_is_not_the_one_the_capsule_names_fails() {
        let k = contract(vec![identity("source", "workspace", "sha256:aa")]);
        let candidate = CandidateObservation {
            input_refs: [("workspace".to_owned(), "sha256:bb".to_owned())].into(),
            ..Default::default()
        };
        let verification = verify(&k, &candidate);
        assert!(!verification.passed());
        assert_eq!(verification.failure().unwrap().1, "input_identity_mismatch");
    }

    #[test]
    fn a_process_observation_is_deferred_to_the_gate_that_actually_probes_it() {
        let k = contract(vec![http("app", "app.http", "/health")]);
        let candidate = CandidateObservation {
            exported_ports: ["app.http".to_owned()].into(),
            runtime_readiness: Some(("app.http".to_owned(), "/health".to_owned())),
            ..Default::default()
        };
        let verification = verify(&k, &candidate);
        assert!(verification.passed());
        assert_eq!(
            verification.verdicts[0].outcome,
            ObservationOutcome::Deferred {
                by: "runtime readiness app.http /health".to_owned()
            }
        );
    }

    #[test]
    fn an_observation_no_gate_performs_fails_closed() {
        // The bug this is here to catch, and the one nobody writes a test for:
        // the Contract says `/health` and the readiness probe hits `/`. Both
        // look fine in isolation. Together they mint a Capsule whose identity
        // rests on an observation that never happens.
        let k = contract(vec![http("app", "app.http", "/health")]);
        let candidate = CandidateObservation {
            exported_ports: ["app.http".to_owned()].into(),
            runtime_readiness: Some(("app.http".to_owned(), "/".to_owned())),
            ..Default::default()
        };
        let verification = verify(&k, &candidate);
        assert!(!verification.passed());
        assert_eq!(
            verification.failure().unwrap().1,
            "observation_unverifiable"
        );
    }

    #[test]
    fn an_observation_of_a_port_the_candidate_does_not_export_fails() {
        let k = contract(vec![http("app", "app.http", "/")]);
        let verification = verify(&k, &CandidateObservation::default());
        assert_eq!(verification.failure().unwrap().1, "port_not_exported");
    }

    #[test]
    fn a_body_digest_is_never_reported_as_checked_by_something_that_did_not_read_it() {
        let mut requirement = http("root", "app.http", "/");
        requirement.body_digest = Some("sha256:cc".to_owned());
        let k = contract(vec![requirement]);
        let candidate = CandidateObservation {
            exported_ports: ["app.http".to_owned()].into(),
            statically_served_paths: ["/".to_owned()].into(),
            ..Default::default()
        };
        let verification = verify(&k, &candidate);
        assert!(verification.seal_admissible(), "{verification:?}");
        assert!(!verification.fully_satisfied());
        assert!(matches!(
            verification.verdicts[0].outcome,
            ObservationOutcome::Deferred { .. }
        ));
    }

    #[test]
    fn runtime_body_observation_must_match_before_interop_is_fully_satisfied() {
        let body = b"ato-k-interop-v1\n";
        let mut requirement = http("proof", "app.http", "/proof.txt");
        requirement.body_digest = Some(format!("sha256:{:x}", sha2::Sha256::digest(body)));
        let k = contract(vec![requirement]);
        let observation = RuntimeObservation {
            http: vec![RuntimeHttpObservation::from_response(
                "app.http",
                "GET",
                "/proof.txt",
                200,
                body,
            )],
            ..Default::default()
        };
        let verification = verify_runtime(&k, &observation);
        assert!(verification.fully_satisfied(), "{verification:?}");
    }

    #[test]
    fn runtime_candidate_with_the_wrong_proof_body_fails_k() {
        let mut requirement = http("proof", "app.http", "/proof.txt");
        requirement.body_digest = Some(format!(
            "sha256:{:x}",
            sha2::Sha256::digest(b"ato-k-interop-v1\n")
        ));
        let k = contract(vec![requirement]);
        let observation = RuntimeObservation {
            http: vec![RuntimeHttpObservation::from_response(
                "app.http",
                "GET",
                "/proof.txt",
                200,
                b"different\n",
            )],
            ..Default::default()
        };
        let verification = verify_runtime(&k, &observation);
        assert!(!verification.fully_satisfied());
        assert_eq!(
            verification.failure().unwrap().1,
            "http_body_digest_mismatch"
        );
    }

    #[test]
    fn deferred_formation_is_not_interop_success() {
        let verification = ContractVerification {
            verdicts: vec![ObservationVerdict {
                id: "proof".to_owned(),
                outcome: ObservationOutcome::Deferred {
                    by: "runtime contract verifier".to_owned(),
                },
            }],
        };
        assert!(verification.seal_admissible());
        assert!(verification.passed());
        assert!(!verification.fully_satisfied());
    }

    #[test]
    fn runtime_receipt_uses_the_shared_schema_and_keeps_transport_hash_separate() {
        let verification = ContractVerification {
            verdicts: vec![ObservationVerdict {
                id: "proof".to_owned(),
                outcome: ObservationOutcome::Satisfied,
            }],
        };
        let receipt = ContractVerificationReceipt::new(
            "sha256:bundle",
            "sha256:contract",
            "sha256:derivation",
            VerificationTargetKind::CliLocal,
            verification,
        );
        let bytes = receipt.canonical_bytes().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema"], "ato.contract-verification-receipt/1");
        assert_eq!(value["target"]["kind"], "cli-local");
        assert_eq!(value["bundle_sha256"], "sha256:bundle");
        assert_eq!(value["contract_ref"], "sha256:contract");
        assert_eq!(value["fully_satisfied"], true);
    }
}
