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

use crate::authoring::{BoundContract, HTTP_CONTRACT_VERIFIER, WORKSPACE_CONTRACT_VERIFIER};

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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationVerdict {
    pub id: String,
    pub outcome: ObservationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractVerification {
    pub verdicts: Vec<ObservationVerdict>,
}

impl ContractVerification {
    /// May this candidate be sealed under the Contract?
    ///
    /// Every condition is either decided satisfied here, or handed to a gate
    /// that will decide it. None is skipped.
    pub fn passed(&self) -> bool {
        self.verdicts
            .iter()
            .all(|verdict| !matches!(verdict.outcome, ObservationOutcome::Failed { .. }))
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
                    } else if requirement.body_digest.is_some() {
                        // Would need the response. Refused at bind time when
                        // asked for as `capture`; refused here when stated,
                        // because nothing on this path reads a body.
                        ObservationOutcome::Failed {
                            code: "body_digest_unverifiable",
                            detail: "a response body is not read by this build; the observation \
                                 would be recorded as checked without anything checking it"
                                .to_owned(),
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

#[cfg(test)]
mod tests {
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
        assert_eq!(
            verify(&k, &candidate).failure().unwrap().1,
            "body_digest_unverifiable"
        );
    }
}
