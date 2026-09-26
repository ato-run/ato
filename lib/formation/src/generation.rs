//! One bounded owner-authorized Python entrypoint substitution, never model argv.
//!
//! The owner freezes the domain against a verified source tree. This pure
//! compiler resolves only those logical ids; it neither reads that tree nor
//! decides whether the resulting program satisfies K. Runtime admission and
//! verification still apply. Paths and TOML must not be sent to the provider.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::authoring::{
    BindingContext, BoundDerivation, EffectClass, HTTP_PROTOCOL, PROCESS_PROTOCOL, bind,
};
use crate::capsule_toml::parse_capsule_toml;
use crate::search::SearchCandidate;

pub const GENERATION_POLICY_SCHEMA: &str = "ato.formation-generation-policy/1";
pub const GENERATION_DRAFT_SCHEMA: &str = "ato.formation-derivation-draft/1";
pub const MAX_ENTRYPOINTS: usize = 16;
pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const MAX_CAPSULE_TOML_BYTES: usize = 64 * 1024;

pub use compile as compile_generation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationPolicy {
    pub schema: String,
    pub base_derivation_ref: String,
    pub entrypoints: BTreeMap<String, String>,
    pub max_generations: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationOperation {
    PythonScript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationDraft {
    pub schema: String,
    pub operation: GenerationOperation,
    pub entrypoint_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationOutcome {
    Admitted,
    Declined,
    Duplicate,
    Invalid,
    Timeout,
    ProviderError,
}

/// One durable generation point per search. Only `admitted` has a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecord {
    pub opened_at_ms: u64,
    pub expires_at_ms: u64,
    pub outcome: Option<GenerationOutcome>,
    pub candidate: Option<SearchCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompiledGeneration {
    pub capsule_toml: String,
    pub derivation: BoundDerivation,
    pub derivation_ref: String,
    pub base_contract_ref: String,
}

/// Fixed, non-sensitive codes: parser diagnostics can contain source or secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GenerationError(pub &'static str);
impl GenerationError {
    pub fn code(&self) -> &'static str {
        self.0
    }
}

pub(crate) fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn logical_id(value: &str) -> bool {
    !value.is_empty()
        && value != "none"
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A deliberately narrow source-relative path grammar, not URL or shell text.
fn entry_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.ends_with(".py")
        && value.split('/').all(|part| {
            part.as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                && !part.contains("..")
        })
}

impl GenerationPolicy {
    pub fn validate(&self) -> Result<(), GenerationError> {
        let mut paths = BTreeSet::new();
        if self.schema != GENERATION_POLICY_SCHEMA
            || !is_sha256(&self.base_derivation_ref)
            || self.max_generations != 1
            || !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms)
            || self.entrypoints.is_empty()
            || self.entrypoints.len() > MAX_ENTRYPOINTS
            || self
                .entrypoints
                .iter()
                .any(|(id, path)| !logical_id(id) || !entry_path(path) || !paths.insert(path))
        {
            return Err(GenerationError("generation_policy_invalid"));
        }
        Ok(())
    }
}

/// Compile one typed field choice against the owner's frozen source and parent.
/// It is the caller's responsibility to prove the selected file exists in that
/// source, and to deduplicate the result against all already authorized Ds.
pub fn compile(
    policy: &GenerationPolicy,
    base_capsule_toml: &str,
    closure_ref: &str,
    base_contract_ref: &str,
    draft: &GenerationDraft,
) -> Result<CompiledGeneration, GenerationError> {
    policy.validate()?;
    if draft.schema != GENERATION_DRAFT_SCHEMA || !logical_id(&draft.entrypoint_id) {
        return Err(GenerationError("generation_draft_invalid"));
    }
    let path = policy
        .entrypoints
        .get(&draft.entrypoint_id)
        .ok_or(GenerationError("generation_entrypoint_unauthorized"))?;
    if !is_sha256(closure_ref) || !is_sha256(base_contract_ref) {
        return Err(GenerationError("generation_reference_invalid"));
    }
    if base_capsule_toml.len() > MAX_CAPSULE_TOML_BYTES {
        return Err(GenerationError("generation_base_too_large"));
    }
    let mut authoring = parse_capsule_toml(base_capsule_toml)
        .map_err(|_| GenerationError("generation_base_invalid"))?;
    let context = BindingContext {
        source_closure_ref: closure_ref,
    };
    let (contract, original) =
        bind(&authoring, &context).map_err(|_| GenerationError("generation_base_invalid"))?;
    let identity_error = |_| GenerationError("generation_canonicalization_failed");
    if original.derivation_ref().map_err(identity_error)? != policy.base_derivation_ref {
        return Err(GenerationError("generation_base_mismatch"));
    }
    if contract.contract_ref().map_err(identity_error)? != base_contract_ref {
        return Err(GenerationError("generation_contract_mismatch"));
    }
    let unsupported = GenerationError("generation_base_unsupported");
    let [serve] = original.steps.as_slice() else {
        return Err(unsupported);
    };
    let [port] = original.ports.as_slice() else {
        return Err(unsupported);
    };
    let version = original.runtimes.get("python").ok_or(unsupported)?;
    // Exact versions only; no ranges, prereleases, catalog defaults or path text.
    let parsed = semver::Version::parse(version).map_err(|_| unsupported)?;
    if parsed.major != 3
        || !parsed.pre.is_empty()
        || !parsed.build.is_empty()
        || parsed.to_string() != *version
        || original.runtimes.len() != 1
    {
        return Err(unsupported);
    }
    let executable = format!("/opt/ato/toolchains/python/{version}/bin/python3");
    if original.inputs.len() != 1
        || !original.state.is_empty()
        || original.workspace_build.is_some()
        || original.workspace_compiler.is_some()
        || original.effects != EffectClass::Pure
        || serve.protocol != PROCESS_PROTOCOL
        || serve.op != "serve"
        || !serve.env.is_empty()
        || !serve.network.is_denied()
        || !serve.bindings.is_empty()
        || !serve.state.is_empty()
        || !serve.runtimes.is_empty()
        || !matches!(serve.cwd.as_str(), "" | ".")
        || serve.source.is_some()
        || serve.root.is_some()
        || serve.entry.is_some()
        || serve.spa_fallback.is_some()
        || port.protocol != HTTP_PROTOCOL
        || port.from != serve.id
        || port.guest_port.is_none_or(|p| p == 0)
        || port.client_address_transport.is_some()
        || serve.argv.len() != 3
        || serve.argv[0] != executable
        || serve.argv[1] != "-B"
        || !serve.argv[2].strip_prefix("/app/").is_some_and(entry_path)
    {
        return Err(unsupported);
    }
    let argv = vec![executable, "-B".into(), format!("/app/{path}")];
    authoring.derivation.steps[0].argv = argv.clone();
    let (generated_contract, derivation) =
        bind(&authoring, &context).map_err(|_| GenerationError("generation_base_invalid"))?;
    if generated_contract != contract {
        return Err(GenerationError("generation_contract_mismatch"));
    }
    let derivation_ref = derivation.derivation_ref().map_err(identity_error)?;
    if derivation_ref == policy.base_derivation_ref {
        return Err(GenerationError("generation_duplicate"));
    }
    // Serialize through the existing authoring grammar, then prove its round trip.
    // Neither source files nor the frozen author's original TOML are modified.
    let mut document: toml::Value = toml::from_str(base_capsule_toml)
        .map_err(|_| GenerationError("generation_base_invalid"))?;
    document["derive"]["step"][0]["argv"] =
        toml::Value::Array(argv.into_iter().map(toml::Value::String).collect());
    let capsule_toml = toml::to_string(&document)
        .map_err(|_| GenerationError("generation_canonicalization_failed"))?;
    let roundtrip = parse_capsule_toml(&capsule_toml)
        .map_err(|_| GenerationError("generation_roundtrip_failed"))?;
    let (roundtrip_k, roundtrip_d) =
        bind(&roundtrip, &context).map_err(|_| GenerationError("generation_roundtrip_failed"))?;
    if roundtrip_k != contract || roundtrip_d != derivation {
        return Err(GenerationError("generation_roundtrip_failed"));
    }
    Ok(CompiledGeneration {
        capsule_toml,
        derivation,
        derivation_ref,
        base_contract_ref: base_contract_ref.into(),
    })
}
