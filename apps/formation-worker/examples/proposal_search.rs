//! Fixed / loopback-mock driver with explicit preregistered live opt-in.
//! Offline preregistration returns before constructing any network client.
//! Reuses production requester/DecisionProvider/receipt authority and Runtime API.
use anyhow::{Context, Result};
use ato_formation::{authoring::BoundContract, proposal::*};
use ato_formation_worker::{
    decision_provider::{
        DecisionPoint, DecisionProvider, OfferedAction, ProviderAnswer, serve_decision,
    },
    runtime_network::{
        Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, SatisfyRequest,
        accept_verified_routes, prepare_submission,
        proposal::{
            budget::{BudgetPlan, CallBudget},
            deepseek::{DeepSeekCandidateProducer, DeepSeekConfig},
            prepare_proposal_submission, serve_general_proposal, serve_proposal,
        },
        source_handoff::{FormationSourceOrigin, FormationSourcePointer},
    },
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    preregister_only: bool,
    #[serde(default)]
    mock_llm: Option<MockLlm>,
    #[serde(default)]
    live_llm: Option<MockLlm>,
    #[serde(default)]
    expected_preregistration: Option<serde_json::Value>,
    api: String,
    token_file: PathBuf,
    source: PathBuf,
    work: PathBuf,
    search_id: String,
    claimant_id: String,
    contract: BoundContract,
    /// Explicit archive mode; never inferred from a normal job or local manifest.
    #[serde(default)]
    source_job: Option<ato_ipc::formation::FormationJobV1>,
    #[serde(default)]
    source_archive: Option<PathBuf>,
    #[serde(default)]
    frozen_request: Option<SatisfyRequest>,
    /// Optional saved API provenance. Both fields require the existing explicit
    /// archive lane; neither turns a local byte proof into owner authorization.
    #[serde(default)]
    formation_source_pointer: Option<FormationSourcePointer>,
    #[serde(default)]
    expected_source_origin: Option<FormationSourceOrigin>,
    #[serde(default)]
    routes: Vec<PathBuf>,
    authorization: Option<ProposalAuthorization>,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    batch_json: String,
    producer_mode: String,
    calls: PathBuf,
    created: PathBuf,
    result: PathBuf,
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    prefer_member: Option<usize>,
    #[serde(default = "settle_seconds")]
    settle_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MockLlm {
    config: DeepSeekConfig,
    budget: BudgetPlan,
    journal: PathBuf,
}
fn settle_seconds() -> u64 {
    180
}
struct RecordingFixedCandidateProducer {
    mode: String,
    batch: String,
    calls: PathBuf,
}
impl CandidateProducer for RecordingFixedCandidateProducer {
    fn propose(&self, request: &ProposalRequest) -> Result<ProducerOutput, ProducerError> {
        let recorded = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.calls)?;
            writeln!(file, "{}", json!({"request":request,"mode":self.mode}))?;
            file.sync_all()?;
            Ok(())
        })();
        if recorded.is_err() {
            return Err(ProducerError::ProviderError);
        }
        match self.mode.as_str() {
            "error" => Err(ProducerError::ProviderError),
            "timeout" => Err(ProducerError::Timeout),
            // Deliberate acceptance-only process crash after successful durable
            // claim and entry into the sole invocation, before any completion.
            "crash" => std::process::exit(86),
            "success" => ProducerOutput::new(
                self.batch.as_bytes().to_vec(),
                ProducerProvenance {
                    provider: "fixed".into(),
                    model: None,
                },
            )
            .map_err(|_| ProducerError::ProviderError),
            _ => Err(ProducerError::ProviderError),
        }
    }
}
struct FixedDecision<'a> {
    preferred: Option<String>,
    failed: BTreeSet<String>,
    evidence: serde_json::Value,
    log: &'a std::cell::RefCell<Vec<serde_json::Value>>,
}
impl DecisionProvider for FixedDecision<'_> {
    fn decide(&self, point: &DecisionPoint) -> ProviderAnswer {
        let choice = self
            .preferred
            .as_ref()
            .and_then(|wanted| {
                point.choices.iter().find(|c| {
                    c.attempt()
                        .is_some_and(|(d, _, _)| d == wanted && !self.failed.contains(d))
                })
            })
            .or_else(|| {
                point.choices.iter().find(|c| {
                    c.attempt()
                        .is_some_and(|(d, _, _)| !self.failed.contains(d))
                })
            })
            .or_else(|| {
                point
                    .choices
                    .iter()
                    .find(|c| matches!(c.action, OfferedAction::EscalateToCandidateProducer {}))
            });
        self.log.borrow_mut().push(json!({"seq":point.seq,"failed_derivations":self.failed,
            "failure_evidence":self.evidence,"escalated":choice.is_some_and(|c| matches!(c.action, OfferedAction::EscalateToCandidateProducer {})),"selected":choice.and_then(|c|c.attempt().map(|(d,_,_)|d)),
            "offered":point.choices.iter().filter_map(|c|c.attempt().map(|(d,_,_)|d)).collect::<Vec<_>>() }));
        match choice {
            Some(c) => ProviderAnswer::Choice {
                choice_id: c.choice_id.clone(),
                evidence: json!({"provider":"fixed"}),
            },
            None => ProviderAnswer::Fallback {
                reason: "unsupported",
            },
        }
    }
}
fn read_config(bytes: &[u8]) -> Result<Config> {
    let config: Config = serde_json::from_slice(bytes)?;
    if config.formation_source_pointer.is_some() {
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        let request = config
            .frozen_request
            .as_ref()
            .context("explicit owner-frozen SatisfyRequest required")?;
        // Reject normalization (for example explicit null becoming omission).
        // This compares JSON values; K and its canonicalization stay Rust-owned.
        anyhow::ensure!(
            raw.get("frozen_request") == Some(&serde_json::to_value(request)?),
            "source-bound frozen request must survive typed decoding unchanged"
        );
    }
    Ok(config)
}
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: proposal_search CONFIG_JSON")?;
    let config = read_config(&std::fs::read(path)?)?;
    let explicit_archive = config.source_job.is_some()
        || config.source_archive.is_some()
        || config.frozen_request.is_some();
    anyhow::ensure!(
        config.formation_source_pointer.is_some() == config.expected_source_origin.is_some()
            && (config.formation_source_pointer.is_none() || explicit_archive),
        "source provenance needs both pointer/origin and the explicit archive lane"
    );
    let mut submission = if explicit_archive {
        let job = config
            .source_job
            .as_ref()
            .context("explicit source_job required")?;
        let file = config
            .source_archive
            .as_ref()
            .context("explicit source_archive required")?;
        let request = config
            .frozen_request
            .clone()
            .context("explicit owner-frozen SatisfyRequest required")?;
        let mut policy = config.policy.clone();
        policy.proposal = config.authorization.clone();
        anyhow::ensure!(
            config.routes.is_empty()
                && config.contract == request.base_contract
                && config.search_id == request.search_id
                && config.budget == request.budget
                && serde_jcs::to_vec(&policy)? == serde_jcs::to_vec(&request.policy)?,
            "explicit source Search configuration differs from frozen request"
        );
        ato_formation_worker::job::source_intake::prepare_source_proposal(
            &ato_formation_worker::job::source_intake::ArchiveFileFetcher(file.clone()),
            job,
            request,
            &config.work,
        )?
    } else if config.routes.is_empty() {
        prepare_proposal_submission(
            &config.source,
            config.contract.clone(),
            &config.work,
            RuntimeConstraintWire::Any,
            config.policy,
            config.budget,
            &config.search_id,
            config
                .authorization
                .clone()
                .context("zero D needs explicit producer opt-in")?,
        )?
    } else {
        let mut s = prepare_submission(
            &config.source,
            &config.routes,
            None,
            &config.work,
            RuntimeConstraintWire::Any,
            config.policy,
            config.budget,
            &config.search_id,
        )?;
        anyhow::ensure!(
            s.request.base_contract == config.contract,
            "known routes changed explicit K"
        );
        if let Some(a) = config.authorization.clone() {
            s.enable_candidate_producer(a)?;
        }
        s
    };
    if let (Some(pointer), Some(origin)) = (
        &config.formation_source_pointer,
        &config.expected_source_origin,
    ) {
        origin.validate_local_source(pointer, &submission)?;
    }
    if config.preregister_only {
        anyhow::ensure!(
            config.mock_llm.is_none() && config.live_llm.is_none() && config.resume.is_none(),
            "preregistration is offline only"
        );
        std::fs::write(
            &config.result,
            serde_json::to_vec_pretty(&submission.proposal_preregistration()?)?,
        )?;
        return Ok(());
    }
    let source_wait = if config.formation_source_pointer.is_some() {
        let seconds = config
            .settle_seconds
            .min(submission.request.budget.deadline_seconds);
        anyhow::ensure!(
            seconds > 0,
            "source requester observation allowance is empty"
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        Some((
            Instant::now() + Duration::from_secs(seconds),
            now.checked_add(
                seconds
                    .checked_mul(1000)
                    .context("source requester deadline overflow")?,
            )
            .context("source requester deadline overflow")?,
        ))
    } else {
        None
    };
    let client = Client::new(&config.api, &std::fs::read_to_string(&config.token_file)?)?;
    let client = source_wait.map_or(client.clone(), |(_, deadline)| {
        client.with_deadline(deadline)
    });
    anyhow::ensure!(
        config.mock_llm.is_none() || config.live_llm.is_none(),
        "ambiguous producer mode"
    );
    let live = config.live_llm.is_some();
    if live {
        let expected = config
            .expected_preregistration
            .as_ref()
            .context("live input pin required")?;
        anyhow::ensure!(
            expected == &submission.proposal_preregistration()?,
            "live input drift"
        );
    }
    let general = config
        .mock_llm
        .or(config.live_llm)
        .map(|m| -> Result<_> {
            let guard = if live || m.journal.exists() {
                // Live journal must have been explicitly initialized by preflight.
                CallBudget::reopen(&m.journal, m.budget)?
            } else {
                CallBudget::create(&m.journal, m.budget)?
            };
            let producer = Arc::new(if live {
                DeepSeekCandidateProducer::new(m.config, "DEEPSEEK_API_KEY", Arc::new(guard))?
            } else {
                DeepSeekCandidateProducer::new_mock(m.config, Arc::new(guard))?
            });
            submission.configure_general_producer(producer.identity())?;
            Ok(producer)
        })
        .transpose()?;
    let created = match (
        &config.formation_source_pointer,
        &config.expected_source_origin,
    ) {
        (Some(pointer), Some(origin)) => {
            // An explicit repeat asks the existing server to replay exactly
            // this source-bound request, never a legacy upload or new Search.
            let created = client.submit_from_formation_source(&submission, pointer, origin)?;
            anyhow::ensure!(
                config
                    .resume
                    .as_ref()
                    .is_none_or(|id| created["satisfy_id"] == id.as_str()),
                "source-bound replay returned another satisfy identity"
            );
            created
        }
        (None, None) => match &config.resume {
            Some(id) => json!({"satisfy_id":id}),
            None => client.submit(&submission)?,
        },
        _ => unreachable!("source provenance configuration validated before credentials"),
    };
    std::fs::write(
        &config.created,
        serde_json::to_vec_pretty(&json!({"created":created,"request":submission.request}))?,
    )?;
    let id = created["satisfy_id"].as_str().context("no satisfy id")?;
    let producer = Arc::new(RecordingFixedCandidateProducer {
        mode: config.producer_mode,
        batch: config.batch_json,
        calls: config.calls,
    });
    let deadline = source_wait.map_or_else(
        || Instant::now() + Duration::from_secs(config.settle_seconds),
        |(deadline, _)| deadline,
    );
    let mut answered = BTreeSet::new();
    let decisions = std::cell::RefCell::new(Vec::new());
    while Instant::now() < deadline {
        if let Ok(status) = client.satisfy_status(id) {
            submission.accept_proposal_round(&status)?;
            if let Some(producer) = &general {
                serve_general_proposal(
                    &client,
                    id,
                    &status,
                    &mut submission,
                    &config.claimant_id,
                    producer.clone(),
                )?;
            } else {
                serve_proposal(
                    &client,
                    id,
                    &status,
                    &mut submission,
                    &config.claimant_id,
                    producer.clone(),
                )?;
            }
            if config.authorization.is_some() || submission.request.policy.decision.is_some() {
                let preferred = config
                    .prefer_member
                    .and_then(|n| {
                        status["proposal_round"]["outcomes"][n]["derivation_ref"].as_str()
                    })
                    .map(str::to_owned);
                serve_decision(
                    &client,
                    id,
                    &status,
                    &FixedDecision {
                        preferred,
                        failed: status["search_state"]["attempts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|a| a["status"] == "fail")
                            .filter_map(|a| a["derivation_ref"].as_str().map(str::to_owned))
                            .collect(),
                        evidence: json!(ato_formation::generation_context::project_failures(
                            &status["search_state"]["attempts"]
                        )),
                        log: &decisions,
                    },
                    &mut answered,
                );
            }
            let (accepted, refused) = accept_verified_routes(&submission, id, &status);
            // Negative control over a COPY of an actual reply, never submitted
            // to the Coordinator: status/choice/compiler success is not a receipt.
            if !accepted.is_empty() {
                let mut without_receipt = status.clone();
                for route in without_receipt["verified_routes"]
                    .as_array_mut()
                    .context("routes")?
                {
                    route["verifier_receipts"] = json!([]);
                }
                anyhow::ensure!(
                    accept_verified_routes(&submission, id, &without_receipt)
                        .0
                        .is_empty(),
                    "route accepted without an actual Verifier receipt"
                );
            }
            let result = json!({"receipt_required_checked":!accepted.is_empty(),"decisions":*decisions.borrow(),"status":status,"accepted":accepted,"refused":refused,
                "contract_ref":submission.request.contract_ref,"frozen_contract":submission.request.base_contract,
                "recipes":submission.proposal_recipes().collect::<std::collections::BTreeMap<_,_>>()});
            std::fs::write(&config.result, serde_json::to_vec_pretty(&result)?)?;
            if !matches!(status["status"].as_str(), Some("running" | "unknown")) {
                anyhow::ensure!(
                    status["status"] != "satisfied" || !accepted.is_empty(),
                    "requester refused route"
                );
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    anyhow::bail!("acceptance observation deadline (not a K verdict)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pointer: bool) -> serde_json::Value {
        let request: SatisfyRequest = serde_json::from_str(include_str!(
            "../tests/fixtures/runtime-network-v0/satisfy-request.json"
        ))
        .unwrap();
        let mut config = json!({
            "api": "http://127.0.0.1:1", "token_file": ".tmp/absent",
            "source": ".tmp/source", "work": ".tmp/work",
            "search_id": request.search_id, "claimant_id": "fixture",
            "contract": request.base_contract, "policy": request.policy,
            "budget": request.budget, "frozen_request": request,
            "batch_json": "{}", "producer_mode": "success",
            "calls": ".tmp/calls", "created": ".tmp/created", "result": ".tmp/result"
        });
        if pointer {
            config["formation_source_pointer"] = json!({
                "source_archive_job_id": "fjob_fixture",
                "binding_digest": format!("sha256:{}", "a".repeat(64))
            });
        }
        config
    }

    #[test]
    fn source_bound_request_preserves_the_owner_json_value() {
        let config = config(true);
        let parsed = read_config(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(parsed.frozen_request.unwrap()).unwrap(),
            config["frozen_request"]
        );
    }

    #[test]
    fn source_bound_request_refuses_null_normalization_but_legacy_is_unchanged() {
        let mut config = config(true);
        config["frozen_request"]["source"]["content_ref"] = serde_json::Value::Null;
        let err = read_config(&serde_json::to_vec(&config).unwrap())
            .err()
            .unwrap();
        assert!(err.to_string().contains("unchanged"));
        config
            .as_object_mut()
            .unwrap()
            .remove("formation_source_pointer");
        assert!(read_config(&serde_json::to_vec(&config).unwrap()).is_ok());
    }
}
