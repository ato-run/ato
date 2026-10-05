//! Explicit, bounded requester integration. The receiver grants a call;
//! neither its saved candidates nor its compiler verdict grant receipt authority.
pub mod budget;
pub mod deepseek;
pub mod provenance;
pub mod reasoning;

use super::*;
use ato_formation::{
    authoring::BoundContract,
    generation_context::{project_failures, project_inspections},
    proposal::{
        AuthorizedSourceText, CandidateProducer, CandidateRegistry, ExplorationContext,
        ExplorationFailure, PROPOSAL_REQUEST_SCHEMA, ProducerError, ProducerOutput,
        ProducerProvenance, ProposalAuthorization, ProposalBudget, ProposalOutcome,
        ProposalRequest, ProposalRequestV2, ProposalRequestV2Schema, SourceContextEntry,
        SourceContextKind, build_source_context,
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
    attempted_claims: std::collections::BTreeSet<u64>,
    accepted_rounds: BTreeMap<u64, Value>,
    recipes: BTreeMap<String, String>,
    source_context: Vec<SourceContextEntry>,
    inspection_context: BTreeMap<String, SourceContextEntry>,
    source_inventory: local::FrozenSource,
    available_variables: Vec<Value>,
    runtime_capabilities: Vec<Value>,
    provider_identity: Option<ProviderIdentity>,
    observed_calls: BTreeMap<u64, ProviderCall>,
}

impl RequesterProposal {
    fn source_text(&self, id: &str) -> Result<SourceContextEntry> {
        if let Some(text) = self.inspection_context.get(id) {
            return Ok(text.clone());
        }
        let auth = self
            .frozen
            .policy
            .proposal
            .as_ref()
            .context("source authorization missing")?;
        let file = auth
            .execution_plan
            .as_ref()
            .and_then(|p| p.files.get(id))
            .context("source_inspection_unauthorized")?;
        let bytes = std::fs::read(self.source_inventory.root.join(&file.path))?;
        anyhow::ensure!(
            format!("sha256:{:x}", Sha256::digest(&bytes)) == file.digest,
            "proposal_source_digest_mismatch"
        );
        build_source_context(
            auth,
            &[AuthorizedSourceText {
                kind: SourceContextKind::VerifiedFile,
                logical_id: id,
                bytes: &bytes,
            }],
        )?
        .into_iter()
        .next()
        .context("source_not_bounded_text")
    }
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

/// Product entry for frozen K with optional reusable routes. Source and all
/// routes are checked by the common preparation path; no fallback K is minted.
#[allow(clippy::too_many_arguments)]
pub fn prepare_exploration_submission(
    dir: &Path,
    routes: &[PathBuf],
    contract: BoundContract,
    work_root: &Path,
    constraint: RuntimeConstraintWire,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    search_id: &str,
    authorization: ProposalAuthorization,
) -> Result<Submission> {
    prepare_exploration_submission_auto(
        dir,
        routes,
        contract,
        work_root,
        constraint,
        policy,
        budget,
        search_id,
        Some(authorization),
        BTreeMap::new(),
    )
}

/// Build private source authorizations from the immutable verified extraction.
/// Toolchain pins remain external; the model only receives bounded public IDs.
#[allow(clippy::too_many_arguments)]
pub fn prepare_exploration_submission_auto(
    dir: &Path,
    routes: &[PathBuf],
    contract: BoundContract,
    work_root: &Path,
    constraint: RuntimeConstraintWire,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    search_id: &str,
    authorization: Option<ProposalAuthorization>,
    toolchains: BTreeMap<String, String>,
) -> Result<Submission> {
    anyhow::ensure!(
        policy.exploration.is_some() && policy.proposal.is_none(),
        "explicit exploration policy required"
    );
    let mut submission = prepare_submission_inner(
        dir,
        routes,
        None,
        work_root,
        constraint,
        policy,
        budget,
        search_id,
        Some(contract),
    )?;
    let authorization = match authorization {
        Some(a) => a,
        None => {
            let mut paths: Vec<_> = source_file_inventory(&submission.frozen_source.root)?
                .into_iter()
                .filter(|p| ato_formation::proposal::source_file_allowed(p))
                .collect();
            paths.sort_by_key(|p| {
                (
                    ato_formation::proposal::source_inspection_priority(p),
                    p.clone(),
                )
            });
            let files = paths
                .into_iter()
                .take(2048)
                .map(|path| -> Result<_> {
                    let id = format!("f_{:x}", Sha256::digest(path.as_bytes()))[..26].to_owned();
                    let digest =
                        file_digest(&mut File::open(submission.frozen_source.root.join(&path))?)?;
                    Ok((
                        id,
                        ato_formation::proposal::VerifiedSourceFile { path, digest },
                    ))
                })
                .collect::<Result<_>>()?;
            ProposalAuthorization {
                execution_plan: Some(ato_formation::proposal::PlanAuthorization {
                    runtime_port_operations: true,
                    files,
                    toolchains,
                    source_oci: None,
                }),
                modifiable_derivation_refs: vec![],
                source_domain: ato_formation::proposal::SourceDomain {
                    entrypoints: BTreeMap::new(),
                    modules: BTreeMap::new(),
                },
                python_http_process: None,
                node_static_workspace: None,
                policy: ato_formation::proposal::CandidateProducerPolicy {
                    max_proposal_rounds: 1,
                    max_proposals: 1,
                    timeout_ms: 30_000,
                    allow_source_text: true,
                    max_source_bytes: 16 * 1024,
                },
            }
        }
    };
    submission.enable_candidate_producer(authorization)?;
    Ok(submission)
}

fn bound_exploration_context(
    context: &mut ato_formation::proposal::ExplorationContext,
) -> Result<()> {
    // Keep the source-owned plan and latest failure. Native operations
    // embed registered helper bytes in canonical D; those bytes need
    // not crowd out the evidence used to repair the typed plan.
    while serde_jcs::to_vec(context)?.len() > 16 * 1024 {
        if context.previous_derivations.len() > 1 {
            context.previous_derivations.pop();
        } else if context.failures.len() > 1 {
            context.failures.pop();
        } else if context.failures.iter().any(|f| f.log_tail.len() > 256) {
            for failure in &mut context.failures {
                failure.log_tail = public_log_tail_limit(&failure.log_tail, 256);
            }
        } else if let Some(d) = context.previous_derivations.pop() {
            context.omitted_derivation_refs.push(d.derivation_ref()?);
        } else if context.successful_derivation.take().is_some() {
            // Its authenticated canonical identity remains explicit.
            // Complete bytes and receipt stay in the owner ledger.
        } else if context.previous_plan.take().is_some() {
        } else {
            anyhow::bail!("exploration_context_bounds");
        }
    }
    Ok(())
}

impl Submission {
    /// Validate an unapproved submission against the locally frozen search.
    /// It is never returned as a normal verified route or Run permission.
    pub fn exploration_result(&mut self, status: &Value) -> Result<Value> {
        self.accept_proposal_round(status)?;
        let state = self.proposal_search(status)?;
        let local = self
            .proposal_state
            .as_ref()
            .context("exploration not enabled")?;
        let submitted = state.exploration_submission.as_ref();
        if let Some(submission) = submitted {
            submission.validate(&state)?;
        }
        let rounds: Vec<_> = state
            .proposal_history
            .iter()
            .chain(state.proposal_round.iter())
            .collect();
        let calls: Vec<_> = local
            .accepted_rounds
            .values()
            .filter(|r| !r["provider_call"].is_null())
            .map(|r| r["provider_call"].clone())
            .collect();
        let final_basis = submitted.and_then(|success| {
            local.accepted_rounds.values().rev().find_map(|round| {
                if !round["outcomes"].as_array()?.iter().any(|o| {
                    o["status"] == "admitted" && o["derivation_ref"] == success.derivation_ref
                }) {
                    return None;
                }
                let bytes = BASE64.decode(round["raw_output_base64"].as_str()?).ok()?;
                let proposal: Value = serde_json::from_slice(&bytes).ok()?;
                Some(proposal["proposals"][0]["operations"][0]["plan"]["basis"].clone())
            })
        });
        Ok(json!({
            "schema":"ato.formation-exploration-result/1", "search_id":state.search_id,
            "frozen_contract":state.frozen.base_contract,"contract_ref":state.frozen.base_contract_ref,
            "submission":submitted,"approval":"not_assessed","deployed":false,
            "final_requirements":submitted.map(|s| &s.derivation.requirements),
            "final_requirement_basis":final_basis,
            "requirement_basis_origin":if submitted.is_none(){"not_submitted"}else if final_basis.is_some(){"validated_source_referenced_execution_plan"}else{"declared_verified_known_derivation"},
            "rounds":rounds,"rounds_consumed":rounds.len(),"effective_max_rounds":state.frozen.policy.exploration.as_ref().unwrap().formation.max_rounds,
            "attempts":state.attempts,"budget":state.budget,"candidate_producer_calls":calls,
            "known_attempts":state.attempts.iter().filter(|a|state.frozen.candidates.iter().any(|c|c.derivation_ref==a.derivation_ref)).count(),
            "generated_attempts":state.attempts.iter().filter(|a|!state.frozen.candidates.iter().any(|c|c.derivation_ref==a.derivation_ref)).count(),
            "permission_reduction":reduction_evidence(&state),
            "proposal_evidence":local.accepted_rounds.values().map(|r| json!({"round_seq":r["round_seq"],"raw_output_digest":r["raw_output_digest"],"raw_output_base64":r["raw_output_base64"],"outcomes":r["outcomes"],"provenance":r["provenance"]})).collect::<Vec<_>>(),
            "stop":status["exploration_stop"],"search_status":status["status"],
            "permission_minimality":"only reductions established by fresh PASS receipts; mathematical minimality is not claimed"
        }))
    }
}

fn public_log_tail_limit(text: &str, limit: usize) -> String {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

pub(super) fn frozen_request(request: &SatisfyRequest) -> Result<FrozenSearchV1> {
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
    if let Some(p) = &request.policy.exploration {
        policy["exploration"] = json!(p);
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
        if let Some(domain) = &authorization.execution_plan {
            for file in domain.files.values() {
                anyhow::ensure!(
                    files.contains(&file.path),
                    "plan source is not a frozen regular file"
                );
                let bytes = std::fs::read(inventory.root.join(&file.path))?;
                anyhow::ensure!(
                    format!("sha256:{:x}", Sha256::digest(&bytes)) == file.digest,
                    "plan source digest mismatch"
                );
            }
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
        let mut inspection_context = BTreeMap::new();
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
            if let Some(domain) = &authorization.execution_plan {
                for (id, file) in &domain.files {
                    if self
                        .request
                        .policy
                        .exploration
                        .as_ref()
                        .is_some_and(|p| p.reasoning.is_some())
                        && file.path.contains('/')
                    {
                        continue;
                    }
                    texts.push((
                        SourceContextKind::VerifiedFile,
                        id.as_str(),
                        std::fs::read(inventory.root.join(&file.path))?,
                    ));
                }
            }
            if self.request.policy.exploration.is_some() {
                for (kind, id, bytes) in &texts {
                    let entries = build_source_context(
                        &authorization,
                        &[AuthorizedSourceText {
                            kind: *kind,
                            logical_id: id,
                            bytes,
                        }],
                    )?;
                    if let Some(entry) = entries.into_iter().next() {
                        inspection_context.insert(id.to_string(), entry);
                    }
                }
                // Deterministic initial inspection, independent of app outcome.
                // The private path map stays inside the requester.
                let mut ids: Vec<_> = authorization
                    .execution_plan
                    .as_ref()
                    .into_iter()
                    .flat_map(|p| p.files.iter())
                    .filter(|(_, f)| {
                        self.request
                            .policy
                            .exploration
                            .as_ref()
                            .is_none_or(|p| p.reasoning.is_none())
                            || !f.path.contains('/')
                    })
                    .collect();
                ids.sort_by_key(|(id, f)| {
                    (
                        ato_formation::proposal::source_inspection_priority(&f.path),
                        *id,
                    )
                });
                // Choose one representative per role before filling spare
                // slots, so nested READMEs cannot crowd out the startup config.
                let mut selected = Vec::new();
                let mut roles = std::collections::BTreeSet::new();
                for (id, file) in &ids {
                    if selected.len() < 4
                        && roles.insert(
                            ato_formation::proposal::source_inspection_priority(&file.path).0,
                        )
                    {
                        selected.push(*id);
                    }
                }
                for (id, _) in &ids {
                    if selected.len() < 4 && !selected.contains(id) {
                        selected.push(*id);
                    }
                }
                bounded_inspection_context(
                    selected
                        .into_iter()
                        .filter_map(|id| inspection_context.get(id))
                        .cloned()
                        .collect(),
                    authorization.policy.max_source_bytes,
                )
            } else {
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
            }
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
            attempted_claims: Default::default(),
            accepted_rounds: Default::default(),
            recipes: BTreeMap::new(),
            source_context,
            inspection_context,
            source_inventory: inventory,
            available_variables: vec![],
            runtime_capabilities: vec![],
            provider_identity: None,
            observed_calls: Default::default(),
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
            local.attempted_claims.is_empty()
                || local.provider_identity.as_ref() == Some(&identity),
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
        let state = self.proposal_search(status)?;
        let mut value = ProposalRequestV2 {
            schema: ProposalRequestV2Schema::V2,
            search_id: request.search_id,
            round_seq: local.frozen.policy.exploration.as_ref().map(|_| {
                (state.proposal_history.len() + usize::from(state.proposal_round.is_some())) as u32
            }),
            frozen_contract: request.frozen_contract,
            runtime_constraint: request.runtime_constraint,
            known_derivations: request.known_derivations,
            failure_evidence: request.failure_evidence,
            inspection_evidence: request.inspection_evidence,
            operation_catalog: request.operation_catalog,
            remaining_budget: request.remaining_budget,
            exploration_context: local.frozen.policy.exploration.as_ref().map(|p| {
                let previous_derivations = state
                    .proposal_history
                    .iter()
                    .chain(state.proposal_round.iter())
                    .flat_map(|r| r.derivations.iter())
                    .rev()
                    .take(3)
                    .cloned()
                    .collect();
                let failures = status["attempts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .rev()
                    .filter_map(|a| {
                        let d = a["derivation_ref"].as_str()?;
                        if !state.attempts.iter().any(|saved| {
                            saved.derivation_ref == d && saved.attempt_id == a["attempt_id"]
                        }) || a["failure"].is_null()
                        {
                            return None;
                        }
                        let f = &a["failure"];
                        Some(ExplorationFailure {
                            derivation_ref: d.into(),
                            stage: bounded_identifier(f["stage"].as_str().unwrap_or("unknown"), 32),
                            code: bounded_identifier(f["code"].as_str().unwrap_or("unknown"), 96),
                            exit_code: execution_fact(a)["exit_code"]
                                .as_i64()
                                .and_then(|n| i32::try_from(n).ok()),
                            log_tail: failure_log_tail(a),
                            artifacts: artifact_refs(a),
                            network_denials: denied_network(a),
                            authority_denials: denied_authority(a),
                        })
                    })
                    .take(4)
                    .collect();
                ExplorationContext {
                    effective_max_rounds: p.formation.max_rounds.get(),
                    ceiling: p.ceiling.clone(),
                    previous_derivations,
                    omitted_derivation_refs: vec![],
                    previous_plan: local.accepted_rounds.values().rev().find_map(|r| {
                        if !r["outcomes"]
                            .as_array()?
                            .iter()
                            .any(|o| o["status"] == "admitted")
                        {
                            return None;
                        }
                        let raw = BASE64.decode(r["raw_output_base64"].as_str()?).ok()?;
                        let batch: Value = serde_json::from_slice(&raw).ok()?;
                        let operation = &batch["proposals"][0]["operations"][0];
                        if operation["operation"] != "execution_plan@1" {
                            return None;
                        }
                        let plan = &operation["plan"];
                        if serde_json::to_vec(plan).ok()?.len() > 8192 {
                            return None;
                        }
                        serde_json::from_value(plan.clone()).ok()
                    }),
                    failures,
                    successful_derivation_ref: state
                        .exploration_submission
                        .as_ref()
                        .map(|s| s.derivation_ref.clone()),
                    successful_derivation: state
                        .exploration_submission
                        .as_ref()
                        .map(|s| s.derivation.clone()),
                    proposal_diagnostics: state
                        .proposal_history
                        .iter()
                        .chain(state.proposal_round.iter())
                        .rev()
                        .find(|r| !r.diagnostics.is_empty())
                        .map_or_else(Vec::new, |r| {
                            r.diagnostics.iter().take(4).cloned().collect()
                        }),
                }
            }),
            source_context: state
                .proposal_history
                .iter()
                .chain(state.proposal_round.iter())
                .rev()
                .find(|r| !r.inspection_requests.is_empty())
                .map(|r| {
                    // Most recent requested files first, then fill remaining
                    // slots from the initial context. A single-file inspection
                    // must not discard the manifest/entrypoint already seen.
                    let mut entries: Vec<_> = r
                        .inspection_requests
                        .iter()
                        .filter_map(|s| local.inspection_context.get(&s.file_id))
                        .cloned()
                        .collect();
                    for initial in &local.source_context {
                        if entries.len() >= 4 {
                            break;
                        }
                        if !entries.iter().any(|e| e.logical_id == initial.logical_id) {
                            // Use the full verified prefix before applying the
                            // same total/per-entry bounds, not a twice-trimmed copy.
                            entries.push(
                                local
                                    .inspection_context
                                    .get(&initial.logical_id)
                                    .unwrap_or(initial)
                                    .clone(),
                            );
                        }
                    }
                    bounded_inspection_context(
                        entries,
                        local
                            .frozen
                            .policy
                            .proposal
                            .as_ref()
                            .unwrap()
                            .policy
                            .max_source_bytes,
                    )
                })
                .unwrap_or_else(|| local.source_context.clone()),
        };
        if let Some(context) = &mut value.exploration_context {
            bound_exploration_context(context)?;
        }
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
        let mut wire_rounds: Vec<&Value> = status["proposal_history"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        if !status["proposal_round"].is_null() {
            wire_rounds.push(&status["proposal_round"]);
        }
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
        anyhow::ensure!(
            wire_rounds.len()
                == state.proposal_history.len() + usize::from(state.proposal_round.is_some()),
            "durable proposal disappeared"
        );
        for (index, round) in wire_rounds.iter().enumerate() {
            anyhow::ensure!(round["round_seq"] == index as u64 + 1, "invalid round");
            if let Some(prior) = local.accepted_rounds.get(&(index as u64 + 1)) {
                anyhow::ensure!(prior == *round, "durable proposal changed");
            }
        }
        anyhow::ensure!(
            local.accepted_rounds.len() <= wire_rounds.len(),
            "durable proposal disappeared"
        );
        for round in wire_rounds {
            let seq = round["round_seq"].as_u64().context("invalid round")?;
            if matches!(round["status"].as_str(), Some("open" | "claimed")) {
                continue;
            }
            self.accept_settled_round(&state, round, seq)?;
        }
        Ok(())
    }

    fn accept_settled_round(
        &mut self,
        state: &SearchStateV1,
        round: &Value,
        seq: u64,
    ) -> Result<()> {
        let local = self
            .proposal_state
            .as_ref()
            .context("producer not enabled")?;
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
        if let Some(observed) = local.observed_calls.get(&seq) {
            anyhow::ensure!(
                call.as_ref() == Some(observed),
                "persisted provider evidence changed"
            );
        }
        let mut registry = CandidateRegistry::new(&local.frozen)?;
        if let Some(policy) = &local.frozen.policy.exploration {
            let used: u32 = state
                .proposal_history
                .iter()
                .chain(state.proposal_round.iter())
                .take(seq as usize - 1)
                .map(|r| r.inspection_requests.len() as u32)
                .sum();
            registry = registry.with_inspection_budget(
                policy
                    .max_inspections
                    .checked_sub(used)
                    .context("inspection budget mismatch")?,
            )?;
        }
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
            if !round["pre_dispatch_error"].is_null() {
                anyhow::ensure!(
                    round["pre_dispatch_error"] == "call_budget_exhausted"
                        && round["status"] == "provider_error"
                        && call.is_none(),
                    "pre-dispatch failure mismatch"
                );
            }
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
            .proposal_history
            .iter()
            .chain(state.proposal_round.iter())
            .nth(seq as usize - 1)
            .context("round absent from search")?;
        anyhow::ensure!(
            serde_json::to_value(record.outcome)? == round["status"]
                && record.candidates == generated
                && serde_json::to_value(&record.inspection_requests)?
                    == json!(
                        outcomes
                            .iter()
                            .flat_map(|o| o["inspection_refs"].as_array().into_iter().flatten())
                            .collect::<Vec<_>>()
                    ),
            "durable candidate scope mismatch"
        );
        anyhow::ensure!(
            record
                .diagnostics
                .iter()
                .any(|d| d == "reasoning_call_budget_exhausted")
                == (round["pre_dispatch_error"] == "call_budget_exhausted"),
            "durable pre-dispatch diagnosis mismatch"
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
        local.recipes.extend(recipes);
        local.accepted_rounds.insert(seq, round.clone());
        Ok(())
    }

    pub fn proposal_recipes(&self) -> impl Iterator<Item = (&String, &String)> {
        self.proposal_state.iter().flat_map(|p| p.recipes.iter())
    }
}

fn bounded_inspection_context(
    mut entries: Vec<SourceContextEntry>,
    cap: usize,
) -> Vec<SourceContextEntry> {
    let share = (cap / entries.len().max(1)).min(ato_formation::proposal::MAX_SOURCE_ENTRY_BYTES);
    for entry in &mut entries {
        let mut end = entry.text.len().min(share);
        while !entry.text.is_char_boundary(end) {
            end -= 1;
        }
        entry.truncated |= end != entry.text.len();
        entry.text.truncate(end);
        entry.content_sha256 = format!("sha256:{:x}", Sha256::digest(entry.text.as_bytes()));
    }
    entries.retain(|e| !e.text.is_empty());
    // Priority selects the subset; wire order is canonical source-ID order.
    entries.sort_by(|a, b| (a.kind, &a.logical_id).cmp(&(b.kind, &b.logical_id)));
    entries
}

fn outcome_wire(outcome: &ProposalOutcome) -> Value {
    match outcome {
        ProposalOutcome::Admitted(c) => json!({"status":"admitted","proposal_id":c.proposal_id(),
            "derivation_ref":c.compiled().derivation_ref,"capsule_toml":c.compiled().capsule_toml,
            "derivation":c.compiled().derivation,"candidate":c.candidate()}),
        ProposalOutcome::Rejected(e) => json!({"status":"rejected","code":e.0}),
        ProposalOutcome::Unsupported => json!({"status":"unsupported"}),
        ProposalOutcome::InspectionRequested(sources) => {
            json!({"status":"rejected","code":"source_inspection_requested","inspection_refs":sources})
        }
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
    if let Some(exploration) = &submission.request.policy.exploration {
        anyhow::ensure!(
            exploration.provider_configuration_ref.as_deref()
                == Some(producer.configuration_ref()?.as_str()),
            "frozen provider configuration mismatch"
        );
    }
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
/// Shared session/API driver. Exact inputs and responses are durable before a
/// proposal reaches the same independent receiver compiler and Runtime.
pub fn serve_reasoning_proposal(
    client: &Client,
    id: &str,
    status: &Value,
    submission: &mut Submission,
    claimant_id: &str,
    producer: Arc<reasoning::ReasoningProducer>,
) -> Result<bool> {
    let policy = submission
        .request
        .policy
        .exploration
        .as_ref()
        .context("exploration missing")?;
    anyhow::ensure!(
        policy.provider_configuration_ref.as_deref() == Some(producer.configuration_ref()),
        "frozen reasoning configuration mismatch"
    );
    if let Some(identity) = producer.api_identity() {
        submission.configure_general_producer(identity)?;
    }
    serve_proposal_inner(
        client,
        id,
        status,
        submission,
        claimant_id,
        Invocation::Reasoning(producer),
    )
}

enum Invocation {
    Fixed(Arc<dyn CandidateProducer + Send + Sync>),
    General(Arc<DeepSeekCandidateProducer>),
    Reasoning(Arc<reasoning::ReasoningProducer>),
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
    if point.is_null()
        || (point["claimed"] != false && !matches!(producer, Invocation::Reasoning(_)))
    {
        return Ok(false);
    }
    let request = submission.proposal_request(status)?;
    let general_request = match &producer {
        Invocation::General(_) | Invocation::Reasoning(_) => {
            Some(submission.proposal_request_v2(status)?)
        }
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
    let seq = point["round_seq"].as_u64().context("invalid round")?;
    if local.attempted_claims.contains(&seq) && !matches!(producer, Invocation::Reasoning(_)) {
        return Ok(false);
    }
    anyhow::ensure!(
        point["revision"] == state.revision && seq == (state.proposal_history.len() + 1) as u64,
        "proposal point revision mismatch"
    );
    local.attempted_claims.insert(seq);
    let saved_claim = if let Invocation::Reasoning(p) = &producer {
        p.saved_claim(id, seq)?
    } else {
        None
    };
    let cached = saved_claim.is_some();
    let expires = state
        .proposal_round
        .as_ref()
        .context("missing open round")?
        .expires_at_ms;
    let retries = state
        .frozen
        .policy
        .exploration
        .as_ref()
        .map_or(0, |p| p.formation.max_retries);
    let client = &client.with_deadline(expires);
    let claim_revision = if point["claimed"] == true {
        state.revision.saturating_sub(1)
    } else {
        state.revision
    };
    let claim_request = serde_jcs::to_vec(&json!({"satisfy_id":id,"revision":claim_revision,
        "claimant_id":claimant_id}))?;
    let claim = match saved_claim.map(Ok).unwrap_or_else(|| {
        if let Invocation::Reasoning(p) = &producer {
            p.coordinator_operation(seq, "claim", &claim_request, retries, expires, || {
                client.claim_proposal(id, claim_revision, claimant_id)
            })
        } else {
            client.claim_proposal(id, state.revision, claimant_id)
        }
    }) {
        Ok(claim) => claim,
        Err(error) if matches!(&producer, Invocation::General(_) | Invocation::Reasoning(_)) => {
            // No send follows an uncertain/refused claim. Surface the receiver
            // diagnostic instead of silently waiting out every generated round.
            return Err(error.context("proposal claim failed before provider send; no retry"));
        }
        Err(_) => return Ok(false),
    };
    let fence = claim["fence"]
        .as_str()
        .context("missing private claim fence")?;
    let revision = claim["revision"]
        .as_u64()
        .context("missing claim revision")?;
    anyhow::ensure!(
        (if cached || point["claimed"] == true {
            Some(state.revision)
        } else {
            state.revision.checked_add(1)
        }) == Some(revision)
            && claim["round_seq"] == seq
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
    if let Invocation::Reasoning(p) = &producer {
        p.save_claim(id, seq, &claim)?;
    }
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
        if let Invocation::Reasoning(producer) = &producer {
            // Reconcile a response already saved by this expired Session; it
            // cannot open another exchange or authorize a late proposal.
            producer.reconcile_expired_session_response(
                &submission.request.search_id,
                expires,
                now_ms,
            )?;
        }
        return Ok(false);
    }
    let mut completion = json!({"revision":revision,"fence":fence});
    let durable_producer = if let Invocation::Reasoning(p) = &producer {
        Some(p.clone())
    } else {
        None
    };
    match producer {
        Invocation::Reasoning(producer) => {
            // Only owner metadata enters reasoning; redeemed values are never fetched here.
            let metadata = client.variable_metadata(
                &submission.request.source.closure_ref,
                &submission.request.search_id,
            )?;
            let runtime_capabilities =
                client.runtime_capabilities(&submission.request.runtime_constraint)?;
            submission
                .proposal_state
                .as_mut()
                .context("producer missing")?
                .available_variables = metadata;
            submission
                .proposal_state
                .as_mut()
                .context("producer missing")?
                .runtime_capabilities = runtime_capabilities;
            let answer = producer.run_round(
                submission,
                status,
                general_request.context("reasoning request missing")?,
                expires,
            );
            match answer {
                Ok(answer) => {
                    completion["raw_output_base64"] = json!(BASE64.encode(answer.output.raw()));
                    if let Some(call) = answer.provider_call {
                        completion["provenance"] = serde_json::to_value(&call.provenance)?;
                        submission
                            .proposal_state
                            .as_mut()
                            .context("producer missing")?
                            .observed_calls
                            .insert(seq, call);
                    } else {
                        completion["provenance"] = json!({"provider":"fixed"});
                    }
                }
                Err(error) => {
                    completion["outcome"] =
                        json!(if error.is::<reasoning::ReasoningSessionDeadline>() {
                            "timeout"
                        } else {
                            "provider_error"
                        });
                    if error.is::<reasoning::ReasoningCallBudgetExhausted>() {
                        completion["pre_dispatch_error"] = json!("call_budget_exhausted");
                    }
                    if let Some(failure) =
                        error.downcast_ref::<reasoning::ReasoningProviderFailure>()
                    {
                        let call = ProviderCall {
                            provenance: failure.0.provenance.clone(),
                            status: if failure.0.class == ErrorClass::Timeout {
                                CallStatus::Timeout
                            } else {
                                CallStatus::ProviderError
                            },
                            error_class: Some(failure.0.class),
                        };
                        completion["outcome"] = serde_json::to_value(call.status)?;
                        completion["provenance"] = serde_json::to_value(&call.provenance)?;
                        completion["error_class"] = serde_json::to_value(call.error_class)?;
                        submission
                            .proposal_state
                            .as_mut()
                            .context("producer missing")?
                            .observed_calls
                            .insert(seq, call);
                    }
                }
            }
        }
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
                .observed_calls
                .insert(seq, call);
        }
    }
    let bytes = serde_json::to_vec(&completion)?;
    // A lost completion response is recovered by the caller's next GET. Never
    // re-enter the producer or synthesize a new batch to repair that transport.
    if let Some(p) = durable_producer {
        let reported = p.coordinator_operation(seq, "complete", &bytes, retries, expires, || {
            client.for_reporting().complete_proposal(id, &bytes)
        });
        if let Err(error) = reported
            && SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() < u128::from(expires)
        {
            return Err(error);
        }
    } else {
        let _ = client.complete_proposal(id, &bytes);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;

fn bounded_identifier(value: &str, max: usize) -> String {
    if value.len() <= max && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        value.into()
    } else {
        "unknown".into()
    }
}
fn public_log_tail(text: &str) -> String {
    let mut out = String::new();
    for line in text
        .lines()
        .rev()
        .take(16)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let lower = line.to_ascii_lowercase();
        if [
            "authorization",
            "bearer ",
            "password=",
            "token=",
            "secret=",
            "credential",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            out.push_str("<redacted-sensitive-log>\n");
            continue;
        }
        for word in line.split_whitespace() {
            let bare = word.trim_matches(['\'', '"', '(', ')', ',']);
            if bare.starts_with("https://") || bare.starts_with("http://") {
                out.push_str(&ato_formation::source::redact_url(bare));
            } else if bare.starts_with('/')
                && !bare.starts_with("/app/")
                && !bare.starts_with("/src/")
                && !bare.starts_with("/opt/ato/toolchains/")
            {
                out.push_str("<host-path>");
            } else {
                out.push_str(word);
            }
            out.push(' ');
        }
        out.push('\n');
    }
    while out.len() > 2048 {
        out.remove(0);
    }
    out
}

fn failure_log_tail(attempt: &Value) -> String {
    let fact = execution_fact(attempt);
    let execution = fact["log_tail"].as_str().unwrap_or("");
    let message = attempt["failure"]["message"].as_str().unwrap_or("");
    // A successful build audit must not hide a later launch/K failure. Put
    // the typed failure last so the existing bounded/redacted tail retains it.
    public_log_tail(&format!("{execution}\n[failure] {message}"))
}

fn denied_network(attempt: &Value) -> Vec<ato_formation::requirements::NetworkRequirement> {
    use ato_formation::requirements::{ExecutionPhase, ExecutionRequirements, NetworkRequirement};
    let mut refused = std::collections::BTreeSet::new();
    for report in attempt["exploration_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "exploration_network_evidence")
        .flat_map(|e| e["reports"].as_array().into_iter().flatten())
    {
        let Ok(phase) = serde_json::from_value::<ExecutionPhase>(report["phase"].clone()) else {
            continue;
        };
        for denied in report["report"]["refused"].as_array().into_iter().flatten() {
            let (Some(host), Some(port)) = (denied["target"].as_str(), denied["port"].as_u64())
            else {
                continue;
            };
            let Ok(port) = u16::try_from(port) else {
                continue;
            };
            let n = NetworkRequirement {
                phase,
                host: host.into(),
                port,
            };
            if (ExecutionRequirements {
                network: vec![n.clone()],
                authority: vec![],
                host: None,
            })
            .validate()
            .is_ok()
            {
                refused.insert(n);
            }
        }
    }
    refused.into_iter().take(16).collect()
}
fn denied_authority(attempt: &Value) -> Vec<ato_formation::requirements::AuthorityRequirement> {
    use ato_formation::requirements::{AuthorityRequirement, ExecutionRequirements};
    let mut refused = std::collections::BTreeSet::new();
    for requirement in attempt["exploration_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "exploration_authority_evidence")
        .flat_map(|e| e["refused"].as_array().into_iter().flatten())
    {
        if let Ok(a) = serde_json::from_value::<AuthorityRequirement>(requirement.clone())
            && (ExecutionRequirements {
                network: vec![],
                authority: vec![a.clone()],
                host: None,
            })
            .validate()
            .is_ok()
        {
            refused.insert(a);
        }
    }
    refused.into_iter().take(16).collect()
}

fn execution_fact(attempt: &Value) -> Value {
    attempt["exploration_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "exploration_execution_facts")
        .flat_map(|e| e["facts"].as_array().into_iter().flatten())
        .last()
        .cloned()
        .unwrap_or(Value::Null)
}
fn artifact_refs(attempt: &Value) -> Vec<String> {
    attempt["exploration_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "exploration_source_oci_evidence")
        .filter_map(|e| e["provenance"]["outputs"]["archive_sha256"].as_str())
        .filter(|s| {
            s.strip_prefix("sha256:").is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
        })
        .take(4)
        .map(str::to_owned)
        .collect()
}

fn reduction_evidence(state: &SearchStateV1) -> Value {
    let Some(best) = &state.exploration_submission else {
        return json!({"verified":false,"reason":"K_not_reached"});
    };
    let first = state
        .attempts
        .iter()
        .position(|a| a.route_accepted)
        .unwrap_or(state.attempts.len());
    let reduction_attempts: Vec<_> = state
        .attempts
        .iter()
        .skip(first.saturating_add(1))
        .map(|a| {
            json!({
                "attempt_id":a.attempt_id,"derivation_ref":a.derivation_ref,"status":a.status,
                "fresh_PASS_accepted":a.route_accepted,"failure_code":a.failure_code
            })
        })
        .collect();
    json!({"attempts":reduction_attempts,"final_verified_derivation_ref":best.derivation_ref,
        "final_receipt_attempt_id":best.attempt_id,
        "successful_D_retained":true,"unverified_reduction_submitted":false,
        "mathematical_minimality_claimed":false})
}
