//! CandidateProducer proposals are untrusted authoring input, never decisions or
//! verification evidence. Ato alone resolves logical IDs and compiles canonical D.
//! This core has no provider transport, durable storage or execution authority.
mod execution_plan;
mod node_static_workspace;
mod python_http;
mod source_context;
pub use execution_plan::{
    CatalogSource, ExecutionPlanProposal, PlanAuthorization, PlanStateRequirement,
    RuntimeSelection, SourceReference, VerifiedSourceFile, isolated_state_id, isolated_state_mount,
    source_file_allowed, source_inspection_priority,
};
pub use node_static_workspace::{
    MAX_WORKSPACE_CANDIDATES, NodeStaticWorkspaceAuthorization, WORKSPACE_HTTP_PORT,
    WorkspaceInstallScope, WorkspaceStaticBuild, relative_dir,
};
pub use python_http::PythonHttpProcess;
pub use source_context::*;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    generation::{self, CompiledGeneration, GenerationPolicy, PythonInvocation},
    search::{FrozenSearchV1, SearchCandidate},
};

pub const PROPOSAL_SCHEMA: &str = "ato.formation-proposal/1";
pub const CATALOG_SCHEMA: &str = "ato.formation-operation-catalog/1";
pub const MAX_PROPOSALS: usize = 4;
pub const MAX_BATCH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProposalError(pub &'static str);

/// One bounded round. Source text is a separate, explicit observation opt-in;
/// it never expands the operation catalog or compiler/Verifier authority.
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
            || (!self.allow_source_text && self.max_source_bytes != 0)
            || (self.allow_source_text && !(1..=MAX_SOURCE_BYTES).contains(&self.max_source_bytes))
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
    pub modifiable_derivation_refs: Vec<String>,
    pub source_domain: SourceDomain,
    /// Ato-owned constructor parameters, not LLM arguments.
    pub python_http_process: Option<PythonHttpProcess>,
    /// Private workspace-ID resolution from the bounded inventory. Absent on
    /// every earlier authorization, whose canonical bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_static_workspace: Option<NodeStaticWorkspaceAuthorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_plan: Option<PlanAuthorization>,
    pub policy: CandidateProducerPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDomain {
    pub entrypoints: BTreeMap<String, String>,
    pub modules: BTreeMap<String, String>,
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
    #[serde(rename = "execution_plan@1")]
    ExecutionPlan {
        toolchains: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_oci: Option<crate::source_oci_plan::SourceOciRecipe>,
        sources: Vec<CatalogSource>,
    },
    #[serde(rename = "python_http_process@1")]
    PythonHttpProcess { entrypoint_ids: Vec<String> },
    #[serde(rename = "python_script@1")]
    PythonScript { entrypoint_ids: Vec<String> },
    #[serde(rename = "python_module@1")]
    PythonModule { module_ids: Vec<String> },
    #[serde(rename = "node_static_workspace@1")]
    NodeStaticWorkspace { workspace_ids: Vec<String> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum OperationInvocation {
    #[serde(rename = "execution_plan@1")]
    ExecutionPlan { plan: Box<ExecutionPlanProposal> },
    #[serde(rename = "python_http_process@1")]
    PythonHttpProcess { entrypoint_id: String },
    #[serde(rename = "python_script@1")]
    PythonScript { entrypoint_id: String },
    #[serde(rename = "python_module@1")]
    PythonModule { module_id: String },
    #[serde(rename = "node_static_workspace@1")]
    NodeStaticWorkspace { workspace_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Proposal {
    InspectSource {
        sources: Vec<SourceReference>,
    },
    ProposeDerivation {
        operations: Vec<OperationInvocation>,
    },
    ModifyDerivation {
        base_derivation_ref: String,
        operations: Vec<OperationInvocation>,
    },
    Unsupported {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
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
    fn compiler_policy(&self, base_ref: &str, id: &str, path: &str) -> GenerationPolicy {
        GenerationPolicy {
            schema: generation::GENERATION_POLICY_SCHEMA.into(),
            base_derivation_ref: base_ref.into(),
            entrypoints: BTreeMap::from([(id.into(), path.into())]),
            max_generations: 1,
            timeout_ms: self.policy.timeout_ms,
        }
    }

    pub fn validate(&self) -> Result<(), ProposalError> {
        self.policy.validate()?;
        let domain = &self.source_domain;
        if (domain.entrypoints.len() + domain.modules.len() == 0
            && self.node_static_workspace.is_none()
            && self.execution_plan.is_none())
            || domain.entrypoints.len() + domain.modules.len() > generation::MAX_ENTRYPOINTS
            || self.modifiable_derivation_refs.len() > 64
            || self
                .modifiable_derivation_refs
                .iter()
                .any(|r| !generation::is_sha256(r))
            || self
                .modifiable_derivation_refs
                .windows(2)
                .any(|w| w[0] >= w[1])
        {
            return Err(ProposalError("proposal_domain_bounds"));
        }
        for (id, path) in &domain.entrypoints {
            if !generation::logical_id(id) || !generation::entry_path(path) {
                return Err(ProposalError("proposal_domain_invalid"));
            }
        }
        for (id, module) in &domain.modules {
            if !generation::logical_id(id)
                || module.len() > 128
                || !module.split('.').all(|part| {
                    !part.is_empty()
                        && part.as_bytes()[0].is_ascii_alphabetic()
                        && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
            {
                return Err(ProposalError("proposal_module_invalid"));
            }
        }
        if let Some(template) = &self.python_http_process {
            template.validate()?;
        }
        if let Some(workspace) = &self.node_static_workspace {
            workspace.validate()?;
        }
        if let Some(plan) = &self.execution_plan {
            plan.validate()?;
        }
        Ok(())
    }

    /// Modify operations exist only when an explicitly authorized known base
    /// exists. Base-free construction has a different operation name.
    pub fn catalog(&self) -> Result<OperationCatalog, ProposalError> {
        self.validate()?;
        let mut operations = Vec::new();
        if let Some(plan) = &self.execution_plan {
            operations.push(OperationDomain::ExecutionPlan {
                toolchains: plan.toolchains.clone(),
                source_oci: plan.source_oci.clone(),
                sources: plan.catalog_sources(),
            });
        }
        let domain = &self.source_domain;
        if self.python_http_process.is_some() && !domain.entrypoints.is_empty() {
            operations.push(OperationDomain::PythonHttpProcess {
                entrypoint_ids: domain.entrypoints.keys().cloned().collect(),
            });
        }
        if !self.modifiable_derivation_refs.is_empty() {
            if !domain.entrypoints.is_empty() {
                operations.push(OperationDomain::PythonScript {
                    entrypoint_ids: domain.entrypoints.keys().cloned().collect(),
                });
            }
            if !domain.modules.is_empty() {
                operations.push(OperationDomain::PythonModule {
                    module_ids: domain.modules.keys().cloned().collect(),
                });
            }
        }
        if let Some(workspace) = &self.node_static_workspace {
            operations.push(OperationDomain::NodeStaticWorkspace {
                workspace_ids: workspace.workspaces.keys().cloned().collect(),
            });
        }
        Ok(OperationCatalog {
            schema: CATALOG_SCHEMA.into(),
            operations,
        })
    }

    pub fn validate_search(&self, frozen: &FrozenSearchV1) -> Result<(), ProposalError> {
        self.validate()?;
        if self.execution_plan.is_some() && frozen.policy.exploration.is_none() {
            return Err(ProposalError("exploration_policy_required"));
        }
        let source = frozen
            .initial_source
            .as_ref()
            .ok_or(ProposalError("proposal_initial_source_required"))?;
        if !generation::is_sha256(&source.closure_ref)
            || !generation::is_sha256(&source.archive_digest)
            || frozen.policy.generation.is_some()
            || !frozen.policy.bindings.is_empty()
            || frozen.policy.network != "denied"
            || frozen
                .candidates
                .iter()
                .any(|c| !crate::search::safe(&c.effects))
        {
            return Err(ProposalError("proposal_search_scope"));
        }
        for reference in &self.modifiable_derivation_refs {
            if !frozen.candidates.iter().any(|c| {
                c.derivation_ref == *reference
                    && c.effects == "pure"
                    && c.materialization == source.materialization()
            }) {
                return Err(ProposalError("proposal_base_unauthorized"));
            }
        }
        if let Some(template) = &self.python_http_process {
            // Validate complete K/port/input coupling before a provider call.
            template.validate_contract(&frozen.base_contract, source)?;
        }
        if let Some(workspace) = &self.node_static_workspace {
            workspace.validate_contract(&frozen.base_contract, source)?;
        }
        Ok(())
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
    InspectionRequested(Vec<SourceReference>),
}

/// Ato-owned candidate set. Frozen candidates are borrowed, never rewritten.
/// This is a pure batch registry, not a replacement durable database. Search
/// integration uses the same iterator through SearchState::candidates.
pub struct CandidateRegistry<'a> {
    frozen: &'a FrozenSearchV1,
    generated: Vec<ValidatedCandidate>,
    inspection_remaining: u32,
}
impl<'a> CandidateRegistry<'a> {
    pub fn new(frozen: &'a FrozenSearchV1) -> Result<Self, ProposalError> {
        frozen
            .canonical_bytes()
            .map_err(|_| ProposalError("proposal_frozen_invalid"))?;
        Ok(Self {
            frozen,
            generated: Vec::new(),
            inspection_remaining: frozen
                .policy
                .exploration
                .as_ref()
                .map_or(0, |p| p.max_inspections),
        })
    }
    pub fn with_inspection_budget(mut self, remaining: u32) -> Result<Self, ProposalError> {
        if remaining > self.inspection_remaining {
            return Err(ProposalError("inspection_budget_invalid"));
        }
        self.inspection_remaining = remaining;
        Ok(self)
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

    /// Only Ato parses producer bytes. `base_recipes` is a private lookup keyed
    /// by explicit authorized D; Propose never reads it. Source-domain files
    /// must be checked against immutable I by the future requester integration.
    pub fn validate_batch(
        &mut self,
        base_recipes: &BTreeMap<String, String>,
        output: &ProducerOutput,
    ) -> Result<Vec<ProposalOutcome>, ProposalError> {
        let authorization = self
            .frozen
            .policy
            .proposal
            .as_ref()
            .ok_or(ProposalError("proposal_without_policy"))?;
        authorization.validate_search(self.frozen)?;
        let bytes = output.raw();
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
        let mut outcomes = Vec::with_capacity(batch.proposals.len());
        for raw in batch.proposals {
            let parsed = serde_json::from_str::<Proposal>(raw.get())
                .map_err(|_| ProposalError("proposal_schema"));
            if let Ok(Proposal::InspectSource { sources }) = &parsed {
                let validated = (|| {
                    if sources.is_empty() || sources.len() > 4 {
                        return Err(ProposalError("source_inspection_bounds"));
                    }
                    let domain = authorization
                        .execution_plan
                        .as_ref()
                        .ok_or(ProposalError("source_inspection_unauthorized"))?;
                    if !authorization.policy.allow_source_text {
                        return Err(ProposalError("source_text_disabled"));
                    }
                    let mut ids = BTreeSet::new();
                    for source in sources {
                        if !ids.insert(&source.file_id)
                            || domain
                                .files
                                .get(&source.file_id)
                                .is_none_or(|f| f.digest != source.digest)
                        {
                            return Err(ProposalError("source_inspection_unauthorized"));
                        }
                    }
                    if sources.len() > self.inspection_remaining as usize {
                        return Err(ProposalError("inspection_budget_exhausted"));
                    }
                    self.inspection_remaining -= sources.len() as u32;
                    Ok(sources.clone())
                })();
                outcomes.push(match validated {
                    Ok(sources) => ProposalOutcome::InspectionRequested(sources),
                    Err(error) => ProposalOutcome::Rejected(error),
                });
                continue;
            }
            let result =
                parsed.and_then(|proposal| compile_proposal(self.frozen, base_recipes, &proposal));
            let outcome = match result {
                Ok(None) => ProposalOutcome::Unsupported,
                Err(error) => ProposalOutcome::Rejected(error),
                Ok(Some((proposal_id, compiled, candidate))) => {
                    if self
                        .candidates()
                        .any(|c| c.derivation_ref == compiled.derivation_ref)
                    {
                        ProposalOutcome::Rejected(ProposalError("proposal_duplicate"))
                    } else if self.generated.len() >= authorization.policy.max_proposals {
                        ProposalOutcome::Rejected(ProposalError("proposal_registry_full"))
                    } else {
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
    frozen: &FrozenSearchV1,
    base_recipes: &BTreeMap<String, String>,
    proposal: &Proposal,
) -> Result<Option<(String, CompiledGeneration, SearchCandidate)>, ProposalError> {
    let authorization = frozen
        .policy
        .proposal
        .as_ref()
        .ok_or(ProposalError("proposal_without_policy"))?;
    let source = frozen
        .initial_source
        .as_ref()
        .ok_or(ProposalError("proposal_initial_source_required"))?;
    let (compiled, mut candidate) = match proposal {
        Proposal::InspectSource { .. } => {
            return Err(ProposalError("source_inspection_requires_registry"));
        }
        Proposal::Unsupported { reason } => {
            if reason.as_deref().is_some_and(|reason| {
                !matches!(
                    reason,
                    "unsupported_toolchain"
                        | "unsupported_dependency_operation"
                        | "unsupported_entrypoint"
                        | "unsupported_build_operation"
                        | "requires_external_service"
                        | "requires_binding"
                        | "source_oci_builder_unavailable"
                        | "insufficient_source"
                        | "unknown"
                )
            }) {
                return Err(ProposalError("unsupported_reason_invalid"));
            }
            return Ok(None);
        }
        Proposal::ProposeDerivation { operations } => match operations.as_slice() {
            [OperationInvocation::ExecutionPlan { plan }] => plan.compile(
                frozen,
                authorization
                    .execution_plan
                    .as_ref()
                    .ok_or(ProposalError("proposal_operation_unauthorized"))?,
            )?,
            [OperationInvocation::PythonHttpProcess { entrypoint_id }] => {
                let template = authorization
                    .python_http_process
                    .as_ref()
                    .ok_or(ProposalError("proposal_operation_unauthorized"))?;
                let path = authorization
                    .source_domain
                    .entrypoints
                    .get(entrypoint_id)
                    .ok_or(ProposalError("proposal_id_unauthorized"))?;
                let compiled = template.compile(source, &frozen.base_contract, path)?;
                let candidate = template.candidate(source, compiled.derivation_ref.clone());
                (compiled, candidate)
            }
            [OperationInvocation::NodeStaticWorkspace { workspace_id }] => {
                let domain = authorization
                    .node_static_workspace
                    .as_ref()
                    .ok_or(ProposalError("proposal_operation_unauthorized"))?;
                let workspace = domain
                    .workspaces
                    .get(workspace_id)
                    .ok_or(ProposalError("proposal_id_unauthorized"))?;
                let compiled = domain.compile(source, &frozen.base_contract, workspace)?;
                let candidate = domain.candidate(source, compiled.derivation_ref.clone());
                (compiled, candidate)
            }
            _ => return Err(ProposalError("proposal_propose_operation")),
        },
        Proposal::ModifyDerivation {
            base_derivation_ref,
            operations,
        } => {
            if !authorization
                .modifiable_derivation_refs
                .contains(base_derivation_ref)
            {
                return Err(ProposalError("proposal_base_unauthorized"));
            }
            let base = frozen
                .candidates
                .iter()
                .find(|c| c.derivation_ref == *base_derivation_ref)
                .ok_or(ProposalError("proposal_base_unauthorized"))?;
            let base_toml = base_recipes
                .get(base_derivation_ref)
                .ok_or(ProposalError("proposal_base_recipe_missing"))?;
            let [operation] = operations.as_slice() else {
                return Err(ProposalError("proposal_operation_count"));
            };
            let (policy, invocation) = match operation {
                OperationInvocation::PythonScript { entrypoint_id } => {
                    let path = authorization
                        .source_domain
                        .entrypoints
                        .get(entrypoint_id)
                        .ok_or(ProposalError("proposal_id_unauthorized"))?;
                    (
                        authorization.compiler_policy(base_derivation_ref, entrypoint_id, path),
                        PythonInvocation::Script(path),
                    )
                }
                OperationInvocation::PythonModule { module_id } => {
                    let module = authorization
                        .source_domain
                        .modules
                        .get(module_id)
                        .ok_or(ProposalError("proposal_id_unauthorized"))?;
                    (
                        authorization.compiler_policy(
                            base_derivation_ref,
                            module_id,
                            &format!("{}.py", module.replace('.', "/")),
                        ),
                        PythonInvocation::Module(module),
                    )
                }
                OperationInvocation::PythonHttpProcess { .. }
                | OperationInvocation::ExecutionPlan { .. }
                | OperationInvocation::NodeStaticWorkspace { .. } => {
                    return Err(ProposalError("proposal_modify_operation"));
                }
            };
            let compiled = generation::compile_invocation(
                &policy,
                base_toml,
                &source.closure_ref,
                &frozen.base_contract_ref,
                invocation,
            )
            .map_err(|e| {
                ProposalError(if e.code() == "generation_duplicate" {
                    "proposal_duplicate"
                } else {
                    e.code()
                })
            })?;
            (compiled, base.clone())
        }
    };
    candidate.derivation_ref = compiled.derivation_ref.clone();
    // Content key ONLY within the frozen search domain (opaque IDs are local).
    // It is not a global semantic Ref; persistence must pair it with search_id.
    let bytes = serde_jcs::to_vec(&(PROPOSAL_SCHEMA, proposal))
        .map_err(|_| ProposalError("proposal_canonicalization"))?;
    let proposal_id = format!("proposal-content:sha256:{:x}", Sha256::digest(bytes));
    Ok(Some((proposal_id, compiled, candidate)))
}

/// Structural read-model check, not a substitute for receiver recompilation.
pub fn validate_candidate_scope(
    frozen: &FrozenSearchV1,
    generated: &[SearchCandidate],
) -> Result<(), ProposalError> {
    let authorization = frozen
        .policy
        .proposal
        .as_ref()
        .ok_or(ProposalError("proposal_without_policy"))?;
    let source = frozen
        .initial_source
        .as_ref()
        .ok_or(ProposalError("proposal_initial_source_required"))?;
    let mut refs: BTreeSet<_> = frozen
        .candidates
        .iter()
        .map(|c| &c.derivation_ref)
        .collect();
    if generated.len() > authorization.policy.max_proposals {
        return Err(ProposalError("proposal_registry_full"));
    }
    for candidate in generated {
        let new_scope =
            authorization.execution_plan.as_ref().is_some_and(|t| {
                let ceiling = t.candidate(source, candidate.derivation_ref.clone());
                let mut compared = candidate.clone();
                compared.provisions = ceiling.provisions.clone();
                compared == ceiling
                    && (!candidate.provisions.is_empty() || t.source_oci.is_some())
                    && candidate
                        .provisions
                        .iter()
                        .all(|p| ceiling.provisions.contains(p))
                    && candidate.provisions.windows(2).all(|p| p[0] < p[1])
            }) || authorization.python_http_process.as_ref().is_some_and(|t| {
                t.candidate(source, candidate.derivation_ref.clone()) == *candidate
            }) || authorization
                .node_static_workspace
                .as_ref()
                .is_some_and(|t| {
                    t.candidate(source, candidate.derivation_ref.clone()) == *candidate
                });
        let modified_scope = frozen.candidates.iter().any(|base| {
            authorization
                .modifiable_derivation_refs
                .contains(&base.derivation_ref)
                && candidate.effects == base.effects
                && candidate.materialization == base.materialization
                && candidate.requirements == base.requirements
                && candidate.provisions == base.provisions
        });
        if !generation::is_sha256(&candidate.derivation_ref)
            || !refs.insert(&candidate.derivation_ref)
            || (!new_scope && !modified_scope)
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derivations: Vec<crate::authoring::BoundDerivation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inspection_requests: Vec<SourceReference>,
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
    pub failure_evidence: Vec<crate::generation_context::FailureSummary>,
    pub inspection_evidence: Vec<crate::generation_context::InspectionSummary>,
    pub operation_catalog: OperationCatalog,
    pub remaining_budget: ProposalBudget,
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
pub trait CandidateProducer<
    Request = ProposalRequest,
    Output = ProducerOutput,
    Error = ProducerError,
>
{
    fn propose(&self, request: &Request) -> Result<Output, Error>;
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
    pub output: ProducerOutput,
}
impl CandidateProducer for FixedCandidateProducer {
    fn propose(&self, _request: &ProposalRequest) -> Result<ProducerOutput, ProducerError> {
        Ok(self.output.clone())
    }
}

/// Opaque untrusted provider response. Bounded before JSON parsing; provenance
/// is operational metadata and never participates in proposal/D identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerOutput {
    raw: Vec<u8>,
    provenance: ProducerProvenance,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerProvenance {
    pub provider: String,
    pub model: Option<String>,
}
impl ProducerOutput {
    pub fn new(raw: Vec<u8>, provenance: ProducerProvenance) -> Result<Self, ProposalError> {
        if raw.len() > MAX_BATCH_BYTES
            || provenance.provider.is_empty()
            || provenance.provider.len() > 64
            || provenance.model.as_ref().is_some_and(|m| m.len() > 128)
        {
            return Err(ProposalError("proposal_output_bounds"));
        }
        Ok(Self { raw, provenance })
    }
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
    pub fn provenance(&self) -> &ProducerProvenance {
        &self.provenance
    }
}
