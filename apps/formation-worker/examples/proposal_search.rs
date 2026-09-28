//! Fixed-only actual acceptance driver. No external model adapter or transport.
//! Reuses production requester/DecisionProvider/receipt authority and Runtime API.
use anyhow::{Context, Result};
use ato_formation::{authoring::BoundContract, proposal::*};
use ato_formation_worker::{
    decision_provider::{DecisionPoint, DecisionProvider, ProviderAnswer, serve_decision},
    runtime_network::{
        Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, accept_verified_routes,
        prepare_submission,
        proposal::{prepare_proposal_submission, serve_proposal},
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
    api: String,
    token_file: PathBuf,
    source: PathBuf,
    work: PathBuf,
    search_id: String,
    claimant_id: String,
    contract: BoundContract,
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
            });
        self.log.borrow_mut().push(json!({"seq":point.seq,"failed_derivations":self.failed,
            "failure_evidence":self.evidence,"selected":choice.and_then(|c|c.attempt().map(|(d,_,_)|d)),
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
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: proposal_search CONFIG_JSON")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let client = Client::new(&config.api, &std::fs::read_to_string(&config.token_file)?)?;
    let mut submission = if config.routes.is_empty() {
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
    let created = match &config.resume {
        Some(id) => json!({"satisfy_id":id}),
        None => client.submit(&submission)?,
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
    let deadline = Instant::now() + Duration::from_secs(config.settle_seconds);
    let mut answered = BTreeSet::new();
    let decisions = std::cell::RefCell::new(Vec::new());
    while Instant::now() < deadline {
        if let Ok(status) = client.satisfy_status(id) {
            submission.accept_proposal_round(&status)?;
            serve_proposal(
                &client,
                id,
                &status,
                &mut submission,
                &config.claimant_id,
                producer.clone(),
            )?;
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
            let result = json!({"decisions":*decisions.borrow(),"status":status,"accepted":accepted,"refused":refused,
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
