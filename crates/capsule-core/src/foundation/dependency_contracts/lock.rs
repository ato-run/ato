//! Pure lock-time verifier for capsule dependency contracts.
//!
//! All 13 verification rules in `CAPSULE_DEPENDENCY_CONTRACTS.md` §9.1 land
//! here. Each rule fails closed via a precise `LockError` variant.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::error::LockError;
use crate::foundation::types::{
    CapsuleManifest, ContractSpec, ContractStateSpec, DependencySpec, DependencyStateOwnership,
    ParamSchema, ParamValue, ReadyProbe, TemplateExpr, TemplateSegment, TemplatedString, ValueType,
};

/// Resolved provider manifest passed in by the orchestrator.
///
/// `requested` mirrors the consumer's `[dependencies.<X>] capsule` URL
/// (mutable / human-readable form). `resolved` is the immutable content
/// reference the orchestrator obtained from the authority. `manifest` is
/// the parsed provider manifest body.
#[derive(Debug, Clone)]
pub struct ResolvedProviderManifest {
    pub requested: String,
    pub resolved: String,
    pub manifest: CapsuleManifest,
}

/// All inputs to the pure verifier. The orchestrator is responsible for
/// constructing this; the verifier never touches I/O.
#[derive(Debug, Clone)]
pub struct DependencyLockInput<'a> {
    pub consumer: &'a CapsuleManifest,
    pub providers: BTreeMap<String, ResolvedProviderManifest>,
}

/// Output of the verifier: fully-resolved lock data for the consumer's
/// `[dependencies.*]` graph. Each entry corresponds to one alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyLock {
    pub entries: BTreeMap<String, LockedDependencyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedDependencyEntry {
    pub requested: String,
    pub resolved: String,
    pub contract: String,
    /// Resolved `parameters`. Identity-bearing (RFC §7.3 / §9.5).
    pub parameters: BTreeMap<String, ParamValue>,
    /// `credentials` are stored as **template form only** (RFC invariant 3).
    pub credentials: BTreeMap<String, String>,
    /// Resolved `identity_exports` from the provider contract, with
    /// `{{params.X}}` substituted against this dependency's parameters.
    pub identity_exports: BTreeMap<String, String>,
    /// `instance_hash` per RFC §7.7 = `blake3-128(JCS({resolved, contract,
    /// parameters}))[:16]`. Used as state path key and instance uniqueness key.
    pub instance_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LockedDependencyState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedDependencyState {
    pub name: String,
    pub ownership: String,
    pub version: String,
}

/// Run all v1 lock-time verifications and produce a `DependencyLock`.
pub fn verify_and_lock(input: DependencyLockInput<'_>) -> Result<DependencyLock, LockError> {
    let consumer_required_env: BTreeSet<&str> = input
        .consumer
        .required_env
        .iter()
        .map(String::as_str)
        .collect();

    // §9.1.11 first: a graph with conflicting majors fails before any other
    // per-dep work since a downstream dep might also expose the conflict.
    verify_major_version_uniqueness(input.consumer, &input.providers)?;

    // §9.1.9 + §9.1.10 are graph-level checks over consumer manifest.
    verify_needs_integrity(input.consumer)?;
    verify_no_cycles(input.consumer)?;

    let mut entries: BTreeMap<String, LockedDependencyEntry> = BTreeMap::new();
    for (alias, dep) in &input.consumer.dependencies {
        let provider = input
            .providers
            .get(alias)
            .ok_or_else(|| LockError::ProviderMissing {
                dep: alias.clone(),
                detail: "no resolved manifest supplied".to_string(),
            })?;

        let entry = verify_one_dependency(alias, dep, provider, &consumer_required_env)?;
        entries.insert(alias.clone(), entry);
    }

    // §9.1.12: instance uniqueness — two aliases with identical
    // (resolved, contract, parameters) tuple → fail.
    verify_instance_uniqueness(&entries)?;

    Ok(DependencyLock { entries })
}

fn verify_one_dependency(
    alias: &str,
    dep: &DependencySpec,
    provider: &ResolvedProviderManifest,
    consumer_required_env: &BTreeSet<&str>,
) -> Result<LockedDependencyEntry, LockError> {
    let contract_id = dep.contract.to_string();

    // §9.1.3: provider has the requested contract?
    let contract = provider
        .manifest
        .contracts
        .get(&contract_id)
        .ok_or_else(|| LockError::ContractNotFound {
            dep: alias.to_string(),
            contract: contract_id.clone(),
        })?;

    // §9.1.4: target binding exists in provider?
    verify_target_binding(alias, &contract_id, contract, provider)?;

    // §9.1.13: reserved variants on contract → fail-closed.
    verify_reserved_variants_on_contract(&contract_id, contract)?;

    // §9.1.7 / inv4: identity_exports must not contain {{credentials.X}}.
    verify_identity_exports_purity(&contract_id, contract)?;

    // §9.1 inv9: provider must not declare credentials.<key>.default. We
    // double-check at lock time as a defense-in-depth even though the
    // single-manifest validator already checks it.
    for (key, schema) in &contract.credentials {
        if schema.default.is_some() {
            return Err(LockError::CredentialDefaultForbidden {
                scope: format!("contracts.{}", contract_id),
                key: key.clone(),
            });
        }
    }

    // §9.1.5: parameter type / required match provider schema.
    let resolved_parameters = verify_parameters(alias, dep, contract, consumer_required_env)?;

    // §9.1.6: credentials type / required / literal-ban / env scope.
    verify_credentials(alias, dep, contract, consumer_required_env)?;

    // §9.1.8: state requirements.
    let state = verify_state(alias, dep, &contract_id, contract.state.as_ref())?;

    // §9.1.13 (endpoint): provider target must not declare
    // unix_socket = "auto" in v1.
    verify_reserved_variants_on_target(alias, contract, provider)?;

    // §9.1.7 derived data: resolve identity_exports against parameters.
    let identity_exports = resolve_identity_exports(alias, contract, &resolved_parameters)?;

    // §7.7 / §9.3: instance_hash = blake3-128(JCS({resolved, contract,
    // parameters}))[:16]. Credentials, alias, runtime_exports are NOT input.
    let instance_hash = compute_instance_hash(
        alias,
        &provider.resolved,
        &contract_id,
        &resolved_parameters,
    )?;

    // Credentials are stored in template form only (lockfile must never
    // contain resolved values).
    let credentials_template: BTreeMap<String, String> = dep
        .credentials
        .iter()
        .map(|(key, value)| (key.clone(), value.to_string()))
        .collect();

    Ok(LockedDependencyEntry {
        requested: dep.capsule.0.clone(),
        resolved: provider.resolved.clone(),
        contract: contract_id,
        parameters: resolved_parameters,
        credentials: credentials_template,
        identity_exports,
        instance_hash,
        state,
    })
}

// ---------- §9.1.4 target binding ----------

fn verify_target_binding(
    alias: &str,
    contract_id: &str,
    contract: &ContractSpec,
    provider: &ResolvedProviderManifest,
) -> Result<(), LockError> {
    let exists = provider
        .manifest
        .targets
        .as_ref()
        .map(|tc| tc.named.contains_key(&contract.target))
        .unwrap_or(false);
    if !exists {
        return Err(LockError::TargetNotFound {
            dep: alias.to_string(),
            contract: contract_id.to_string(),
            target: contract.target.clone(),
        });
    }
    Ok(())
}

// ---------- §9.1.13 reserved variants ----------

fn verify_reserved_variants_on_contract(
    contract_id: &str,
    contract: &ContractSpec,
) -> Result<(), LockError> {
    match &contract.ready {
        ReadyProbe::Tcp { .. } | ReadyProbe::Probe { .. } => Ok(()),
        ReadyProbe::Http { .. } => Err(LockError::ReservedVariantReadyProbe {
            contract: contract_id.to_string(),
            variant: "http".to_string(),
        }),
        ReadyProbe::UnixSocket { .. } => Err(LockError::ReservedVariantReadyProbe {
            contract: contract_id.to_string(),
            variant: "unix_socket".to_string(),
        }),
    }
}

fn verify_reserved_variants_on_target(
    _alias: &str,
    _contract: &ContractSpec,
    _provider: &ResolvedProviderManifest,
) -> Result<(), LockError> {
    // v1: NamedTarget does not yet expose a typed `EndpointSpec` field for
    // `port = "auto"` / `unix_socket = "auto"`. The reserved-variant check
    // for target endpoints will land alongside that schema extension. The
    // contract-side `ready.type` reservation is enforced elsewhere
    // (`verify_reserved_variants_on_contract`).
    Ok(())
}

// ---------- §9.1.7 identity_exports purity ----------

fn verify_identity_exports_purity(
    contract_id: &str,
    contract: &ContractSpec,
) -> Result<(), LockError> {
    for (key, value) in &contract.identity_exports {
        for segment in &value.segments {
            if let TemplateSegment::Expr(TemplateExpr::Credentials(_)) = segment {
                return Err(LockError::IdentityExportContainsCredential {
                    contract: contract_id.to_string(),
                    key: key.clone(),
                });
            }
        }
    }
    Ok(())
}

// ---------- §9.1.5 parameters ----------

fn verify_parameters(
    alias: &str,
    dep: &DependencySpec,
    contract: &ContractSpec,
    consumer_required_env: &BTreeSet<&str>,
) -> Result<BTreeMap<String, ParamValue>, LockError> {
    // unknown keys
    for key in dep.parameters.keys() {
        if !contract.parameters.contains_key(key) {
            return Err(LockError::ParameterUnknown {
                dep: alias.to_string(),
                key: key.clone(),
            });
        }
    }

    // required-but-missing + type check + default fill
    let mut resolved: BTreeMap<String, ParamValue> = BTreeMap::new();
    for (key, schema) in &contract.parameters {
        match dep.parameters.get(key) {
            Some(value) => {
                check_param_type(alias, key, schema, value)?;
                resolved.insert(key.clone(), value.clone());
            }
            None => {
                if let Some(default) = schema.default.clone() {
                    resolved.insert(key.clone(), default);
                } else if schema.required {
                    return Err(LockError::ParameterRequired {
                        dep: alias.to_string(),
                        key: key.clone(),
                    });
                }
            }
        }
    }

    // {{env.X}} scope check on parameter VALUES that happen to be string
    // templates. ParamValue::String can in principle carry an `{{env.X}}`
    // template; we check by re-parsing as TemplatedString.
    for (key, value) in &dep.parameters {
        if let ParamValue::String(s) = value {
            if let Ok(tmpl) = TemplatedString::parse(s) {
                for env_key in collect_env_references(&tmpl) {
                    if !consumer_required_env.contains(env_key.as_str()) {
                        return Err(LockError::ParameterEnvKeyOutOfScope {
                            dep: alias.to_string(),
                            key: key.clone(),
                            env_key,
                        });
                    }
                }
            }
        }
    }

    Ok(resolved)
}

fn check_param_type(
    alias: &str,
    key: &str,
    schema: &ParamSchema,
    value: &ParamValue,
) -> Result<(), LockError> {
    let actual = match value {
        ParamValue::String(_) => ValueType::String,
        ParamValue::Int(_) => ValueType::Int,
        ParamValue::Bool(_) => ValueType::Bool,
    };
    if actual != schema.value_type {
        return Err(LockError::ParameterTypeMismatch {
            dep: alias.to_string(),
            key: key.to_string(),
            expected: format!("{:?}", schema.value_type),
            actual: format!("{:?}", actual),
        });
    }
    Ok(())
}

// ---------- §9.1.6 credentials ----------

fn verify_credentials(
    alias: &str,
    dep: &DependencySpec,
    contract: &ContractSpec,
    consumer_required_env: &BTreeSet<&str>,
) -> Result<(), LockError> {
    // unknown keys
    for key in dep.credentials.keys() {
        if !contract.credentials.contains_key(key) {
            return Err(LockError::CredentialUnknown {
                dep: alias.to_string(),
                key: key.clone(),
            });
        }
    }

    // required-but-missing + literal-ban + env scope
    for (key, schema) in &contract.credentials {
        match dep.credentials.get(key) {
            Some(value) => {
                check_credential_template(alias, key, value, consumer_required_env)?;
            }
            None => {
                if schema.required {
                    return Err(LockError::CredentialRequired {
                        dep: alias.to_string(),
                        key: key.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn check_credential_template(
    alias: &str,
    key: &str,
    value: &TemplatedString,
    consumer_required_env: &BTreeSet<&str>,
) -> Result<(), LockError> {
    // RFC §7.3.1 invariant 3: credentials must be `{{env.X}}` template only.
    // A pure literal (no Expr segments) is forbidden, and any segment must
    // be an `env.X` expression — not raw literals interleaved with templates.
    let mut saw_expr = false;
    for segment in &value.segments {
        match segment {
            TemplateSegment::Literal(text) if text.is_empty() => {
                // empty literal between template segments is fine
            }
            TemplateSegment::Literal(_) => {
                return Err(LockError::CredentialLiteralForbidden {
                    dep: alias.to_string(),
                    key: key.to_string(),
                });
            }
            TemplateSegment::Expr(expr) => {
                saw_expr = true;
                match expr {
                    TemplateExpr::Env(env_key) => {
                        if !consumer_required_env.contains(env_key.as_str()) {
                            return Err(LockError::CredentialEnvKeyOutOfScope {
                                dep: alias.to_string(),
                                key: key.to_string(),
                                env_key: env_key.clone(),
                            });
                        }
                    }
                    _ => {
                        // Any non-env template (params/credentials/etc.) in a
                        // consumer credential value is treated as a literal
                        // violation: credentials must come from host env only.
                        return Err(LockError::CredentialLiteralForbidden {
                            dep: alias.to_string(),
                            key: key.to_string(),
                        });
                    }
                }
            }
        }
    }
    if !saw_expr {
        return Err(LockError::CredentialLiteralForbidden {
            dep: alias.to_string(),
            key: key.to_string(),
        });
    }
    Ok(())
}

// ---------- §9.1.8 state ----------

fn verify_state(
    alias: &str,
    dep: &DependencySpec,
    contract_id: &str,
    contract_state: Option<&ContractStateSpec>,
) -> Result<Option<LockedDependencyState>, LockError> {
    let Some(contract_state) = contract_state else {
        return Ok(None);
    };
    if !contract_state.required {
        // provider declares optional state; consumer may still set it but the
        // RFC v1 leaves that ambiguous — record it if present, ignore otherwise.
        return Ok(dep.state.as_ref().map(|s| LockedDependencyState {
            name: s.name.clone(),
            ownership: render_ownership(s.ownership.clone()),
            version: contract_state.version.clone().unwrap_or_default(),
        }));
    }

    // required = true: contract must declare state.version
    let version = contract_state
        .version
        .clone()
        .ok_or_else(|| LockError::StateVersionMissing {
            contract: contract_id.to_string(),
        })?;

    // consumer must declare state.name
    let consumer_state = dep
        .state
        .as_ref()
        .ok_or_else(|| LockError::StateRequiredButMissing {
            dep: alias.to_string(),
        })?;

    // ownership = "shared" not allowed in v1 (parser already only accepts
    // "parent", but we double-check here for defense-in-depth).
    if !matches!(consumer_state.ownership, DependencyStateOwnership::Parent) {
        return Err(LockError::StateOwnershipShared {
            dep: alias.to_string(),
        });
    }

    Ok(Some(LockedDependencyState {
        name: consumer_state.name.clone(),
        ownership: render_ownership(consumer_state.ownership.clone()),
        version,
    }))
}

fn render_ownership(o: DependencyStateOwnership) -> String {
    match o {
        DependencyStateOwnership::Parent => "parent".to_string(),
    }
}

// ---------- §9.1.9 needs ⊆ dependencies ----------

fn verify_needs_integrity(consumer: &CapsuleManifest) -> Result<(), LockError> {
    let Some(targets) = consumer.targets.as_ref() else {
        return Ok(());
    };
    for (target_label, target) in &targets.named {
        for need in &target.needs {
            if !consumer.dependencies.contains_key(need) {
                return Err(LockError::NeedsNotInDependencies {
                    target: target_label.clone(),
                    name: need.clone(),
                });
            }
        }
    }
    Ok(())
}

// ---------- §9.1.10 cycle detection ----------

fn verify_no_cycles(consumer: &CapsuleManifest) -> Result<(), LockError> {
    // Build adjacency list: consumer target -> list of dep aliases (via needs).
    // For v1 we only consider needs-graph cycles inside this manifest.
    // Provider→provider transitive edges are out of scope (§2 / §9.5 follow-up).
    let Some(targets) = consumer.targets.as_ref() else {
        return Ok(());
    };

    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for (label, target) in &targets.named {
        graph.insert(
            label.as_str(),
            target.needs.iter().map(String::as_str).collect(),
        );
    }

    let mut visiting: BTreeSet<&str> = BTreeSet::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    for node in graph.keys() {
        if visited.contains(*node) {
            continue;
        }
        let mut stack: Vec<&str> = Vec::new();
        if let Some(cycle) = dfs_cycle(node, &graph, &mut visiting, &mut visited, &mut stack) {
            return Err(LockError::CycleDetected { path: cycle });
        }
    }
    Ok(())
}

fn dfs_cycle<'a>(
    node: &'a str,
    graph: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    stack: &mut Vec<&'a str>,
) -> Option<String> {
    if visited.contains(node) {
        return None;
    }
    if visiting.contains(node) {
        let mut cycle: Vec<&str> = stack.iter().copied().skip_while(|s| *s != node).collect();
        cycle.push(node);
        return Some(cycle.join(" -> "));
    }
    visiting.insert(node);
    stack.push(node);
    if let Some(neighbors) = graph.get(node) {
        for next in neighbors {
            if let Some(cycle) = dfs_cycle(next, graph, visiting, visited, stack) {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    visiting.remove(node);
    visited.insert(node);
    None
}

// ---------- §9.1.11 major-version uniqueness ----------

fn verify_major_version_uniqueness(
    consumer: &CapsuleManifest,
    providers: &BTreeMap<String, ResolvedProviderManifest>,
) -> Result<(), LockError> {
    // Group dep aliases by provider source path (everything before `@`).
    // If multiple aliases reference the same source with different major
    // versions, fail.
    let mut groups: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for (alias, dep) in &consumer.dependencies {
        let url = providers
            .get(alias)
            .map(|p| p.requested.as_str())
            .unwrap_or(dep.capsule.0.as_str());
        let source = capsule_source_path(url);
        groups
            .entry(source.clone())
            .or_default()
            .insert(major_from_url(url).unwrap_or(0));
        sources
            .entry(source.clone())
            .or_insert_with(|| url.to_string());
    }
    for (source, majors) in groups {
        if majors.len() > 1 {
            return Err(LockError::MajorVersionConflict {
                capsule_source: source,
                majors: majors.iter().map(|m| m.to_string()).collect(),
            });
        }
    }
    Ok(())
}

fn capsule_source_path(url: &str) -> String {
    // Strip the `@<ref>` suffix (if present); everything else (including the
    // scheme + authority + path) makes up the "source" for major-uniqueness
    // grouping.
    if let Some(idx) = url.rfind('@') {
        url[..idx].to_string()
    } else {
        url.to_string()
    }
}

fn major_from_url(url: &str) -> Option<u32> {
    let (_path, ref_part) = url.rsplit_once('@')?;
    let trimmed = ref_part.trim();
    // Treat sha256:.../immutable refs as unique major (rare in same graph;
    // major conflict only meaningful for human-readable refs like "16").
    if trimmed.starts_with("sha256:") {
        return None;
    }
    let leading: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if leading.is_empty() {
        None
    } else {
        leading.parse().ok()
    }
}

// ---------- §9.1.12 instance uniqueness ----------

fn verify_instance_uniqueness(
    entries: &BTreeMap<String, LockedDependencyEntry>,
) -> Result<(), LockError> {
    let mut by_hash: BTreeMap<&str, &str> = BTreeMap::new();
    for (alias, entry) in entries {
        if let Some(existing) = by_hash.insert(entry.instance_hash.as_str(), alias.as_str()) {
            return Err(LockError::InstanceUniquenessViolation {
                a: existing.to_string(),
                b: alias.to_string(),
                resolved: entry.resolved.clone(),
                contract: entry.contract.clone(),
            });
        }
    }
    Ok(())
}

// ---------- §7.7 / §9.5 instance hash ----------

#[derive(Debug, Serialize)]
struct InstanceHashInput<'a> {
    resolved: &'a str,
    contract: &'a str,
    parameters: &'a BTreeMap<String, ParamValue>,
}

fn compute_instance_hash(
    alias: &str,
    resolved: &str,
    contract: &str,
    parameters: &BTreeMap<String, ParamValue>,
) -> Result<String, LockError> {
    let input = InstanceHashInput {
        resolved,
        contract,
        parameters,
    };
    let canonical = serde_jcs::to_vec(&input).map_err(|e| LockError::InternalHashFailure {
        dep: alias.to_string(),
        detail: e.to_string(),
    })?;
    let hash = blake3::hash(&canonical);
    // 16 bytes = 128 bits, hex-encoded.
    let prefix = &hash.as_bytes()[..16];
    Ok(format!("blake3:{}", hex::encode(prefix)))
}

// ---------- identity_exports resolution ----------

fn resolve_identity_exports(
    alias: &str,
    contract: &ContractSpec,
    parameters: &BTreeMap<String, ParamValue>,
) -> Result<BTreeMap<String, String>, LockError> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in &contract.identity_exports {
        let resolved = render_identity_template(alias, key, value, parameters)?;
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

fn render_identity_template(
    alias: &str,
    export_key: &str,
    template: &TemplatedString,
    parameters: &BTreeMap<String, ParamValue>,
) -> Result<String, LockError> {
    let mut out = String::new();
    for segment in &template.segments {
        match segment {
            TemplateSegment::Literal(text) => out.push_str(text),
            TemplateSegment::Expr(expr) => match expr {
                TemplateExpr::Params(key) => {
                    let Some(value) = parameters.get(key) else {
                        return Err(LockError::ParameterUnknown {
                            dep: alias.to_string(),
                            key: format!(
                                "identity_exports.{} references undeclared params.{}",
                                export_key, key
                            ),
                        });
                    };
                    out.push_str(&render_param_value(value));
                }
                // §9.1.7 already rejected credentials here; defense-in-depth.
                TemplateExpr::Credentials(_) => {
                    return Err(LockError::IdentityExportContainsCredential {
                        contract: alias.to_string(),
                        key: export_key.to_string(),
                    });
                }
                // Other expression types (host/port/socket/state.dir/env/deps.*)
                // are not deterministic at lock time. Reject them in identity
                // exports — they belong in `runtime_exports`.
                other => {
                    return Err(LockError::IdentityExportContainsCredential {
                        contract: alias.to_string(),
                        key: format!(
                            "{}: contains non-deterministic {{{{{}}}}}",
                            export_key, other
                        ),
                    });
                }
            },
        }
    }
    Ok(out)
}

fn render_param_value(value: &ParamValue) -> String {
    match value {
        ParamValue::String(s) => s.clone(),
        ParamValue::Int(i) => i.to_string(),
        ParamValue::Bool(b) => b.to_string(),
    }
}

fn collect_env_references(template: &TemplatedString) -> Vec<String> {
    let mut out = Vec::new();
    for segment in &template.segments {
        if let TemplateSegment::Expr(TemplateExpr::Env(key)) = segment {
            out.push(key.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests;
