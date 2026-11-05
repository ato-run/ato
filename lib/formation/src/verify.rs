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

use crate::authoring::{
    BoundContract, HTTP_CONTRACT_VERIFIER, INSTANCE_SNAPSHOT_CONTRACT_VERIFIER,
    SCENARIO_CONTRACT_VERIFIER, TLS_CONTRACT_VERIFIER, TlsStart, WORKSPACE_CONTRACT_VERIFIER,
};
use crate::scenario::{
    SCENARIO_VERIFIER_VERSION, ScenarioObservation, ScenarioResult, is_scenario_ref,
};

pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA: &str = "ato.contract-verification-receipt/1";
pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2: &str = "ato.contract-verification-receipt/2";
/// /3 adds structured TLS evidence to an observation: the certificate the run
/// actually received, not just the fact that a handshake succeeded.
pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V3: &str = "ato.contract-verification-receipt/3";
/// /4 adds the connector evidence a relayed Run produces: which connector
/// reached the Ports, the relay image digest and the relay container id.
pub const CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V4: &str = "ato.contract-verification-receipt/4";
/// The trust anchor label a certificate observation must carry to satisfy K:
/// the public root set, never a private or test anchor.
pub const TLS_TRUST_ANCHOR_PUBLIC_ROOTS: &str = "public-roots";

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
    /// Portable Instance snapshot installed before the candidate starts.
    pub instance_snapshot_ref: Option<String>,
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

/// One certificate as presented on a port the runtime could reach.
///
/// Everything here is a measured fact of this attempt. The verdict is not
/// stored: `verify_runtime` decides it, so a receipt can only state what was
/// observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTlsObservation {
    /// The TLS requirement this observation answers.
    pub requirement_id: String,
    /// The logical port this was observed on, as the Contract names it.
    pub port: String,
    /// The name the handshake verified the certificate against.
    pub server_name: String,
    pub start: TlsStart,
    /// The leaf serial number, lowercase hex without separators.
    pub presented_serial: String,
    /// The dNSName entries of the leaf's subjectAltName, ASCII-lowercased.
    pub san: Vec<String>,
    /// Validity bounds, RFC 3339 UTC.
    pub not_before: String,
    pub not_after: String,
    /// Whole days from the observation to `not_after`; negative once expired.
    pub days_remaining: i64,
    /// Leaf plus intermediates actually presented.
    pub chain_length: usize,
    /// The trust anchor set used to decide `trusted`.
    pub trust_anchor: String,
    /// The certificate passed full validation (chain to the anchors, name and
    /// validity) at observation time. Name and validity are also reported on
    /// their own so a failure can say which check failed.
    pub trusted: bool,
    /// The TLS handshake finished. A certificate can be presented and validate
    /// while the rest of the handshake fails (a broken server key, a proxy that
    /// resets after the certificate flight). Such a session is not an
    /// observation that K holds: nothing completed on that connection.
    pub handshake_completed: bool,
    /// The leaf's SAN covers `server_name`.
    pub server_name_matches: bool,
    /// Observation time was inside the certificate's validity.
    pub within_validity: bool,
}

/// Evidence gathered from an actual running candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeObservation {
    pub input_refs: BTreeMap<String, String>,
    pub http: Vec<RuntimeHttpObservation>,
    pub instance_snapshot_ref: Option<String>,
    /// What the runtime's scenario executor established, by requirement id.
    /// Empty on paths that do not execute scenarios (Formation, hosted), where
    /// a scenario requirement then fails closed as unobserved.
    pub scenarios: Vec<ScenarioObservation>,
    /// What the runtime saw when it connected to each TLS-observed port.
    pub tls: Vec<RuntimeTlsObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationTargetKind {
    CliLocal,
    AtoRunHosted,
    /// A Formation attempt on a Runtime: local, or a Runtime Network ticket.
    FormationRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationTarget {
    pub kind: VerificationTargetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Satisfied,
    Deferred,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `ato.contract.tls@1` only: the certificate that port presented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsEvidence>,
}

/// The certificate a run actually received, as receipt evidence.
///
/// The verdict (satisfied or not) is the observation's outcome; this records
/// what was presented, including the leaf serial that ops compares with the
/// current export. It is evidence, never Contract identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsEvidence {
    pub server_name: String,
    pub start: TlsStart,
    pub presented_serial: String,
    pub san: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub days_remaining: i64,
    pub chain_length: usize,
    pub trust_anchor: String,
    pub handshake_completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The request this attempt spent budget for. Retries, other Derivations
    /// and redeliveries of the same request carry the same id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Immutable objects fetched from outside the bundle for this attempt.
    /// Only emitted with receipt schema /2; never part of K or D identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_fetches: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portability_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_oci_image_loaded: Option<String>,
    /// How this Run reached its route's logical Ports: `direct` on a daemon
    /// whose container network this process can reach, `relay` when it cannot.
    /// A receipt that records it is schema /4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// The digest of the relay image the Run built and loaded, when it used one.
    /// Evidence only: it is not part of K, D or the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_image_digest: Option<String>,
    /// The relay container the Run owned, when it used one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_container_id: Option<String>,
    /// Whether the daemon ran this route's platform through emulation. True is
    /// evidence of how it ran, never a warning that replaces a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_emulated: Option<bool>,
    /// Every container of an OCI service group, in start order. Empty for
    /// every other realization, and then absent from the receipt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<VerificationServiceEvidence>,
}

/// One container of an OCI service group. Evidence only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationServiceEvidence {
    pub name: String,
    pub container_id: String,
    pub image: String,
}

/// Shared proof emitted by both local and hosted execution paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVerificationReceipt {
    pub schema: String,
    /// The transport the verified bytes arrived in, when there was one. A
    /// Formation attempt runs from a frozen source and has no `.capsule`
    /// yet; it names none rather than inventing a digest. Never identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_sha256: Option<String>,
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
            bundle_sha256: Some(bundle_sha256.into()),
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

                                tls: None,
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

                                    tls: None,
                                })
                        }
                        INSTANCE_SNAPSHOT_CONTRACT_VERIFIER => runtime
                            .instance_snapshot_ref
                            .as_ref()
                            .map(|digest| VerificationEvidence {
                                method: None,
                                path: None,
                                status: None,
                                body_sha256: None,
                                input: None,
                                digest: Some(digest.clone()),

                                tls: None,
                            }),
                        SCENARIO_CONTRACT_VERIFIER => runtime
                            .scenarios
                            .iter()
                            .find(|observation| observation.requirement_id == requirement.id)
                            .map(|observation| VerificationEvidence {
                                method: None,
                                path: Some(format!("scenario:{}", requirement.id)),
                                status: None,
                                body_sha256: Some(observation.transcript_digest.clone()),
                                input: None,
                                digest: Some(observation.scenario_ref.clone()),

                                tls: None,
                            }),
                        TLS_CONTRACT_VERIFIER => runtime
                            .tls
                            .iter()
                            .find(|observation| observation.requirement_id == requirement.id)
                            .map(|observation| VerificationEvidence {
                                method: None,
                                path: None,
                                status: None,
                                body_sha256: None,
                                input: None,
                                digest: None,
                                tls: Some(TlsEvidence {
                                    server_name: observation.server_name.clone(),
                                    start: observation.start,
                                    presented_serial: observation.presented_serial.clone(),
                                    san: observation.san.clone(),
                                    not_before: observation.not_before.clone(),
                                    not_after: observation.not_after.clone(),
                                    days_remaining: observation.days_remaining,
                                    chain_length: observation.chain_length,
                                    trust_anchor: observation.trust_anchor.clone(),
                                    handshake_completed: observation.handshake_completed,
                                }),
                            }),
                        _ => None,
                    });
                receipt_observation(verdict, evidence)
            })
            .collect();
        Self {
            schema: CONTRACT_VERIFICATION_RECEIPT_SCHEMA.to_owned(),
            bundle_sha256: Some(bundle_sha256.into()),
            contract_ref: contract_ref.into(),
            derivation_ref: derivation_ref.into(),
            target: VerificationTarget { kind: target },
            execution: None,
            observations,
            fully_satisfied,
        }
    }

    /// A receipt for an attempt that verified a candidate before any
    /// transport existed: the same schema and the same evidence rules as
    /// [`Self::from_runtime`], with no bundle digest.
    pub fn from_attempt(
        contract_ref: impl Into<String>,
        derivation_ref: impl Into<String>,
        contract: &BoundContract,
        runtime: &RuntimeObservation,
        verification: ContractVerification,
    ) -> Self {
        Self::for_attempt(
            VerificationTargetKind::FormationRuntime,
            None,
            contract_ref,
            derivation_ref,
            contract,
            runtime,
            verification,
        )
    }

    /// A receipt for one attempt on `target`. `bundle_sha256` names the
    /// transport the verified bytes arrived in when there was one (a Run of
    /// a `.capsule`), and is absent — never invented — when there was not.
    pub fn for_attempt(
        target: VerificationTargetKind,
        bundle_sha256: Option<String>,
        contract_ref: impl Into<String>,
        derivation_ref: impl Into<String>,
        contract: &BoundContract,
        runtime: &RuntimeObservation,
        verification: ContractVerification,
    ) -> Self {
        Self {
            bundle_sha256,
            ..Self::from_runtime(
                String::new(),
                contract_ref,
                derivation_ref,
                target,
                contract,
                runtime,
                verification,
            )
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
                INSTANCE_SNAPSHOT_CONTRACT_VERIFIER => {
                    let expected = requirement.digest.as_deref().unwrap_or_default();
                    match candidate.instance_snapshot_ref.as_deref() {
                        Some(actual) if actual == expected => ObservationOutcome::Satisfied,
                        Some(actual) => ObservationOutcome::Failed {
                            code: "instance_snapshot_mismatch",
                            detail: format!("restored {actual}; expected {expected}"),
                        },
                        None => ObservationOutcome::Failed {
                            code: "instance_snapshot_missing",
                            detail: "the candidate installed no portable Instance snapshot"
                                .to_owned(),
                        },
                    }
                }
                SCENARIO_CONTRACT_VERIFIER => {
                    // A scenario executes HTTP operations against a running
                    // candidate. Formation has no running candidate, so it can
                    // only name the gate that will decide it; a Deferred
                    // outcome never makes a receipt fully satisfied.
                    ObservationOutcome::Deferred {
                        by: format!("runtime scenario verifier {}", requirement.id),
                    }
                }
                TLS_CONTRACT_VERIFIER => {
                    // A certificate is presented by a running listener.
                    // Formation builds artifacts and starts nothing, so it
                    // names the runtime gate that will decide this.
                    ObservationOutcome::Deferred {
                        by: format!("runtime tls verifier {}", requirement.id),
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
                INSTANCE_SNAPSHOT_CONTRACT_VERIFIER => {
                    let expected = requirement.digest.as_deref().unwrap_or_default();
                    match candidate.instance_snapshot_ref.as_deref() {
                        Some(actual) if actual == expected => ObservationOutcome::Satisfied,
                        Some(actual) => ObservationOutcome::Failed {
                            code: "instance_snapshot_mismatch",
                            detail: format!("restored {actual}; expected {expected}"),
                        },
                        None => ObservationOutcome::Failed {
                            code: "instance_snapshot_missing",
                            detail: "the runtime restored no portable Instance snapshot".to_owned(),
                        },
                    }
                }
                SCENARIO_CONTRACT_VERIFIER => {
                    let expected = requirement.digest.as_deref().unwrap_or_default();
                    match candidate
                        .scenarios
                        .iter()
                        .find(|observation| observation.requirement_id == requirement.id)
                    {
                        None => ObservationOutcome::Failed {
                            code: "scenario_observation_missing",
                            detail: format!(
                                "the runtime executed no scenario for {}",
                                requirement.id
                            ),
                        },
                        Some(observation) if observation.scenario_ref != expected => {
                            ObservationOutcome::Failed {
                                code: "scenario_object_mismatch",
                                detail: format!(
                                    "executed scenario {}; K pins {expected}",
                                    observation.scenario_ref
                                ),
                            }
                        }
                        Some(observation)
                            if observation.verifier_version != SCENARIO_VERIFIER_VERSION =>
                        {
                            ObservationOutcome::Failed {
                                code: "scenario_verifier_version_unsupported",
                                detail: format!(
                                    "the runtime executed verifier version {}; this build \
                                     decides {SCENARIO_VERIFIER_VERSION}",
                                    observation.verifier_version
                                ),
                            }
                        }
                        Some(observation) if observation.result == ScenarioResult::Failed => {
                            let code = match observation.code.as_deref() {
                                Some("scenario_isolation_unavailable") => {
                                    "scenario_isolation_unavailable"
                                }
                                _ => "scenario_step_failed",
                            };
                            let step = observation
                                .failed_step
                                .as_deref()
                                .map(|step| format!("{step}: "))
                                .unwrap_or_default();
                            let reason = observation
                                .code
                                .as_deref()
                                .filter(|observation_code| *observation_code != code)
                                .map(|observation_code| format!("{observation_code}: "))
                                .unwrap_or_default();
                            ObservationOutcome::Failed {
                                code,
                                detail: format!(
                                    "{step}{reason}{}",
                                    observation.detail.as_deref().unwrap_or(
                                        "a step did not \
                                     meet its expectation"
                                    )
                                ),
                            }
                        }
                        Some(observation) if !is_scenario_ref(&observation.transcript_digest) => {
                            ObservationOutcome::Failed {
                                code: "scenario_transcript_mismatch",
                                detail: format!(
                                    "the scenario transcript digest {} is not a sha256 \
                                     reference",
                                    observation.transcript_digest
                                ),
                            }
                        }
                        Some(_) => ObservationOutcome::Satisfied,
                    }
                }
                TLS_CONTRACT_VERIFIER => {
                    let port = requirement.port.as_deref().unwrap_or_default();
                    let server_name = requirement.server_name.as_deref().unwrap_or_default();
                    let start = requirement.start.unwrap_or(TlsStart::Implicit);
                    match candidate
                        .tls
                        .iter()
                        .find(|observation| observation.requirement_id == requirement.id)
                    {
                        None => ObservationOutcome::Failed {
                            code: "tls_observation_missing",
                            detail: format!(
                                "the runtime observed no TLS handshake for {}",
                                requirement.id
                            ),
                        },
                        Some(observation)
                            if observation.port != port
                                || observation.server_name != server_name
                                || observation.start != start =>
                        {
                            ObservationOutcome::Failed {
                                code: "tls_observation_mismatch",
                                detail: format!(
                                    "observed {} on {} with start {}; K observes {} on {port} \
                                     with start {}",
                                    observation.server_name,
                                    observation.port,
                                    observation.start.as_str(),
                                    server_name,
                                    start.as_str()
                                ),
                            }
                        }
                        // Name and validity are checked before the whole-chain
                        // result so the failure names what is actually wrong:
                        // full validation also fails for those two reasons.
                        Some(observation) if !observation.server_name_matches => {
                            ObservationOutcome::Failed {
                                code: "tls_server_name_mismatch",
                                detail: format!(
                                    "the certificate presented on {port} covers {}; K requires \
                                     {server_name}",
                                    if observation.san.is_empty() {
                                        "no DNS name".to_owned()
                                    } else {
                                        observation.san.join(", ")
                                    }
                                ),
                            }
                        }
                        Some(observation) if !observation.within_validity => {
                            ObservationOutcome::Failed {
                                code: "tls_certificate_outside_validity",
                                detail: format!(
                                    "the certificate presented on {port} is valid {} to {}; it \
                                     is not valid now",
                                    observation.not_before, observation.not_after
                                ),
                            }
                        }
                        Some(observation) if !observation.trusted => ObservationOutcome::Failed {
                            code: "tls_chain_untrusted",
                            detail: format!(
                                "the certificate presented on {port} for {server_name} does not \
                                 validate to a public root ({})",
                                observation.trust_anchor
                            ),
                        },
                        Some(observation) if !observation.handshake_completed => {
                            ObservationOutcome::Failed {
                                code: "tls_handshake_incomplete",
                                detail: format!(
                                    "a certificate valid for {server_name} was presented on                                      {port}, but the TLS handshake did not complete"
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

    #[test]
    fn runner_group_evidence_decodes_and_round_trips() {
        // The Runner's ready evidence for an OCI service group reaches the
        // hosted verifier verbatim; refusing its `services` would stop every
        // group verification.
        let raw = r#"{"realization":"oci_service_group","runtime_executable":"docker","platform":"linux/amd64","services":[{"name":"backend","container_id":"c1","image":"a@sha256:1"},{"name":"web","container_id":"c2","image":"b@sha256:2"}]}"#;
        let evidence: VerificationExecutionEvidence = serde_json::from_str(raw).unwrap();
        assert_eq!(evidence.services.len(), 2);
        assert_eq!(evidence.services[1].name, "web");
        assert!(
            serde_json::to_string(&evidence)
                .unwrap()
                .contains("\"services\"")
        );
        let single: VerificationExecutionEvidence =
            serde_json::from_str(r#"{"realization":"oci"}"#).unwrap();
        assert!(!serde_json::to_string(&single).unwrap().contains("services"));
    }
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

            server_name: None,
            start: None,
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

            server_name: None,
            start: None,
        }
    }

    fn instance_snapshot(id: &str, digest: &str) -> BoundRequirement {
        BoundRequirement {
            id: id.to_owned(),
            verifier: INSTANCE_SNAPSHOT_CONTRACT_VERIFIER.to_owned(),
            port: None,
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: None,
            digest: Some(digest.to_owned()),

            server_name: None,
            start: None,
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
            instance_snapshot_ref: None,
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
    fn runtime_satisfies_snapshot_requirement_when_exact_snapshot_was_restored() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let k = contract(vec![instance_snapshot("saved-state", &digest)]);
        let verification = verify_runtime(
            &k,
            &RuntimeObservation {
                instance_snapshot_ref: Some(digest),
                ..Default::default()
            },
        );
        assert!(verification.fully_satisfied());
    }

    #[test]
    fn runtime_rejects_snapshot_requirement_when_no_snapshot_was_restored() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let k = contract(vec![instance_snapshot("saved-state", &digest)]);
        let verification = verify_runtime(&k, &RuntimeObservation::default());
        assert_eq!(
            verification.failure().unwrap().1,
            "instance_snapshot_missing"
        );
    }

    fn scenario_requirement(id: &str, port: &str, digest: &str) -> BoundRequirement {
        BoundRequirement {
            id: id.to_owned(),
            verifier: SCENARIO_CONTRACT_VERIFIER.to_owned(),
            port: Some(port.to_owned()),
            method: None,
            path: None,
            status: None,
            body_digest: None,
            input: None,
            digest: Some(digest.to_owned()),

            server_name: None,
            start: None,
        }
    }

    const SCENARIO_REF: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const TRANSCRIPT: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn seal_defers_a_scenario_to_the_runtime_verifier() {
        let k = contract(vec![scenario_requirement("crud", "app.http", SCENARIO_REF)]);
        let candidate = CandidateObservation {
            exported_ports: ["app.http".to_owned()].into(),
            ..Default::default()
        };
        let verification = verify(&k, &candidate);
        // A Deferred verdict is seal-admissible but never fully satisfied.
        assert!(verification.seal_admissible());
        assert!(!verification.fully_satisfied());
        match &verification.verdicts[0].outcome {
            ObservationOutcome::Deferred { by } => {
                assert!(by.contains("runtime scenario verifier crud"), "{by}")
            }
            other => panic!("expected a deferred scenario, got {other:?}"),
        }
    }

    #[test]
    fn runtime_scenario_verdicts_cover_every_refusal() {
        let k = contract(vec![scenario_requirement("crud", "app.http", SCENARIO_REF)]);
        let satisfied = ScenarioObservation::satisfied("crud", SCENARIO_REF, TRANSCRIPT);
        let accepted = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![satisfied],
                ..Default::default()
            },
        );
        assert!(accepted.fully_satisfied(), "{accepted:?}");

        let missing = verify_runtime(&k, &RuntimeObservation::default());
        assert_eq!(missing.failure().unwrap().1, "scenario_observation_missing");

        let other_ref = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
        let mismatch = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![ScenarioObservation::satisfied(
                    "crud", other_ref, TRANSCRIPT,
                )],
                ..Default::default()
            },
        );
        assert_eq!(mismatch.failure().unwrap().1, "scenario_object_mismatch");

        let mut unsupported = ScenarioObservation::satisfied("crud", SCENARIO_REF, TRANSCRIPT);
        unsupported.verifier_version = SCENARIO_VERIFIER_VERSION + 1;
        let unsupported = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![unsupported],
                ..Default::default()
            },
        );
        assert_eq!(
            unsupported.failure().unwrap().1,
            "scenario_verifier_version_unsupported"
        );

        let malformed = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![ScenarioObservation::satisfied(
                    "crud",
                    SCENARIO_REF,
                    "not-a-digest",
                )],
                ..Default::default()
            },
        );
        assert_eq!(
            malformed.failure().unwrap().1,
            "scenario_transcript_mismatch"
        );

        let failed = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![ScenarioObservation::failed(
                    "crud",
                    SCENARIO_REF,
                    TRANSCRIPT,
                    Some("create".to_owned()),
                    "status_mismatch",
                    "status 500; expected 201",
                )],
                ..Default::default()
            },
        );
        let (_, code, detail) = failed.failure().unwrap();
        assert_eq!(code, "scenario_step_failed");
        assert!(detail.contains("create"), "{detail}");
        assert!(detail.contains("status_mismatch"), "{detail}");

        let isolated = verify_runtime(
            &k,
            &RuntimeObservation {
                scenarios: vec![ScenarioObservation::isolation_unavailable(
                    "crud",
                    SCENARIO_REF,
                )],
                ..Default::default()
            },
        );
        assert_eq!(
            isolated.failure().unwrap().1,
            "scenario_isolation_unavailable"
        );
    }

    #[test]
    fn a_scenario_receipt_uses_the_existing_evidence_fields() {
        let k = contract(vec![scenario_requirement("crud", "app.http", SCENARIO_REF)]);
        let runtime = RuntimeObservation {
            scenarios: vec![ScenarioObservation::satisfied(
                "crud",
                SCENARIO_REF,
                TRANSCRIPT,
            )],
            ..Default::default()
        };
        let verification = verify_runtime(&k, &runtime);
        let receipt = ContractVerificationReceipt::from_attempt(
            "sha256:k",
            "sha256:d",
            &k,
            &runtime,
            verification,
        );
        assert!(receipt.fully_satisfied);
        let observation = &receipt.observations[0];
        assert_eq!(observation.id, "crud");
        let evidence = observation.evidence.as_ref().expect("scenario evidence");
        assert_eq!(evidence.path.as_deref(), Some("scenario:crud"));
        assert_eq!(evidence.body_sha256.as_deref(), Some(TRANSCRIPT));
        assert_eq!(evidence.digest.as_deref(), Some(SCENARIO_REF));
        assert!(evidence.method.is_none());
        assert!(evidence.status.is_none());
        assert!(evidence.input.is_none());
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
