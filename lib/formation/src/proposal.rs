//! CandidateProducer proposals are untrusted authoring input, never decisions or
//! verification evidence. Ato alone resolves logical IDs and compiles canonical D.
//! This core has no provider transport, durable storage or execution authority.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    generation::{self, CompiledGeneration, GenerationPolicy, PythonInvocation},
    search::{CandidateInput, FrozenSearchV1, SearchCandidate},
};

pub const PROPOSAL_SCHEMA: &str = "ato.formation-proposal/1";
pub const CATALOG_SCHEMA: &str = "ato.formation-operation-catalog/1";
pub const MAX_PROPOSALS: usize = 4;
pub const MAX_BATCH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProposalError(pub &'static str);

/// v0 has one bounded round. External source text is disabled until the source
/// policy/transport adapter is implemented; having a field is not an opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProducerPolicy {
    pub max_proposal_rounds: u32,
    pub max_proposals: usize,
    pub timeout_ms: u64,
    pub allow_source_text: bool,
    pub max_source_bytes: usize,
}
impl CandidateProducerPolicy {
    pub fn validate(&self) -> Result<(), ProposalError> {
        if self.max_proposal_rounds != 1
            || !(1..=MAX_PROPOSALS).contains(&self.max_proposals)
            || !(1..=30_000).contains(&self.timeout_ms)
            || self.allow_source_text
            || self.max_source_bytes != 0
        {
            return Err(ProposalError("proposal_policy_bounds"));
        }
        Ok(())
    }
}

/// Private owner-issued ID resolution; never serialize this to a provider.
/// Callers must verify these files in the immutable source inventory before
/// freezing this authorization. A module names a .py file, not host packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalAuthorization {
    pub base_derivation_ref: String,
    pub entrypoints: BTreeMap<String, String>,
    pub modules: BTreeMap<String, String>,
    pub policy: CandidateProducerPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationCatalog {
    pub schema: String,
    pub operations: Vec<OperationDomain>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum OperationDomain {
    #[serde(rename = "python_script@1")]
    PythonScript { entrypoint_ids: Vec<String> },
    #[serde(rename = "python_module@1")]
    PythonModule { module_ids: Vec<String> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum OperationInvocation {
    #[serde(rename = "python_script@1")]
    PythonScript { entrypoint_id: String },
    #[serde(rename = "python_module@1")]
    PythonModule { module_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Proposal {
    ProposeDerivation {
        operations: Vec<OperationInvocation>,
    },
    ModifyDerivation {
        base_derivation_ref: String,
        operations: Vec<OperationInvocation>,
    },
    Unsupported {},
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalBatch {
    pub schema: String,
    pub proposals: Vec<Proposal>,
}

/// Parse the batch envelope once, then each proposal separately. One malformed
/// member must not discard other valid members. Preserve duplicate JSON field
/// rejection by deserializing each member from its original raw bytes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBatch<'a> {
    schema: String,
    #[serde(borrow)]
    proposals: Vec<&'a serde_json::value::RawValue>,
}

impl ProposalAuthorization {
    fn compiler_policy(&self, id: &str, path: &str) -> GenerationPolicy {
        GenerationPolicy {
            schema: generation::GENERATION_POLICY_SCHEMA.into(),
            base_derivation_ref: self.base_derivation_ref.clone(),
            entrypoints: BTreeMap::from([(id.into(), path.into())]),
            max_generations: 1,
            timeout_ms: self.policy.timeout_ms,
        }
    }

    pub fn validate(&self) -> Result<(), ProposalError> {
        self.policy.validate()?;
        if self.entrypoints.len() + self.modules.len() == 0
            || self.entrypoints.len() + self.modules.len() > generation::MAX_ENTRYPOINTS
        {
            return Err(ProposalError("proposal_domain_bounds"));
        }
        for (id, path) in &self.entrypoints {
            self.compiler_policy(id, path)
                .validate()
                .map_err(|_| ProposalError("proposal_domain_invalid"))?;
        }
        for (id, module) in &self.modules {
            if !module.split('.').all(|part| {
                !part.is_empty()
                    && part.as_bytes()[0].is_ascii_alphabetic()
                    && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }) || module.len() > 128
            {
                return Err(ProposalError("proposal_module_invalid"));
            }
            self.compiler_policy(id, &format!("{}.py", module.replace('.', "/")))
                .validate()
                .map_err(|_| ProposalError("proposal_domain_invalid"))?;
        }
        Ok(())
    }

    pub fn catalog(&self) -> Result<OperationCatalog, ProposalError> {
        self.validate()?;
        let mut operations = Vec::new();
        if !self.entrypoints.is_empty() {
            operations.push(OperationDomain::PythonScript {
                entrypoint_ids: self.entrypoints.keys().cloned().collect(),
            });
        }
        if !self.modules.is_empty() {
            operations.push(OperationDomain::PythonModule {
                module_ids: self.modules.keys().cloned().collect(),
            });
        }
        Ok(OperationCatalog {
            schema: CATALOG_SCHEMA.into(),
            operations,
        })
    }
}

/// Compiled but not executed or verified. No public constructor bypasses the
/// validator; the receiver will persist canonical recipe plus proposal provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCandidate {
    proposal_id: String,
    compiled: CompiledGeneration,
    candidate: SearchCandidate,
}
impl ValidatedCandidate {
    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }
    pub fn compiled(&self) -> &CompiledGeneration {
        &self.compiled
    }
    pub fn candidate(&self) -> &SearchCandidate {
        &self.candidate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalOutcome {
    Admitted(Box<ValidatedCandidate>),
    Unsupported,
    Rejected(ProposalError),
}

/// Ato-owned candidate set. Frozen candidates are borrowed, never rewritten.
/// This is a pure batch registry, not a replacement durable database. Search
/// integration uses the same iterator through SearchState::candidates.
pub struct CandidateRegistry<'a> {
    frozen: &'a FrozenSearchV1,
    generated: Vec<ValidatedCandidate>,
}
impl<'a> CandidateRegistry<'a> {
    pub fn new(frozen: &'a FrozenSearchV1) -> Result<Self, ProposalError> {
        frozen
            .canonical_bytes()
            .map_err(|_| ProposalError("proposal_frozen_invalid"))?;
        Ok(Self {
            frozen,
            generated: Vec::new(),
        })
    }
    pub fn candidates(&self) -> impl Iterator<Item = &SearchCandidate> {
        self.frozen
            .candidates
            .iter()
            .chain(self.generated.iter().map(|c| &c.candidate))
    }
    pub fn generated(&self) -> &[ValidatedCandidate] {
        &self.generated
    }

    /// Source presence must have been checked against the verified closure when
    /// the owner froze authorization. `base_toml` is private trusted input, not
    /// provider output. Hash/Contract/permission checks are repeated here.
    pub fn validate_batch(
        &mut self,
        authorization: &ProposalAuthorization,
        base_toml: &str,
        bytes: &[u8],
    ) -> Result<Vec<ProposalOutcome>, ProposalError> {
        authorization.validate()?;
        if self.frozen.policy.proposal.as_ref() != Some(authorization) {
            return Err(ProposalError("proposal_authorization_mismatch"));
        }
        if self.frozen.policy.generation.is_some()
            || !self.frozen.policy.bindings.is_empty()
            || self.frozen.policy.network != "denied"
        {
            return Err(ProposalError("proposal_search_scope"));
        }
        if bytes.len() > MAX_BATCH_BYTES {
            return Err(ProposalError("proposal_batch_bounds"));
        }
        let batch: RawBatch<'_> =
            serde_json::from_slice(bytes).map_err(|_| ProposalError("proposal_batch_schema"))?;
        if batch.schema != PROPOSAL_SCHEMA
            || batch.proposals.len() > authorization.policy.max_proposals
        {
            return Err(ProposalError("proposal_batch_bounds"));
        }
        let base = self
            .frozen
            .candidates
            .iter()
            .find(|c| c.derivation_ref == authorization.base_derivation_ref)
            .ok_or(ProposalError("proposal_base_unauthorized"))?;
        let CandidateInput::Source { closure_ref, .. } = &base.materialization else {
            return Err(ProposalError("proposal_base_unsupported"));
        };
        if base.effects != "pure" {
            return Err(ProposalError("proposal_effect_unauthorized"));
        }
        let mut outcomes = Vec::with_capacity(batch.proposals.len());
        for raw in batch.proposals {
            let result = serde_json::from_str::<Proposal>(raw.get())
                .map_err(|_| ProposalError("proposal_schema"))
                .and_then(|proposal| {
                    compile_proposal(
                        authorization,
                        base_toml,
                        closure_ref,
                        &self.frozen.base_contract_ref,
                        &proposal,
                    )
                });
            let outcome = match result {
                Ok(None) => ProposalOutcome::Unsupported,
                Err(error) => ProposalOutcome::Rejected(error),
                Ok(Some((proposal_id, compiled))) => {
                    if self
                        .candidates()
                        .any(|c| c.derivation_ref == compiled.derivation_ref)
                    {
                        ProposalOutcome::Rejected(ProposalError("proposal_duplicate"))
                    } else if self.generated.len() >= authorization.policy.max_proposals {
                        ProposalOutcome::Rejected(ProposalError("proposal_registry_full"))
                    } else {
                        let mut candidate = base.clone();
                        candidate.derivation_ref = compiled.derivation_ref.clone();
                        let validated = ValidatedCandidate {
                            proposal_id,
                            compiled,
                            candidate,
                        };
                        self.generated.push(validated.clone());
                        ProposalOutcome::Admitted(Box::new(validated))
                    }
                }
            };
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
}

fn compile_proposal(
    authorization: &ProposalAuthorization,
    base_toml: &str,
    closure_ref: &str,
    contract_ref: &str,
    proposal: &Proposal,
) -> Result<Option<(String, CompiledGeneration)>, ProposalError> {
    let operations = match proposal {
        Proposal::Unsupported {} => return Ok(None),
        Proposal::ProposeDerivation { operations } => operations,
        Proposal::ModifyDerivation {
            base_derivation_ref,
            operations,
        } => {
            if *base_derivation_ref != authorization.base_derivation_ref {
                return Err(ProposalError("proposal_base_unauthorized"));
            }
            operations
        }
    };
    // v0 permits one serving operation, not arbitrary sequencing or extra effects.
    let [operation] = operations.as_slice() else {
        return Err(ProposalError("proposal_operation_count"));
    };
    let (policy, invocation) = match operation {
        OperationInvocation::PythonScript { entrypoint_id } => {
            let path = authorization
                .entrypoints
                .get(entrypoint_id)
                .ok_or(ProposalError("proposal_id_unauthorized"))?;
            (
                authorization.compiler_policy(entrypoint_id, path),
                PythonInvocation::Script(path),
            )
        }
        OperationInvocation::PythonModule { module_id } => {
            let module = authorization
                .modules
                .get(module_id)
                .ok_or(ProposalError("proposal_id_unauthorized"))?;
            (
                authorization
                    .compiler_policy(module_id, &format!("{}.py", module.replace('.', "/"))),
                PythonInvocation::Module(module),
            )
        }
    };
    let compiled =
        generation::compile_invocation(&policy, base_toml, closure_ref, contract_ref, invocation)
            .map_err(|e| {
            ProposalError(if e.code() == "generation_duplicate" {
                "proposal_duplicate"
            } else {
                e.code()
            })
        })?;
    let bytes = serde_jcs::to_vec(&(PROPOSAL_SCHEMA, proposal))
        .map_err(|_| ProposalError("proposal_canonicalization"))?;
    let proposal_id = format!("sha256:{:x}", Sha256::digest(bytes));
    Ok(Some((proposal_id, compiled)))
}

/// Read-model validation for persisted candidates. The durable receiver must
/// separately recompile the stored proposal and compare its canonical D/recipe.
pub fn validate_candidate_scope(
    known: &[SearchCandidate],
    base_ref: &str,
    generated: &[SearchCandidate],
) -> Result<(), ProposalError> {
    let base = known
        .iter()
        .find(|c| c.derivation_ref == base_ref)
        .ok_or(ProposalError("proposal_base_unauthorized"))?;
    let mut refs: BTreeSet<_> = known.iter().map(|c| &c.derivation_ref).collect();
    if generated.len() > MAX_PROPOSALS {
        return Err(ProposalError("proposal_registry_full"));
    }
    for candidate in generated {
        if !generation::is_sha256(&candidate.derivation_ref)
            || !refs.insert(&candidate.derivation_ref)
            || candidate.effects != base.effects
            || candidate.materialization != base.materialization
            || candidate.requirements != base.requirements
            || candidate.provisions != base.provisions
        {
            return Err(ProposalError("proposal_candidate_scope"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRoundOutcome {
    Completed,
    ProviderError,
    Timeout,
}

/// Durable read model only; claim/CAS/completion transitions belong to the API
/// receiver. A completed round with no candidates is not a failed K observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRoundRecord {
    pub opened_at_ms: u64,
    pub expires_at_ms: u64,
    pub outcome: Option<ProposalRoundOutcome>,
    pub candidates: Vec<SearchCandidate>,
}

pub const PROPOSAL_REQUEST_SCHEMA: &str = "ato.formation-proposal-request/1";

/// Provider projection, not the durable search row. No private ID resolutions,
/// raw receipt/log, host identity, Binding value or credential belongs here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRequest {
    pub schema: String,
    pub search_id: String,
    pub frozen_contract: crate::authoring::BoundContract,
    pub runtime_constraint: crate::search::RuntimeConstraint,
    pub known_derivations: Vec<String>,
    pub source_context: Vec<SourceExcerpt>,
    pub failure_evidence: Vec<crate::generation_context::FailureSummary>,
    pub inspection_evidence: Vec<crate::generation_context::InspectionSummary>,
    pub operation_catalog: OperationCatalog,
    pub remaining_budget: ProposalBudget,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceExcerpt {
    pub source_id: String,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalBudget {
    pub rounds_remaining: u32,
    pub max_proposals: usize,
    pub timeout_ms: u64,
    pub attempts_remaining: u64,
}

/// Implementations propose only. Caller must claim durably before invocation
/// and enforce timeout; this trait is not authorization to call an external LLM.
pub trait CandidateProducer {
    fn propose(&self, request: &ProposalRequest) -> Result<ProposalBatch, ProducerError>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProducerError {
    #[error("candidate_producer_error")]
    ProviderError,
    #[error("candidate_producer_timeout")]
    Timeout,
}

/// Preregistered fixture producer, with no transport and no K verdict.
pub struct FixedCandidateProducer {
    pub batch: ProposalBatch,
}
impl CandidateProducer for FixedCandidateProducer {
    fn propose(&self, _request: &ProposalRequest) -> Result<ProposalBatch, ProducerError> {
        Ok(self.batch.clone())
    }
}
