//! Explicit, bounded requester integration. The receiver grants a call;
//! neither its saved candidates nor its compiler verdict grant receipt authority.
pub mod budget;
pub mod deepseek;
pub mod provenance;

use super::*;
use ato_formation::{
    authoring::BoundContract,
    generation_context::{project_failures, project_inspections},
    proposal::{
        AuthorizedSourceText, CandidateProducer, CandidateRegistry, PROPOSAL_REQUEST_SCHEMA,
        ProducerError, ProducerOutput, ProducerProvenance, ProposalAuthorization, ProposalBudget,
        ProposalOutcome, ProposalRequest, ProposalRequestV2, ProposalRequestV2Schema,
        SourceContextEntry, SourceContextKind, build_source_context,
    },
    search::{FrozenSearchV1, InitialSource, SearchStateV1},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use deepseek::{DeepSeekCandidateProducer, GeneralFailure, GeneralOutput};
use provenance::{CallStatus, ErrorClass, ProviderCall, ProviderIdentity};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct RequesterProposal {
    frozen: FrozenSearchV1,
    bases: BTreeMap<String, String>,
    attempted_claim: bool,
    accepted_round: Option<Value>,
    recipes: BTreeMap<String, String>,
    source_context: Vec<SourceContextEntry>,
    provider_identity: Option<ProviderIdentity>,
    observed_call: Option<ProviderCall>,
}

/// No preset, route or placeholder D is planned. K is supplied explicitly by
/// the owner; I is independently frozen by the ordinary verified source path.
#[allow(clippy::too_many_arguments)]
pub fn prepare_proposal_submission(
    dir: &Path,
    contract: BoundContract,
    work_root: &Path,
    constraint: RuntimeConstraintWire,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    search_id: &str,
    authorization: ProposalAuthorization,
) -> Result<Submission> {
    anyhow::ensure!(
        policy.proposal.is_none(),
        "use the explicit authorization argument"
    );
    let mut submission = prepare_submission_inner(
        dir,
        &[],
        None,
        work_root,
        constraint,
        policy,
        budget,
        search_id,
        Some(contract),
    )?;
    submission.enable_candidate_producer(authorization)?;
    Ok(submission)
}

fn frozen_request(request: &SatisfyRequest) -> Result<FrozenSearchV1> {
    let mut budget = serde_json::to_value(&request.budget)?;
    let mode = budget
        .as_object_mut()
        .context("budget object")?
        .remove("mode");
    let candidates: Vec<_> = request
        .authorized_derivations
        .iter()
        .map(|d| {
            json!({
                "derivation_ref":d.derivation_ref, "effects":d.effects,
                "requirements":d.requirements, "provisions":d.provisions,
                "materialization":{"kind":"source", "closure_ref":request.source.closure_ref,
                    "archive_digest":request.source.archive_digest}
            })
        })
        .collect();
    let mut policy = json!({"mode":mode, "budget":budget,
        "runtime_constraint":request.runtime_constraint, "network":request.policy.network,
        "allow_managed":request.policy.allow_managed,"bindings":request.bindings});
    if let Some(p) = &request.policy.proposal {
        policy["proposal"] = json!(p);
    }
    if let Some(p) = &request.policy.decision {
        policy["decision"] = json!(p);
    }
    if let Some(p) = &request.policy.generation {
        policy["generation"] = json!(p);
    }
    Ok(serde_json::from_value(
        json!({"contract_ref":request.contract_ref,
        "base_contract_ref":request.base_contract_ref,"base_contract":request.base_contract,
        "browser_contract":request.browser_contract,"policy":policy,"candidates":candidates,
        "initial_source":request.initial_source}),
    )?)
}

impl Submission {
    /// Explicit opt-in only, before submission. Re-extract the digest-verified
    /// archive rather than trusting the mutable source directory or a host package.
    pub fn enable_candidate_producer(
        &mut self,
        authorization: ProposalAuthorization,
    ) -> Result<()> {
        anyhow::ensure!(self.proposal_state.is_none(), "producer already enabled");
        authorization.validate()?;
        let verified = FileVerifiedArchive::verify(
            self.source_file()?,
            &self.request.source.archive_digest,
            self.archive.metadata()?.len(),
            SourceLimits::default(),
        )?;
        let inventory = local::freeze_verified_file(
            verified,
            self.frozen_source
                .root
                .parent()
                .context("source scratch missing")?,
            SourceLimits::default(),
        )?;
        anyhow::ensure!(
            inventory.closure_ref.as_str() == self.request.source.closure_ref
                && inventory.closure_ref == self.frozen_source.closure_ref,
            "initial source differs from verified snapshot"
        );
        let files = source_file_inventory(&inventory.root)?;
        for path in authorization.source_domain.entrypoints.values() {
            anyhow::ensure!(
                files.contains(path),
                "entrypoint is not a frozen regular file"
            );
        }
        for module in authorization.source_domain.modules.values() {
            let path = module.replace('.', "/");
            let parts: Vec<_> = module.split('.').collect();
            for n in 1..parts.len() {
                anyhow::ensure!(
                    files.contains(&format!("{}/__init__.py", parts[..n].join("/"))),
                    "module package is not in frozen source"
                );
            }
            anyhow::ensure!(
                files.contains(&format!("{path}.py"))
                    || (files.contains(&format!("{path}/__init__.py"))
                        && files.contains(&format!("{path}/__main__.py"))),
                "module entrypoint is not in frozen source"
            );
        }
        // Workspace IDs resolve only to the inventory of this verified extraction.
        if let Some(domain) = &authorization.node_static_workspace {
            let recomputed = ato_formation::workspace::inventory(&inventory.root)
                .ok()
                .and_then(|inventory| inventory.authorization());
            anyhow::ensure!(
                recomputed.as_ref() == Some(domain),
                "workspace domain differs from the frozen source inventory"
            );
        }
        // Read only the independent digest-verified extraction, never the user's
        // mutable working tree. No read/crawl at all when source text is disabled.
        let source_context = if authorization.policy.allow_source_text {
            let mut texts = Vec::new();
            for (id, path) in &authorization.source_domain.entrypoints {
                texts.push((
                    SourceContextKind::Entrypoint,
                    id.as_str(),
                    std::fs::read(inventory.root.join(path))?,
                ));
            }
            for (id, module) in &authorization.source_domain.modules {
                let path = module.replace('.', "/");
                let file = if files.contains(&format!("{path}.py")) {
                    format!("{path}.py")
                } else {
                    format!("{path}/__main__.py")
                };
                texts.push((
                    SourceContextKind::Module,
                    id.as_str(),
                    std::fs::read(inventory.root.join(file))?,
                ));
            }
            build_source_context(
                &authorization,
                &texts
                    .iter()
                    .map(|(kind, id, bytes)| AuthorizedSourceText {
                        kind: *kind,
                        logical_id: id,
                        bytes,
                    })
                    .collect::<Vec<_>>(),
            )?
        } else {
            vec![]
        };
        let mut request = self.request.clone();
        request.initial_source = Some(InitialSource {
            closure_ref: request.source.closure_ref.clone(),
            archive_digest: request.source.archive_digest.clone(),
        });
        request.policy.proposal = Some(authorization);
        let frozen = frozen_request(&request)?;
        frozen.canonical_bytes()?;
        CandidateRegistry::new(&frozen)?;
        self.proposal_state = Some(RequesterProposal {
            frozen,
            bases: request
                .authorized_derivations
                .iter()
                .map(|d| (d.derivation_ref.clone(), d.capsule_toml.clone()))
                .collect(),
            attempted_claim: false,
            accepted_round: None,
            recipes: BTreeMap::new(),
            source_context,
            provider_identity: None,
            observed_call: None,
        });
        self.request = request;
        Ok(())
    }

    pub(super) fn validate_proposal_submission(&self) -> Result<()> {
        match (&self.proposal_state, &self.request.policy.proposal) {
            (None, None) => Ok(()),
            (Some(local), Some(_)) => {
                anyhow::ensure!(
                    frozen_request(&self.request)? == local.frozen
                        && self
                            .request
                            .initial_source
                            .as_ref()
                            .is_some_and(|source| source.closure_ref
                                == self.request.source.closure_ref
                                && source.archive_digest == self.request.source.archive_digest),
                    "local proposal authorization changed"
                );
                let bases: BTreeMap<_, _> = self
                    .request
                    .authorized_derivations
                    .iter()
                    .map(|d| (d.derivation_ref.clone(), d.capsule_toml.clone()))
                    .collect();
                anyhow::ensure!(bases == local.bases, "original proposal bases changed");
                Ok(())
            }
            _ => anyhow::bail!("CandidateProducer requires explicit verified enablement"),
        }
    }

    fn proposal_search(&self, status: &Value) -> Result<SearchStateV1> {
        self.validate_proposal_submission()?;
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
        anyhow::ensure!(
            frozen_request(&self.request)? == local.frozen,
            "local frozen proposal request changed"
        );
        let state: SearchStateV1 = serde_json::from_value(status["search_state"].clone())?;
        anyhow::ensure!(
            state.search_id == self.request.search_id && state.frozen == local.frozen,
            "receiver changed frozen proposal search"
        );
        state.canonical_bytes()?;
        Ok(state)
    }

    /// Bounded semantic evidence only. No source/domain map, logs, receipts,
    /// bindings, credentials, private paths or arbitrary runtime facts.
    pub fn proposal_request(&self, status: &Value) -> Result<ProposalRequest> {
        let state = self.proposal_search(status)?;
        let policy = &state
            .frozen
            .policy
            .proposal
            .as_ref()
            .context("policy missing")?
            .policy;
        Ok(ProposalRequest {
            schema: PROPOSAL_REQUEST_SCHEMA.into(),
            search_id: self.request.search_id.clone(),
            frozen_contract: state.frozen.base_contract.clone(),
            runtime_constraint: state.frozen.policy.runtime_constraint.clone(),
            known_derivations: state
                .frozen
                .candidates
                .iter()
                .map(|c| c.derivation_ref.clone())
                .collect(),
            failure_evidence: project_failures(&serde_json::to_value(&state.attempts)?),
            inspection_evidence: project_inspections(&serde_json::to_value(&state.evidence)?),
            operation_catalog: state
                .frozen
                .policy
                .proposal
                .as_ref()
                .context("policy missing")?
                .catalog()?,
            remaining_budget: ProposalBudget {
                rounds_remaining: u32::from(
                    state
                        .proposal_round
                        .as_ref()
                        .is_some_and(|r| r.outcome.is_none()),
                ),
                max_proposals: policy.max_proposals,
                timeout_ms: policy.timeout_ms,
                attempts_remaining: state.frozen.policy.budget.max_attempts.saturating_sub(
                    state
                        .budget
                        .attempts_used
                        .saturating_add(state.budget.attempts_reserved),
                ),
            },
        })
    }

    /// Pin the operational provider identity before sending or rehydrating a
    /// general round. Restarts must supply the same explicit configuration.
    pub fn configure_general_producer(&mut self, identity: ProviderIdentity) -> Result<()> {
        identity.validate()?;
        self.validate_proposal_submission()?;
        let local = self
            .proposal_state
            .as_mut()
            .context("producer not enabled")?;
        anyhow::ensure!(
            local
                .frozen
                .policy
                .proposal
                .as_ref()
                .is_some_and(|a| a.policy.allow_source_text),
            "source opt-in required"
        );
        anyhow::ensure!(
            local
                .provider_identity
                .as_ref()
                .is_none_or(|old| old == &identity),
            "configured provider changed"
        );
        anyhow::ensure!(
            !local.attempted_claim || local.provider_identity.as_ref() == Some(&identity),
            "provider configured after claim"
        );
        local.provider_identity = Some(identity);
        Ok(())
    }
    pub fn proposal_request_v2(&self, status: &Value) -> Result<ProposalRequestV2> {
        let request = self.proposal_request(status)?;
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
        let value = ProposalRequestV2 {
            schema: ProposalRequestV2Schema::V2,
            search_id: request.search_id,
            frozen_contract: request.frozen_contract,
            runtime_constraint: request.runtime_constraint,
            known_derivations: request.known_derivations,
            failure_evidence: request.failure_evidence,
            inspection_evidence: request.inspection_evidence,
            operation_catalog: request.operation_catalog,
            remaining_budget: request.remaining_budget,
            source_context: local.source_context.clone(),
        };
        value.validate(
            local
                .frozen
                .policy
                .proposal
                .as_ref()
                .context("policy missing")?,
        )?;
        Ok(value)
    }

    /// Owner-local preregistration evidence, NOT a provider request or a search
    /// observation. Exports only immutable inputs, never invents failure evidence.
    pub fn proposal_preregistration(&self) -> Result<Value> {
        self.validate_proposal_submission()?;
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
        let auth = local
            .frozen
            .policy
            .proposal
            .as_ref()
            .context("policy missing")?;
        let context_bytes = serde_jcs::to_vec(&local.source_context)?;
        Ok(json!({
            "frozen": local.frozen,
            "operation_catalog": auth.catalog()?,
            "source_context": local.source_context,
            "source_context_sha256": format!("sha256:{:x}", Sha256::digest(&context_bytes)),
            "source_context_bytes": context_bytes.len(),
            "source_text_bytes": local.source_context.iter().map(|e| e.text.len()).sum::<usize>()
        }))
    }

    /// Independently compile exact saved bytes before expanding receipt/recipe
    /// authorization. All comparisons finish before any local admission occurs.
    pub fn accept_proposal_round(&mut self, status: &Value) -> Result<()> {
        if self.proposal_state.is_none() {
            return Ok(());
        }
        let state = self.proposal_search(status)?;
        let round = &status["proposal_round"];
        if round.is_null() || matches!(round["status"].as_str(), Some("open" | "claimed")) {
            anyhow::ensure!(
                self.proposal_state
                    .as_ref()
                    .and_then(|p| p.accepted_round.as_ref())
                    .is_none(),
                "durable proposal disappeared"
            );
            return Ok(());
        }
        anyhow::ensure!(round["round_seq"] == 1, "invalid round");
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
        if let Some(prior) = &local.accepted_round {
            anyhow::ensure!(prior == round, "durable proposal changed");
        }
        let call: Option<ProviderCall> = round
            .get("provider_call")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?;
        if let Some(call) = &call {
            call.validate(
                local
                    .provider_identity
                    .as_ref()
                    .context("unexpected general provider evidence")?,
            )?;
            anyhow::ensure!(
                if round["raw_output_base64"].is_string() {
                    call.status == CallStatus::Success
                } else {
                    serde_json::to_value(call.status)? == round["status"]
                },
                "durable provider status mismatch"
            );
        }
        if let Some(observed) = &local.observed_call {
            anyhow::ensure!(
                call.as_ref() == Some(observed),
                "persisted provider evidence changed"
            );
        }
        let mut registry = CandidateRegistry::new(&local.frozen)?;
        let outcomes = if let Some(base64) = round["raw_output_base64"].as_str() {
            anyhow::ensure!(base64.len() <= 21848, "raw output too large");
            let raw = BASE64.decode(base64)?;
            anyhow::ensure!(
                BASE64.encode(&raw) == base64
                    && round["raw_output_digest"] == format!("sha256:{:x}", Sha256::digest(&raw)),
                "raw output changed"
            );
            let provenance = &round["provenance"];
            let output_provenance = if let Some(call) = &call {
                anyhow::ensure!(
                    serde_json::to_value(&call.provenance)? == *provenance,
                    "raw provenance differs from call evidence"
                );
                ProducerProvenance {
                    provider: call.provenance.provider.clone(),
                    model: Some(call.provenance.model.clone()),
                }
            } else {
                anyhow::ensure!(
                    local.provider_identity.is_none()
                        && (provenance == &json!({"provider":"fixed"})
                            || provenance == &json!({"provider":"fixed","model":null})),
                    "invalid or downgraded provenance"
                );
                ProducerProvenance {
                    provider: "fixed".into(),
                    model: None,
                }
            };
            let output = ProducerOutput::new(raw, output_provenance)?;
            match registry.validate_batch(&local.bases, &output) {
                Ok(outcomes) => {
                    anyhow::ensure!(
                        round["status"] == "completed" && round["error_class"].is_null(),
                        "completion mismatch"
                    );
                    outcomes.iter().map(outcome_wire).collect::<Vec<_>>()
                }
                Err(_) => {
                    anyhow::ensure!(
                        round["status"] == "provider_error"
                            && round["error_class"] == "invalid_output",
                        "invalid-output mismatch"
                    );
                    vec![]
                }
            }
        } else {
            anyhow::ensure!(
                matches!(round["status"].as_str(), Some("provider_error" | "timeout"))
                    && round["error_class"] == round["status"]
                    && round["provenance"].is_null()
                    && round["raw_output_digest"].is_null(),
                "missing raw output"
            );
            vec![]
        };
        anyhow::ensure!(
            round["outcomes"] == json!(outcomes),
            "durable validator result mismatch"
        );
        let generated: Vec<_> = registry
            .generated()
            .iter()
            .map(|c| c.candidate().clone())
            .collect();
        let record = state
            .proposal_round
            .as_ref()
            .context("round absent from search")?;
        anyhow::ensure!(
            serde_json::to_value(record.outcome)? == round["status"]
                && record.candidates == generated,
            "durable candidate scope mismatch"
        );
        let recipes: BTreeMap<_, _> = registry
            .generated()
            .iter()
            .map(|c| {
                (
                    c.compiled().derivation_ref.clone(),
                    c.compiled().capsule_toml.clone(),
                )
            })
            .collect();
        let local = self
            .proposal_state
            .as_mut()
            .context("producer not enabled")?;
        for derivation in recipes.keys() {
            self.contracts
                .insert(derivation.clone(), local.frozen.base_contract.clone());
        }
        local.recipes = recipes;
        local.accepted_round = Some(round.clone());
        Ok(())
    }

    pub fn proposal_recipes(&self) -> impl Iterator<Item = (&String, &String)> {
        self.proposal_state.iter().flat_map(|p| p.recipes.iter())
    }
}

fn outcome_wire(outcome: &ProposalOutcome) -> Value {
    match outcome {
        ProposalOutcome::Admitted(c) => json!({"status":"admitted","proposal_id":c.proposal_id(),
            "derivation_ref":c.compiled().derivation_ref,"capsule_toml":c.compiled().capsule_toml,
            "derivation":c.compiled().derivation,"candidate":c.candidate()}),
        ProposalOutcome::Rejected(e) => json!({"status":"rejected","code":e.0}),
        ProposalOutcome::Unsupported => json!({"status":"unsupported"}),
    }
}

impl Client {
    pub fn claim_proposal(&self, id: &str, revision: u64, claimant_id: &str) -> Result<Value> {
        self.send(
            self.http
                .post(self.url(&format!("/satisfy/{id}/proposal/claim")))
                .json(&json!({"revision":revision,"claimant_id":claimant_id})),
        )?
        .context("empty proposal claim")
    }
    /// Serialize once; any retry by a caller must reuse these exact bytes. The
    /// production service below deliberately does not retry completion or producer.
    pub fn complete_proposal(&self, id: &str, bytes: &[u8]) -> Result<Value> {
        self.send(
            self.http
                .post(self.url(&format!("/satisfy/{id}/proposal/complete")))
                .header("content-type", "application/json")
                .body(bytes.to_vec()),
        )?
        .context("empty proposal completion")
    }
}

/// One possible invocation per successful durable claim, even with stale GETs.
/// Transport/lost claim errors consume this local opportunity without calling.
/// A restarted requester observes claimed/terminal state and never recovers a fence.
pub fn serve_proposal(
    client: &Client,
    id: &str,
    status: &Value,
    submission: &mut Submission,
    claimant_id: &str,
    producer: Arc<dyn CandidateProducer + Send + Sync>,
) -> Result<bool> {
    serve_proposal_inner(
        client,
        id,
        status,
        submission,
        claimant_id,
        Invocation::Fixed(producer),
    )
}

/// Same durable claim/completion engine; no separate generated-D executor.
pub fn serve_general_proposal(
    client: &Client,
    id: &str,
    status: &Value,
    submission: &mut Submission,
    claimant_id: &str,
    producer: Arc<DeepSeekCandidateProducer>,
) -> Result<bool> {
    submission.configure_general_producer(producer.identity())?;
    serve_proposal_inner(
        client,
        id,
        status,
        submission,
        claimant_id,
        Invocation::General(producer),
    )
}
enum Invocation {
    Fixed(Arc<dyn CandidateProducer + Send + Sync>),
    General(Arc<DeepSeekCandidateProducer>),
}
fn serve_proposal_inner(
    client: &Client,
    id: &str,
    status: &Value,
    submission: &mut Submission,
    claimant_id: &str,
    producer: Invocation,
) -> Result<bool> {
    if submission.proposal_state.is_none() {
        return Ok(false);
    }
    submission.accept_proposal_round(status)?;
    let point = &status["proposal_point"];
    if point.is_null() || point["claimed"] != false {
        return Ok(false);
    }
    let request = submission.proposal_request(status)?;
    let general_request = match &producer {
        Invocation::General(_) => Some(submission.proposal_request_v2(status)?),
        Invocation::Fixed(_) => {
            anyhow::ensure!(
                submission
                    .proposal_state
                    .as_ref()
                    .is_none_or(|s| s.provider_identity.is_none()),
                "general round cannot use fixed producer"
            );
            None
        }
    };
    let state = submission.proposal_search(status)?;
    let local = submission
        .proposal_state
        .as_mut()
        .context("producer not enabled")?;
    if local.attempted_claim {
        return Ok(false);
    }
    anyhow::ensure!(
        point["revision"] == state.revision && point["round_seq"] == 1,
        "proposal point revision mismatch"
    );
    local.attempted_claim = true;
    let Ok(claim) = client.claim_proposal(id, state.revision, claimant_id) else {
        return Ok(false);
    };
    let fence = claim["fence"]
        .as_str()
        .context("missing private claim fence")?;
    let revision = claim["revision"]
        .as_u64()
        .context("missing claim revision")?;
    anyhow::ensure!(
        state.revision.checked_add(1) == Some(revision)
            && claim["round_seq"] == 1
            && fence.len() == 36
            && fence
                .bytes()
                .enumerate()
                .all(|(i, b)| if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }),
        "invalid claim response"
    );
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let expires = state
        .proposal_round
        .as_ref()
        .context("missing open round")?
        .expires_at_ms;
    let remaining = expires
        .saturating_sub(now_ms)
        .min(request.remaining_budget.timeout_ms);
    if remaining == 0 {
        return Ok(false);
    }
    let mut completion = json!({"revision":revision,"fence":fence});
    match producer {
        Invocation::Fixed(producer) => {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = tx.send(producer.propose(&request));
            });
            let answer = match rx.recv_timeout(Duration::from_millis(remaining)) {
                Ok(answer) => answer,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(ProducerError::Timeout),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    Err(ProducerError::ProviderError)
                }
            };
            match answer {
                Ok(output)
                    if output.provenance().provider == "fixed"
                        && output.provenance().model.is_none() =>
                {
                    completion["raw_output_base64"] = json!(BASE64.encode(output.raw()));
                    completion["provenance"] = json!({"provider":"fixed"});
                }
                Err(ProducerError::Timeout) => completion["outcome"] = json!("timeout"),
                _ => completion["outcome"] = json!("provider_error"),
            }
        }
        Invocation::General(producer) => {
            // Leave time for the single completion. A bounded blocking transport
            // owns its timeout, so no background model call survives this return.
            let Some(call_time) = remaining.checked_sub(500).filter(|n| *n > 0) else {
                return Ok(false);
            };
            let mut request = general_request.context("general request missing")?;
            request.remaining_budget.timeout_ms = call_time;
            let (raw, call) = match producer.propose(&request) {
                Ok(GeneralOutput { output, provenance }) => (
                    Some(output),
                    ProviderCall {
                        provenance,
                        status: CallStatus::Success,
                        error_class: None,
                    },
                ),
                Err(GeneralFailure { class, provenance }) => (
                    None,
                    ProviderCall {
                        provenance,
                        status: if class == ErrorClass::Timeout {
                            CallStatus::Timeout
                        } else {
                            CallStatus::ProviderError
                        },
                        error_class: Some(class),
                    },
                ),
            };
            call.validate(&producer.identity())?;
            completion["provenance"] = serde_json::to_value(&call.provenance)?;
            if let Some(raw) = raw {
                completion["raw_output_base64"] = json!(BASE64.encode(raw.raw()));
            } else {
                completion["outcome"] = serde_json::to_value(call.status)?;
                completion["error_class"] = serde_json::to_value(call.error_class)?;
            }
            submission
                .proposal_state
                .as_mut()
                .context("producer missing")?
                .observed_call = Some(call);
        }
    }
    let bytes = serde_json::to_vec(&completion)?;
    // A lost completion response is recovered by the caller's next GET. Never
    // re-enter the producer or synthesize a new batch to repair that transport.
    let _ = client.complete_proposal(id, &bytes);
    Ok(true)
}

#[cfg(test)]
mod tests;
