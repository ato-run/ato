//! Versioned provider observation DTO. No filesystem, provider or HTTP authority.
//! Callers supply only owner-authorized bytes from the verified frozen archive.
use super::*;

pub const PROPOSAL_REQUEST_V2_SCHEMA: &str = "ato.formation-proposal-request/2";
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
pub const MAX_SOURCE_ENTRY_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceContextKind {
    Entrypoint,
    Module,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceEncoding {
    #[serde(rename = "utf8")]
    Utf8,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceContextEntry {
    pub source_id: String,
    pub kind: SourceContextKind,
    pub logical_id: String,
    pub encoding: SourceEncoding,
    pub truncated: bool,
    /// Digest of the exact transmitted text bytes (after deterministic truncation).
    pub content_sha256: String,
    pub text: String,
}
/// Borrowed, already verified bytes. This is NOT a source membership proof;
/// the requester must establish membership using its frozen archive inventory.
pub struct AuthorizedSourceText<'a> {
    pub kind: SourceContextKind,
    pub logical_id: &'a str,
    pub bytes: &'a [u8],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalRequestV2Schema {
    #[serde(rename = "ato.formation-proposal-request/2")]
    V2,
}
/// Deliberately distinct from request/1: strict deserialization cannot reinterpret
/// a /2 payload (or its source fields) as the source-free legacy wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRequestV2 {
    pub schema: ProposalRequestV2Schema,
    pub search_id: String,
    pub frozen_contract: crate::authoring::BoundContract,
    pub runtime_constraint: crate::search::RuntimeConstraint,
    pub known_derivations: Vec<String>,
    pub failure_evidence: Vec<crate::generation_context::FailureSummary>,
    pub inspection_evidence: Vec<crate::generation_context::InspectionSummary>,
    pub operation_catalog: OperationCatalog,
    pub remaining_budget: ProposalBudget,
    pub source_context: Vec<SourceContextEntry>,
}
fn authorized(auth: &ProposalAuthorization, kind: SourceContextKind, id: &str) -> bool {
    match kind {
        SourceContextKind::Entrypoint => auth.source_domain.entrypoints.contains_key(id),
        SourceContextKind::Module => auth.source_domain.modules.contains_key(id),
    }
}
fn text(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    // UTF-8 encoded binary is not necessarily text. No lossy decoding or base64.
    (!text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
    .then_some(text)
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn build_source_context(
    auth: &ProposalAuthorization,
    sources: &[AuthorizedSourceText<'_>],
) -> Result<Vec<SourceContextEntry>, ProposalError> {
    auth.validate()?;
    if !auth.policy.allow_source_text {
        return Err(ProposalError("source_text_disabled"));
    }
    let mut ordered = BTreeMap::new();
    // Check the complete domain before omitting unavailable text: invalid bytes
    // must not let unauthorized or duplicate source IDs bypass validation.
    for source in sources {
        if !authorized(auth, source.kind, source.logical_id)
            || ordered
                .insert((source.kind, source.logical_id), source.bytes)
                .is_some()
        {
            return Err(ProposalError("source_context_domain"));
        }
    }
    let valid: Vec<_> = ordered
        .into_iter()
        .filter_map(|(key, bytes)| text(bytes).map(|t| (key, t)))
        .collect();
    // Equal deterministic shares ensure one file cannot starve all other files.
    let per_entry = MAX_SOURCE_ENTRY_BYTES.min(auth.policy.max_source_bytes / valid.len().max(1));
    let mut result = Vec::new();
    for ((kind, id), full) in valid {
        let mut end = full.len().min(per_entry);
        while !full.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            continue;
        }
        let text = &full[..end];
        let identity = serde_jcs::to_vec(&(kind, id, digest(full.as_bytes())))
            .map_err(|_| ProposalError("source_context_canonical"))?;
        result.push(SourceContextEntry {
            source_id: format!("s_{:x}", Sha256::digest(identity)),
            kind,
            logical_id: id.to_owned(),
            encoding: SourceEncoding::Utf8,
            truncated: end != full.len(),
            content_sha256: digest(text.as_bytes()),
            text: text.to_owned(),
        });
    }
    Ok(result)
}
impl ProposalRequestV2 {
    pub fn new(
        request: ProposalRequest,
        auth: &ProposalAuthorization,
        sources: &[AuthorizedSourceText<'_>],
    ) -> Result<Self, ProposalError> {
        if request.schema != PROPOSAL_REQUEST_SCHEMA {
            return Err(ProposalError("proposal_request_schema"));
        }
        let value = Self {
            schema: ProposalRequestV2Schema::V2,
            search_id: request.search_id,
            frozen_contract: request.frozen_contract,
            runtime_constraint: request.runtime_constraint,
            known_derivations: request.known_derivations,
            failure_evidence: request.failure_evidence,
            inspection_evidence: request.inspection_evidence,
            operation_catalog: request.operation_catalog,
            remaining_budget: request.remaining_budget,
            source_context: build_source_context(auth, sources)?,
        };
        value.validate(auth)?;
        Ok(value)
    }
    pub fn validate(&self, auth: &ProposalAuthorization) -> Result<(), ProposalError> {
        auth.validate()?;
        if !auth.policy.allow_source_text {
            return Err(ProposalError("source_text_disabled"));
        }
        let mut total = 0usize;
        let mut previous = None;
        let mut ids = BTreeSet::new();
        for entry in &self.source_context {
            let key = (entry.kind, entry.logical_id.as_str());
            let id = entry.source_id.strip_prefix("s_").unwrap_or("");
            if previous.is_some_and(|p| p >= key)
                || !ids.insert(&entry.source_id)
                || !authorized(auth, entry.kind, &entry.logical_id)
                || id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || entry.text.is_empty()
                || entry.text.len() > MAX_SOURCE_ENTRY_BYTES
                || text(entry.text.as_bytes()).is_none()
                || entry.content_sha256 != digest(entry.text.as_bytes())
            {
                return Err(ProposalError("source_context_invalid"));
            }
            total = total
                .checked_add(entry.text.len())
                .ok_or(ProposalError("source_context_bounds"))?;
            if total > auth.policy.max_source_bytes {
                return Err(ProposalError("source_context_bounds"));
            }
            previous = Some(key);
        }
        if self.operation_catalog != auth.catalog()?
            || self.remaining_budget.max_proposals > auth.policy.max_proposals
            || self.remaining_budget.rounds_remaining > 1
            || self.remaining_budget.timeout_ms > auth.policy.timeout_ms
        {
            return Err(ProposalError("proposal_request_domain"));
        }
        Ok(())
    }
    pub fn canonical_bytes(&self, auth: &ProposalAuthorization) -> Result<Vec<u8>, ProposalError> {
        self.validate(auth)?;
        serde_jcs::to_vec(self).map_err(|_| ProposalError("source_context_canonical"))
    }
    pub fn source_context_sha256(
        &self,
        auth: &ProposalAuthorization,
    ) -> Result<String, ProposalError> {
        self.validate(auth)?;
        Ok(digest(
            &serde_jcs::to_vec(&self.source_context)
                .map_err(|_| ProposalError("source_context_canonical"))?,
        ))
    }
}
