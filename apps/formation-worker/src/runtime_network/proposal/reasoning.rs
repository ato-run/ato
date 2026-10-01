//! Shared, bounded reasoning exchanges. A model proposes; only Ato resolves
//! verified source IDs, compiles D and grants a Runtime attempt.
use super::*;
use anyhow::{Context, Result, ensure};
use ato_formation::proposal::{Proposal, ProposalBatch, REASONING_BATCH_SCHEMA, SourceReference};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub const INPUT_SCHEMA: &str = "ato.formation-reasoning-input/1";
pub const SESSION_RESPONSE_SCHEMA: &str = "ato.formation-session-response/1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningProviderConfig {
    Api(deepseek::DeepSeekConfig),
    Session(SessionConfig),
}
impl ReasoningProviderConfig {
    pub fn configuration_ref(&self, budget: &budget::BudgetPlan) -> Result<String> {
        match self {
            Self::Api(c) => c.configuration_ref(budget),
            Self::Session(c) => {
                ensure!(
                    c.provider == "codex_session"
                        && c.model == "codex-session"
                        && c.prompt_version == deepseek::PROMPT_VERSION_V5,
                    "invalid session provider"
                );
                budget.validate()?;
                Ok(format!(
                    "sha256:{:x}",
                    Sha256::digest(serde_jcs::to_vec(&(c, budget, deepseek::PROMPT_V5))?)
                ))
            }
        }
    }
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicSource {
    pub reference: SourceReference,
    pub source_relative_path: String,
    pub purpose: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub archive_digest: String,
    pub closure_ref: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextOmission {
    pub logical_id: String,
    pub reason: String,
    pub acquired_bytes: usize,
    pub transmitted_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningInput {
    #[serde(default)]
    pub catalog_sources_in_inventory: bool,
    pub goal: Option<String>,
    pub schema: String,
    pub call_id: String,
    pub frozen_contract_ref: String,
    pub source_identity: SourceIdentity,
    pub inventory: Vec<PublicSource>,
    pub request: ProposalRequestV2,
    pub projection: Vec<ContextOmission>,
    pub rounds_remaining: u32,
    pub calls_remaining: u32,
    pub inspections_remaining: u32,
    pub inspection_source_bytes_remaining: u64,
    pub inspection_feedback: Vec<String>,
}
impl ReasoningInput {
    fn validate(&self, auth: &ato_formation::proposal::ProposalAuthorization) -> Result<()> {
        let mut request = self.request.clone();
        if self.catalog_sources_in_inventory {
            for operation in &mut request.operation_catalog.operations {
                if let ato_formation::proposal::OperationDomain::ExecutionPlan { sources, .. } =
                    operation
                {
                    ensure!(sources.is_empty(), "duplicate common source catalog");
                    *sources = self
                        .inventory
                        .iter()
                        .map(|s| ato_formation::proposal::CatalogSource {
                            reference: s.reference.clone(),
                            purpose: s.purpose.clone(),
                        })
                        .collect();
                }
            }
        }
        request.validate(auth)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepRecord {
    schema: String,
    configuration_ref: String,
    input_sha256: String,
    output_sha256: String,
    raw_output_base64: String,
    provider_call: Option<ProviderCall>,
    #[serde(default)]
    failure: Option<ErrorClass>,
    elapsed_ms: u64,
    inspected: Vec<SourceReference>,
    inspection_error: Option<String>,
    inspected_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionResponse {
    schema: String,
    input_sha256: String,
    output: ProposalBatch,
}

pub struct ReasoningProducer {
    config: ReasoningProviderConfig,
    configuration_ref: String,
    budget: budget::BudgetPlan,
    directory: PathBuf,
    api: Option<Arc<DeepSeekCandidateProducer>>,
}
#[derive(Debug)]
pub(super) struct ReasoningProviderFailure(pub GeneralFailure);
impl std::fmt::Display for ReasoningProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reasoning provider failed: {:?}", self.0.class)
    }
}
impl std::error::Error for ReasoningProviderFailure {}

pub(super) struct RoundAnswer {
    pub output: ProducerOutput,
    pub provider_call: Option<ProviderCall>,
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub(super) fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    // Publish complete, durable bytes without replacing an existing exchange.
    // The staging file is in the same owner-owned directory as the destination.
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().context("evidence parent")?)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().context("evidence parent")?)?.sync_all()?;
    Ok(())
}
fn bounded_read(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "reasoning evidence bounds");
    Ok(bytes)
}
impl ReasoningProducer {
    pub fn new(
        config: ReasoningProviderConfig,
        budget: budget::BudgetPlan,
        directory: PathBuf,
        api: Option<Arc<DeepSeekCandidateProducer>>,
    ) -> Result<Self> {
        let configuration_ref = config.configuration_ref(&budget)?;
        ensure!(
            config.is_session() == api.is_none(),
            "reasoning transport mismatch"
        );
        std::fs::create_dir_all(&directory)?;
        let binding = serde_jcs::to_vec(&(configuration_ref.as_str(), &budget))?;
        let path = directory.join("binding.json");
        if path.exists() {
            ensure!(
                bounded_read(&path, 4096)? == binding,
                "reasoning binding changed"
            );
        } else {
            save(&path, &binding)?;
        }
        Ok(Self {
            config,
            configuration_ref,
            budget,
            directory,
            api,
        })
    }
    pub fn configuration_ref(&self) -> &str {
        &self.configuration_ref
    }
    pub fn api_identity(&self) -> Option<ProviderIdentity> {
        self.api.as_ref().map(|p| p.identity())
    }
    fn records(&self) -> Result<Vec<(String, ReasoningInput, StepRecord)>> {
        let mut paths = std::fs::read_dir(&self.directory)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut records = Vec::new();
        for path in paths {
            let Some(name) = path
                .file_name()
                .and_then(|p| p.to_str())
                .and_then(|p| p.strip_suffix(".record.json"))
            else {
                continue;
            };
            let record: StepRecord = serde_json::from_slice(&bounded_read(&path, 32 * 1024)?)?;
            let input_bytes = bounded_read(
                &self.directory.join(format!("{name}.input.json")),
                64 * 1024,
            )?;
            let input: ReasoningInput = serde_json::from_slice(&input_bytes)?;
            let output = BASE64.decode(&record.raw_output_base64)?;
            ensure!(
                record.schema == "ato.formation-reasoning-step/1"
                    && input.schema == INPUT_SCHEMA
                    && record.configuration_ref == self.configuration_ref
                    && record.input_sha256 == digest(&input_bytes)
                    && record.output_sha256 == digest(&output)
                    && output.len() <= ato_formation::proposal::MAX_BATCH_BYTES,
                "reasoning evidence changed"
            );
            records.push((name.to_owned(), input, record));
        }
        Ok(records)
    }
    pub(super) fn saved_claim(&self, id: &str, seq: u64) -> Result<Option<Value>> {
        let path = self.directory.join(format!("r{seq:03}.claim.json"));
        if !path.exists() {
            return Ok(None);
        }
        let saved: Value = serde_json::from_slice(&bounded_read(&path, 4096)?)?;
        ensure!(
            saved["satisfy_id"] == id && saved["configuration_ref"] == self.configuration_ref,
            "reasoning claim binding changed"
        );
        Ok(Some(saved["claim"].clone()))
    }
    pub(super) fn save_claim(&self, id: &str, seq: u64, claim: &Value) -> Result<()> {
        let path = self.directory.join(format!("r{seq:03}.claim.json"));
        let value =
            json!({"satisfy_id":id,"configuration_ref":self.configuration_ref,"claim":claim});
        if path.exists() {
            ensure!(
                serde_json::from_slice::<Value>(&bounded_read(&path, 4096)?)? == value,
                "reasoning claim changed"
            );
        } else {
            save(&path, &serde_jcs::to_vec(&value)?)?;
        }
        Ok(())
    }
    fn finish_round(
        &self,
        raw: Vec<u8>,
        inspections: Vec<SourceReference>,
        calls: Vec<ProviderCall>,
    ) -> Result<RoundAnswer> {
        let final_raw = if inspections.is_empty() {
            raw
        } else {
            let mut batch: Value = serde_json::from_slice(&raw)?;
            batch["schema"] = json!(REASONING_BATCH_SCHEMA);
            batch["inspection_history"] = json!(inspections);
            serde_jcs::to_vec(&batch)?
        };
        let provider_call = if let Some(api) = &self.api {
            let mut provenance = api.identity().unknown_usage();
            provenance.usage = provenance::Usage {
                input_tokens: Some(
                    calls
                        .iter()
                        .filter_map(|c| c.provenance.usage.input_tokens)
                        .sum(),
                ),
                output_tokens: Some(
                    calls
                        .iter()
                        .filter_map(|c| c.provenance.usage.output_tokens)
                        .sum(),
                ),
            };
            provenance.estimated_cost_usd_micros = Some(
                calls
                    .iter()
                    .filter_map(|c| c.provenance.estimated_cost_usd_micros)
                    .sum(),
            );
            provenance.latency_ms =
                Some(calls.iter().filter_map(|c| c.provenance.latency_ms).sum());
            Some(ProviderCall {
                provenance,
                status: CallStatus::Success,
                error_class: None,
            })
        } else {
            None
        };
        let provenance = provider_call.as_ref().map_or(
            ProducerProvenance {
                provider: "fixed".into(),
                model: None,
            },
            |c| ProducerProvenance {
                provider: c.provenance.provider.clone(),
                model: Some(c.provenance.model.clone()),
            },
        );
        Ok(RoundAnswer {
            output: ProducerOutput::new(final_raw, provenance)?,
            provider_call,
        })
    }
    pub fn accounting(&self) -> Result<Value> {
        let records = self.records()?;
        let calls: Vec<_> = records.iter().map(|(name,input,r)| json!({"call_id":input.call_id,
            "input_sha256":r.input_sha256,"output_sha256":r.output_sha256,"latency_ms":r.elapsed_ms,
            "provider_call":r.provider_call,"inspection_refs":r.inspected,"inspection_error":r.inspection_error,
            "input_file":format!("{name}.input.json"),"output_file":format!("{name}.record.json")})).collect();
        Ok(
            json!({"schema":"ato.formation-reasoning-accounting/1", "provider":if self.config.is_session(){"codex_session"}else{"deepseek"},
            "calls":calls,"call_count":calls.len(),"API_calls":records.iter().filter(|(_,_,r)|r.provider_call.is_some()).count(),
            "session_token_usage":"not_exposed; no fabricated usage", "session_cost":"not_exposed", "configuration_ref":self.configuration_ref}),
        )
    }
    pub(super) fn run_round(
        &self,
        submission: &Submission,
        status: &Value,
        mut request: ProposalRequestV2,
        expires: u64,
    ) -> Result<RoundAnswer> {
        let state = submission.proposal_search(status)?;
        let local = submission
            .proposal_state
            .as_ref()
            .context("reasoning not enabled")?;
        let auth = local
            .frozen
            .policy
            .proposal
            .as_ref()
            .context("proposal domain missing")?;
        let policy = state
            .frozen
            .policy
            .exploration
            .as_ref()
            .context("exploration missing")?;
        let limits = policy
            .reasoning
            .as_ref()
            .context("reasoning limits missing")?;
        let round = request.round_seq.context("reasoning round missing")?;
        let domain = auth
            .execution_plan
            .as_ref()
            .context("execution plan domain missing")?;
        let inventory: Vec<_> = domain
            .catalog_sources()
            .into_iter()
            .map(|s| PublicSource {
                source_relative_path: domain.files[&s.reference.file_id].path.clone(),
                reference: s.reference,
                purpose: s.purpose,
            })
            .collect();
        let records = self.records()?;
        ensure!(
            records
                .iter()
                .all(|(_, i, _)| i.request.search_id == request.search_id
                    && i.goal == limits.goal
                    && i.frozen_contract_ref == state.frozen.base_contract_ref
                    && i.source_identity.closure_ref == submission.request.source.closure_ref
                    && i.source_identity.archive_digest
                        == submission.request.source.archive_digest),
            "reasoning search changed"
        );
        for (_, input, _) in &records {
            input.validate(auth)?;
            for entry in &input.request.source_context {
                let verified = local
                    .inspection_context
                    .get(&entry.logical_id)
                    .context("recorded source text absent")?;
                ensure!(
                    verified.text.starts_with(&entry.text),
                    "recorded source text changed"
                );
            }
        }
        let mut acquired: BTreeMap<_, _> = local
            .source_context
            .iter()
            .map(|e| {
                (
                    e.logical_id.clone(),
                    local
                        .inspection_context
                        .get(&e.logical_id)
                        .unwrap_or(e)
                        .clone(),
                )
            })
            .collect();
        let mut inspected = BTreeMap::<String, SourceReference>::new();
        let mut round_inspections = Vec::new();
        let mut inspection_bytes = 0_u64;
        let mut inspection_exchanges = 0_u32;
        let mut feedback = Vec::new();
        let mut round_calls = Vec::new();
        for (_, input, r) in &records {
            if !r.inspected.is_empty() || r.inspection_error.is_some() {
                inspection_exchanges += 1;
            }
            inspection_bytes = inspection_bytes
                .checked_add(r.inspected_bytes)
                .context("inspection bytes overflow")?;
            for source in &r.inspected {
                ato_formation::proposal::validate_inspection_sources(
                    auth,
                    std::slice::from_ref(source),
                    policy.max_inspections,
                    1,
                )?;
                acquired.insert(
                    source.file_id.clone(),
                    local
                        .inspection_context
                        .get(&source.file_id)
                        .context("verified source text missing")?
                        .clone(),
                );
                inspected.insert(source.file_id.clone(), source.clone());
                if input.request.round_seq == Some(round) {
                    round_inspections.push(source.clone());
                }
            }
            if input.request.round_seq == Some(round) {
                if let Some(e) = &r.inspection_error {
                    feedback.push(e.clone());
                }
                if let Some(call) = &r.provider_call {
                    round_calls.push(call.clone());
                }
            }
        }
        // A lost completion must deliver the already recorded final answer,
        // never open another inference call after restart.
        if let Some((_, _, last)) = records
            .iter()
            .rev()
            .find(|(_, i, _)| i.request.round_seq == Some(round))
        {
            if let Some(class) = last.failure {
                let call = last
                    .provider_call
                    .as_ref()
                    .context("failed call evidence absent")?;
                return Err(anyhow::anyhow!(ReasoningProviderFailure(GeneralFailure {
                    class,
                    provenance: call.provenance.clone(),
                })));
            }
            if last.inspected.is_empty() && last.inspection_error.is_none() {
                return self.finish_round(
                    BASE64.decode(&last.raw_output_base64)?,
                    round_inspections,
                    round_calls,
                );
            }
        }
        let prior_inspection_ms: u64 = records
            .iter()
            .filter(|(_, _, r)| !r.inspected.is_empty() || r.inspection_error.is_some())
            .map(|(_, _, r)| r.elapsed_ms)
            .sum();
        let started = Instant::now();
        let prior_in_round = records
            .iter()
            .filter(|(_, i, _)| i.request.round_seq == Some(round))
            .count();
        let mut step = prior_in_round as u32 + 1;
        loop {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            ensure!(now < expires, "reasoning round deadline");
            let name = format!("r{round:03}_s{step:03}");
            let call_id = format!("{}_r{round}_s{step}", request.search_id);
            let input_path = self.directory.join(format!("{name}.input.json"));
            let record_path = self.directory.join(format!("{name}.record.json"));
            let input_count = std::fs::read_dir(&self.directory)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".input.json"))
                .count();
            ensure!(
                input_path.exists() || input_count < self.budget.max_calls as usize,
                "reasoning call budget exhausted"
            );
            request.remaining_budget.timeout_ms = request
                .remaining_budget
                .timeout_ms
                .min(expires - now)
                .min(30_000);
            let mut entries: Vec<_> = acquired.values().cloned().collect();
            entries.sort_by_key(|e| {
                let file = &domain.files[&e.logical_id];
                let priority = ato_formation::proposal::source_inspection_priority(&file.path).0;
                (
                    if priority == 0 {
                        0
                    } else if priority == 2 {
                        1
                    } else if inspected.contains_key(&e.logical_id) {
                        2
                    } else {
                        3
                    },
                    priority,
                    e.logical_id.clone(),
                )
            });
            request.source_context = bounded_inspection_context(
                entries.into_iter().take(4).collect(),
                auth.policy.max_source_bytes,
            );
            let mut input = ReasoningInput {
                catalog_sources_in_inventory: true,
                goal: limits.goal.clone(),
                schema: INPUT_SCHEMA.into(),
                call_id,
                frozen_contract_ref: state.frozen.base_contract_ref.clone(),
                source_identity: SourceIdentity {
                    archive_digest: submission.request.source.archive_digest.clone(),
                    closure_ref: submission.request.source.closure_ref.clone(),
                },
                inventory: inventory.clone(),
                request: request.clone(),
                projection: vec![],
                rounds_remaining: policy.formation.max_rounds.get().saturating_sub(round - 1),
                calls_remaining: self.budget.max_calls.saturating_sub(input_count as u32),
                inspections_remaining: policy
                    .max_inspections
                    .saturating_sub(inspection_exchanges.max(inspected.len() as u32)),
                inspection_source_bytes_remaining: limits
                    .inspection_source_bytes
                    .saturating_sub(inspection_bytes),
                inspection_feedback: feedback.clone(),
            };
            // The exact same public source refs appear once, in inventory.
            // The original full catalog is reconstructed and validated locally.
            for operation in &mut input.request.operation_catalog.operations {
                if let ato_formation::proposal::OperationDomain::ExecutionPlan { sources, .. } =
                    operation
                {
                    sources.clear();
                }
            }
            loop {
                input.projection = acquired
                    .values()
                    .filter_map(|e| {
                        let transmitted = input
                            .request
                            .source_context
                            .iter()
                            .find(|t| t.logical_id == e.logical_id)
                            .map_or(0, |t| t.text.len());
                        (transmitted < e.text.len()).then(|| ContextOmission {
                            logical_id: e.logical_id.clone(),
                            reason: if transmitted == 0 {
                                "slot_or_input_budget"
                            } else {
                                "text_prefix_budget"
                            }
                            .into(),
                            acquired_bytes: e.text.len(),
                            transmitted_bytes: transmitted,
                        })
                    })
                    .collect();
                let bytes = serde_jcs::to_vec(&input)?;
                if (bytes.len() + deepseek::PROMPT_V5.len() + 1024) as u64
                    <= self.budget.input_token_cap
                {
                    break;
                }
                let entry = input
                    .request
                    .source_context
                    .iter_mut()
                    .max_by_key(|e| e.text.len())
                    .context("non-text reasoning context exceeds input budget")?;
                let mut end = entry.text.len().saturating_sub(512);
                while !entry.text.is_char_boundary(end) {
                    end -= 1;
                }
                entry.text.truncate(end);
                entry.truncated = true;
                entry.content_sha256 = digest(entry.text.as_bytes());
                input.request.source_context.retain(|e| !e.text.is_empty());
            }
            input.validate(auth)?;
            let mut bytes = serde_jcs::to_vec(&input)?;
            if input_path.exists() {
                bytes = bounded_read(&input_path, 64 * 1024)?;
                let saved: ReasoningInput = serde_json::from_slice(&bytes)?;
                ensure!(
                    saved.request.search_id == input.request.search_id
                        && saved.request.round_seq == input.request.round_seq
                        && saved.schema == INPUT_SCHEMA
                        && saved.goal == input.goal
                        && saved.call_id == input.call_id
                        && saved.frozen_contract_ref == input.frozen_contract_ref
                        && saved.inventory == input.inventory
                        && saved.source_identity.closure_ref == input.source_identity.closure_ref
                        && saved.source_identity.archive_digest
                            == input.source_identity.archive_digest,
                    "pending reasoning input mismatch"
                );
                saved.validate(auth)?;
                for entry in &saved.request.source_context {
                    let verified = local
                        .inspection_context
                        .get(&entry.logical_id)
                        .context("pending source text absent")?;
                    ensure!(
                        verified.text.starts_with(&entry.text),
                        "pending source text changed"
                    );
                }
                input = saved;
            } else {
                save(&input_path, &bytes)?;
            }
            let input_sha256 = digest(&bytes);
            let call_started = Instant::now();
            let (raw, call) = if let Some(api) = &self.api {
                let answer = match api.propose_reasoning(&input) {
                    Ok(answer) => answer,
                    Err(failure) => {
                        let failure = *failure;
                        let call = ProviderCall {
                            provenance: failure.provenance.clone(),
                            status: if failure.class == ErrorClass::Timeout {
                                CallStatus::Timeout
                            } else {
                                CallStatus::ProviderError
                            },
                            error_class: Some(failure.class),
                        };
                        let record = StepRecord {
                            schema: "ato.formation-reasoning-step/1".into(),
                            configuration_ref: self.configuration_ref.clone(),
                            input_sha256,
                            output_sha256: digest(&[]),
                            raw_output_base64: String::new(),
                            provider_call: Some(call),
                            failure: Some(failure.class),
                            elapsed_ms: call_started.elapsed().as_millis().min(u64::MAX as u128)
                                as u64,
                            inspected: vec![],
                            inspection_error: None,
                            inspected_bytes: 0,
                        };
                        save(&record_path, &serde_jcs::to_vec(&record)?)?;
                        return Err(anyhow::anyhow!(ReasoningProviderFailure(failure)));
                    }
                };
                let call = ProviderCall {
                    provenance: answer.provenance,
                    status: CallStatus::Success,
                    error_class: None,
                };
                call.validate(&api.identity())?;
                (answer.output.raw().to_vec(), Some(call))
            } else {
                let response_path = self.directory.join(format!("{name}.response.json"));
                loop {
                    if response_path.exists() {
                        break;
                    }
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
                    ensure!(
                        now < expires,
                        "session response timeout; same search and budget retained"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
                let response: SessionResponse = serde_json::from_slice(&bounded_read(
                    &response_path,
                    ato_formation::proposal::MAX_BATCH_BYTES + 4096,
                )?)?;
                ensure!(
                    response.schema == SESSION_RESPONSE_SCHEMA
                        && response.input_sha256 == input_sha256,
                    "session response input mismatch"
                );
                (serde_jcs::to_vec(&response.output)?, None)
            };
            ensure!(
                raw.len() <= ato_formation::proposal::MAX_BATCH_BYTES,
                "reasoning output bounds"
            );
            let parsed = serde_json::from_slice::<ProposalBatch>(&raw);
            let inspection = parsed.as_ref().ok().and_then(|b| {
                if b.schema == ato_formation::proposal::PROPOSAL_SCHEMA && b.proposals.len() == 1 {
                    if let Proposal::InspectSource { sources } = &b.proposals[0] {
                        Some(sources.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
            let mut retrieved = Vec::new();
            let mut error = None;
            let mut retrieved_bytes = 0_u64;
            if let Some(sources) = inspection {
                inspection_exchanges += 1;
                let result = (|| -> Result<_> {
                    ensure!(
                        inspection_exchanges <= policy.max_inspections
                            && u128::from(prior_inspection_ms) + started.elapsed().as_millis()
                                <= u128::from(limits.inspection_timeout_ms),
                        "inspection budget exhausted"
                    );
                    let remaining = policy
                        .max_inspections
                        .saturating_sub(inspected.len() as u32);
                    let refs = ato_formation::proposal::validate_inspection_sources(
                        auth, &sources, remaining, 4,
                    )?;
                    ensure!(
                        refs.iter().all(|r| !inspected.contains_key(&r.file_id)
                            && (!acquired.contains_key(&r.file_id)
                                || input.projection.iter().any(|e| e.logical_id == r.file_id))),
                        "no new inspection"
                    );
                    for reference in &refs {
                        let text = local
                            .inspection_context
                            .get(&reference.file_id)
                            .context("verified source is not readable bounded text")?;
                        retrieved_bytes = retrieved_bytes
                            .checked_add(text.text.len() as u64)
                            .context("inspection byte overflow")?;
                    }
                    ensure!(
                        inspection_bytes + retrieved_bytes <= limits.inspection_source_bytes,
                        "inspection source byte budget exhausted"
                    );
                    Ok(refs)
                })();
                match result {
                    Ok(refs) => {
                        inspection_bytes += retrieved_bytes;
                        for r in &refs {
                            acquired.insert(
                                r.file_id.clone(),
                                local.inspection_context[&r.file_id].clone(),
                            );
                            inspected.insert(r.file_id.clone(), r.clone());
                        }
                        round_inspections.extend(refs.clone());
                        retrieved = refs;
                    }
                    Err(e) => {
                        retrieved_bytes = 0;
                        error = Some(if e.to_string().starts_with("source_") {
                            e.to_string()
                        } else {
                            "inspection_budget_or_progress".into()
                        });
                        feedback = vec![error.clone().unwrap_or_default()];
                    }
                }
            }
            let record = StepRecord {
                schema: "ato.formation-reasoning-step/1".into(),
                configuration_ref: self.configuration_ref.clone(),
                input_sha256,
                output_sha256: digest(&raw),
                raw_output_base64: BASE64.encode(&raw),
                provider_call: call.clone(),
                failure: None,
                elapsed_ms: call_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                inspected: retrieved.clone(),
                inspection_error: error.clone(),
                inspected_bytes: retrieved_bytes,
            };
            save(&record_path, &serde_jcs::to_vec(&record)?)?;
            if let Some(call) = call {
                round_calls.push(call);
            }
            if !retrieved.is_empty() || error.is_some() {
                step += 1;
                continue;
            }
            return self.finish_round(raw, round_inspections, round_calls);
        }
    }
}
