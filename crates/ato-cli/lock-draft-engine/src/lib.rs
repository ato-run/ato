use std::collections::{BTreeMap, BTreeSet};

pub mod leip;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEFAULT_NODE_RUNTIME_VERSION: &str = "20.12.0";
const DEFAULT_PYTHON_RUNTIME_VERSION: &str = "3.11.10";
const DEFAULT_DENO_RUNTIME_VERSION: &str = "2.6.8";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockDraftReadiness {
    Draft,
    ReadyToFinalize,
    FinalizedLocally,
    PreviewVerified,
    Publishable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockDraftConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SelectedTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_tools: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepoFileKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoFileEntry {
    pub path: String,
    #[serde(default = "default_repo_file_kind")]
    pub kind: RepoFileKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

fn default_repo_file_kind() -> RepoFileKind {
    RepoFileKind::File
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ManifestSource {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_target_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExistingAtoLockSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LockDraftExternalDependency {
    pub name: String,
    pub source: String,
    pub source_type: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub injection_bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockDraftRuntimePlatform {
    pub os: String,
    pub arch: String,
    pub target_triple: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LockDraftInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_target: Option<SelectedTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_file_index: Vec<RepoFileEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub file_text_map: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_source: Option<ManifestSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_ato_lock_summary: Option<ExistingAtoLockSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_dependency_hints: Vec<LockDraftExternalDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockDraft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_tools: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_platforms: Vec<LockDraftRuntimePlatform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_native_lockfiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_native_lockfiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_capsule_dependencies: Vec<LockDraftExternalDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_commands: Vec<String>,
    pub readiness: LockDraftReadiness,
    pub confidence: LockDraftConfidence,
    pub draft_hash: String,
}

#[derive(Debug, Error)]
pub enum LockDraftError {
    #[error("failed to parse manifest source: {0}")]
    ManifestParse(String),
    #[error("failed to serialize lock draft: {0}")]
    Serialize(String),
}

#[derive(Debug, Clone, Default)]
struct ResolvedTarget {
    label: Option<String>,
    runtime: Option<String>,
    driver: Option<String>,
    entrypoint: Option<String>,
    run_command: Option<String>,
    cmd: Vec<String>,
    runtime_version: Option<String>,
    runtime_tools: BTreeMap<String, String>,
    dependencies_path: Option<String>,
}

#[derive(Debug, Clone)]
struct ManifestView {
    raw: toml::Value,
    selected_target: ResolvedTarget,
}

pub fn evaluate_lock_draft(input: &LockDraftInput) -> Result<LockDraft, LockDraftError> {
    let manifest = parse_manifest_view(input.manifest_source.as_ref())?;
    let target = resolve_target(input.selected_target.as_ref(), manifest.as_ref());

    let runtime = normalize_scalar(target.runtime.clone());
    let driver = resolve_driver(&target, input);
    let runtime_tools = normalize_runtime_tools(&target.runtime_tools);
    let effective_driver = effective_lock_driver(runtime.as_deref(), driver.as_deref());
    let required_runtime_version = required_runtime_version(
        runtime.as_deref(),
        effective_driver.as_deref(),
        target.runtime_version.as_deref(),
    );
    let runtime_platforms = runtime_platforms(
        runtime.as_deref(),
        effective_driver.as_deref(),
        &runtime_tools,
    );

    let required_native_lockfiles = required_native_lockfiles(
        runtime.as_deref(),
        effective_driver.as_deref(),
        input,
        &target,
    );
    let missing_native_lockfiles = missing_native_lockfiles(&required_native_lockfiles, input);
    let external_capsule_dependencies = external_capsule_dependencies(input, manifest.as_ref())?;

    let mut blocking_issues = Vec::new();
    let mut warnings = Vec::new();

    if runtime.is_none() {
        blocking_issues.push(
            "LockDraft could not resolve a primary runtime from the manifest or host hints."
                .to_string(),
        );
    }

    if effective_driver.is_some()
        && runtime_version_is_required(runtime.as_deref(), effective_driver.as_deref())
        && target.runtime_version.is_none()
        && required_runtime_version.is_some()
    {
        warnings.push(
            "runtime_version was inferred by the shared LockDraft engine. Pin it in the manifest to make local finalize explicit."
                .to_string(),
        );
    }

    if !missing_native_lockfiles.is_empty() {
        warnings.push(format!(
            "Native lockfile is missing. Generate one of: {}",
            missing_native_lockfiles.join(", ")
        ));
    }

    if target.entrypoint.is_none() && target.run_command.is_none() && target.cmd.is_empty() {
        warnings.push(
            "No explicit entrypoint or run command was provided; readiness is based on repository heuristics."
                .to_string(),
        );
    }

    let suggested_commands = suggested_commands(
        runtime.as_deref(),
        effective_driver.as_deref(),
        input,
        &target,
        &missing_native_lockfiles,
    );

    let readiness = if blocking_issues.is_empty() && missing_native_lockfiles.is_empty() {
        LockDraftReadiness::ReadyToFinalize
    } else {
        LockDraftReadiness::Draft
    };

    let confidence = confidence(&runtime, &driver, &target, &blocking_issues, &warnings);

    let mut draft = LockDraft {
        runtime,
        driver,
        required_runtime_version,
        runtime_tools,
        runtime_platforms,
        required_native_lockfiles,
        missing_native_lockfiles,
        external_capsule_dependencies,
        blocking_issues,
        warnings,
        suggested_commands,
        readiness,
        confidence,
        draft_hash: String::new(),
    };

    draft.draft_hash = draft_hash(&draft)?;
    Ok(draft)
}

pub fn evaluate_lock_draft_json(input_json: &str) -> Result<String, LockDraftError> {
    let input: LockDraftInput = serde_json::from_str(input_json)
        .map_err(|err| LockDraftError::Serialize(err.to_string()))?;
    let draft = evaluate_lock_draft(&input)?;
    serde_json::to_string(&draft).map_err(|err| LockDraftError::Serialize(err.to_string()))
}

pub fn lock_draft_schema_json() -> String {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "LockDraft Contract",
        "type": "object",
        "properties": {
            "input": {
                "type": "object",
                "required": ["repo_file_index", "file_text_map", "external_dependency_hints"],
                "properties": {
                    "selected_target": { "type": ["object", "null"] },
                    "repo_file_index": { "type": "array" },
                    "file_text_map": { "type": "object" },
                    "manifest_source": { "type": ["object", "null"] },
                    "existing_ato_lock_summary": { "type": ["object", "null"] },
                    "external_dependency_hints": { "type": "array" }
                }
            },
            "output": {
                "type": "object",
                "required": ["runtime_tools", "runtime_platforms", "required_native_lockfiles", "missing_native_lockfiles", "external_capsule_dependencies", "blocking_issues", "warnings", "suggested_commands", "readiness", "confidence", "draft_hash"],
                "properties": {
                    "runtime": { "type": ["string", "null"] },
                    "driver": { "type": ["string", "null"] },
                    "required_runtime_version": { "type": ["string", "null"] },
                    "runtime_tools": { "type": "object" },
                    "runtime_platforms": { "type": "array" },
                    "required_native_lockfiles": { "type": "array" },
                    "missing_native_lockfiles": { "type": "array" },
                    "external_capsule_dependencies": { "type": "array" },
                    "blocking_issues": { "type": "array" },
                    "warnings": { "type": "array" },
                    "suggested_commands": { "type": "array" },
                    "readiness": { "enum": ["draft", "ready_to_finalize", "finalized_locally", "preview_verified", "publishable"] },
                    "confidence": { "enum": ["low", "medium", "high"] },
                    "draft_hash": { "type": "string" }
                }
            }
        }
    })
    .to_string()
}

#[cfg(feature = "wasm")]
mod wasm_exports {
    use super::*;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(js_name = evaluateLockDraftJson)]
    pub fn evaluate_lock_draft_json_wasm(input_json: &str) -> Result<String, JsValue> {
        evaluate_lock_draft_json(input_json).map_err(|err| JsValue::from_str(&err.to_string()))
    }

    #[wasm_bindgen(js_name = lockDraftSchemaJson)]
    pub fn lock_draft_schema_json_wasm() -> String {
        lock_draft_schema_json()
    }

    /// Primary LEIP v1 inference API.
    /// Accepts a `LeipInput` JSON string and returns a `LeipResult` JSON string.
    #[wasm_bindgen(js_name = evaluateLaunchGraphsJson)]
    pub fn evaluate_launch_graphs_json_wasm(input_json: &str) -> Result<String, JsValue> {
        leip::evaluate_launch_graphs_json(input_json)
            .map_err(|err| JsValue::from_str(&err.to_string()))
    }

    /// Compatibility LEIP wrapper.
    /// Accepts a `LockDraftInput` JSON string (maps `selected_target` → `target_hint`)
    /// and returns a `LeipResult` JSON string.
    #[wasm_bindgen(js_name = evaluateLaunchEnvelopesJson)]
    pub fn evaluate_launch_envelopes_json_wasm(input_json: &str) -> Result<String, JsValue> {
        leip::evaluate_launch_envelopes_json(input_json)
            .map_err(|err| JsValue::from_str(&err.to_string()))
    }
}

fn confidence(
    runtime: &Option<String>,
    driver: &Option<String>,
    target: &ResolvedTarget,
    blocking_issues: &[String],
    warnings: &[String],
) -> LockDraftConfidence {
    if runtime.is_none()
        || blocking_issues
            .iter()
            .any(|issue| issue.contains("could not resolve"))
    {
        return LockDraftConfidence::Low;
    }
    if !blocking_issues.is_empty() {
        return LockDraftConfidence::Low;
    }
    if driver.is_some()
        && (target.entrypoint.is_some() || target.run_command.is_some() || !target.cmd.is_empty())
        && warnings.is_empty()
    {
        return LockDraftConfidence::High;
    }
    LockDraftConfidence::Medium
}

fn draft_hash(draft: &LockDraft) -> Result<String, LockDraftError> {
    let mut cloned = draft.clone();
    cloned.draft_hash.clear();
    let bytes =
        serde_json::to_vec(&cloned).map_err(|err| LockDraftError::Serialize(err.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn effective_lock_driver(runtime: Option<&str>, driver: Option<&str>) -> Option<String> {
    if runtime == Some("web") && matches!(driver, Some("static") | Some("deno")) {
        return Some("deno".to_string());
    }
    driver.map(|value| value.to_string())
}

fn external_capsule_dependencies(
    input: &LockDraftInput,
    manifest: Option<&ManifestView>,
) -> Result<Vec<LockDraftExternalDependency>, LockDraftError> {
    if let Some(manifest) = manifest {
        if let Some(targets) = manifest.raw.get("targets").and_then(toml::Value::as_table) {
            let mut collected = Vec::new();
            let mut seen = BTreeMap::<String, String>::new();
            for (target_label, raw_target) in targets {
                let Some(external_dependencies) = raw_target
                    .get("external_dependencies")
                    .and_then(toml::Value::as_array)
                else {
                    continue;
                };

                for raw_dependency in external_dependencies {
                    let Some(table) = raw_dependency.as_table() else {
                        continue;
                    };
                    let Some(alias) = table
                        .get("alias")
                        .and_then(toml::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    else {
                        continue;
                    };
                    let Some(source) = table
                        .get("source")
                        .and_then(toml::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    else {
                        continue;
                    };
                    let source_type = table
                        .get("source_type")
                        .and_then(toml::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("store");
                    if let Some(existing) = seen.get(alias) {
                        if existing != source {
                            return Err(LockDraftError::ManifestParse(format!(
                                "external dependency alias '{}' maps to multiple sources in target '{}'",
                                alias, target_label
                            )));
                        }
                        continue;
                    }
                    let injection_bindings = table
                        .get("injection_bindings")
                        .and_then(toml::Value::as_table)
                        .map(|bindings| {
                            bindings
                                .iter()
                                .filter_map(|(key, value)| {
                                    value
                                        .as_str()
                                        .map(|value| (key.to_string(), value.trim().to_string()))
                                })
                                .collect::<BTreeMap<_, _>>()
                        })
                        .unwrap_or_default();
                    seen.insert(alias.to_string(), source.to_string());
                    collected.push(LockDraftExternalDependency {
                        name: alias.to_string(),
                        source: source.to_string(),
                        source_type: source_type.to_string(),
                        injection_bindings,
                    });
                }
            }
            collected.sort_by(|left, right| left.name.cmp(&right.name));
            return Ok(collected);
        }
    }

    let mut dependencies = input.external_dependency_hints.clone();
    dependencies.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(dependencies)
}

fn file_exists(input: &LockDraftInput, path: &str) -> bool {
    let normalized = normalize_path(path);
    if input
        .repo_file_index
        .iter()
        .any(|entry| entry.kind == RepoFileKind::File && normalize_path(&entry.path) == normalized)
    {
        return true;
    }
    input
        .file_text_map
        .keys()
        .any(|entry| normalize_path(entry) == normalized)
}

fn file_text<'a>(input: &'a LockDraftInput, path: &str) -> Option<&'a str> {
    let normalized = normalize_path(path);
    input
        .file_text_map
        .iter()
        .find_map(|(key, value)| (normalize_path(key) == normalized).then_some(value.as_str()))
}

fn infer_driver_from_repo(target: &ResolvedTarget, input: &LockDraftInput) -> Option<String> {
    if let Some(driver) = infer_driver_from_cmd(&target.cmd) {
        return Some(driver);
    }
    if let Some(driver) = infer_driver_from_run_command(target.run_command.as_deref()) {
        return Some(driver);
    }
    if let Some(entrypoint) = target.entrypoint.as_deref() {
        let lower = entrypoint.to_ascii_lowercase();
        if lower.ends_with(".py") {
            return Some("python".to_string());
        }
        if lower.ends_with(".ts")
            || lower.ends_with(".tsx")
            || lower.ends_with(".js")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
        {
            if file_exists(input, "deno.json") || file_exists(input, "deno.lock") {
                return Some("deno".to_string());
            }
            return Some("node".to_string());
        }
        if lower.ends_with(".rs") {
            return Some("native".to_string());
        }
        if lower.ends_with(".go") {
            return Some("go".to_string());
        }
    }
    if file_exists(input, "deno.json")
        || file_exists(input, "deno.lock")
        || file_exists(input, "deno.jsonc")
    {
        return Some("deno".to_string());
    }
    if file_exists(input, "pyproject.toml")
        || file_exists(input, "requirements.txt")
        || file_exists(input, "uv.lock")
    {
        return Some("python".to_string());
    }
    if file_exists(input, "package.json") {
        return Some("node".to_string());
    }
    if file_exists(input, "Cargo.toml") {
        return Some("native".to_string());
    }
    if file_exists(input, "go.mod") {
        return Some("go".to_string());
    }
    None
}

fn infer_driver_from_cmd(cmd: &[String]) -> Option<String> {
    let program = cmd.first()?.trim().to_ascii_lowercase();
    match program.as_str() {
        "deno" => Some("deno".to_string()),
        "node" | "nodejs" | "npm" | "pnpm" | "bun" | "yarn" => Some("node".to_string()),
        "python" | "python3" | "py" | "uv" => Some("python".to_string()),
        "cargo" => Some("native".to_string()),
        "go" => Some("go".to_string()),
        _ => None,
    }
}

fn infer_driver_from_run_command(run_command: Option<&str>) -> Option<String> {
    let command = normalize_scalar(run_command.map(str::to_string))?;
    let first = command
        .split_whitespace()
        .next()?
        .trim()
        .to_ascii_lowercase();
    match first.as_str() {
        "deno" => Some("deno".to_string()),
        "node" | "nodejs" | "npm" | "pnpm" | "bun" | "yarn" => Some("node".to_string()),
        "python" | "python3" | "py" | "uv" => Some("python".to_string()),
        "cargo" => Some("native".to_string()),
        "go" => Some("go".to_string()),
        _ => None,
    }
}

fn missing_native_lockfiles(required: &[String], input: &LockDraftInput) -> Vec<String> {
    if required.is_empty() {
        return Vec::new();
    }
    if required
        .iter()
        .any(|candidate| file_exists(input, candidate))
    {
        return Vec::new();
    }
    required.to_vec()
}

fn normalize_path(value: &str) -> String {
    value.trim().replace('\\', "/").to_ascii_lowercase()
}

fn normalize_runtime_tools(runtime_tools: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    runtime_tools
        .iter()
        .filter_map(|(key, value)| {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            (!key.is_empty() && !value.is_empty()).then_some((key, value.to_string()))
        })
        .collect()
}

fn normalize_scalar(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_manifest_view(
    manifest_source: Option<&ManifestSource>,
) -> Result<Option<ManifestView>, LockDraftError> {
    let Some(manifest_source) = manifest_source else {
        return Ok(None);
    };
    let raw: toml::Value = manifest_source
        .text
        .parse()
        .map_err(|err: toml::de::Error| LockDraftError::ManifestParse(err.to_string()))?;
    let selected_target =
        parse_selected_target_from_manifest(&raw, manifest_source.selected_target_label.as_deref());
    Ok(Some(ManifestView {
        raw,
        selected_target,
    }))
}

fn parse_selected_target_from_manifest(
    raw: &toml::Value,
    selected_target_label: Option<&str>,
) -> ResolvedTarget {
    let target_table = if let Some(targets) = raw.get("targets").and_then(toml::Value::as_table) {
        let default_target = selected_target_label
            .map(str::to_string)
            .or_else(|| {
                raw.get("default_target")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "source".to_string());
        targets
            .get(&default_target)
            .or_else(|| targets.get("source"))
            .cloned()
    } else {
        None
    };

    if let Some(target) = target_table {
        return resolved_target_from_value(
            Some(selected_target_label.map(str::to_string).or_else(|| {
                raw.get("default_target")
                    .and_then(toml::Value::as_str)
                    .map(|value| value.to_string())
            })),
            &target,
        );
    }

    resolved_target_from_value(None, raw)
}

fn parse_runtime_selector(
    runtime: Option<String>,
    driver: Option<String>,
) -> (Option<String>, Option<String>) {
    let runtime = normalize_scalar(runtime);
    let driver = normalize_scalar(driver);
    if let Some(runtime_value) = runtime.as_deref() {
        if let Some((base_runtime, inferred_driver)) = runtime_value.split_once('/') {
            return (
                normalize_scalar(Some(base_runtime.to_string())),
                driver.or_else(|| normalize_scalar(Some(inferred_driver.to_string()))),
            );
        }
    }
    (runtime, driver)
}

fn required_native_lockfiles(
    runtime: Option<&str>,
    effective_driver: Option<&str>,
    input: &LockDraftInput,
    target: &ResolvedTarget,
) -> Vec<String> {
    match effective_driver {
        Some("node") if node_project_present(input, target) => {
            vec![
                "package-lock.json".to_string(),
                "pnpm-lock.yaml".to_string(),
                "yarn.lock".to_string(),
                "bun.lock".to_string(),
                "bun.lockb".to_string(),
            ]
        }
        Some("python") if python_project_present(input, target) => vec!["uv.lock".to_string()],
        Some("deno")
            if runtime != Some("web")
                && !cmd_contains_no_lock(target)
                && deno_project_present(input, target) =>
        {
            vec!["deno.lock".to_string()]
        }
        Some("native") if rust_project_present(input, target) => vec!["Cargo.lock".to_string()],
        Some("go") if go_project_present(input, target) => vec!["go.sum".to_string()],
        _ => Vec::new(),
    }
}

fn required_runtime_version(
    runtime: Option<&str>,
    effective_driver: Option<&str>,
    configured_runtime_version: Option<&str>,
) -> Option<String> {
    if let Some(runtime_version) = normalize_scalar(configured_runtime_version.map(str::to_string))
    {
        return Some(runtime_version);
    }

    match (runtime, effective_driver) {
        (Some("source"), Some("node")) => Some(DEFAULT_NODE_RUNTIME_VERSION.to_string()),
        (Some("source"), Some("python")) => Some(DEFAULT_PYTHON_RUNTIME_VERSION.to_string()),
        (Some("source"), Some("deno")) => Some(DEFAULT_DENO_RUNTIME_VERSION.to_string()),
        (Some("web"), Some("deno")) => Some(DEFAULT_DENO_RUNTIME_VERSION.to_string()),
        _ => None,
    }
}

fn resolve_driver(target: &ResolvedTarget, input: &LockDraftInput) -> Option<String> {
    let explicit = normalize_scalar(target.driver.clone());
    if explicit.is_some() {
        return explicit;
    }
    infer_driver_from_repo(target, input)
}

fn resolve_target(
    selected_target: Option<&SelectedTarget>,
    manifest: Option<&ManifestView>,
) -> ResolvedTarget {
    let mut resolved = manifest
        .map(|manifest| manifest.selected_target.clone())
        .unwrap_or_default();

    if let Some(selected_target) = selected_target {
        let override_target = ResolvedTarget {
            label: selected_target.label.clone(),
            runtime: selected_target.runtime.clone(),
            driver: selected_target.driver.clone(),
            entrypoint: selected_target.entrypoint.clone(),
            run_command: selected_target.run_command.clone(),
            cmd: selected_target.cmd.clone(),
            runtime_version: selected_target.runtime_version.clone(),
            runtime_tools: selected_target.runtime_tools.clone(),
            dependencies_path: selected_target.dependencies_path.clone(),
        };
        resolved = merge_target(resolved, override_target);
    }

    let (runtime, driver) = parse_runtime_selector(resolved.runtime, resolved.driver);
    resolved.runtime = runtime;
    resolved.driver = driver;
    resolved.runtime_version = normalize_scalar(resolved.runtime_version);
    resolved.entrypoint = normalize_scalar(resolved.entrypoint);
    resolved.run_command = normalize_scalar(resolved.run_command);
    resolved.dependencies_path = normalize_scalar(resolved.dependencies_path);
    resolved.runtime_tools = normalize_runtime_tools(&resolved.runtime_tools);
    resolved
}

fn resolved_target_from_value(
    label: Option<Option<String>>,
    target: &toml::Value,
) -> ResolvedTarget {
    let runtime_tools = target
        .get("runtime_tools")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.to_string(), value.trim().to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let cmd = target
        .get("cmd")
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    ResolvedTarget {
        label: label.flatten(),
        runtime: target
            .get("runtime")
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
        driver: target
            .get("driver")
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
        entrypoint: target
            .get("entrypoint")
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
        run_command: target
            .get("run")
            .or_else(|| target.get("run_command"))
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
        cmd,
        runtime_version: target
            .get("runtime_version")
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
        runtime_tools,
        dependencies_path: target
            .get("dependencies")
            .and_then(toml::Value::as_str)
            .map(|value| value.to_string()),
    }
}

fn runtime_platforms(
    runtime: Option<&str>,
    effective_driver: Option<&str>,
    runtime_tools: &BTreeMap<String, String>,
) -> Vec<LockDraftRuntimePlatform> {
    let needs_universal_lock = runtime == Some("web")
        || (runtime == Some("source")
            && (matches!(
                effective_driver,
                Some("python") | Some("node") | Some("deno")
            ) || !runtime_tools.is_empty()));

    if !needs_universal_lock {
        return Vec::new();
    }

    SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .map(|platform| LockDraftRuntimePlatform {
            os: platform.0.to_string(),
            arch: platform.1.to_string(),
            target_triple: platform.2.to_string(),
        })
        .collect()
}

fn runtime_version_is_required(runtime: Option<&str>, effective_driver: Option<&str>) -> bool {
    (runtime == Some("source")
        && matches!(
            effective_driver,
            Some("python") | Some("node") | Some("deno")
        ))
        || (runtime == Some("web") && effective_driver == Some("deno"))
}

fn suggested_commands(
    runtime: Option<&str>,
    effective_driver: Option<&str>,
    input: &LockDraftInput,
    target: &ResolvedTarget,
    missing_native_lockfiles: &[String],
) -> Vec<String> {
    if missing_native_lockfiles.is_empty() {
        return Vec::new();
    }

    let mut commands = BTreeSet::new();
    match effective_driver {
        Some("node") => {
            let package_manager = detect_node_package_manager(input);
            let command = match package_manager.as_deref() {
                Some("pnpm") => "pnpm install --lockfile-only",
                Some("bun") => "bun install --lockfile-only",
                Some("yarn") => "yarn install --mode=skip-builds",
                _ => "npm install --package-lock-only",
            };
            commands.insert(command.to_string());
        }
        Some("python") => {
            if file_exists(input, "requirements.txt") && !file_exists(input, "pyproject.toml") {
                commands.insert("uv pip compile requirements.txt -o uv.lock".to_string());
            } else {
                commands.insert("uv lock".to_string());
            }
        }
        Some("deno") if runtime != Some("web") => {
            let entrypoint = target
                .entrypoint
                .clone()
                .unwrap_or_else(|| "main.ts".to_string());
            commands.insert(format!(
                "deno cache --lock=deno.lock --frozen=false {}",
                entrypoint
            ));
        }
        Some("native") => {
            commands.insert("cargo generate-lockfile".to_string());
        }
        Some("go") => {
            commands.insert("go mod tidy".to_string());
        }
        _ => {}
    }
    commands.into_iter().collect()
}

fn cmd_contains_no_lock(target: &ResolvedTarget) -> bool {
    if target.cmd.iter().any(|item| item == "--no-lock") {
        return true;
    }
    target
        .run_command
        .as_deref()
        .map(|command| command.contains("--no-lock"))
        .unwrap_or(false)
}

fn deno_project_present(input: &LockDraftInput, target: &ResolvedTarget) -> bool {
    target
        .entrypoint
        .as_deref()
        .map(|entrypoint| {
            let lower = entrypoint.to_ascii_lowercase();
            lower.ends_with(".ts")
                || lower.ends_with(".tsx")
                || lower.ends_with(".js")
                || lower.ends_with(".mjs")
        })
        .unwrap_or(false)
        || file_exists(input, "deno.json")
        || file_exists(input, "deno.lock")
}

fn detect_node_package_manager(input: &LockDraftInput) -> Option<String> {
    if file_exists(input, "pnpm-lock.yaml") {
        return Some("pnpm".to_string());
    }
    if file_exists(input, "bun.lock") || file_exists(input, "bun.lockb") {
        return Some("bun".to_string());
    }
    if file_exists(input, "yarn.lock") {
        return Some("yarn".to_string());
    }
    let package_json = file_text(input, "package.json")?;
    let parsed: serde_json::Value = serde_json::from_str(package_json).ok()?;
    let package_manager = parsed
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if package_manager.starts_with("pnpm@") {
        return Some("pnpm".to_string());
    }
    if package_manager.starts_with("bun@") {
        return Some("bun".to_string());
    }
    if package_manager.starts_with("yarn@") {
        return Some("yarn".to_string());
    }
    if package_manager.starts_with("npm@") {
        return Some("npm".to_string());
    }
    None
}

fn go_project_present(input: &LockDraftInput, target: &ResolvedTarget) -> bool {
    file_exists(input, "go.mod")
        || target
            .entrypoint
            .as_deref()
            .map(|entrypoint| entrypoint.to_ascii_lowercase().ends_with(".go"))
            .unwrap_or(false)
}

fn merge_target(base: ResolvedTarget, override_target: ResolvedTarget) -> ResolvedTarget {
    ResolvedTarget {
        label: override_target.label.or(base.label),
        runtime: override_target.runtime.or(base.runtime),
        driver: override_target.driver.or(base.driver),
        entrypoint: override_target.entrypoint.or(base.entrypoint),
        run_command: override_target.run_command.or(base.run_command),
        cmd: if override_target.cmd.is_empty() {
            base.cmd
        } else {
            override_target.cmd
        },
        runtime_version: override_target.runtime_version.or(base.runtime_version),
        runtime_tools: if override_target.runtime_tools.is_empty() {
            base.runtime_tools
        } else {
            override_target.runtime_tools
        },
        dependencies_path: override_target.dependencies_path.or(base.dependencies_path),
    }
}

fn node_project_present(input: &LockDraftInput, target: &ResolvedTarget) -> bool {
    file_exists(input, "package.json")
        || target
            .dependencies_path
            .as_deref()
            .map(|path| normalize_path(path).ends_with("package.json"))
            .unwrap_or(false)
        || target
            .entrypoint
            .as_deref()
            .or(target.run_command.as_deref())
            .map(|command| {
                let lower = command.to_ascii_lowercase();
                lower.ends_with(".js")
                    || lower.ends_with(".mjs")
                    || lower.ends_with(".cjs")
                    || lower.ends_with(".ts")
                    || lower.ends_with(".tsx")
            })
            .unwrap_or(false)
}

fn python_project_present(input: &LockDraftInput, target: &ResolvedTarget) -> bool {
    file_exists(input, "pyproject.toml")
        || file_exists(input, "requirements.txt")
        || target
            .dependencies_path
            .as_deref()
            .map(|path| {
                let normalized = normalize_path(path);
                normalized.ends_with("pyproject.toml") || normalized.ends_with("requirements.txt")
            })
            .unwrap_or(false)
        || target
            .entrypoint
            .as_deref()
            .map(|entrypoint| entrypoint.to_ascii_lowercase().ends_with(".py"))
            .unwrap_or(false)
}

fn rust_project_present(input: &LockDraftInput, target: &ResolvedTarget) -> bool {
    file_exists(input, "Cargo.toml")
        || target
            .entrypoint
            .as_deref()
            .map(|entrypoint| entrypoint.to_ascii_lowercase().ends_with(".rs"))
            .unwrap_or(false)
}

const SUPPORTED_RUNTIME_PLATFORMS: &[(&str, &str, &str)] = &[
    ("macos", "x86_64", "x86_64-apple-darwin"),
    ("macos", "aarch64", "aarch64-apple-darwin"),
    ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
    ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
    ("windows", "x86_64", "x86_64-pc-windows-msvc"),
    ("windows", "aarch64", "aarch64-pc-windows-msvc"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn node_input() -> LockDraftInput {
        LockDraftInput {
            selected_target: Some(SelectedTarget {
                runtime: Some("source".to_string()),
                driver: Some("node".to_string()),
                entrypoint: Some("src/index.ts".to_string()),
                runtime_version: Some("20.12.0".to_string()),
                ..Default::default()
            }),
            repo_file_index: vec![
                RepoFileEntry {
                    path: "package.json".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
                RepoFileEntry {
                    path: "src/index.ts".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn node_lock_draft_requires_a_native_lockfile() {
        let draft = evaluate_lock_draft(&node_input()).expect("node draft");
        assert_eq!(draft.runtime.as_deref(), Some("source"));
        assert_eq!(draft.driver.as_deref(), Some("node"));
        assert_eq!(
            draft.missing_native_lockfiles,
            vec![
                "package-lock.json".to_string(),
                "pnpm-lock.yaml".to_string(),
                "yarn.lock".to_string(),
                "bun.lock".to_string(),
                "bun.lockb".to_string(),
            ]
        );
        assert_eq!(draft.readiness, LockDraftReadiness::Draft);
        assert_eq!(
            draft.suggested_commands,
            vec!["npm install --package-lock-only".to_string()]
        );
    }

    #[test]
    fn node_lock_draft_is_ready_when_lockfile_exists() {
        let mut input = node_input();
        input.repo_file_index.push(RepoFileEntry {
            path: "package-lock.json".to_string(),
            kind: RepoFileKind::File,
            size: None,
        });
        let draft = evaluate_lock_draft(&input).expect("node draft");
        assert_eq!(draft.missing_native_lockfiles, Vec::<String>::new());
        assert_eq!(draft.readiness, LockDraftReadiness::ReadyToFinalize);
        assert_eq!(draft.required_runtime_version.as_deref(), Some("20.12.0"));
        assert_eq!(
            draft.runtime_platforms.len(),
            SUPPORTED_RUNTIME_PLATFORMS.len()
        );
    }

    #[test]
    fn node_lock_draft_can_finalize_with_inferred_runtime_version() {
        let mut input = node_input();
        input.selected_target = Some(SelectedTarget {
            runtime_version: None,
            ..input.selected_target.expect("selected target")
        });
        input.repo_file_index.push(RepoFileEntry {
            path: "pnpm-lock.yaml".to_string(),
            kind: RepoFileKind::File,
            size: None,
        });
        input.file_text_map.insert(
            "package.json".to_string(),
            r#"{"name":"demo","packageManager":"pnpm@10.0.0"}"#.to_string(),
        );

        let draft = evaluate_lock_draft(&input).expect("node draft");
        assert_eq!(draft.readiness, LockDraftReadiness::ReadyToFinalize);
        assert_eq!(
            draft.required_runtime_version.as_deref(),
            Some(DEFAULT_NODE_RUNTIME_VERSION)
        );
        assert!(draft
            .warnings
            .iter()
            .any(|warning| warning.contains("runtime_version was inferred")));
    }

    #[test]
    fn python_requirements_prefers_uv_compile_hint() {
        let input = LockDraftInput {
            selected_target: Some(SelectedTarget {
                runtime: Some("source".to_string()),
                driver: Some("python".to_string()),
                entrypoint: Some("app.py".to_string()),
                runtime_version: Some("3.11.10".to_string()),
                ..Default::default()
            }),
            repo_file_index: vec![
                RepoFileEntry {
                    path: "requirements.txt".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
                RepoFileEntry {
                    path: "app.py".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
            ],
            ..Default::default()
        };
        let draft = evaluate_lock_draft(&input).expect("python draft");
        assert_eq!(draft.missing_native_lockfiles, vec!["uv.lock".to_string()]);
        assert_eq!(
            draft.suggested_commands,
            vec!["uv pip compile requirements.txt -o uv.lock".to_string()]
        );
    }

    #[test]
    fn manifest_source_can_supply_external_dependencies() {
        let input = LockDraftInput {
            manifest_source: Some(ManifestSource {
                text: r#"
default_target = "web"

[targets.web]
runtime = "web/static"
runtime_version = "2.6.8"

[[targets.web.external_dependencies]]
alias = "auth"
source = "capsule://store/acme/auth-svc"
source_type = "store"
"#
                .to_string(),
                selected_target_label: None,
            }),
            ..Default::default()
        };
        let draft = evaluate_lock_draft(&input).expect("manifest draft");
        assert_eq!(draft.external_capsule_dependencies.len(), 1);
        assert_eq!(draft.external_capsule_dependencies[0].name, "auth");
        assert_eq!(draft.runtime.as_deref(), Some("web"));
        assert_eq!(draft.driver.as_deref(), Some("static"));
    }

    #[test]
    fn json_entrypoint_and_lock_hash_are_stable() {
        let input = LockDraftInput {
            selected_target: Some(SelectedTarget {
                runtime: Some("source/deno".to_string()),
                entrypoint: Some("main.ts".to_string()),
                runtime_version: Some("2.6.8".to_string()),
                ..Default::default()
            }),
            repo_file_index: vec![
                RepoFileEntry {
                    path: "main.ts".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
                RepoFileEntry {
                    path: "deno.lock".to_string(),
                    kind: RepoFileKind::File,
                    size: None,
                },
            ],
            ..Default::default()
        };
        let left = evaluate_lock_draft(&input).expect("left");
        let right = evaluate_lock_draft(&input).expect("right");
        assert_eq!(left, right);
        assert!(left.draft_hash.starts_with("sha256:"));
    }
}
