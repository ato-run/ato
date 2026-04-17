use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::application::compat_import::{
    compile_compatibility_project, CompatibilityCompileResult, CompatibilityDiagnostic,
    CompatibilityDiagnosticSeverity, ProvenanceRecord as CompatibilityProvenanceRecord,
};
use crate::application::engine::build::native_delivery::{
    detect_build_strategy_with_legacy_fallback, imported_native_artifact_closure,
    imported_native_artifact_delivery_contract, imported_native_artifact_type,
    native_delivery_build_environment_skeleton, native_delivery_contract_from_build_plan,
    path_has_extension, NativeBuildCommand, NativeBuildPlan,
};
use crate::application::pipeline::cleanup::CleanupScope;
use crate::application::ports::OutputPort;
use crate::project::init::detect::{
    detect_project, DetectedProject, NodePackageManager, ProjectType,
};
use crate::project::init::recipe::{project_info_from_detection, ProjectInfo};
use crate::reporters::CliReporter;
use anyhow::{Context, Result};
use capsule_core::ato_lock::{
    self, closure_info, normalize_lock_closure, AtoLock, UnresolvedReason, UnresolvedValue,
};
use capsule_core::common::paths::{ato_cache_dir, ato_runs_dir, path_contains_workspace_state_dir};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::importer::{
    probe_ecosystem_lockfile_evidence, probe_native_framework_evidence, ImportedEvidence,
};
use capsule_core::input_resolver::{
    ResolvedCanonicalLock, ResolvedCompatibilityProject, ResolvedSingleScript, ResolvedSourceOnly,
    SingleScriptLanguage, ATO_LOCK_FILE_NAME,
};
use capsule_core::CapsuleReporter;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use walkdir::WalkDir;

const GLOBAL_RUN_SOURCE_INFERENCE_DIR: &str = "source-inference";
const WORKSPACE_RUN_SOURCE_INFERENCE_DIR: &str = ".ato/tmp/source-inference";
const SINGLE_SCRIPT_CACHE_SUBDIR: &str = "source-inference/single-script-cache";
#[derive(Debug, Clone)]
pub(crate) enum SourceInferenceInput {
    SourceEvidence(SourceEvidenceInput),
    DraftLock(DraftLockInput),
    CanonicalLock(CanonicalLockInput),
}

#[derive(Debug, Clone)]
pub(crate) struct SourceEvidenceInput {
    pub(crate) project_root: PathBuf,
    pub(crate) explicit_native_artifact: Option<PathBuf>,
    pub(crate) single_script_language: Option<SingleScriptLanguage>,
    pub(crate) authoritative_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct DraftLockInput {
    pub(crate) project_root: PathBuf,
    pub(crate) draft_lock: AtoLock,
    pub(crate) provenance: Vec<SourceInferenceProvenance>,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalLockInput {
    pub(crate) project_root: PathBuf,
    pub(crate) canonical_path: PathBuf,
    pub(crate) lock: AtoLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaterializationMode {
    RunAttempt,
    InitWorkspace,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceInferenceResult {
    pub(crate) input_kind: SourceInferenceInputKind,
    pub(crate) lock: AtoLock,
    pub(crate) provenance: Vec<SourceInferenceProvenance>,
    pub(crate) diagnostics: Vec<SourceInferenceDiagnostic>,
    pub(crate) infer: InferResult,
    pub(crate) resolve: ResolveResult,
    pub(crate) selection_gate: Option<SelectionGate>,
    pub(crate) approval_gate: Option<ApprovalGate>,
}

#[derive(Debug, Clone)]
struct InferredSourceDraft {
    result: SourceInferenceResult,
}

#[derive(Debug, Clone)]
struct ResolvedSourceModel {
    result: SourceInferenceResult,
    #[cfg_attr(not(test), allow(dead_code))]
    import_involved: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    build_derive_involved: bool,
}

#[derive(Debug, Clone)]
struct GatedSourceModel {
    result: SourceInferenceResult,
}

#[derive(Debug, Clone)]
struct MaterializationAdapter {
    workspace_root: PathBuf,
    project_root: PathBuf,
    original_manifest: Option<toml::Value>,
    use_global_run_state: bool,
}

#[derive(Debug, Clone)]
struct SourceNativeDeliveryPlan {
    plan: NativeBuildPlan,
    framework_evidence: Vec<ImportedEvidence>,
    closure_complete: bool,
}

#[derive(Debug, Clone)]
struct ImportedNativeArtifactCandidate {
    artifact_path: PathBuf,
    artifact_type: &'static str,
}

#[derive(Debug, Clone)]
struct DesktopExecutionOverride {
    process: Value,
    runtime: Value,
    resolved_target: Value,
    provenance_note: String,
    source_field: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ElectronBuilderDirectoriesConfig {
    output: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ElectronBuilderConfig {
    #[serde(rename = "productName")]
    product_name: Option<String>,
    directories: Option<ElectronBuilderDirectoriesConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceInferenceInputKind {
    SourceEvidence,
    DraftLock,
    CanonicalLock,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InferResult {
    pub(crate) candidate_sets: Vec<CandidateSet>,
    pub(crate) unresolved: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolveResult {
    pub(crate) resolved_process: bool,
    pub(crate) resolved_runtime: bool,
    pub(crate) resolved_target_compatibility: bool,
    pub(crate) resolved_dependency_closure: bool,
    pub(crate) unresolved: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CandidateSet {
    pub(crate) field: String,
    pub(crate) ranked: Vec<RankedCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RankedCandidate {
    pub(crate) label: String,
    pub(crate) score: u16,
    pub(crate) entrypoint: Vec<String>,
    pub(crate) rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SelectionGate {
    pub(crate) field: String,
    pub(crate) candidates: Vec<RankedCandidate>,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApprovalGate {
    pub(crate) capability: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceInferenceProvenanceKind {
    ExplicitArtifact,
    CompatibilityImport,
    CanonicalInput,
    DeterministicHeuristic,
    ImporterObservation,
    MetadataObservation,
    SelectionGate,
    ApprovalGate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SourceInferenceProvenance {
    pub(crate) field: String,
    pub(crate) kind: SourceInferenceProvenanceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) importer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceInferenceDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SourceInferenceDiagnostic {
    pub(crate) severity: SourceInferenceDiagnosticSeverity,
    pub(crate) field: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RunMaterialization {
    pub(crate) workspace_root: PathBuf,
    pub(crate) project_root: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) raw_manifest: Option<toml::Value>,
    pub(crate) lock: AtoLock,
    pub(crate) lock_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMaterialization {
    pub(crate) lock_path: PathBuf,
    pub(crate) sidecar_path: PathBuf,
    pub(crate) provenance_cache_path: PathBuf,
    pub(crate) binding_seed_path: PathBuf,
    pub(crate) policy_bundle_path: PathBuf,
    pub(crate) attestation_store_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct SourceInferenceSidecar {
    mode: MaterializationModeSerde,
    input_kind: SourceInferenceInputKind,
    provenance: Vec<SourceInferenceProvenance>,
    diagnostics: Vec<SourceInferenceDiagnostic>,
    selection_gate: Option<SelectionGate>,
    approval_gate: Option<ApprovalGate>,
    infer: InferResult,
    resolve: ResolveResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum MaterializationModeSerde {
    RunAttempt,
    InitWorkspace,
}

pub(crate) fn materialize_run_from_source_only(
    source: &ResolvedSourceOnly,
    scope: Option<&mut CleanupScope>,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<RunMaterialization> {
    let mut scope = scope;
    let adapter = prepare_run_materialization_adapter(source, scope.as_deref_mut())?;
    // Auto-copy .env.example → .env for directory-based projects (D2 feature).
    if source.single_script.is_none() {
        maybe_copy_env_example_for_source_run(&adapter.project_root);
    }
    let input = SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
        project_root: adapter.project_root.clone(),
        explicit_native_artifact: None,
        single_script_language: source.single_script.as_ref().map(|script| script.language),
        authoritative_root: Some(source.project_root.clone()),
    });
    let result =
        execute_shared_engine(input, MaterializationMode::RunAttempt, assume_yes, reporter)?;
    materialize_run_model(adapter, scope, result)
}

fn maybe_copy_env_example_for_source_run(project_root: &Path) {
    const EXAMPLE_NAMES: &[&str] = &[".env.example", ".env.template", ".env.sample"];
    for name in EXAMPLE_NAMES {
        let src = project_root.join(name);
        if src.exists() && !project_root.join(".env").exists() {
            if fs::copy(&src, project_root.join(".env")).is_ok() {
                eprintln!("✓ Copied {} → .env", name);
            }
            return;
        }
    }
}

pub(crate) fn materialize_run_from_explicit_native_artifact(
    artifact_path: &Path,
    scope: Option<&mut CleanupScope>,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<RunMaterialization> {
    let project_root = artifact_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let adapter = MaterializationAdapter {
        workspace_root: project_root.clone(),
        project_root: project_root.clone(),
        original_manifest: None,
        use_global_run_state: false,
    };
    let input = SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
        project_root,
        explicit_native_artifact: Some(artifact_path.to_path_buf()),
        single_script_language: None,
        authoritative_root: None,
    });
    let result =
        execute_shared_engine(input, MaterializationMode::RunAttempt, assume_yes, reporter)?;
    materialize_run_model(adapter, scope, result)
}

fn prepare_single_script_workspace(
    script: &ResolvedSingleScript,
    scope: Option<&mut CleanupScope>,
) -> Result<PathBuf> {
    match script.language {
        SingleScriptLanguage::Python => prepare_python_single_script_workspace(script, scope),
        SingleScriptLanguage::TypeScript | SingleScriptLanguage::JavaScript => {
            prepare_deno_single_script_workspace(script, scope)
        }
    }
}

fn prepare_durable_source_workspace(source: &ResolvedSourceOnly) -> Result<PathBuf> {
    if let Some(script) = source.single_script.as_ref() {
        match script.language {
            SingleScriptLanguage::Python => {
                prepare_durable_python_single_script_workspace(script, &source.project_root)?;
            }
            SingleScriptLanguage::TypeScript | SingleScriptLanguage::JavaScript => {
                prepare_durable_deno_single_script_workspace(script, &source.project_root)?;
            }
        }
    }

    Ok(source.project_root.clone())
}

fn prepare_run_materialization_adapter(
    source: &ResolvedSourceOnly,
    scope: Option<&mut CleanupScope>,
) -> Result<MaterializationAdapter> {
    let project_root = if let Some(script) = source.single_script.as_ref() {
        prepare_single_script_workspace(script, scope)?
    } else {
        source.project_root.clone()
    };
    Ok(MaterializationAdapter {
        workspace_root: source.project_root.clone(),
        project_root,
        original_manifest: None,
        use_global_run_state: source.single_script.is_some(),
    })
}

fn prepare_workspace_materialization_adapter(
    source: &ResolvedSourceOnly,
) -> Result<MaterializationAdapter> {
    let project_root = prepare_durable_source_workspace(source)?;
    Ok(MaterializationAdapter {
        workspace_root: source.project_root.clone(),
        project_root,
        original_manifest: None,
        use_global_run_state: false,
    })
}

fn prepare_python_single_script_workspace(
    script: &ResolvedSingleScript,
    _scope: Option<&mut CleanupScope>,
) -> Result<PathBuf> {
    let script_text = fs::read_to_string(&script.path)
        .with_context(|| format!("failed to read script {}", script.path.display()))?;
    let metadata = parse_pep723_python_metadata(&script_text)?;
    let cache_root = single_script_cache_root(script, &script_text)?;

    fs::create_dir_all(&cache_root).with_context(|| {
        format!(
            "failed to create single-script cache {}",
            cache_root.display()
        )
    })?;

    write_if_absent_or_same(&cache_root.join("main.py"), &script_text)?;

    if !metadata.dependencies.is_empty() {
        write_if_absent_or_same(
            &cache_root.join("requirements.txt"),
            &format!("{}\n", metadata.dependencies.join("\n")),
        )?;
    }

    let pyproject = python_pyproject_for_single_script(&metadata);
    write_if_absent_or_same(&cache_root.join("pyproject.toml"), &pyproject)?;

    generate_uv_lock_for_single_script(&cache_root)?;

    Ok(cache_root)
}

fn prepare_durable_python_single_script_workspace(
    script: &ResolvedSingleScript,
    project_root: &Path,
) -> Result<()> {
    let script_text = fs::read_to_string(&script.path)
        .with_context(|| format!("failed to read script {}", script.path.display()))?;
    let metadata = parse_pep723_python_metadata(&script_text)?;
    let entrypoint_path = project_root.join("main.py");

    if script.path != entrypoint_path {
        write_if_absent_or_same(&entrypoint_path, &script_text)?;
    }

    let pyproject = python_pyproject_for_single_script(&metadata);
    write_if_absent_or_same(&project_root.join("pyproject.toml"), &pyproject)?;

    generate_uv_lock_for_single_script(project_root)
}

fn generate_uv_lock_for_single_script(project_root: &Path) -> Result<()> {
    if project_root.join("uv.lock").exists() {
        return Ok(());
    }

    let output = std::process::Command::new("uv")
        .arg("lock")
        .current_dir(project_root)
        .output()
        .with_context(|| format!("failed to execute uv lock in {}", project_root.display()))?;

    if output.status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "failed to generate uv.lock for single-file Python script (status {}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn prepare_deno_single_script_workspace(
    script: &ResolvedSingleScript,
    _scope: Option<&mut CleanupScope>,
) -> Result<PathBuf> {
    let script_text = fs::read_to_string(&script.path)
        .with_context(|| format!("failed to read script {}", script.path.display()))?;
    let metadata = parse_deno_script_metadata(&script_text, &script.path);
    let cache_root = single_script_cache_root(script, &script_text)?;

    fs::create_dir_all(&cache_root).with_context(|| {
        format!(
            "failed to create single-script cache {}",
            cache_root.display()
        )
    })?;
    let entrypoint = deno_single_script_entrypoint_name(&script.path);
    write_if_absent_or_same(&cache_root.join(entrypoint), &script_text)?;
    let deno_json = deno_json_for_single_script(&metadata);
    write_if_absent_or_same(
        &cache_root.join("deno.json"),
        &(serde_json::to_string_pretty(&deno_json)
            .context("failed to serialize deno.json for single-file Deno script")?
            + "\n"),
    )?;

    generate_deno_lock_for_single_script(&cache_root, entrypoint)?;

    Ok(cache_root)
}

fn prepare_durable_deno_single_script_workspace(
    script: &ResolvedSingleScript,
    project_root: &Path,
) -> Result<()> {
    let script_text = fs::read_to_string(&script.path)
        .with_context(|| format!("failed to read script {}", script.path.display()))?;
    let metadata = parse_deno_script_metadata(&script_text, &script.path);
    let entrypoint = deno_single_script_entrypoint_name(&script.path);
    let entrypoint_path = project_root.join(entrypoint);

    if script.path != entrypoint_path {
        write_if_absent_or_same(&entrypoint_path, &script_text)?;
    }

    let deno_json_raw = serde_json::to_string_pretty(&deno_json_for_single_script(&metadata))
        .context("failed to serialize deno.json for durable single-file Deno init")?
        + "\n";
    write_if_absent_or_same(&project_root.join("deno.json"), &deno_json_raw)?;

    generate_deno_lock_for_single_script(project_root, entrypoint)
}

fn write_if_absent_or_same(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        let existing = fs::read_to_string(path)
            .with_context(|| format!("failed to read existing {}", path.display()))?;
        if existing == content {
            return Ok(());
        }

        anyhow::bail!(
            "refusing to overwrite existing file during durable single-file init: {}",
            path.display()
        );
    }

    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn single_script_cache_root(script: &ResolvedSingleScript, script_text: &str) -> Result<PathBuf> {
    let language = match script.language {
        SingleScriptLanguage::Python => "python",
        SingleScriptLanguage::TypeScript => "typescript",
        SingleScriptLanguage::JavaScript => "javascript",
    };
    let cache_key = crate::utils::hash::compute_sha256_hex(
        format!("v1\0{language}\0{}\0{}", script.path.display(), script_text).as_bytes(),
    );

    Ok(ato_cache_dir()
        .join(SINGLE_SCRIPT_CACHE_SUBDIR)
        .join(format!("{}-{}", language, &cache_key[..16])))
}

fn generate_deno_lock_for_single_script(project_root: &Path, entrypoint: &str) -> Result<()> {
    if project_root.join("deno.lock").exists() {
        return Ok(());
    }

    let output = std::process::Command::new("deno")
        .args(["cache", "--lock=deno.lock", entrypoint])
        .current_dir(project_root)
        .output()
        .with_context(|| format!("failed to execute deno cache in {}", project_root.display()))?;

    if output.status.success() {
        if project_root.join("deno.lock").exists() {
            return Ok(());
        }

        fs::write(project_root.join("deno.lock"), "{}\n").with_context(|| {
            format!(
                "deno cache succeeded but failed to synthesize empty deno.lock for {}",
                project_root.display()
            )
        })?;
        return Ok(());
    }

    if !output.status.success() {
        anyhow::bail!(
            "failed to generate deno.lock for single-file Deno script (status {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    anyhow::bail!(
        "deno cache finished without creating deno.lock for {}",
        project_root.display()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DenoScriptMetadata {
    is_jsx: bool,
    jsx_import_source: Option<String>,
    bare_imports: BTreeMap<String, String>,
}

fn is_deno_jsx_script(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("tsx") || value.eq_ignore_ascii_case("jsx"))
        .unwrap_or(false)
}

fn deno_single_script_entrypoint_name(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("tsx") => "main.tsx",
        Some("js") => "main.js",
        Some("jsx") => "main.jsx",
        _ => "main.ts",
    }
}

fn parse_deno_script_metadata(script_text: &str, path: &Path) -> DenoScriptMetadata {
    let jsx_import_source = script_text.lines().find_map(|line| {
        let marker = "@jsxImportSource";
        let index = line.find(marker)?;
        let value = line[index + marker.len()..]
            .trim()
            .trim_end_matches("*/")
            .trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    });

    DenoScriptMetadata {
        is_jsx: is_deno_jsx_script(path),
        jsx_import_source,
        bare_imports: infer_deno_bare_imports(script_text),
    }
}

fn deno_json_for_single_script(metadata: &DenoScriptMetadata) -> serde_json::Value {
    let mut root = serde_json::Map::new();

    if !metadata.bare_imports.is_empty() {
        root.insert(
            "imports".to_string(),
            serde_json::to_value(&metadata.bare_imports).unwrap_or_else(|_| json!({})),
        );
    }

    if metadata.is_jsx {
        root.insert(
            "compilerOptions".to_string(),
            json!({
                "jsx": "react-jsx",
                "jsxImportSource": metadata
                    .jsx_import_source
                    .clone()
                    .unwrap_or_else(|| "npm:react".to_string()),
            }),
        );
    }

    Value::Object(root)
}

fn infer_deno_bare_imports(script_text: &str) -> BTreeMap<String, String> {
    let mut imports = BTreeMap::new();
    for specifier in extract_script_import_specifiers(script_text) {
        if !is_bare_dependency_specifier(&specifier) {
            continue;
        }
        imports.insert(specifier.clone(), format!("npm:{specifier}"));
    }
    imports
}

fn extract_script_import_specifiers(script_text: &str) -> Vec<String> {
    let patterns = [
        r#"(?m)\b(?:import|export)\s[^\n;]*?\bfrom\s*[\"']([^\"']+)[\"']"#,
        r#"(?m)^\s*import\s*[\"']([^\"']+)[\"']"#,
        r#"(?m)\bimport\s*\(\s*[\"']([^\"']+)[\"']\s*\)"#,
        r#"(?m)\brequire\s*\(\s*[\"']([^\"']+)[\"']\s*\)"#,
    ];
    let mut specifiers = Vec::new();

    for pattern in patterns {
        let regex = Regex::new(pattern).expect("static import regex must compile");
        for captures in regex.captures_iter(script_text) {
            if let Some(specifier) = captures.get(1) {
                specifiers.push(specifier.as_str().to_string());
            }
        }
    }

    specifiers.sort();
    specifiers.dedup();
    specifiers
}

fn is_bare_dependency_specifier(specifier: &str) -> bool {
    let trimmed = specifier.trim();
    if trimmed.is_empty() {
        return false;
    }

    !(trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with("#")
        || trimmed.contains("://")
        || trimmed.starts_with("npm:")
        || trimmed.starts_with("jsr:")
        || trimmed.starts_with("node:")
        || trimmed.starts_with("file:")
        || trimmed.starts_with("data:"))
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Pep723PythonMetadata {
    dependencies: Vec<String>,
    requires_python: Option<String>,
}

fn python_pyproject_for_single_script(metadata: &Pep723PythonMetadata) -> String {
    let mut pyproject =
        String::from("[project]\nname = \"ato-single-script\"\nversion = \"0.1.0\"\n");
    if let Some(requires_python) = metadata.requires_python.as_deref() {
        pyproject.push_str(&format!("requires-python = \"{}\"\n", requires_python));
    }
    if !metadata.dependencies.is_empty() {
        pyproject.push_str("dependencies = [\n");
        for dependency in &metadata.dependencies {
            pyproject.push_str(&format!("  \"{}\",\n", dependency));
        }
        pyproject.push_str("]\n");
    }
    pyproject
}

fn parse_pep723_python_metadata(script_text: &str) -> Result<Pep723PythonMetadata> {
    let mut in_block = false;
    let mut block = Vec::new();

    for line in script_text.lines() {
        let trimmed = line.trim_start();
        if !in_block {
            if trimmed == "# /// script" {
                in_block = true;
            }
            continue;
        }

        if trimmed == "# ///" {
            let block_text = block.join("\n");
            if block_text.trim().is_empty() {
                return Ok(Pep723PythonMetadata::default());
            }
            let value: toml::Value = toml::from_str(&block_text)
                .with_context(|| "failed to parse PEP 723 script metadata block")?;
            let dependencies = value
                .get("dependencies")
                .and_then(toml::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let requires_python = value
                .get("requires-python")
                .and_then(toml::Value::as_str)
                .map(str::to_string);
            return Ok(Pep723PythonMetadata {
                dependencies,
                requires_python,
            });
        }

        let content = trimmed
            .strip_prefix("# ")
            .or_else(|| trimmed.strip_prefix('#'))
            .ok_or_else(|| anyhow::anyhow!("invalid PEP 723 metadata line: {}", line))?;
        block.push(content.to_string());
    }

    if in_block {
        anyhow::bail!("unterminated PEP 723 script metadata block");
    }

    Ok(Pep723PythonMetadata::default())
}

pub(crate) fn materialize_run_from_canonical_lock(
    canonical: &ResolvedCanonicalLock,
    scope: Option<&mut CleanupScope>,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<RunMaterialization> {
    let adapter = MaterializationAdapter {
        workspace_root: canonical.project_root.clone(),
        project_root: canonical.project_root.clone(),
        original_manifest: None,
        use_global_run_state: false,
    };
    let input = SourceInferenceInput::CanonicalLock(CanonicalLockInput {
        project_root: canonical.project_root.clone(),
        canonical_path: canonical.path.clone(),
        lock: canonical.lock.clone(),
    });
    let result =
        execute_shared_engine(input, MaterializationMode::RunAttempt, assume_yes, reporter)?;
    materialize_run_model(adapter, scope, result)
}

pub(crate) fn materialize_run_from_compatibility(
    project: &ResolvedCompatibilityProject,
    scope: Option<&mut CleanupScope>,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<RunMaterialization> {
    let (draft_input, compiled) = draft_lock_input_from_compatibility(project)?;
    let original_manifest =
        toml::from_str(&project.manifest.raw_text).unwrap_or_else(|_| project.manifest.raw.clone());
    let adapter = MaterializationAdapter {
        workspace_root: project.project_root.clone(),
        project_root: project.project_root.clone(),
        original_manifest: Some(original_manifest),
        use_global_run_state: false,
    };
    let mut result = execute_shared_engine(
        SourceInferenceInput::DraftLock(draft_input),
        MaterializationMode::RunAttempt,
        assume_yes,
        reporter,
    )?;
    result.diagnostics.extend(
        compiled
            .diagnostics
            .iter()
            .map(convert_compatibility_diagnostic),
    );
    materialize_run_model(adapter, scope, result)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn execute_init_from_source_only(
    project_root: &Path,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<WorkspaceMaterialization> {
    let source = ResolvedSourceOnly {
        project_root: project_root.to_path_buf(),
        single_script: None,
    };
    execute_init_from_resolved_source_only(&source, reporter, assume_yes)
}

pub(crate) fn execute_init_from_resolved_source_only(
    source: &ResolvedSourceOnly,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<WorkspaceMaterialization> {
    let adapter = prepare_workspace_materialization_adapter(source)?;
    let input = SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
        project_root: adapter.project_root.clone(),
        explicit_native_artifact: None,
        single_script_language: source.single_script.as_ref().map(|script| script.language),
        authoritative_root: Some(source.project_root.clone()),
    });
    let result = execute_shared_engine(
        input,
        MaterializationMode::InitWorkspace,
        assume_yes,
        reporter,
    )?;
    materialize_workspace_model(adapter, result)
}

pub(crate) fn execute_init_from_compatibility(
    project: &ResolvedCompatibilityProject,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<WorkspaceMaterialization> {
    let (draft_input, compiled) = draft_lock_input_from_compatibility(project)?;
    let adapter = MaterializationAdapter {
        workspace_root: project.project_root.clone(),
        project_root: project.project_root.clone(),
        original_manifest: None,
        use_global_run_state: false,
    };
    let mut result = execute_shared_engine(
        SourceInferenceInput::DraftLock(draft_input),
        MaterializationMode::InitWorkspace,
        assume_yes,
        reporter,
    )?;
    result.diagnostics.extend(
        compiled
            .diagnostics
            .iter()
            .map(convert_compatibility_diagnostic),
    );
    materialize_workspace_model(adapter, result)
}

pub(crate) fn execute_shared_engine(
    input: SourceInferenceInput,
    mode: MaterializationMode,
    assume_yes: bool,
    reporter: Arc<CliReporter>,
) -> Result<SourceInferenceResult> {
    let inferred = infer_phase(input)?;
    let resolved = resolve_phase(inferred)?;
    let gated = apply_gates_phase(resolved, mode, assume_yes, reporter)?;
    Ok(gated.result)
}

pub(crate) fn draft_lock_input_from_compatibility(
    project: &ResolvedCompatibilityProject,
) -> Result<(DraftLockInput, CompatibilityCompileResult)> {
    let compiled = compile_compatibility_project(project)?;
    let input = DraftLockInput {
        project_root: project.project_root.clone(),
        draft_lock: compiled.draft_lock.clone(),
        provenance: compiled
            .provenance
            .iter()
            .map(convert_compatibility_provenance)
            .collect(),
    };
    Ok((input, compiled))
}

fn infer_phase(input: SourceInferenceInput) -> Result<InferredSourceDraft> {
    let result = match input {
        SourceInferenceInput::SourceEvidence(input) => infer_from_source_evidence(input)?,
        SourceInferenceInput::DraftLock(input) => infer_from_draft_lock(input)?,
        SourceInferenceInput::CanonicalLock(input) => infer_from_canonical_lock(input)?,
    };
    Ok(InferredSourceDraft { result })
}

fn resolve_phase(mut inferred: InferredSourceDraft) -> Result<ResolvedSourceModel> {
    let import_involved = inferred
        .result
        .provenance
        .iter()
        .any(|record| record.kind == SourceInferenceProvenanceKind::CompatibilityImport);
    let build_derive_involved = resolve(&mut inferred.result)?;
    Ok(ResolvedSourceModel {
        result: inferred.result,
        import_involved,
        build_derive_involved,
    })
}

fn apply_gates_phase(
    mut resolved: ResolvedSourceModel,
    mode: MaterializationMode,
    assume_yes: bool,
    reporter: Arc<CliReporter>,
) -> Result<GatedSourceModel> {
    enforce_mode_preconditions(&mut resolved.result, mode, assume_yes, reporter)?;
    Ok(GatedSourceModel {
        result: resolved.result,
    })
}

fn infer_from_source_evidence(input: SourceEvidenceInput) -> Result<SourceInferenceResult> {
    let detected = detect_project(&input.project_root)?;
    let info = project_info_from_detection(&detected)?;
    let desktop_execution = infer_desktop_execution_override(
        &input.project_root,
        &detected,
        &info,
        input.explicit_native_artifact.as_deref(),
    )?;
    let metadata = source_metadata(
        &detected,
        input
            .authoritative_root
            .as_deref()
            .unwrap_or(input.project_root.as_path()),
        input.single_script_language,
    );
    let runtime_kind = desktop_execution
        .as_ref()
        .and_then(|override_contract| {
            override_contract
                .resolved_target
                .get("driver")
                .and_then(Value::as_str)
        })
        .unwrap_or_else(|| runtime_kind_from_project(&detected));
    let process_candidates = if desktop_execution.is_some() {
        Vec::new()
    } else {
        process_candidates_for_source(&detected, &info)
    };
    let mut lock = AtoLock::default();
    let mut provenance = vec![SourceInferenceProvenance {
        field: "contract.metadata".to_string(),
        kind: SourceInferenceProvenanceKind::ExplicitArtifact,
        source_path: Some(input.project_root.clone()),
        importer_id: None,
        evidence_kind: None,
        source_field: Some("project_root".to_string()),
        note: Some("source-only workspace analyzed for shared inference".to_string()),
    }];
    let mut diagnostics = Vec::new();
    let mut unresolved = Vec::new();
    let candidate_set = CandidateSet {
        field: "contract.process".to_string(),
        ranked: process_candidates.clone(),
    };

    lock.contract
        .entries
        .insert("metadata".to_string(), metadata);
    lock.contract.entries.insert(
        "network".to_string(),
        inferred_network_contract(&detected, input.single_script_language, &input.project_root),
    );
    lock.contract.entries.insert(
        "env_contract".to_string(),
        inferred_env_contract(&input.project_root),
    );
    lock.contract.entries.insert(
        "filesystem".to_string(),
        inferred_filesystem_contract(&detected),
    );
    let runtime_resolution = desktop_execution
        .as_ref()
        .map(|override_contract| override_contract.runtime.clone())
        .unwrap_or_else(|| inferred_runtime_resolution(&detected, &input.project_root));
    lock.resolution
        .entries
        .insert("runtime".to_string(), runtime_resolution.clone());
    lock.resolution.entries.insert(
        "resolved_targets".to_string(),
        desktop_execution
            .as_ref()
            .map(|override_contract| Value::Array(vec![override_contract.resolved_target.clone()]))
            .unwrap_or_else(|| {
                Value::Array(vec![{
                    let mut target = serde_json::Map::new();
                    target.insert("label".to_string(), Value::String("default".to_string()));
                    target.insert("runtime".to_string(), Value::String("source".to_string()));
                    if runtime_kind != "source" {
                        target.insert(
                            "driver".to_string(),
                            Value::String(runtime_kind.to_string()),
                        );
                    }
                    if input.single_script_language.is_some() {
                        target.insert(
                            "source_layout".to_string(),
                            Value::String("anchored_entrypoint".to_string()),
                        );
                    }
                    target.insert("compatible".to_string(), Value::Bool(true));
                    Value::Object(target)
                }])
            }),
    );
    lock.resolution.entries.insert(
        "closure".to_string(),
        inferred_closure_state(&input.project_root),
    );
    apply_source_native_delivery_inference(
        &input.project_root,
        &detected,
        input.explicit_native_artifact.as_deref(),
        &mut lock,
        &mut provenance,
        &mut diagnostics,
    )?;

    provenance.push(SourceInferenceProvenance {
        field: "resolution.runtime".to_string(),
        kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
        source_path: Some(input.project_root.clone()),
        importer_id: None,
        evidence_kind: None,
        source_field: Some("project_type".to_string()),
        note: Some(format!(
            "runtime resolved from deterministic project-type inference{}",
            runtime_resolution
                .get("version")
                .and_then(Value::as_str)
                .map(|version| format!(" with version {version}"))
                .unwrap_or_default()
        )),
    });
    for evidence in observed_closure_evidence(&input.project_root) {
        provenance.push(importer_observation_provenance(
            "resolution.closure",
            &evidence,
            "importer evidence observed while building metadata-only closure state",
        ));
    }

    if let Some(override_contract) = desktop_execution {
        lock.contract.entries.insert(
            "workloads".to_string(),
            Value::Array(vec![json!({
                "name": "main",
                "target": "desktop",
                "process": override_contract.process.clone(),
            })]),
        );
        lock.contract
            .entries
            .insert("process".to_string(), override_contract.process);
        lock.resolution.entries.insert(
            "target_selection".to_string(),
            json!({
                "default_target": "desktop",
                "source": "shared_source_inference",
            }),
        );
        provenance.push(SourceInferenceProvenance {
            field: "contract.process".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(input.project_root.clone()),
            importer_id: None,
            evidence_kind: Some("desktop_native_execution".to_string()),
            source_field: Some(override_contract.source_field.clone()),
            note: Some(override_contract.provenance_note.clone()),
        });
        provenance.push(SourceInferenceProvenance {
            field: "resolution.runtime".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(input.project_root.clone()),
            importer_id: None,
            evidence_kind: Some("desktop_native_execution".to_string()),
            source_field: Some(override_contract.source_field.clone()),
            note: Some(
                "desktop native execution overrides runtime selection to driver=native with a fixed desktop target"
                    .to_string(),
            ),
        });
        provenance.push(SourceInferenceProvenance {
            field: "resolution.resolved_targets".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(input.project_root.clone()),
            importer_id: None,
            evidence_kind: Some("desktop_native_execution".to_string()),
            source_field: Some(override_contract.source_field),
            note: Some(
                "desktop native execution records a single immutable target-compatible native run contract"
                    .to_string(),
            ),
        });
    } else if process_candidates.is_empty() {
        lock.contract
            .entries
            .insert("workloads".to_string(), Value::Array(Vec::new()));
        lock.contract.unresolved.push(UnresolvedValue {
            field: Some("contract.process".to_string()),
            reason: UnresolvedReason::InsufficientEvidence,
            detail: Some("could not infer a primary process from source evidence".to_string()),
            candidates: Vec::new(),
        });
        diagnostics.push(SourceInferenceDiagnostic {
            severity: SourceInferenceDiagnosticSeverity::Error,
            field: "contract.process".to_string(),
            message: "source inference could not determine a runnable process".to_string(),
        });
        unresolved.push("contract.process".to_string());
    } else if is_equal_ranked(&process_candidates) {
        lock.contract
            .entries
            .insert("workloads".to_string(), Value::Array(Vec::new()));
        lock.contract.unresolved.push(UnresolvedValue {
            field: Some("contract.process".to_string()),
            reason: UnresolvedReason::ExplicitSelectionRequired,
            detail: Some("multiple equal-ranked process candidates remain".to_string()),
            candidates: process_candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect(),
        });
        diagnostics.push(SourceInferenceDiagnostic {
            severity: SourceInferenceDiagnosticSeverity::Warning,
            field: "contract.process".to_string(),
            message: "multiple equal-ranked process candidates require explicit selection"
                .to_string(),
        });
        unresolved.push("contract.process".to_string());
    } else if let Some(candidate) = process_candidates.first() {
        lock.contract.entries.insert(
            "workloads".to_string(),
            Value::Array(vec![json!({
                "name": "main",
                "process": process_value_from_candidate(Some(input.project_root.as_path()), Some(candidate)),
            })]),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            process_value_from_candidate(Some(input.project_root.as_path()), Some(candidate)),
        );
        provenance.push(SourceInferenceProvenance {
            field: "contract.process".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(input.project_root.clone()),
            importer_id: None,
            evidence_kind: None,
            source_field: Some(candidate.label.clone()),
            note: Some(candidate.rationale.clone()),
        });
    }

    Ok(SourceInferenceResult {
        input_kind: SourceInferenceInputKind::SourceEvidence,
        lock,
        provenance,
        diagnostics,
        infer: InferResult {
            candidate_sets: if process_candidates.is_empty() {
                Vec::new()
            } else {
                vec![candidate_set]
            },
            unresolved,
        },
        resolve: ResolveResult {
            resolved_process: false,
            resolved_runtime: false,
            resolved_target_compatibility: false,
            resolved_dependency_closure: false,
            unresolved: Vec::new(),
        },
        selection_gate: None,
        approval_gate: None,
    })
}

fn infer_from_draft_lock(input: DraftLockInput) -> Result<SourceInferenceResult> {
    let mut lock = input.draft_lock;
    let mut provenance = input.provenance;
    promote_draft_execution_resolution(&mut lock, &input.project_root, &mut provenance);

    let mut infer_unresolved = Vec::new();
    if !lock.contract.entries.contains_key("process") {
        infer_unresolved.push("contract.process".to_string());
    }
    if !lock.resolution.entries.contains_key("runtime") {
        infer_unresolved.push("resolution.runtime".to_string());
    }
    if !lock.resolution.entries.contains_key("closure") {
        infer_unresolved.push("resolution.closure".to_string());
    }

    Ok(SourceInferenceResult {
        input_kind: SourceInferenceInputKind::DraftLock,
        lock,
        provenance,
        diagnostics: Vec::new(),
        infer: InferResult {
            candidate_sets: Vec::new(),
            unresolved: infer_unresolved,
        },
        resolve: ResolveResult {
            resolved_process: false,
            resolved_runtime: false,
            resolved_target_compatibility: false,
            resolved_dependency_closure: false,
            unresolved: Vec::new(),
        },
        selection_gate: None,
        approval_gate: None,
    })
}

fn infer_from_canonical_lock(input: CanonicalLockInput) -> Result<SourceInferenceResult> {
    let mut provenance = vec![SourceInferenceProvenance {
        field: "root".to_string(),
        kind: SourceInferenceProvenanceKind::CanonicalInput,
        source_path: Some(input.canonical_path),
        importer_id: None,
        evidence_kind: None,
        source_field: Some(ATO_LOCK_FILE_NAME.to_string()),
        note: Some("persisted canonical lock reused as shared source inference input".to_string()),
    }];
    let mut infer_unresolved = Vec::new();
    if !input.lock.contract.entries.contains_key("process") {
        infer_unresolved.push("contract.process".to_string());
    }
    if !input.lock.resolution.entries.contains_key("runtime") {
        infer_unresolved.push("resolution.runtime".to_string());
    }
    provenance.push(SourceInferenceProvenance {
        field: "contract.process".to_string(),
        kind: SourceInferenceProvenanceKind::CanonicalInput,
        source_path: Some(input.project_root),
        importer_id: None,
        evidence_kind: None,
        source_field: Some("ato.lock.json".to_string()),
        note: Some(
            "canonical lock drives run/init materialization without re-inferring semantics"
                .to_string(),
        ),
    });

    Ok(SourceInferenceResult {
        input_kind: SourceInferenceInputKind::CanonicalLock,
        lock: input.lock,
        provenance,
        diagnostics: Vec::new(),
        infer: InferResult {
            candidate_sets: Vec::new(),
            unresolved: infer_unresolved,
        },
        resolve: ResolveResult {
            resolved_process: false,
            resolved_runtime: false,
            resolved_target_compatibility: false,
            resolved_dependency_closure: false,
            unresolved: Vec::new(),
        },
        selection_gate: None,
        approval_gate: None,
    })
}

fn promote_draft_execution_resolution(
    lock: &mut AtoLock,
    project_root: &Path,
    provenance: &mut Vec<SourceInferenceProvenance>,
) {
    if !lock.resolution.entries.contains_key("runtime") {
        if let Some(runtime) = draft_runtime_from_resolution(lock) {
            lock.resolution
                .entries
                .insert("runtime".to_string(), runtime);
            provenance.push(SourceInferenceProvenance {
                field: "resolution.runtime".to_string(),
                kind: SourceInferenceProvenanceKind::CompatibilityImport,
                source_path: Some(project_root.to_path_buf()),
                importer_id: None,
                evidence_kind: None,
                source_field: Some("resolution.target_selection".to_string()),
                note: Some(
                    "draft compatibility target hints promoted into an execution-ready runtime"
                        .to_string(),
                ),
            });
        }
    }

    if !lock.resolution.entries.contains_key("closure") {
        lock.resolution
            .entries
            .insert("closure".to_string(), inferred_closure_state(project_root));
        provenance.push(SourceInferenceProvenance {
            field: "resolution.closure".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(project_root.to_path_buf()),
            importer_id: None,
            evidence_kind: None,
            source_field: Some("project_root".to_string()),
            note: Some(
                "dependency closure remained unresolved in the draft lock, so run uses metadata-only/incomplete observed lockfile state"
                    .to_string(),
            ),
        });
        for evidence in observed_closure_evidence(project_root) {
            provenance.push(importer_observation_provenance(
                "resolution.closure",
                &evidence,
                "importer evidence observed while promoting draft closure state",
            ));
        }
    }
}

fn draft_runtime_from_resolution(lock: &AtoLock) -> Option<Value> {
    let selected_target = selected_draft_target(lock)?;
    let kind = selected_target
        .get("driver")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            selected_target
                .get("runtime")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| sole_object_key(lock.resolution.entries.get("runtime_hints")))
        .or_else(|| sole_object_key(lock.resolution.entries.get("locked_runtimes")))?;

    let version = selected_target
        .get("runtime_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            lock.resolution
                .entries
                .get("runtime_hints")
                .and_then(|value| value.get(&kind))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            lock.resolution
                .entries
                .get("locked_runtimes")
                .and_then(|value| value.get(&kind))
                .and_then(|value| value.get("version"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });

    let mut runtime = serde_json::Map::new();
    runtime.insert("kind".to_string(), Value::String(kind));
    runtime.insert(
        "resolved_by".to_string(),
        Value::String("compatibility_target_selection".to_string()),
    );
    if let Some(label) = selected_target.get("label").and_then(Value::as_str) {
        runtime.insert(
            "selected_target".to_string(),
            Value::String(label.to_string()),
        );
    }
    if let Some(version) = version {
        runtime.insert("version".to_string(), Value::String(version));
    }

    Some(Value::Object(runtime))
}

fn selected_draft_target(lock: &AtoLock) -> Option<&Value> {
    let targets = lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(Value::as_array)?;

    let default_target = lock
        .resolution
        .entries
        .get("target_selection")
        .and_then(|value| value.get("default_target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(default_target) = default_target {
        if let Some(target) = targets.iter().find(|target| {
            target
                .get("label")
                .and_then(Value::as_str)
                .map(|label| label == default_target)
                .unwrap_or(false)
        }) {
            return Some(target);
        }
    }

    if targets.len() == 1 {
        return targets.first();
    }

    None
}

fn sole_object_key(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.keys().next().cloned()
}

fn resolve(result: &mut SourceInferenceResult) -> Result<bool> {
    let build_derive_involved = maybe_promote_native_build_closure(result)?;
    normalize_lock_closure(&mut result.lock)?;
    ensure_incomplete_closure_unresolved_marker(&mut result.lock)?;

    let process_resolved = result.lock.contract.entries.contains_key("process");
    let runtime_resolved = result.lock.resolution.entries.contains_key("runtime");
    let target_resolved = result
        .lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(Value::as_array)
        .map(|targets| !targets.is_empty())
        .unwrap_or(false);
    let closure_resolved = result.lock.resolution.entries.contains_key("closure");

    let unresolved = collect_unresolved_paths(&result.lock);
    result.resolve = ResolveResult {
        resolved_process: process_resolved,
        resolved_runtime: runtime_resolved,
        resolved_target_compatibility: target_resolved,
        resolved_dependency_closure: closure_resolved,
        unresolved: unresolved.clone(),
    };

    if !process_resolved {
        if let Some(gate) = selection_gate_from_lock(&result.lock, &result.infer.candidate_sets) {
            result.selection_gate = Some(gate);
        }
    }

    Ok(build_derive_involved)
}

fn maybe_promote_native_build_closure(result: &mut SourceInferenceResult) -> Result<bool> {
    if matches!(result.input_kind, SourceInferenceInputKind::CanonicalLock) {
        return Ok(false);
    }

    let should_promote = match result.lock.resolution.entries.get("closure") {
        None => true,
        Some(closure) => {
            let info = closure_info(closure)?;
            info.status == "incomplete"
        }
    };
    if !should_promote {
        return Ok(false);
    }

    let Some(project_root) = result_project_root(result) else {
        return Ok(false);
    };
    let Ok(Some(plan)) = detect_promotable_native_build_plan(&project_root) else {
        return Ok(false);
    };

    let mut imported_evidence = observed_closure_evidence(&plan.workspace_root);
    imported_evidence.extend(
        probe_native_framework_evidence(&plan.workspace_root)?
            .into_iter()
            .filter(|evidence| evidence.importer_id.as_str() == plan.framework.as_str()),
    );
    let inputs = imported_evidence
        .iter()
        .map(|evidence| {
            json!({
                "kind": evidence.evidence_kind.as_str(),
                "name": evidence.importer_id.as_str(),
                "digest": evidence.digest,
            })
        })
        .collect::<Vec<_>>();

    result.lock.resolution.entries.insert(
        "closure".to_string(),
        json!({
            "kind": "build_closure",
            "status": "complete",
            "inputs": inputs,
            "build_environment": native_delivery_build_environment_skeleton(&plan),
        }),
    );
    result.lock.contract.entries.insert(
        "delivery".to_string(),
        native_delivery_contract_from_build_plan(&plan, "source-derivation", "complete")?,
    );
    result
        .lock
        .resolution
        .unresolved
        .retain(|value| value.field.as_deref() != Some("resolution.closure"));
    result.provenance.push(SourceInferenceProvenance {
        field: "resolution.closure".to_string(),
        kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
        source_path: Some(plan.workspace_root.clone()),
        importer_id: None,
        evidence_kind: None,
        source_field: Some("[artifact]/[finalize]".to_string()),
        note: Some(
            "native delivery build plan resolved into build_closure using observed lockfiles and build-environment skeleton"
                .to_string(),
        ),
    });
    for evidence in &imported_evidence {
        result.provenance.push(importer_observation_provenance(
            "resolution.closure",
            evidence,
            "importer evidence promoted into build_closure input",
        ));
    }
    result.provenance.push(SourceInferenceProvenance {
        field: "contract.delivery".to_string(),
        kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
        source_path: Some(plan.workspace_root.clone()),
        importer_id: None,
        evidence_kind: None,
        source_field: Some("[artifact]/[finalize]".to_string()),
        note: Some(
            "native delivery contract promoted from source-draft to source-derivation after build_closure resolved"
                .to_string(),
        ),
    });

    Ok(true)
}

fn detect_promotable_native_build_plan(project_root: &Path) -> Result<Option<NativeBuildPlan>> {
    let manifest_path = project_root.join("capsule.toml");
    if manifest_path.is_file() {
        let decision = capsule_core::router::route_manifest(
            &manifest_path,
            capsule_core::router::ExecutionProfile::Release,
            None,
        )?;
        if let Some(plan) = detect_build_strategy_with_legacy_fallback(&decision.plan)? {
            return Ok(Some(plan));
        }
    }

    let detected = detect_project(project_root)?;
    let Some(plan) = infer_source_native_delivery_plan(project_root, &detected)? else {
        return Ok(None);
    };
    if plan.closure_complete {
        return Ok(Some(plan.plan));
    }
    Ok(None)
}

fn apply_source_native_delivery_inference(
    project_root: &Path,
    detected: &DetectedProject,
    explicit_native_artifact: Option<&Path>,
    lock: &mut AtoLock,
    provenance: &mut Vec<SourceInferenceProvenance>,
    diagnostics: &mut Vec<SourceInferenceDiagnostic>,
) -> Result<()> {
    if let Some(explicit_artifact) = explicit_native_artifact {
        if let Some(artifact_type) = imported_native_artifact_type(explicit_artifact) {
            lock.contract.entries.insert(
                "delivery".to_string(),
                imported_native_artifact_delivery_contract(
                    &relative_or_absolute_path(project_root, explicit_artifact),
                    artifact_type,
                ),
            );
            lock.resolution.entries.insert(
                "closure".to_string(),
                imported_native_artifact_closure(explicit_artifact, artifact_type)?,
            );
            provenance.push(SourceInferenceProvenance {
                field: "contract.delivery".to_string(),
                kind: SourceInferenceProvenanceKind::ExplicitArtifact,
                source_path: Some(explicit_artifact.to_path_buf()),
                importer_id: None,
                evidence_kind: Some("native_artifact".to_string()),
                source_field: Some(artifact_type.to_string()),
                note: Some(
                    "explicit native artifact input forces artifact-import delivery semantics for source-started run"
                        .to_string(),
                ),
            });
            provenance.push(SourceInferenceProvenance {
                field: "resolution.closure".to_string(),
                kind: SourceInferenceProvenanceKind::ExplicitArtifact,
                source_path: Some(explicit_artifact.to_path_buf()),
                importer_id: None,
                evidence_kind: Some("native_artifact".to_string()),
                source_field: Some(artifact_type.to_string()),
                note: Some(
                    "explicit native artifact input hashes the selected artifact into imported_artifact_closure"
                        .to_string(),
                ),
            });
            return Ok(());
        }
    }

    if let Some(plan) = infer_source_native_delivery_plan(project_root, detected)? {
        lock.contract.entries.insert(
            "delivery".to_string(),
            native_delivery_contract_from_build_plan(&plan.plan, "source-draft", "incomplete")?,
        );
        provenance.push(SourceInferenceProvenance {
            field: "contract.delivery".to_string(),
            kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
            source_path: Some(project_root.to_path_buf()),
            importer_id: None,
            evidence_kind: None,
            source_field: Some("framework-source".to_string()),
            note: Some(
                "source-only native framework evidence compiled into a durable desktop delivery draft"
                    .to_string(),
            ),
        });
        for evidence in &plan.framework_evidence {
            provenance.push(importer_observation_provenance(
                "contract.delivery",
                evidence,
                "framework importer evidence observed while creating native delivery draft",
            ));
        }
        if !plan.closure_complete {
            diagnostics.push(SourceInferenceDiagnostic {
                severity: SourceInferenceDiagnosticSeverity::Warning,
                field: "resolution.closure".to_string(),
                message: "native delivery source was detected, but build closure remains incomplete until required lockfile evidence is materialized".to_string(),
            });
        }
        return Ok(());
    }

    match infer_imported_native_artifact_candidate(project_root)? {
        ImportedArtifactProbe::Single(candidate) => {
            lock.contract.entries.insert(
                "delivery".to_string(),
                imported_native_artifact_delivery_contract(
                    &relative_or_absolute_path(project_root, &candidate.artifact_path),
                    candidate.artifact_type,
                ),
            );
            lock.resolution.entries.insert(
                "closure".to_string(),
                imported_native_artifact_closure(&candidate.artifact_path, candidate.artifact_type)?,
            );
            provenance.push(SourceInferenceProvenance {
                field: "contract.delivery".to_string(),
                kind: SourceInferenceProvenanceKind::ExplicitArtifact,
                source_path: Some(candidate.artifact_path.clone()),
                importer_id: None,
                evidence_kind: Some("native_artifact".to_string()),
                source_field: Some(candidate.artifact_type.to_string()),
                note: Some(
                    "existing native artifact detected in source-only workspace; delivery mode is artifact-import"
                        .to_string(),
                ),
            });
            provenance.push(SourceInferenceProvenance {
                field: "resolution.closure".to_string(),
                kind: SourceInferenceProvenanceKind::ExplicitArtifact,
                source_path: Some(candidate.artifact_path),
                importer_id: None,
                evidence_kind: Some("native_artifact".to_string()),
                source_field: Some(candidate.artifact_type.to_string()),
                note: Some(
                    "existing native artifact hashed as imported_artifact_closure with provenance-limited semantics"
                        .to_string(),
                ),
            });
        }
        ImportedArtifactProbe::Ambiguous(paths) => diagnostics.push(SourceInferenceDiagnostic {
            severity: SourceInferenceDiagnosticSeverity::Warning,
            field: "contract.delivery".to_string(),
            message: format!(
                "multiple imported native artifact candidates were detected ({}) so source-only init did not choose one automatically",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
        ImportedArtifactProbe::None => {}
    }

    Ok(())
}

#[derive(Debug, Clone)]
enum ImportedArtifactProbe {
    None,
    Single(ImportedNativeArtifactCandidate),
    Ambiguous(Vec<PathBuf>),
}

fn infer_imported_native_artifact_candidate(project_root: &Path) -> Result<ImportedArtifactProbe> {
    let mut candidates = Vec::new();
    if let Some(artifact_type) = imported_native_artifact_type(project_root) {
        candidates.push(ImportedNativeArtifactCandidate {
            artifact_path: project_root.to_path_buf(),
            artifact_type,
        });
    }

    for entry in WalkDir::new(project_root)
        .follow_links(false)
        .sort_by_file_name()
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| should_walk_source_entry(project_root, entry.path()))
    {
        let entry = entry?;
        let path = entry.path();
        if path == project_root {
            continue;
        }
        if let Some(artifact_type) = imported_native_artifact_type(path) {
            candidates.push(ImportedNativeArtifactCandidate {
                artifact_path: path.to_path_buf(),
                artifact_type,
            });
        }
    }

    candidates.sort_by(|left, right| left.artifact_path.cmp(&right.artifact_path));
    candidates.dedup_by(|left, right| left.artifact_path == right.artifact_path);

    Ok(match candidates.len() {
        0 => ImportedArtifactProbe::None,
        1 => ImportedArtifactProbe::Single(candidates.remove(0)),
        _ => ImportedArtifactProbe::Ambiguous(
            candidates
                .into_iter()
                .map(|candidate| candidate.artifact_path)
                .collect(),
        ),
    })
}

fn infer_source_native_delivery_plan(
    project_root: &Path,
    detected: &DetectedProject,
) -> Result<Option<SourceNativeDeliveryPlan>> {
    let framework_evidence = probe_native_framework_evidence(project_root)?;
    let mut frameworks = framework_evidence
        .iter()
        .map(|evidence| evidence.importer_id)
        .collect::<Vec<_>>();
    frameworks.sort();
    frameworks.dedup();
    let [framework] = frameworks.as_slice() else {
        return Ok(None);
    };

    let framework_name = framework.as_str().to_string();
    let artifact_relative = detect_framework_artifact_relative(project_root, *framework)
        .unwrap_or_else(|| default_framework_artifact_relative(project_root, detected, *framework));
    let target = inferred_delivery_target(&artifact_relative);
    let staged_delivery_config_toml =
        inferred_delivery_config_toml(&framework_name, &target, &artifact_relative);
    let build_command = infer_source_native_build_command(project_root, detected, *framework);
    let plan = NativeBuildPlan {
        workspace_root: project_root.to_path_buf(),
        legacy_manifest_bridge: None,
        package_name: detected.name.clone(),
        package_version: infer_project_version(detected, project_root)
            .unwrap_or_else(|| "0.1.0".to_string()),
        delivery_config_path: None,
        staged_delivery_config_toml,
        source_app_path: project_root.join(&artifact_relative),
        input_relative: artifact_relative,
        build_command,
        framework: framework_name,
        target,
    };
    Ok(Some(SourceNativeDeliveryPlan {
        closure_complete: source_native_closure_complete(project_root, *framework, &plan),
        plan,
        framework_evidence,
    }))
}

fn detect_framework_artifact_relative(
    project_root: &Path,
    framework: capsule_core::importer::ImporterId,
) -> Option<PathBuf> {
    let roots = match framework {
        capsule_core::importer::ImporterId::Tauri => vec![
            project_root.join("src-tauri/target/release/bundle"),
            project_root.join("src-tauri/target/release"),
        ],
        capsule_core::importer::ImporterId::Electron => vec![
            project_root.join("dist"),
            project_root.join("out"),
            project_root.join("release"),
        ],
        capsule_core::importer::ImporterId::Wails => {
            vec![project_root.join("build/bin"), project_root.join("dist")]
        }
        _ => Vec::new(),
    };

    let mut candidates = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .sort_by_file_name()
            .max_depth(6)
            .into_iter()
            .filter_entry(|entry| should_walk_source_entry(project_root, entry.path()))
        {
            let entry = entry.ok()?;
            let path = entry.path();
            if imported_native_artifact_type(path).is_some() {
                let relative = path.strip_prefix(project_root).ok()?.to_path_buf();
                candidates.push(relative);
            }
        }
    }

    candidates.sort();
    candidates.dedup();
    candidates.into_iter().next()
}

fn should_walk_source_entry(project_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(project_root) else {
        return true;
    };
    if relative.as_os_str().is_empty() {
        return true;
    }

    !path_contains_workspace_state_dir(relative)
}

fn default_framework_artifact_relative(
    project_root: &Path,
    detected: &DetectedProject,
    framework: capsule_core::importer::ImporterId,
) -> PathBuf {
    let file_name = match std::env::consts::OS {
        "windows" => format!("{}.exe", detected.name),
        "linux" => format!("{}.AppImage", detected.name),
        _ => format!("{}.app", detected.name),
    };

    match framework {
        capsule_core::importer::ImporterId::Tauri => match std::env::consts::OS {
            "windows" => PathBuf::from(format!("src-tauri/target/release/{}", file_name)),
            "linux" => PathBuf::from(format!(
                "src-tauri/target/release/bundle/appimage/{}",
                file_name
            )),
            _ => PathBuf::from(format!(
                "src-tauri/target/release/bundle/macos/{}",
                file_name
            )),
        },
        capsule_core::importer::ImporterId::Electron => {
            default_electron_artifact_relative(project_root, detected, &file_name)
        }
        capsule_core::importer::ImporterId::Wails => {
            PathBuf::from(format!("build/bin/{}", file_name))
        }
        _ => PathBuf::from(file_name),
    }
}

fn default_electron_artifact_relative(
    project_root: &Path,
    detected: &DetectedProject,
    default_file_name: &str,
) -> PathBuf {
    let builder_config = read_electron_builder_config(project_root);
    let output_root = builder_config
        .as_ref()
        .and_then(|config| config.directories.as_ref())
        .and_then(|directories| directories.output.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("dist");

    let product_name = builder_config
        .as_ref()
        .and_then(|config| config.product_name.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&detected.name);

    match std::env::consts::OS {
        "windows" => PathBuf::from(output_root).join(default_file_name),
        "linux" => PathBuf::from(output_root).join(default_file_name),
        _ => PathBuf::from(output_root)
            .join(electron_builder_macos_output_dir())
            .join(format!("{}.app", product_name)),
    }
}

fn electron_builder_macos_output_dir() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "mac",
        _ => "mac-arm64",
    }
}

fn read_electron_builder_config(project_root: &Path) -> Option<ElectronBuilderConfig> {
    let yaml_candidates = [
        project_root.join("electron-builder.yml"),
        project_root.join("electron-builder.yaml"),
    ];

    for path in yaml_candidates {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(parsed) = serde_yaml::from_str::<ElectronBuilderConfig>(&raw) {
            return Some(parsed);
        }
    }

    let json_path = project_root.join("electron-builder.json");
    if let Ok(raw) = fs::read_to_string(json_path) {
        if let Ok(parsed) = serde_json::from_str::<ElectronBuilderConfig>(&raw) {
            return Some(parsed);
        }
    }

    let package_json_path = project_root.join("package.json");
    let raw = fs::read_to_string(package_json_path).ok()?;
    let parsed = serde_json::from_str::<Value>(&raw).ok()?;
    let build = parsed.get("build")?.clone();
    serde_json::from_value::<ElectronBuilderConfig>(build).ok()
}

fn inferred_delivery_target(artifact_relative: &Path) -> String {
    if path_has_extension(artifact_relative, "exe") {
        return format!("windows/{}", normalized_delivery_arch());
    }
    if path_has_extension(artifact_relative, "AppImage") {
        return format!("linux/{}", normalized_delivery_arch());
    }
    match std::env::consts::ARCH {
        "x86_64" => "darwin/x86_64".to_string(),
        _ => "darwin/arm64".to_string(),
    }
}

fn inferred_delivery_config_toml(
    framework: &str,
    target: &str,
    artifact_relative: &Path,
) -> String {
    let input = artifact_relative.to_string_lossy();
    let (tool, args) = if path_has_extension(artifact_relative, "exe") {
        ("signtool", vec!["sign", "/fd", "SHA256", input.as_ref()])
    } else if path_has_extension(artifact_relative, "AppImage") {
        ("host-finalizer", vec![input.as_ref()])
    } else {
        (
            "codesign",
            vec!["--deep", "--force", "--sign", "-", input.as_ref()],
        )
    };
    let rendered_args = args
        .into_iter()
        .map(|value| format!("{:?}", value))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema_version = \"0.1\"\n[artifact]\nframework = {:?}\nstage = \"unsigned\"\ntarget = {:?}\ninput = {:?}\n[finalize]\ntool = {:?}\nargs = [{}]\n",
        framework, target, input.as_ref(), tool, rendered_args
    )
}

fn normalized_delivery_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

fn infer_source_native_build_command(
    project_root: &Path,
    detected: &DetectedProject,
    framework: capsule_core::importer::ImporterId,
) -> Option<NativeBuildCommand> {
    if let Some(node) = detected.node.as_ref() {
        if matches!(framework, capsule_core::importer::ImporterId::Tauri) && node.scripts.has_tauri
        {
            return Some(tauri_build_command(project_root, node.package_manager));
        }

        if matches!(framework, capsule_core::importer::ImporterId::Electron) {
            if let Some(script_name) = preferred_electron_packaged_build_script(project_root) {
                return Some(node_script_build_command(
                    project_root,
                    node.package_manager,
                    &script_name,
                ));
            }
        }

        if node.scripts.has_build {
            return Some(node_script_build_command(
                project_root,
                node.package_manager,
                "build",
            ));
        }
    }

    match framework {
        capsule_core::importer::ImporterId::Tauri
            if project_root.join("src-tauri/Cargo.toml").is_file() =>
        {
            Some(NativeBuildCommand {
                program: "cargo".to_string(),
                args: vec![
                    "build".to_string(),
                    "--manifest-path".to_string(),
                    "src-tauri/Cargo.toml".to_string(),
                    "--release".to_string(),
                ],
                working_dir: project_root.to_path_buf(),
            })
        }
        capsule_core::importer::ImporterId::Wails if project_root.join("wails.json").is_file() => {
            Some(NativeBuildCommand {
                program: "wails".to_string(),
                args: vec!["build".to_string()],
                working_dir: project_root.to_path_buf(),
            })
        }
        _ => None,
    }
}

fn preferred_electron_packaged_build_script(project_root: &Path) -> Option<String> {
    let package_json_path = project_root.join("package.json");
    let raw = fs::read_to_string(&package_json_path).ok()?;
    let parsed = serde_json::from_str::<Value>(&raw).ok()?;
    let scripts = parsed.get("scripts")?.as_object()?;

    let candidates: Vec<&str> = match std::env::consts::OS {
        "macos" => vec!["build:mac", "build:darwin", "build:desktop", "build:unpack"],
        "linux" => vec!["build:linux", "build:desktop", "build:unpack"],
        "windows" => vec![
            "build:win",
            "build:windows",
            "build:desktop",
            "build:unpack",
        ],
        _ => vec!["build:desktop", "build:unpack", "build:mac", "build:linux"],
    };

    for candidate in candidates {
        if scripts
            .get(candidate)
            .and_then(Value::as_str)
            .is_some_and(script_looks_like_electron_packaging)
        {
            return Some(candidate.to_string());
        }
    }

    if scripts
        .get("build")
        .and_then(Value::as_str)
        .is_some_and(script_looks_like_electron_packaging)
    {
        return Some("build".to_string());
    }

    None
}

fn script_looks_like_electron_packaging(script: &str) -> bool {
    let normalized = script.trim().to_ascii_lowercase();
    normalized.contains("electron-builder")
        || normalized.contains("electron-forge make")
        || normalized.contains("electron-packager")
}

fn node_script_build_command(
    project_root: &Path,
    package_manager: NodePackageManager,
    script_name: &str,
) -> NativeBuildCommand {
    let (program, args) = node_script_command(package_manager, script_name, &[]);
    NativeBuildCommand {
        program,
        args,
        working_dir: project_root.to_path_buf(),
    }
}

fn tauri_build_command(
    project_root: &Path,
    package_manager: NodePackageManager,
) -> NativeBuildCommand {
    let (program, args) = node_script_command(package_manager, "tauri", &["build"]);
    NativeBuildCommand {
        program,
        args,
        working_dir: project_root.to_path_buf(),
    }
}

fn node_script_command(
    package_manager: NodePackageManager,
    script_name: &str,
    trailing_args: &[&str],
) -> (String, Vec<String>) {
    match package_manager {
        NodePackageManager::Bun => {
            let mut args = vec!["run".to_string(), script_name.to_string()];
            args.extend(trailing_args.iter().map(|value| (*value).to_string()));
            ("bun".to_string(), args)
        }
        NodePackageManager::Deno => {
            let mut args = vec!["task".to_string(), script_name.to_string()];
            args.extend(trailing_args.iter().map(|value| (*value).to_string()));
            ("deno".to_string(), args)
        }
        NodePackageManager::Pnpm => {
            let mut args = vec![script_name.to_string()];
            args.extend(trailing_args.iter().map(|value| (*value).to_string()));
            ("pnpm".to_string(), args)
        }
        NodePackageManager::Yarn => {
            let mut args = vec![script_name.to_string()];
            args.extend(trailing_args.iter().map(|value| (*value).to_string()));
            ("yarn".to_string(), args)
        }
        NodePackageManager::Npm | NodePackageManager::Unknown => {
            let mut args = vec!["run".to_string(), script_name.to_string()];
            if !trailing_args.is_empty() {
                args.push("--".to_string());
                args.extend(trailing_args.iter().map(|value| (*value).to_string()));
            }
            ("npm".to_string(), args)
        }
    }
}

fn source_native_closure_complete(
    project_root: &Path,
    framework: capsule_core::importer::ImporterId,
    plan: &NativeBuildPlan,
) -> bool {
    if plan.build_command.is_none() {
        return false;
    }
    let observed = observed_closure_evidence(project_root)
        .into_iter()
        .map(|evidence| evidence.importer_id)
        .collect::<Vec<_>>();

    let has = |importer: capsule_core::importer::ImporterId| observed.contains(&importer);
    let has_node_lock = [
        capsule_core::importer::ImporterId::Npm,
        capsule_core::importer::ImporterId::Pnpm,
        capsule_core::importer::ImporterId::Yarn,
        capsule_core::importer::ImporterId::Bun,
        capsule_core::importer::ImporterId::Deno,
    ]
    .into_iter()
    .any(has);

    match framework {
        capsule_core::importer::ImporterId::Tauri => {
            has(capsule_core::importer::ImporterId::Cargo) && has_node_lock
        }
        capsule_core::importer::ImporterId::Electron => has_node_lock,
        capsule_core::importer::ImporterId::Wails => {
            has(capsule_core::importer::ImporterId::Go)
                && (!project_root.join("package.json").exists()
                    && !project_root.join("deno.json").exists()
                    && !project_root.join("deno.jsonc").exists()
                    || has_node_lock)
        }
        _ => false,
    }
}

fn relative_or_absolute_path(project_root: &Path, artifact_path: &Path) -> PathBuf {
    artifact_path
        .strip_prefix(project_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| artifact_path.to_path_buf())
}

fn infer_desktop_execution_override(
    project_root: &Path,
    detected: &DetectedProject,
    info: &ProjectInfo,
    explicit_native_artifact: Option<&Path>,
) -> Result<Option<DesktopExecutionOverride>> {
    if let Some(explicit_artifact) = explicit_native_artifact {
        if let Some(artifact_type) = imported_native_artifact_type(explicit_artifact) {
            return Ok(Some(desktop_execution_from_artifact(
                project_root,
                explicit_artifact,
                artifact_type,
                "explicit-native-artifact".to_string(),
                "explicit native artifact input fixed the desktop execution path before run"
                    .to_string(),
            )?));
        }
    }

    if let Some(plan) = infer_source_native_delivery_plan(project_root, detected)? {
        if let Some(process) =
            infer_source_native_run_process(project_root, detected, info, &plan.plan.framework)
        {
            return Ok(Some(desktop_execution_from_process(
                process,
                format!("framework:{}", plan.plan.framework),
                format!(
                    "desktop source-derived execution selected a native dev/run process for framework '{}'",
                    plan.plan.framework
                ),
            )));
        }

        if plan.plan.source_app_path.exists() {
            if let Some(artifact_type) = imported_native_artifact_type(&plan.plan.source_app_path) {
                return Ok(Some(desktop_execution_from_artifact(
                    project_root,
                    &plan.plan.source_app_path,
                    artifact_type,
                    format!("framework-artifact:{}", plan.plan.framework),
                    format!(
                        "desktop source-derived execution fell back to the built native artifact for framework '{}'",
                        plan.plan.framework
                    ),
                )?));
            }
        }
    }

    match infer_imported_native_artifact_candidate(project_root)? {
        ImportedArtifactProbe::Single(candidate) => Ok(Some(desktop_execution_from_artifact(
            project_root,
            &candidate.artifact_path,
            candidate.artifact_type,
            format!("artifact-import:{}", candidate.artifact_type),
            "desktop artifact-import execution selected the single observed native artifact"
                .to_string(),
        )?)),
        ImportedArtifactProbe::Ambiguous(_) | ImportedArtifactProbe::None => Ok(None),
    }
}

fn desktop_execution_from_process(
    process: Value,
    source_field: String,
    provenance_note: String,
) -> DesktopExecutionOverride {
    let mut resolved_target = serde_json::Map::new();
    resolved_target.insert("label".to_string(), Value::String("desktop".to_string()));
    resolved_target.insert("runtime".to_string(), Value::String("source".to_string()));
    resolved_target.insert("driver".to_string(), Value::String("native".to_string()));
    resolved_target.insert("compatible".to_string(), Value::Bool(true));
    if let Some(entrypoint) = process.get("entrypoint").cloned() {
        resolved_target.insert("entrypoint".to_string(), entrypoint);
    }
    if let Some(cmd) = process.get("cmd").cloned() {
        resolved_target.insert("cmd".to_string(), cmd);
    }
    if let Some(run_command) = process.get("run_command").cloned() {
        resolved_target.insert("run_command".to_string(), run_command);
    }

    DesktopExecutionOverride {
        process,
        runtime: json!({
            "kind": "native",
            "resolved_by": "shared_source_inference",
            "selected_target": "desktop",
        }),
        resolved_target: Value::Object(resolved_target),
        provenance_note,
        source_field,
    }
}

fn desktop_execution_from_artifact(
    project_root: &Path,
    artifact_path: &Path,
    artifact_type: &str,
    source_field: String,
    provenance_note: String,
) -> Result<DesktopExecutionOverride> {
    let launch_path = native_artifact_launch_path(artifact_path, artifact_type)?;
    Ok(desktop_execution_from_process(
        json!({
            "entrypoint": relative_or_absolute_path(project_root, &launch_path),
            "cmd": [],
        }),
        source_field,
        provenance_note,
    ))
}

fn infer_source_native_run_process(
    project_root: &Path,
    detected: &DetectedProject,
    info: &ProjectInfo,
    framework: &str,
) -> Option<Value> {
    if let Some(node) = detected.node.as_ref() {
        if node.scripts.has_dev {
            return node_package_manager_process(node.package_manager, "dev");
        }
        if node.scripts.has_start {
            return node_package_manager_process(node.package_manager, "start");
        }
    }

    match framework {
        "tauri" if project_root.join("src-tauri/Cargo.toml").is_file() => Some(json!({
            "entrypoint": "cargo",
            "cmd": ["run", "--manifest-path", "src-tauri/Cargo.toml"],
        })),
        "wails" if project_root.join("wails.json").is_file() => Some(json!({
            "entrypoint": "wails",
            "cmd": ["dev"],
        })),
        "electron" if !info.entrypoint.is_empty() => Some(json!({
            "entrypoint": info.entrypoint.first().cloned().unwrap_or_default(),
            "cmd": info.entrypoint.iter().skip(1).cloned().collect::<Vec<_>>(),
        })),
        _ => None,
    }
}

fn node_package_manager_process(
    package_manager: NodePackageManager,
    script_name: &str,
) -> Option<Value> {
    let (entrypoint, cmd) = match package_manager {
        NodePackageManager::Bun => (
            "bun".to_string(),
            vec!["run".to_string(), script_name.to_string()],
        ),
        NodePackageManager::Deno => (
            "deno".to_string(),
            vec!["task".to_string(), script_name.to_string()],
        ),
        NodePackageManager::Pnpm => ("pnpm".to_string(), vec![script_name.to_string()]),
        NodePackageManager::Yarn => ("yarn".to_string(), vec![script_name.to_string()]),
        NodePackageManager::Npm | NodePackageManager::Unknown => (
            "npm".to_string(),
            vec!["run".to_string(), script_name.to_string()],
        ),
    };
    Some(json!({
        "entrypoint": entrypoint,
        "cmd": cmd,
    }))
}

fn native_artifact_launch_path(artifact_path: &Path, artifact_type: &str) -> Result<PathBuf> {
    if artifact_type != "macos_app_bundle" {
        return Ok(artifact_path.to_path_buf());
    }

    let macos_dir = artifact_path.join("Contents").join("MacOS");
    let expected_name = artifact_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut files = fs::read_dir(&macos_dir)
        .with_context(|| format!("failed to read {}", macos_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate {}", macos_dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();

    if let Some(expected_name) = expected_name {
        if let Some(path) = files.iter().find(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value == expected_name)
                .unwrap_or(false)
        }) {
            return Ok(path.clone());
        }
    }

    match files.as_slice() {
        [single] => Ok(single.clone()),
        [] => anyhow::bail!(
            "native artifact run could not find an executable within {}",
            macos_dir.display()
        ),
        _ => anyhow::bail!(
            "native artifact run found multiple executable candidates within {}",
            macos_dir.display()
        ),
    }
}

fn result_project_root(result: &SourceInferenceResult) -> Option<PathBuf> {
    let path = result
        .provenance
        .iter()
        .find_map(|record| record.source_path.clone())?;

    if path.is_dir() {
        return Some(path);
    }

    Some(path.parent().map(Path::to_path_buf).unwrap_or(path))
}

fn ensure_incomplete_closure_unresolved_marker(lock: &mut AtoLock) -> Result<()> {
    let Some(closure) = lock.resolution.entries.get("closure") else {
        return Ok(());
    };

    let info = closure_info(closure)?;
    if info.status == "complete" {
        lock.resolution
            .unresolved
            .retain(|value| value.field.as_deref() != Some("resolution.closure"));
        return Ok(());
    }
    if info.kind != "metadata_only" && info.status != "incomplete" {
        return Ok(());
    }

    let detail = if info.kind == "metadata_only" {
        "dependency closure remains metadata-only and incomplete until durable dependency inputs are captured"
    } else {
        "dependency closure remains incomplete until all required dependency inputs are captured"
    };

    if let Some(unresolved) = lock
        .resolution
        .unresolved
        .iter_mut()
        .find(|value| value.field.as_deref() == Some("resolution.closure"))
    {
        unresolved.reason = UnresolvedReason::InsufficientEvidence;
        unresolved.detail = Some(detail.to_string());
        unresolved.candidates.clear();
        return Ok(());
    }

    lock.resolution.unresolved.push(UnresolvedValue {
        field: Some("resolution.closure".to_string()),
        reason: UnresolvedReason::InsufficientEvidence,
        detail: Some(detail.to_string()),
        candidates: Vec::new(),
    });

    Ok(())
}

fn enforce_mode_preconditions(
    result: &mut SourceInferenceResult,
    mode: MaterializationMode,
    assume_yes: bool,
    reporter: Arc<CliReporter>,
) -> Result<()> {
    if result.approval_gate.is_some() {
        anyhow::bail!(AtoExecutionError::manual_intervention_required(
            "script-capable resolution requires an explicit approval mode",
            None,
            vec![
                "re-run with an explicit source inference approval mode once implemented"
                    .to_string()
            ],
        ));
    }

    if let Some(selection_gate) = result.selection_gate.clone() {
        match mode {
            MaterializationMode::RunAttempt => {
                let selection = prompt_selection_if_allowed(&selection_gate, assume_yes, reporter)?;
                if let Some(selection) = selection {
                    apply_selection(result, &selection_gate.field, &selection)?;
                } else {
                    anyhow::bail!(AtoExecutionError::ambiguous_entrypoint(
                        selection_gate.message,
                        selection_gate
                            .candidates
                            .iter()
                            .map(|candidate| candidate.label.clone())
                            .collect(),
                    ));
                }
            }
            MaterializationMode::InitWorkspace => {
                if let Some(selection) =
                    prompt_selection_if_allowed(&selection_gate, assume_yes, reporter)?
                {
                    apply_selection(result, &selection_gate.field, &selection)?;
                }
            }
        }
    }

    if matches!(mode, MaterializationMode::RunAttempt) {
        if !result.lock.contract.entries.contains_key("process") {
            anyhow::bail!(AtoExecutionError::ambiguous_entrypoint(
                "run requires a selected process before execution",
                explicit_candidates(&result.lock),
            ));
        }
        if !result.lock.resolution.entries.contains_key("runtime") {
            anyhow::bail!(AtoExecutionError::runtime_not_resolved(
                "run requires a resolved runtime before execution",
                None,
            ));
        }
        if result
            .lock
            .resolution
            .entries
            .get("resolved_targets")
            .and_then(Value::as_array)
            .map(|targets| targets.is_empty())
            .unwrap_or(true)
        {
            anyhow::bail!(AtoExecutionError::execution_contract_invalid(
                "run requires at least one resolved target-compatible execution candidate",
                Some("resolution.resolved_targets"),
                None,
            ));
        }
        if !result.lock.resolution.entries.contains_key("closure") {
            anyhow::bail!(AtoExecutionError::lock_incomplete(
                "run requires dependency closure state before execution",
                Some("resolution.closure"),
            ));
        }
    }

    Ok(())
}

fn materialize_run_result(
    workspace_root: &Path,
    project_root: &Path,
    use_global_run_state: bool,
    result: SourceInferenceResult,
    mut scope: Option<&mut CleanupScope>,
    original_manifest: Option<&toml::Value>,
) -> Result<RunMaterialization> {
    let run_state_root = if use_global_run_state {
        ato_runs_dir().join(GLOBAL_RUN_SOURCE_INFERENCE_DIR)
    } else {
        workspace_root.join(WORKSPACE_RUN_SOURCE_INFERENCE_DIR)
    };
    let run_state_dir = run_state_root.join(unique_attempt_token());
    fs::create_dir_all(&run_state_dir)
        .with_context(|| format!("Failed to create {}", run_state_dir.display()))?;
    if let Some(scope) = scope.as_mut() {
        scope.register_remove_dir(run_state_dir.clone());
    }

    let lock_path = run_state_dir.join(ATO_LOCK_FILE_NAME);
    ato_lock::write_pretty_to_path(&result.lock, &lock_path)?;

    let sidecar_path = run_state_dir.join("provenance.json");
    write_sidecar(&sidecar_path, &result, MaterializationMode::RunAttempt)?;

    Ok(RunMaterialization {
        workspace_root: workspace_root.to_path_buf(),
        project_root: project_root.to_path_buf(),
        raw_manifest: original_manifest.cloned(),
        lock: result.lock,
        lock_path,
    })
}

fn materialize_workspace_result(
    project_root: &Path,
    result: SourceInferenceResult,
) -> Result<WorkspaceMaterialization> {
    crate::project::init::materialize::materialize_workspace_result(project_root, result)
}

fn materialize_run_model(
    adapter: MaterializationAdapter,
    scope: Option<&mut CleanupScope>,
    result: SourceInferenceResult,
) -> Result<RunMaterialization> {
    materialize_run_result(
        &adapter.workspace_root,
        &adapter.project_root,
        adapter.use_global_run_state,
        result,
        scope,
        adapter.original_manifest.as_ref(),
    )
}

fn materialize_workspace_model(
    adapter: MaterializationAdapter,
    result: SourceInferenceResult,
) -> Result<WorkspaceMaterialization> {
    materialize_workspace_result(&adapter.project_root, result)
}

pub(crate) fn write_sidecar(
    path: &Path,
    result: &SourceInferenceResult,
    mode: MaterializationMode,
) -> Result<()> {
    let sidecar = SourceInferenceSidecar {
        mode: match mode {
            MaterializationMode::RunAttempt => MaterializationModeSerde::RunAttempt,
            MaterializationMode::InitWorkspace => MaterializationModeSerde::InitWorkspace,
        },
        input_kind: result.input_kind,
        provenance: result.provenance.clone(),
        diagnostics: result.diagnostics.clone(),
        selection_gate: result.selection_gate.clone(),
        approval_gate: result.approval_gate.clone(),
        infer: result.infer.clone(),
        resolve: result.resolve.clone(),
    };
    let raw = serde_json::to_string_pretty(&sidecar)
        .context("Failed to serialize source inference sidecar")?;
    fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))
}

fn prompt_selection_if_allowed(
    gate: &SelectionGate,
    assume_yes: bool,
    reporter: Arc<CliReporter>,
) -> Result<Option<RankedCandidate>> {
    if assume_yes || reporter.is_json() || !io::stdin().is_terminal() || !io::stderr().is_terminal()
    {
        return Ok(None);
    }

    futures::executor::block_on(reporter.warn(gate.message.clone()))?;
    for (index, candidate) in gate.candidates.iter().enumerate() {
        futures::executor::block_on(reporter.notify(format!(
            "  {}. {} -> {}",
            index + 1,
            candidate.label,
            candidate.entrypoint.join(" ")
        )))?;
    }
    eprint!(
        "Select candidate [1-{}] or press Enter to abort: ",
        gate.candidates.len()
    );
    io::stderr()
        .flush()
        .context("failed to flush source inference selection prompt")?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let index = trimmed
        .parse::<usize>()
        .ok()
        .and_then(|value| value.checked_sub(1))
        .filter(|value| *value < gate.candidates.len())
        .ok_or_else(|| anyhow::anyhow!("invalid source inference selection: {trimmed}"))?;
    Ok(Some(gate.candidates[index].clone()))
}

fn apply_selection(
    result: &mut SourceInferenceResult,
    field: &str,
    selection: &RankedCandidate,
) -> Result<()> {
    if field == "contract.process" {
        let project_root = result
            .lock
            .contract
            .entries
            .get("metadata")
            .and_then(|value| value.get("source_root"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        result.lock.contract.entries.insert(
            "process".to_string(),
            process_value_from_candidate(project_root.as_deref(), Some(selection)),
        );
        result.lock.contract.unresolved.retain(|value| {
            !(value.reason == UnresolvedReason::ExplicitSelectionRequired
                && value
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("process candidates"))
        });
        result.provenance.push(SourceInferenceProvenance {
            field: field.to_string(),
            kind: SourceInferenceProvenanceKind::SelectionGate,
            source_path: None,
            importer_id: None,
            evidence_kind: None,
            source_field: Some(selection.label.clone()),
            note: Some("interactive selection resolved equal-ranked process ambiguity".to_string()),
        });
        result.selection_gate = None;
        resolve(result)?;
    }
    Ok(())
}

fn selection_gate_from_lock(
    lock: &AtoLock,
    candidate_sets: &[CandidateSet],
) -> Option<SelectionGate> {
    let unresolved = lock.contract.unresolved.iter().find(|value| {
        value.reason == UnresolvedReason::ExplicitSelectionRequired && value.candidates.len() > 1
    })?;
    let candidates = candidate_sets
        .iter()
        .find(|set| set.field == "contract.process")
        .map(|set| set.ranked.clone())
        .unwrap_or_else(|| {
            unresolved
                .candidates
                .iter()
                .map(|candidate| RankedCandidate {
                    label: candidate.clone(),
                    score: 0,
                    entrypoint: Vec::new(),
                    rationale: "selection required".to_string(),
                })
                .collect()
        });
    Some(SelectionGate {
        field: "contract.process".to_string(),
        candidates,
        message: unresolved
            .detail
            .clone()
            .unwrap_or_else(|| "explicit process selection is required".to_string()),
    })
}

fn process_candidates_for_source(
    detected: &DetectedProject,
    info: &ProjectInfo,
) -> Vec<RankedCandidate> {
    let mut candidates = Vec::new();
    match detected.project_type {
        ProjectType::NodeJs => {
            if let Some(node) = detected.node.as_ref() {
                if node.scripts.has_start {
                    candidates.push(RankedCandidate {
                        label: "package.json:scripts.start".to_string(),
                        score: 100,
                        entrypoint: info.entrypoint.clone(),
                        rationale:
                            "explicit package.json start script outranks other Node candidates"
                                .to_string(),
                    });
                }
                if node.scripts.has_dev {
                    let dev_entry = info.node_dev_entrypoint.clone().unwrap_or_else(|| {
                        vec!["npm".to_string(), "run".to_string(), "dev".to_string()]
                    });
                    candidates.push(RankedCandidate {
                        label: "package.json:scripts.dev".to_string(),
                        score: if node.scripts.has_start { 90 } else { 100 },
                        entrypoint: dev_entry,
                        rationale: "package.json dev script is an explicit execution candidate"
                            .to_string(),
                    });
                }
                if !node.scripts.has_start && !node.scripts.has_dev {
                    candidates.extend(existing_candidates(
                        &detected.dir,
                        &[
                            "src/main.tsx",
                            "src/index.ts",
                            "src/main.ts",
                            "src/main.jsx",
                            "src/index.jsx",
                            "src/main.js",
                            "src/index.js",
                            "main.tsx",
                            "index.ts",
                            "main.ts",
                            "main.jsx",
                            "index.jsx",
                            "index.js",
                            "main.js",
                            "app.js",
                            "server.js",
                        ],
                        70,
                        if matches!(node.package_manager, NodePackageManager::Deno) {
                            "deno:file_layout"
                        } else if node.is_bun {
                            "bun:file_layout"
                        } else {
                            "node:file_layout"
                        },
                        if matches!(node.package_manager, NodePackageManager::Deno) {
                            ""
                        } else if node.is_bun {
                            "bun"
                        } else {
                            "node"
                        },
                        if matches!(node.package_manager, NodePackageManager::Deno) {
                            "well-known Deno file layout used as deterministic fallback"
                        } else {
                            "well-known Node file layout used as deterministic fallback"
                        },
                    ));
                }
            }
        }
        ProjectType::Python => {
            candidates.extend(existing_candidates(
                &detected.dir,
                &["main.py", "app.py", "run.py", "server.py"],
                90,
                "python:file",
                "",
                "explicit Python entry file outranks convention-only fallbacks",
            ));
            if candidates.is_empty() && !info.entrypoint.is_empty() {
                candidates.push(RankedCandidate {
                    label: "python:project_info".to_string(),
                    score: 80,
                    entrypoint: info.entrypoint.clone(),
                    rationale:
                        "project-info fallback used when no explicit Python entry file exists"
                            .to_string(),
                });
            }
        }
        ProjectType::Rust | ProjectType::Go | ProjectType::Ruby => {
            if !info.entrypoint.is_empty() {
                candidates.push(RankedCandidate {
                    label: format!(
                        "{}:entrypoint",
                        detected.project_type.as_str().to_ascii_lowercase()
                    ),
                    score: 90,
                    entrypoint: info.entrypoint.clone(),
                    rationale:
                        "deterministic language-specific entrypoint inferred from project metadata"
                            .to_string(),
                });
            }
        }
        ProjectType::Unknown => {
            candidates.extend(existing_candidates(
                &detected.dir,
                &["main.py", "index.js", "main.sh", "run.sh"],
                60,
                "generic:file_layout",
                "",
                "generic file-layout fallback used due to insufficient explicit metadata",
            ));
            if candidates.is_empty() && !info.entrypoint.is_empty() {
                candidates.push(RankedCandidate {
                    label: "generic:project_info".to_string(),
                    score: 50,
                    entrypoint: info.entrypoint.clone(),
                    rationale: "generic project-info fallback used after deterministic file scan"
                        .to_string(),
                });
            }
        }
    }

    sort_ranked_candidates(&mut candidates);
    candidates
}

fn sort_ranked_candidates(candidates: &mut [RankedCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.entrypoint.join("\0").cmp(&right.entrypoint.join("\0")))
    });
}

fn existing_candidates(
    dir: &Path,
    files: &[&str],
    score: u16,
    label_prefix: &str,
    program: &str,
    rationale: &str,
) -> Vec<RankedCandidate> {
    files
        .iter()
        .filter(|candidate| dir.join(candidate).exists())
        .map(|candidate| RankedCandidate {
            label: format!("{label_prefix}:{candidate}"),
            score,
            entrypoint: if program.is_empty() {
                vec![(*candidate).to_string()]
            } else {
                vec![program.to_string(), (*candidate).to_string()]
            },
            rationale: rationale.to_string(),
        })
        .collect()
}

fn runtime_kind_from_project(detected: &DetectedProject) -> &'static str {
    match detected.project_type {
        ProjectType::NodeJs => detected
            .node
            .as_ref()
            .map(|node| match node.package_manager {
                NodePackageManager::Bun => "bun",
                NodePackageManager::Deno => "deno",
                _ => "node",
            })
            .unwrap_or("node"),
        ProjectType::Python => "python",
        ProjectType::Rust => "rust",
        ProjectType::Go => "go",
        ProjectType::Ruby => "ruby",
        ProjectType::Unknown => "source",
    }
}

fn inferred_runtime_resolution(detected: &DetectedProject, project_root: &Path) -> Value {
    let runtime_kind = runtime_kind_from_project(detected);
    let mut runtime = serde_json::Map::new();
    runtime.insert("kind".to_string(), Value::String(runtime_kind.to_string()));
    runtime.insert(
        "resolved_by".to_string(),
        Value::String("shared_source_inference".to_string()),
    );
    if let Some(version) = inferred_runtime_version(detected, project_root, runtime_kind) {
        runtime.insert("version".to_string(), Value::String(version));
    }
    Value::Object(runtime)
}

fn inferred_runtime_version(
    detected: &DetectedProject,
    project_root: &Path,
    runtime_kind: &str,
) -> Option<String> {
    match detected.project_type {
        ProjectType::NodeJs => infer_node_runtime_version(project_root, runtime_kind),
        ProjectType::Python => infer_first_existing_trimmed(project_root, &[".python-version"])
            .or_else(|| Some("3.12".to_string())),
        ProjectType::Rust => {
            infer_rust_runtime_version(project_root).or_else(|| Some("stable".to_string()))
        }
        ProjectType::Go => infer_first_existing_trimmed(project_root, &[".go-version"])
            .or_else(|| Some("1.22".to_string())),
        ProjectType::Ruby => infer_first_existing_trimmed(project_root, &[".ruby-version"])
            .or_else(|| Some("3.3".to_string())),
        ProjectType::Unknown => None,
    }
}

fn infer_node_runtime_version(project_root: &Path, runtime_kind: &str) -> Option<String> {
    if runtime_kind.eq_ignore_ascii_case("deno") {
        return infer_first_existing_trimmed(project_root, &[".deno-version"])
            .or_else(|| Some("2".to_string()));
    }

    if runtime_kind.eq_ignore_ascii_case("bun") {
        return infer_first_existing_trimmed(project_root, &[".bun-version"])
            .or_else(|| Some("1.1".to_string()));
    }

    infer_first_existing_trimmed(project_root, &[".nvmrc", ".node-version"])
        .or_else(|| infer_node_engine_version(project_root))
        .or_else(|| Some("20".to_string()))
}

fn infer_node_engine_version(project_root: &Path) -> Option<String> {
    let package_json_path = project_root.join("package.json");
    let raw = fs::read_to_string(package_json_path).ok()?;
    let package_json = serde_json::from_str::<Value>(&raw).ok()?;
    package_json
        .get("engines")
        .and_then(|value| value.get("node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .trim_start_matches('^')
                .trim_start_matches('~')
                .to_string()
        })
}

fn infer_rust_runtime_version(project_root: &Path) -> Option<String> {
    let toolchain = project_root.join("rust-toolchain.toml");
    if let Ok(raw) = fs::read_to_string(toolchain) {
        if let Ok(value) = toml::from_str::<toml::Value>(&raw) {
            if let Some(channel) = value
                .get("toolchain")
                .and_then(|value| value.get("channel"))
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(channel.to_string());
            }
        }
    }

    infer_first_existing_trimmed(project_root, &["rust-toolchain"])
}

fn infer_first_existing_trimmed(project_root: &Path, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        fs::read_to_string(project_root.join(name))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Reads `vite.config.{ts,js,mjs,cjs}` and extracts `server.port`.
/// Falls back to Vite's default port (5173) if not found.
fn detect_vite_port(project_root: &Path) -> u16 {
    let config_names = [
        "vite.config.ts",
        "vite.config.js",
        "vite.config.mjs",
        "vite.config.cjs",
    ];
    for name in &config_names {
        let path = project_root.join(name);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(port) = extract_vite_server_port(&content) {
            return port;
        }
    }
    5173
}

fn extract_vite_server_port(content: &str) -> Option<u16> {
    let re = Regex::new(r"port\s*:\s*(\d{4,5})").ok()?;
    let cap = re.captures(content)?;
    cap.get(1)?.as_str().parse().ok()
}

/// Extracts metadata from the `scripts.dev` field of a `package.json` string.
fn extract_dev_script_info(package_json: &str) -> Option<DevScriptInfo> {
    let json: Value = serde_json::from_str(package_json).ok()?;
    let dev = json.get("scripts")?.get("dev")?.as_str()?;
    Some(DevScriptInfo {
        command: dev.to_string(),
        is_vite: dev.contains("vite"),
        is_next: dev.contains("next"),
    })
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DevScriptInfo {
    command: String,
    is_vite: bool,
    is_next: bool,
}

fn inferred_network_contract(
    detected: &DetectedProject,
    single_script_language: Option<SingleScriptLanguage>,
    project_root: &Path,
) -> Value {
    if single_script_language.is_some() {
        return json!({
            "ingress": Vec::<Value>::new(),
        });
    }

    let expected_port = match detected.project_type {
        ProjectType::NodeJs => detected
            .node
            .as_ref()
            .map(|node| {
                if node.has_hono {
                    3000
                } else if node.scripts.has_dev {
                    let dev_info = fs::read_to_string(project_root.join("package.json"))
                        .ok()
                        .as_deref()
                        .and_then(extract_dev_script_info);
                    let is_vite = dev_info.map(|d| d.is_vite).unwrap_or(false);
                    if is_vite {
                        detect_vite_port(project_root)
                    } else {
                        3000
                    }
                } else {
                    3000
                }
            })
            .unwrap_or(3000),
        ProjectType::Python => 8000,
        _ => 0,
    };
    json!({
        "ingress": if expected_port > 0 {
            vec![json!({"port": expected_port, "protocol": "http"})]
        } else {
            Vec::<Value>::new()
        },
    })
}

fn inferred_env_contract(project_root: &Path) -> Value {
    let explicit = [".env.example", ".env.template"]
        .iter()
        .filter_map(|name| {
            let path = project_root.join(name);
            if path.exists() {
                Some(json!({"source": name, "classification": "explicit_example"}))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    json!({
        "required": Vec::<Value>::new(),
        "optional_hints": explicit,
    })
}

fn inferred_filesystem_contract(detected: &DetectedProject) -> Value {
    let cache = match detected.project_type {
        ProjectType::NodeJs => vec![json!("node_modules")],
        ProjectType::Python => vec![json!("__pycache__")],
        ProjectType::Rust => vec![json!("target")],
        _ => Vec::new(),
    };
    json!({
        "cache_hints": cache,
        "persistent_hints": Vec::<Value>::new(),
    })
}

fn inferred_closure_state(project_root: &Path) -> Value {
    let explicit_locks = observed_closure_evidence(project_root)
        .into_iter()
        .map(|evidence| json!(evidence.importer_id.as_str()))
        .collect::<Vec<_>>();

    json!({
        "kind": "metadata_only",
        "status": "incomplete",
        "observed_lockfiles": explicit_locks,
    })
}

fn observed_closure_evidence(project_root: &Path) -> Vec<ImportedEvidence> {
    probe_ecosystem_lockfile_evidence(project_root).unwrap_or_default()
}

fn importer_observation_provenance(
    field: &str,
    evidence: &ImportedEvidence,
    note: &str,
) -> SourceInferenceProvenance {
    SourceInferenceProvenance {
        field: field.to_string(),
        kind: SourceInferenceProvenanceKind::ImporterObservation,
        source_path: Some(evidence.primary_path.clone()),
        importer_id: Some(evidence.importer_id.as_str().to_string()),
        evidence_kind: Some(evidence.evidence_kind.as_str().to_string()),
        source_field: Some(evidence.importer_id.as_str().to_string()),
        note: Some(note.to_string()),
    }
}

fn source_metadata(
    detected: &DetectedProject,
    project_root: &Path,
    single_script_language: Option<SingleScriptLanguage>,
) -> Value {
    json!({
        "name": detected.name,
        "version": infer_project_version(detected, project_root).unwrap_or_else(|| "0.1.0".to_string()),
        "capsule_type": if single_script_language.is_some() { "job" } else { "app" },
        "source_root": project_root,
        "project_type": detected.project_type.as_str(),
    })
}

fn infer_project_version(detected: &DetectedProject, project_root: &Path) -> Option<String> {
    match detected.project_type {
        ProjectType::NodeJs => infer_package_json_string_field(project_root, "version"),
        ProjectType::Python => infer_pyproject_version(project_root),
        ProjectType::Rust => infer_cargo_package_field(project_root, "version"),
        ProjectType::Go | ProjectType::Ruby | ProjectType::Unknown => None,
    }
}

fn infer_package_json_string_field(project_root: &Path, field: &str) -> Option<String> {
    let raw = fs::read_to_string(project_root.join("package.json")).ok()?;
    let package_json = serde_json::from_str::<Value>(&raw).ok()?;
    package_json
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn infer_pyproject_version(project_root: &Path) -> Option<String> {
    let raw = fs::read_to_string(project_root.join("pyproject.toml")).ok()?;
    let value = toml::from_str::<toml::Value>(&raw).ok()?;
    value
        .get("project")
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn infer_cargo_package_field(project_root: &Path, field: &str) -> Option<String> {
    let raw = fs::read_to_string(project_root.join("Cargo.toml")).ok()?;
    let value = toml::from_str::<toml::Value>(&raw).ok()?;
    value
        .get("package")
        .and_then(|value| value.get(field))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn process_value_from_candidate(
    project_root: Option<&Path>,
    candidate: Option<&RankedCandidate>,
) -> Value {
    if let Some(candidate) = candidate {
        if let Some((entrypoint, args, run_command)) =
            project_root.and_then(|root| resolve_node_script_process(root, &candidate.entrypoint))
        {
            return json!({
                "entrypoint": entrypoint,
                "cmd": args,
                "run_command": run_command,
            });
        }

        let entrypoint = candidate.entrypoint.first().cloned().unwrap_or_default();
        let args = if candidate.entrypoint.len() > 1 {
            candidate.entrypoint[1..]
                .iter()
                .cloned()
                .map(Value::String)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        json!({
            "entrypoint": entrypoint,
            "cmd": args,
        })
    } else {
        json!({})
    }
}

fn resolve_node_script_process(
    project_root: &Path,
    candidate_entrypoint: &[String],
) -> Option<(String, Vec<String>, String)> {
    let script_name = package_manager_script_name(candidate_entrypoint)?;
    let script = package_json_script(project_root, script_name)?;
    if contains_shell_control_operators(&script) {
        return None;
    }

    let tokens = shell_words::split(&script).ok()?;
    let first = tokens.first()?.trim();
    if first.is_empty() {
        return None;
    }

    if first == "node" {
        let entrypoint = tokens.get(1)?.trim();
        if entrypoint.is_empty() {
            return None;
        }
        // Skip inline eval scripts (node -e "...") — they are not file entrypoints
        // and cannot be used with deno cache. Fall back to npm run dev.
        if entrypoint.starts_with('-') {
            return None;
        }
        let args = tokens.iter().skip(2).cloned().collect::<Vec<_>>();
        return Some((entrypoint.to_string(), args, join_shell_tokens(&tokens)));
    }

    if is_package_binary_command(first) {
        let mut args = tokens.iter().skip(1).cloned().collect::<Vec<_>>();
        let entrypoint = format!("npm:{first}");
        let mut run_tokens = vec![entrypoint.clone()];
        run_tokens.extend(args.iter().cloned());
        return Some((
            entrypoint,
            std::mem::take(&mut args),
            join_shell_tokens(&run_tokens),
        ));
    }

    None
}

fn package_manager_script_name(candidate_entrypoint: &[String]) -> Option<&str> {
    match candidate_entrypoint {
        [first, second, third, ..] if first == "npm" && second == "run" => Some(third.as_str()),
        [first, second, ..] if matches!(first.as_str(), "npm" | "pnpm" | "yarn") => {
            Some(second.as_str())
        }
        [first, second, third, ..] if first == "bun" && second == "run" => Some(third.as_str()),
        [first, second, ..] if first == "bun" => Some(second.as_str()),
        _ => None,
    }
}

fn package_json_script(project_root: &Path, script_name: &str) -> Option<String> {
    let raw = fs::read_to_string(project_root.join("package.json")).ok()?;
    let package_json = serde_json::from_str::<Value>(&raw).ok()?;
    package_json
        .get("scripts")
        .and_then(|value| value.get(script_name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn contains_shell_control_operators(script: &str) -> bool {
    ["&&", "||", ";", "|", ">", "<", "$(", "`"]
        .iter()
        .any(|token| script.contains(token))
}

fn is_package_binary_command(command: &str) -> bool {
    !command.is_empty()
        && !matches!(
            command,
            "npm" | "pnpm" | "yarn" | "bun" | "npx" | "node" | "deno"
        )
        && !command.starts_with('.')
        && !command.starts_with('/')
}

fn join_shell_tokens(tokens: &[String]) -> String {
    tokens.join(" ")
}

fn collect_unresolved_paths(lock: &AtoLock) -> Vec<String> {
    let mut paths = Vec::new();
    if !lock.contract.unresolved.is_empty() {
        paths.push("contract".to_string());
    }
    if !lock.resolution.unresolved.is_empty() {
        paths.push("resolution".to_string());
    }
    if !lock.binding.unresolved.is_empty() {
        paths.push("binding".to_string());
    }
    paths
}

fn explicit_candidates(lock: &AtoLock) -> Vec<String> {
    lock.contract
        .unresolved
        .iter()
        .flat_map(|value| value.candidates.clone())
        .collect()
}

fn convert_compatibility_provenance(
    record: &CompatibilityProvenanceRecord,
) -> SourceInferenceProvenance {
    SourceInferenceProvenance {
        field: record.field.lock_path(),
        kind: match record.kind {
            crate::application::compat_import::ProvenanceKind::ManifestExplicit => {
                SourceInferenceProvenanceKind::CompatibilityImport
            }
            crate::application::compat_import::ProvenanceKind::LegacyLockResolved => {
                SourceInferenceProvenanceKind::CompatibilityImport
            }
            crate::application::compat_import::ProvenanceKind::NormalizedDefault => {
                SourceInferenceProvenanceKind::DeterministicHeuristic
            }
            crate::application::compat_import::ProvenanceKind::CompilerInferred => {
                SourceInferenceProvenanceKind::CompatibilityImport
            }
        },
        source_path: record.source_path.clone(),
        importer_id: None,
        evidence_kind: None,
        source_field: record.source_field.clone(),
        note: record.note.clone(),
    }
}

fn convert_compatibility_diagnostic(
    diagnostic: &CompatibilityDiagnostic,
) -> SourceInferenceDiagnostic {
    SourceInferenceDiagnostic {
        severity: match diagnostic.severity {
            CompatibilityDiagnosticSeverity::Warning => SourceInferenceDiagnosticSeverity::Warning,
            CompatibilityDiagnosticSeverity::Error => SourceInferenceDiagnosticSeverity::Error,
        },
        field: diagnostic.lock_path.clone(),
        message: diagnostic.message.clone(),
    }
}

fn unique_attempt_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("attempt-{nanos}")
}

fn is_equal_ranked(candidates: &[RankedCandidate]) -> bool {
    candidates.len() > 1 && candidates[0].score == candidates[1].score
}

/// Parse `pyproject.toml` content and return `("uv", "run <script>")` when
/// a `[project.scripts]` table is present, using the first script name found.
#[allow(dead_code)]
pub(crate) fn detect_uv_entrypoint(pyproject_content: &str) -> Option<(String, String)> {
    let value: toml::Value = toml::from_str(pyproject_content).ok()?;
    let scripts = value.get("project")?.get("scripts")?.as_table()?;
    let script_name = scripts.keys().next()?;
    Some(("uv".to_string(), format!("run {}", script_name)))
}

/// Environment and network hints inferred from AI agent SDK dependencies.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct AiAgentHint {
    /// Environment variable keys required by the detected SDKs, e.g.
    /// `["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]`.
    pub required_env: Vec<String>,
    /// Egress hosts that should be allowed for the detected SDKs, e.g.
    /// `["api.anthropic.com"]`.
    pub egress_hosts: Vec<String>,
}

/// Scan `requirements.txt`, `pyproject.toml`, and `package.json` for known
/// LLM SDK dependencies and return an [`AiAgentHint`] when any are found.
///
/// Matches are performed against exact package names (not substrings) to
/// avoid false positives like `openai-whisper` or commented-out lines.
#[allow(dead_code)]
pub(crate) fn detect_ai_agent_hint(project_root: &Path) -> Option<AiAgentHint> {
    // (package-name → (ENV_VAR, host)) — matched exactly (case-insensitive)
    // against package identifiers extracted from manifest files.
    const PY_SDKS: &[(&str, &str, &str)] = &[
        ("anthropic", "ANTHROPIC_API_KEY", "api.anthropic.com"),
        ("openai", "OPENAI_API_KEY", "api.openai.com"),
        (
            "google-generativeai",
            "GOOGLE_API_KEY",
            "generativelanguage.googleapis.com",
        ),
        ("mistralai", "MISTRAL_API_KEY", "api.mistral.ai"),
        ("groq", "GROQ_API_KEY", "api.groq.com"),
    ];
    const NODE_SDKS: &[(&str, &str, &str)] = &[
        (
            "@anthropic-ai/sdk",
            "ANTHROPIC_API_KEY",
            "api.anthropic.com",
        ),
        ("openai", "OPENAI_API_KEY", "api.openai.com"),
    ];

    let mut hint = AiAgentHint::default();
    let push = |env: &str, host: &str, hint: &mut AiAgentHint| -> bool {
        if hint.egress_hosts.iter().any(|h| h == host) {
            return false;
        }
        hint.required_env.push(env.to_string());
        hint.egress_hosts.push(host.to_string());
        true
    };
    let mut found = false;

    // requirements.txt — line-based parse, skip comments/flags.
    if let Ok(content) = fs::read_to_string(project_root.join("requirements.txt")) {
        for line in content.lines() {
            let Some(pkg) = parse_requirements_line(line) else {
                continue;
            };
            let pkg_lower = pkg.to_ascii_lowercase();
            for (needle, env, host) in PY_SDKS {
                if pkg_lower == *needle {
                    found |= push(env, host, &mut hint);
                }
            }
        }
    }

    // pyproject.toml — parse via toml to get [project].dependencies and
    // [tool.poetry.dependencies].
    if let Ok(content) = fs::read_to_string(project_root.join("pyproject.toml")) {
        for pkg in extract_pyproject_dependencies(&content) {
            let pkg_lower = pkg.to_ascii_lowercase();
            for (needle, env, host) in PY_SDKS {
                if pkg_lower == *needle {
                    found |= push(env, host, &mut hint);
                }
            }
        }
    }

    // package.json — parse JSON and inspect dependencies / devDependencies.
    if let Ok(content) = fs::read_to_string(project_root.join("package.json")) {
        if let Ok(value) = serde_json::from_str::<Value>(&content) {
            for section in ["dependencies", "devDependencies", "peerDependencies"] {
                let Some(map) = value.get(section).and_then(|v| v.as_object()) else {
                    continue;
                };
                for name in map.keys() {
                    for (needle, env, host) in NODE_SDKS {
                        if name == *needle {
                            found |= push(env, host, &mut hint);
                        }
                    }
                }
            }
        }
    }

    if found {
        Some(hint)
    } else {
        None
    }
}

/// Extract a package name from a single `requirements.txt` line.
/// Returns `None` for blank lines, comments, or option flags.
#[allow(dead_code)]
fn parse_requirements_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    // Package name ends at the first version specifier, extras bracket, or
    // env-marker separator.
    let end = trimmed
        .find(|c: char| matches!(c, '=' | '<' | '>' | '~' | '!' | ';' | '[' | ' ' | '\t'))
        .unwrap_or(trimmed.len());
    let pkg = trimmed[..end].trim();
    if pkg.is_empty() {
        None
    } else {
        Some(pkg)
    }
}

/// Extract dependency package names from `pyproject.toml`.
/// Handles both PEP-621 `[project].dependencies` (list of PEP 508 strings)
/// and `[tool.poetry.dependencies]` (table keyed by package name).
#[allow(dead_code)]
fn extract_pyproject_dependencies(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return out;
    };

    if let Some(deps) = value
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for entry in deps {
            if let Some(s) = entry.as_str() {
                if let Some(pkg) = parse_requirements_line(s) {
                    out.push(pkg.to_string());
                }
            }
        }
    }
    if let Some(deps) = value
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for name in deps.keys() {
            out.push(name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use capsule_core::ato_lock::{self, AtoLock};
    use capsule_core::input_resolver::{
        resolve_authoritative_input, ResolveInputOptions, ResolvedInput, ResolvedSingleScript,
        ResolvedSourceOnly, SingleScriptLanguage,
    };
    use tempfile::tempdir;

    use super::*;

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn reporter() -> Arc<CliReporter> {
        Arc::new(CliReporter::new(false))
    }

    fn load_materialized_lock(path: &Path) -> AtoLock {
        ato_lock::load_unvalidated_from_path(path).expect("load durable ato.lock.json")
    }

    fn write_macos_app_bundle(path: &Path) {
        let executable = path.join("Contents/MacOS/app");
        fs::create_dir_all(
            executable
                .parent()
                .expect("macOS app executable path must have a parent"),
        )
        .expect("create app bundle");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("write app executable");
    }

    #[test]
    fn source_only_node_project_infers_process_runtime_and_closure() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('ok')").expect("write index");

        let result = execute_shared_engine(
            SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                project_root: dir.path().to_path_buf(),
                explicit_native_artifact: None,
                single_script_language: None,
                authoritative_root: None,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect("run engine");

        assert!(result.lock.contract.entries.contains_key("process"));
        assert!(result.lock.resolution.entries.contains_key("runtime"));
        assert!(result.lock.resolution.entries.contains_key("closure"));
        assert!(result.selection_gate.is_none());
    }

    #[test]
    fn source_only_inference_emits_normalized_incomplete_metadata_closure() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('ok')").expect("write index");

        let result = execute_shared_engine(
            SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                project_root: dir.path().to_path_buf(),
                explicit_native_artifact: None,
                single_script_language: None,
                authoritative_root: None,
            }),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("run engine");

        assert_eq!(
            result.lock.resolution.entries.get("closure"),
            Some(&json!({
                "kind": "metadata_only",
                "status": "incomplete",
                "observed_lockfiles": [],
            }))
        );
    }

    #[test]
    fn source_only_node_project_resolves_package_script_to_npm_specifier() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"dev":"vite --host 127.0.0.1 --port 5175"},"devDependencies":{"vite":"5.4.2"}}"#,
        )
        .expect("write package json");

        let result = execute_shared_engine(
            SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                project_root: dir.path().to_path_buf(),
                explicit_native_artifact: None,
                single_script_language: None,
                authoritative_root: None,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect("run engine");

        assert_eq!(
            result.lock.contract.entries.get("process"),
            Some(&json!({
                "entrypoint": "npm:vite",
                "cmd": ["--host", "127.0.0.1", "--port", "5175"],
                "run_command": "npm:vite --host 127.0.0.1 --port 5175",
            }))
        );
    }

    #[test]
    fn source_only_python_project_uses_script_path_as_entrypoint() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("requirements.txt"), "fastapi==0.115.0\n")
            .expect("write requirements");
        fs::write(dir.path().join("main.py"), "print('ok')\n").expect("write main");

        let result = execute_shared_engine(
            SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                project_root: dir.path().to_path_buf(),
                explicit_native_artifact: None,
                single_script_language: None,
                authoritative_root: None,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect("run engine");

        assert_eq!(
            result.lock.contract.entries.get("process"),
            Some(&json!({
                "entrypoint": "main.py",
                "cmd": [],
            }))
        );
    }

    #[test]
    fn draft_lock_input_preserves_existing_process_without_reinference() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "custom", "cmd": ["serve"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "custom", "cmd": ["serve"]}}]),
        );
        let result = execute_shared_engine(
            SourceInferenceInput::DraftLock(DraftLockInput {
                project_root: PathBuf::from("."),
                draft_lock: lock.clone(),
                provenance: Vec::new(),
            }),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("run engine");

        assert_eq!(
            result.lock.contract.entries.get("process"),
            lock.contract.entries.get("process")
        );
    }

    #[test]
    fn canonical_lock_infer_phase_does_not_generate_source_candidates() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "default", "runtime": "source", "driver": "deno", "entrypoint": "main.ts", "compatible": true}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "runtime_closure", "status": "complete", "inputs": []}),
        );

        let inferred = infer_phase(SourceInferenceInput::CanonicalLock(CanonicalLockInput {
            project_root: PathBuf::from("."),
            canonical_path: PathBuf::from("ato.lock.json"),
            lock,
        }))
        .expect("infer phase");

        assert!(inferred.result.infer.candidate_sets.is_empty());
        assert_eq!(
            inferred.result.input_kind,
            SourceInferenceInputKind::CanonicalLock
        );
        assert!(inferred
            .result
            .provenance
            .iter()
            .all(|record| record.kind != SourceInferenceProvenanceKind::DeterministicHeuristic));
    }

    #[test]
    fn materialize_workspace_writes_lock_and_sidecar() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('ok')").expect("write index");

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize workspace");

        assert!(materialized.lock_path.exists());
        assert!(materialized.sidecar_path.exists());
        assert!(materialized.provenance_cache_path.exists());
        assert!(materialized.binding_seed_path.exists());
    }

    #[test]
    fn durable_init_materializes_single_typescript_script_into_workspace() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("scratch.ts");
        fs::write(&script_path, "console.log('hello durable init');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::TypeScript,
            }),
        };

        let materialized = execute_init_from_resolved_source_only(&source, reporter(), true)
            .expect("materialize workspace");

        assert!(materialized.lock_path.exists());
        assert!(dir.path().join("main.ts").exists());
        assert!(dir.path().join("deno.json").exists());
        assert!(dir.path().join("deno.lock").exists());
    }

    #[test]
    fn durable_init_materializes_single_javascript_script_into_workspace() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("scratch.js");
        fs::write(&script_path, "console.log('hello durable js');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::JavaScript,
            }),
        };

        let materialized = execute_init_from_resolved_source_only(&source, reporter(), true)
            .expect("materialize workspace");

        assert!(materialized.lock_path.exists());
        assert!(dir.path().join("main.js").exists());
        assert!(dir.path().join("deno.json").exists());
        assert!(dir.path().join("deno.lock").exists());
    }

    #[test]
    fn durable_init_materializes_single_python_script_into_workspace() {
        if std::process::Command::new("uv")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("scratch.py");
        fs::write(
            &script_path,
            "# /// script\n# requires-python = \">=3.11\"\n# dependencies = [\n#   \"rich\",\n# ]\n# ///\nprint('hello durable python')\n",
        )
        .expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::Python,
            }),
        };

        let materialized = execute_init_from_resolved_source_only(&source, reporter(), true)
            .expect("materialize workspace");

        assert!(materialized.lock_path.exists());
        assert!(dir.path().join("main.py").exists());
        assert!(dir.path().join("pyproject.toml").exists());
        assert!(dir.path().join("uv.lock").exists());
    }

    #[test]
    fn javascript_single_script_virtual_workspace_generates_deno_lock() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.js");
        fs::write(&script_path, "console.log('hello from js');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::JavaScript,
            }),
        };

        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(materialized.workspace_root, dir.path());
        assert_ne!(materialized.project_root, materialized.workspace_root);
        assert_eq!(routed.plan.execution_runtime().as_deref(), Some("source"));
        assert_eq!(routed.plan.execution_driver().as_deref(), Some("deno"));
        assert_eq!(routed.plan.execution_port(), None);
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("main.js")
        );
        assert_eq!(
            routed.plan.execution_source_layout().as_deref(),
            Some("anchored_entrypoint")
        );
        assert_eq!(
            materialized
                .lock
                .contract
                .entries
                .get("metadata")
                .and_then(|value| value.get("source_root")),
            Some(&json!(dir.path())),
        );
        assert!(materialized.project_root.join("deno.lock").exists());
    }

    #[test]
    fn single_script_run_materialization_reuses_cached_workspace_root() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let _env_lock = env_lock();
        let dir = tempdir().expect("tempdir");
        let ato_home = dir.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let script_path = dir.path().join("hello.js");
        fs::write(&script_path, "console.log('hello cache');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::JavaScript,
            }),
        };

        let first =
            materialize_run_from_source_only(&source, None, reporter(), true).expect("first");
        let second =
            materialize_run_from_source_only(&source, None, reporter(), true).expect("second");

        assert_eq!(first.workspace_root, dir.path());
        assert_eq!(first.project_root, second.project_root);
        assert!(first.project_root.starts_with(
            ato_home
                .join("cache")
                .join("source-inference")
                .join("single-script-cache")
        ));
        assert!(!dir.path().join(".ato").exists());
        assert!(first
            .lock_path
            .starts_with(ato_home.join("runs").join("source-inference")));
        assert!(!first.lock_path.starts_with(&first.project_root));
    }

    #[test]
    fn typescript_single_script_bare_imports_become_deno_imports() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.ts");
        fs::write(
            &script_path,
            "import { z } from \"zod\";\nimport pc from \"picocolors\";\nconsole.log(pc.green(z.string().parse('ok')));\n",
        )
        .expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::TypeScript,
            }),
        };

        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let deno_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(materialized.project_root.join("deno.json"))
                .expect("read deno json"),
        )
        .expect("parse deno json");

        assert_eq!(
            deno_json
                .get("imports")
                .and_then(|value| value.get("zod"))
                .and_then(serde_json::Value::as_str),
            Some("npm:zod")
        );
        assert_eq!(
            deno_json
                .get("imports")
                .and_then(|value| value.get("picocolors"))
                .and_then(serde_json::Value::as_str),
            Some("npm:picocolors")
        );
        assert!(materialized.project_root.join("deno.lock").exists());
    }

    #[test]
    fn jsx_single_script_virtual_workspace_writes_jsx_compiler_options() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.jsx");
        fs::write(
            &script_path,
            "/** @jsxImportSource npm:preact */\nexport const App = <div>hello</div>;\n",
        )
        .expect("write jsx script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::JavaScript,
            }),
        };

        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let deno_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(materialized.project_root.join("deno.json"))
                .expect("read deno json"),
        )
        .expect("parse deno json");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("main.jsx")
        );
        assert_eq!(
            deno_json
                .get("compilerOptions")
                .and_then(|value| value.get("jsx"))
                .and_then(serde_json::Value::as_str),
            Some("react-jsx")
        );
        assert_eq!(
            deno_json
                .get("compilerOptions")
                .and_then(|value| value.get("jsxImportSource"))
                .and_then(serde_json::Value::as_str),
            Some("npm:preact")
        );
    }

    #[test]
    fn run_materialization_writes_lock_without_generated_manifest_bridge() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('ok')").expect("write index");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let sidecar_path = materialized
            .lock_path
            .parent()
            .expect("run state dir")
            .join("provenance.json");

        assert!(materialized.lock_path.exists());
        assert!(sidecar_path.exists());
        assert!(materialized.raw_manifest.is_none());
        assert!(!dir.path().join(".ato.run.generated.capsule.toml").exists());
    }

    #[test]
    fn run_materialization_writes_run_command_for_resolved_package_script() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","scripts":{"dev":"vite --host 127.0.0.1 --port 5175"},"devDependencies":{"vite":"5.4.2"}}"#,
        )
        .expect("write package json");
        fs::write(
            dir.path().join("package-lock.json"),
            r#"{"name":"demo","lockfileVersion":3}"#,
        )
        .expect("write lock");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(
            routed.plan.execution_run_command().as_deref(),
            Some("npm:vite --host 127.0.0.1 --port 5175")
        );
        assert_ne!(routed.plan.execution_entrypoint().as_deref(), Some("npm"));
    }

    #[test]
    fn run_materialization_omits_invalid_source_driver_for_generic_source_only_project() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("index.js"), "console.log('ok')").expect("write index");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(routed.plan.execution_runtime().as_deref(), Some("source"));
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("index.js")
        );
        assert!(routed.plan.execution_driver().is_none());
        assert!(materialized.raw_manifest.is_none());
        assert!(!dir.path().join(".ato.run.generated.capsule.toml").exists());
    }

    #[test]
    fn init_persists_unresolved_when_equal_rank_candidates_remain() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("package.json"), r#"{"name":"demo"}"#)
            .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('a')").expect("write index");
        fs::write(dir.path().join("main.js"), "console.log('b')").expect("write main");

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize workspace");
        let lock = capsule_core::ato_lock::load_unvalidated_from_path(&materialized.lock_path)
            .expect("read materialized lock");

        assert!(!lock.contract.entries.contains_key("process"));
        assert!(lock
            .contract
            .unresolved
            .iter()
            .any(|value| value.reason == UnresolvedReason::ExplicitSelectionRequired));
    }

    #[test]
    fn equal_ranked_candidates_are_sorted_deterministically() {
        let mut candidates = vec![
            RankedCandidate {
                label: "same".to_string(),
                score: 100,
                entrypoint: vec!["z-entry".to_string()],
                rationale: "later".to_string(),
            },
            RankedCandidate {
                label: "alpha".to_string(),
                score: 100,
                entrypoint: vec!["m-entry".to_string()],
                rationale: "first label".to_string(),
            },
            RankedCandidate {
                label: "same".to_string(),
                score: 100,
                entrypoint: vec!["a-entry".to_string()],
                rationale: "first entrypoint".to_string(),
            },
        ];

        sort_ranked_candidates(&mut candidates);

        assert_eq!(candidates[0].label, "alpha");
        assert_eq!(candidates[1].entrypoint, vec!["a-entry"]);
        assert_eq!(candidates[2].entrypoint, vec!["z-entry"]);
        assert!(is_equal_ranked(&candidates));
    }

    #[test]
    fn run_fails_when_equal_rank_candidates_remain_without_selection() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("package.json"), r#"{"name":"demo"}"#)
            .expect("write package json");
        fs::write(dir.path().join("index.js"), "console.log('a')").expect("write index");
        fs::write(dir.path().join("main.js"), "console.log('b')").expect("write main");

        let error = execute_shared_engine(
            SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                project_root: dir.path().to_path_buf(),
                explicit_native_artifact: None,
                single_script_language: None,
                authoritative_root: None,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect_err("run should fail without explicit selection");

        assert!(error.to_string().contains("ATO_ERR_AMBIGUOUS_ENTRYPOINT"));
    }

    #[test]
    fn compatibility_draft_handoff_does_not_reinfer_process() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "npm"
cmd = ["start"]
driver = "node"
    runtime_version = "20"
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };
        let (draft_input, compiled) =
            draft_lock_input_from_compatibility(&project).expect("compile compatibility draft");

        let result = execute_shared_engine(
            SourceInferenceInput::DraftLock(draft_input),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("run shared engine");

        assert_eq!(
            result.lock.contract.entries.get("process"),
            compiled.draft_lock.contract.entries.get("process")
        );
    }

    #[test]
    fn compatibility_import_stays_out_of_source_candidate_generation() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "node"
entrypoint = "npm"
cmd = ["start"]
runtime_version = "20"
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };
        let (draft_input, _) =
            draft_lock_input_from_compatibility(&project).expect("compile compatibility draft");

        let inferred = infer_phase(SourceInferenceInput::DraftLock(draft_input)).expect("infer");

        assert!(inferred.result.infer.candidate_sets.is_empty());
        assert!(inferred
            .result
            .provenance
            .iter()
            .any(|record| record.kind == SourceInferenceProvenanceKind::CompatibilityImport));
        assert!(inferred
            .result
            .provenance
            .iter()
            .all(|record| !(record.field == "contract.process"
                && record.kind == SourceInferenceProvenanceKind::DeterministicHeuristic)));

        let resolved = resolve_phase(inferred).expect("resolve");
        assert!(resolved.import_involved);
    }

    #[test]
    fn compatibility_run_materialization_writes_lock_without_bridge() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };

        let materialized = materialize_run_from_compatibility(&project, None, reporter(), true)
            .expect("run materialize");
        let sidecar_path = materialized
            .lock_path
            .parent()
            .expect("run state dir")
            .join("provenance.json");
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar_path).expect("read sidecar"))
                .expect("parse sidecar");

        assert_eq!(sidecar["input_kind"], "draft_lock");
        assert!(materialized.lock_path.exists());
        assert!(sidecar_path.exists());
        assert!(materialized.raw_manifest.is_some());
        assert!(!dir.path().join(".ato.run.generated.capsule.toml").exists());
        assert!(materialized.lock.contract.entries.contains_key("process"));
        assert!(materialized.lock.resolution.entries.contains_key("runtime"));
        assert!(materialized.lock.resolution.entries.contains_key("closure"));
    }

    #[test]
    fn compatibility_run_materialization_preserves_selected_target_and_network_policy() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[network]
egress_allow = ["api.github.com"]

[targets.web]
runtime = "web"
driver = "static"
entrypoint = "public/index.html"
port = 8080
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };

        let materialized = materialize_run_from_compatibility(&project, None, reporter(), true)
            .expect("run materialize");
        let generated = materialized.raw_manifest.expect("raw manifest");

        assert_eq!(
            generated
                .get("default_target")
                .and_then(toml::Value::as_str),
            Some("web")
        );
        assert_eq!(
            generated
                .get("network")
                .and_then(|network| network.get("egress_allow"))
                .and_then(toml::Value::as_array)
                .and_then(|values| values.first())
                .and_then(toml::Value::as_str),
            Some("api.github.com")
        );
        assert_eq!(
            generated
                .get("targets")
                .and_then(|targets| targets.get("web"))
                .and_then(|target| target.get("runtime"))
                .and_then(toml::Value::as_str),
            Some("web")
        );
        assert_eq!(
            generated
                .get("targets")
                .and_then(|targets| targets.get("web"))
                .and_then(|target| target.get("driver"))
                .and_then(toml::Value::as_str),
            Some("static")
        );
    }

    #[test]
    fn compatibility_run_materialization_preserves_ipc_section() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"

[ipc.imports.greeter]
from = "missing-service"
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };

        let materialized = materialize_run_from_compatibility(&project, None, reporter(), true)
            .expect("run materialize");
        let generated = materialized.raw_manifest.expect("raw manifest");

        assert_eq!(
            generated
                .get("ipc")
                .and_then(|ipc| ipc.get("imports"))
                .and_then(|imports| imports.get("greeter"))
                .and_then(|greeter| greeter.get("from"))
                .and_then(toml::Value::as_str),
            Some("missing-service")
        );
    }

    #[test]
    fn compatibility_native_delivery_promotes_build_closure() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"build":"npm run tauri build","tauri":"tauri"}}"#,
        )
        .expect("write package json");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(dir.path().join("src-tauri/tauri.conf.json"), "{}\n")
            .expect("write tauri config");
        write_macos_app_bundle(
            &dir.path()
                .join("src-tauri/target/release/bundle/macos/MyApp.app"),
        );
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "pnpm"
cmd = ["build"]
working_dir = "."

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/MyApp.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/MyApp.app"]
"#,
        )
        .expect("write manifest");
        fs::write(dir.path().join("Cargo.lock"), "version = 3\n").expect("write cargo lock");
        fs::write(dir.path().join("package-lock.json"), "{}").expect("write package lock");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };
        let (draft_input, _) =
            draft_lock_input_from_compatibility(&project).expect("compile compatibility draft");

        let result = execute_shared_engine(
            SourceInferenceInput::DraftLock(draft_input),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("run shared engine");

        let closure = result
            .lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");
        let delivery = result
            .lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("build_closure")
        );
        assert_eq!(
            closure.get("status").and_then(Value::as_str),
            Some("complete")
        );
        let inputs = closure
            .get("inputs")
            .and_then(Value::as_array)
            .expect("closure inputs");
        assert!(inputs.len() >= 2);
        assert!(inputs.iter().any(|value| {
            value.get("name").and_then(Value::as_str) == Some("cargo")
                && value.get("kind").and_then(Value::as_str) == Some("lockfile")
                && value
                    .get("digest")
                    .and_then(Value::as_str)
                    .is_some_and(|digest| digest.starts_with("blake3:"))
        }));
        assert!(inputs.iter().any(|value| {
            value.get("name").and_then(Value::as_str) == Some("npm")
                && value.get("kind").and_then(Value::as_str) == Some("lockfile")
                && value
                    .get("digest")
                    .and_then(Value::as_str)
                    .is_some_and(|digest| digest.starts_with("blake3:"))
        }));

        let environment = closure
            .get("build_environment")
            .and_then(Value::as_object)
            .expect("build_environment");
        assert!(environment
            .get("toolchains")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("rust"))));
        assert!(environment
            .get("package_managers")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("cargo"))
                    && values.iter().any(|value| value.as_str() == Some("npm"))
            }));
        assert!(environment
            .get("helper_tools")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("tauri-cli"))
                    && values
                        .iter()
                        .any(|value| value.as_str() == Some("codesign"))
            }));
        assert!(result
            .lock
            .resolution
            .unresolved
            .iter()
            .all(|value| value.field.as_deref() != Some("resolution.closure")));
        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("source-derivation")
        );
        assert_eq!(
            delivery
                .get("build")
                .and_then(|value| value.get("closure_status"))
                .and_then(Value::as_str),
            Some("complete")
        );
    }

    #[test]
    fn durable_init_source_only_tauri_promotes_to_source_derivation() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"my-tauri-app","version":"1.2.3","scripts":{"build":"npm run tauri build"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("package-lock.json"), "{}\n").expect("write package lock");
        fs::write(dir.path().join("Cargo.lock"), "version = 3\n").expect("write cargo lock");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"my-tauri-app\"\nversion = \"1.2.3\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(dir.path().join("src-tauri/tauri.conf.json"), "{}\n")
            .expect("write tauri config");
        write_macos_app_bundle(
            &dir.path()
                .join("src-tauri/target/release/bundle/macos/my-tauri-app.app"),
        );

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize tauri workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let closure = lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");
        let environment = closure
            .get("build_environment")
            .and_then(Value::as_object)
            .expect("build_environment");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("source-derivation")
        );
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("build_closure")
        );
        assert!(environment
            .get("toolchains")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("rust"))
                    && values.iter().any(|value| value.as_str() == Some("node"))
            }));
        assert!(environment
            .get("package_managers")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("cargo"))
                    && values.iter().any(|value| value.as_str() == Some("npm"))
            }));
        assert!(environment
            .get("helper_tools")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("tauri-cli"))
                    && values
                        .iter()
                        .any(|value| value.as_str() == Some("codesign"))
            }));
    }

    #[test]
    fn durable_init_source_only_tauri_promotes_with_nested_cargo_lock() {
        let dir = tempdir().expect("tempdir");
        fs::write(
                dir.path().join("package.json"),
                r#"{"name":"my-tauri-app","version":"1.2.3","scripts":{"build":"npm run tauri build"}}"#,
            )
            .expect("write package json");
        fs::write(dir.path().join("package-lock.json"), "{}\n").expect("write package lock");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(dir.path().join("src-tauri/Cargo.lock"), "version = 3\n")
            .expect("write nested cargo lock");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"my-tauri-app\"\nversion = \"1.2.3\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(dir.path().join("src-tauri/tauri.conf.json"), "{}\n")
            .expect("write tauri config");
        write_macos_app_bundle(
            &dir.path()
                .join("src-tauri/target/release/bundle/macos/my-tauri-app.app"),
        );

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize tauri workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let environment = lock
            .resolution
            .entries
            .get("closure")
            .and_then(|value| value.get("build_environment"))
            .and_then(Value::as_object)
            .expect("build_environment");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("source-derivation")
        );
        assert!(environment
            .get("package_managers")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("cargo"))
                    && values.iter().any(|value| value.as_str() == Some("npm"))
            }));
    }

    #[test]
    fn infer_source_native_build_command_prefers_tauri_script_over_generic_build() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"my-tauri-app","version":"1.2.3","scripts":{"build":"tsc && vite build","tauri":"tauri"},"packageManager":"npm@10.0.0"}"#,
        )
        .expect("write package json");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"my-tauri-app\"\nversion = \"1.2.3\"\n",
        )
        .expect("write Cargo.toml");

        let detected = detect_project(dir.path()).expect("detect project");
        let command = infer_source_native_build_command(
            dir.path(),
            &detected,
            capsule_core::importer::ImporterId::Tauri,
        )
        .expect("build command");

        assert_eq!(command.program, "npm");
        assert_eq!(command.args, vec!["run", "tauri", "--", "build"]);
    }

    #[test]
    fn durable_init_source_only_electron_promotes_to_source_derivation() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"my-electron-app","version":"2.0.0","scripts":{"build":"electron-builder"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("package-lock.json"), "{}\n").expect("write package lock");
        fs::write(dir.path().join("electron-builder.json"), "{}\n")
            .expect("write electron builder config");
        write_macos_app_bundle(&dir.path().join("dist/my-electron-app.app"));

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize electron workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let closure = lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");
        let environment = closure
            .get("build_environment")
            .and_then(Value::as_object)
            .expect("build_environment");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("source-derivation")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("framework"))
                .and_then(Value::as_str),
            Some("electron")
        );
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("build_closure")
        );
        assert!(environment
            .get("toolchains")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("node"))));
        assert!(environment
            .get("package_managers")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("npm"))));
        assert!(environment
            .get("helper_tools")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("electron"))
                    && values
                        .iter()
                        .any(|value| value.as_str() == Some("codesign"))
            }));
    }

    #[test]
    fn infer_source_native_build_command_prefers_electron_packaged_script_over_generic_build() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"sample-project","version":"1.0.0","scripts":{"build":"electron-vite build","build:mac":"electron-vite build && electron-builder --mac"},"packageManager":"bun@1.1.6"}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("bun.lockb"), "lock").expect("write bun lock");
        fs::write(
            dir.path().join("electron-builder.yml"),
            "productName: sample-project\n",
        )
        .expect("write electron builder config");

        let detected = detect_project(dir.path()).expect("detect project");
        let command = infer_source_native_build_command(
            dir.path(),
            &detected,
            capsule_core::importer::ImporterId::Electron,
        )
        .expect("build command");

        assert_eq!(command.program, "bun");
        assert_eq!(command.args, vec!["run", "build:mac"]);
    }

    #[test]
    fn durable_init_source_only_electron_uses_builder_product_name_for_expected_app_path() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"tsugy-electron-app","version":"1.0.0","scripts":{"build":"electron-vite build","build:mac":"electron-vite build && electron-builder --mac"},"packageManager":"bun@1.1.6"}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("bun.lockb"), "lock").expect("write bun lock");
        fs::write(
            dir.path().join("electron-builder.yml"),
            "productName: sample-project\ndirectories:\n  output: dist\n",
        )
        .expect("write electron builder config");

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize electron workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let artifact_path = lock
            .contract
            .entries
            .get("delivery")
            .and_then(|value| value.get("artifact"))
            .and_then(|value| value.get("path"))
            .and_then(Value::as_str)
            .expect("delivery artifact path");

        assert_eq!(artifact_path, "dist/mac-arm64/sample-project.app");
    }

    #[test]
    fn durable_init_source_only_wails_promotes_to_source_derivation() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/my-wails-app\n",
        )
        .expect("write go.mod");
        fs::write(
            dir.path().join("go.sum"),
            "example.com/pkg v1.0.0 h1:demo\n",
        )
        .expect("write go.sum");
        fs::write(dir.path().join("wails.json"), "{}\n").expect("write wails config");
        write_macos_app_bundle(&dir.path().join("build/bin/my-wails-app.app"));

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize wails workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let closure = lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");
        let environment = closure
            .get("build_environment")
            .and_then(Value::as_object)
            .expect("build_environment");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("source-derivation")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("framework"))
                .and_then(Value::as_str),
            Some("wails")
        );
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("build_closure")
        );
        assert!(environment
            .get("toolchains")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("go"))));
        assert!(environment
            .get("package_managers")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("go"))));
        assert!(environment
            .get("helper_tools")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("wails"))
                    && values
                        .iter()
                        .any(|value| value.as_str() == Some("codesign"))
            }));
    }

    #[test]
    fn durable_init_source_only_appimage_becomes_artifact_import() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("dist")).expect("create dist");
        fs::write(dir.path().join("dist/MyApp.AppImage"), "appimage").expect("write appimage");

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize artifact-import workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let closure = lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("artifact-import")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("artifact_type"))
                .and_then(Value::as_str),
            Some("appimage")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("provenance_limited"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("imported_artifact_closure")
        );
        assert_eq!(
            closure
                .get("artifact")
                .and_then(|value| value.get("artifact_type"))
                .and_then(Value::as_str),
            Some("appimage")
        );
    }

    #[test]
    fn durable_init_source_only_windows_executable_becomes_artifact_import() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("dist")).expect("create dist");
        fs::write(dir.path().join("dist/MyApp.exe"), "exe").expect("write exe");

        let materialized = execute_init_from_source_only(dir.path(), reporter(), true)
            .expect("materialize artifact-import workspace");
        let lock = load_materialized_lock(&materialized.lock_path);

        let delivery = lock
            .contract
            .entries
            .get("delivery")
            .expect("contract.delivery");
        let closure = lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");

        assert_eq!(
            delivery.get("mode").and_then(Value::as_str),
            Some("artifact-import")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("artifact_type"))
                .and_then(Value::as_str),
            Some("windows_executable")
        );
        assert_eq!(
            delivery
                .get("artifact")
                .and_then(|value| value.get("provenance_limited"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("imported_artifact_closure")
        );
        assert_eq!(
            closure
                .get("artifact")
                .and_then(|value| value.get("artifact_type"))
                .and_then(Value::as_str),
            Some("windows_executable")
        );
    }

    #[test]
    fn run_materialization_source_only_tauri_routes_native_dev_command() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"my-tauri-app","version":"1.2.3","scripts":{"dev":"tauri dev"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("package-lock.json"), "{}\n").expect("write package lock");
        fs::write(dir.path().join("Cargo.lock"), "version = 3\n").expect("write cargo lock");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"my-tauri-app\"\nversion = \"1.2.3\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(dir.path().join("src-tauri/tauri.conf.json"), "{}\n")
            .expect("write tauri config");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(routed.plan.execution_driver().as_deref(), Some("native"));
        assert_eq!(routed.plan.selected_target_label(), "desktop");
        assert_eq!(routed.plan.execution_entrypoint().as_deref(), Some("npm"));
        assert_eq!(routed.plan.targets_oci_cmd(), vec!["run", "dev"]);
    }

    #[test]
    fn run_materialization_source_only_app_bundle_routes_inner_executable() {
        let dir = tempdir().expect("tempdir");
        write_macos_app_bundle(&dir.path().join("dist/MyApp.app"));

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(routed.plan.execution_driver().as_deref(), Some("native"));
        assert_eq!(routed.plan.selected_target_label(), "desktop");
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("dist/MyApp.app/Contents/MacOS/app")
        );
    }

    #[test]
    fn run_materialization_source_only_appimage_routes_native_artifact() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("dist")).expect("create dist");
        fs::write(dir.path().join("dist/MyApp.AppImage"), "appimage").expect("write appimage");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: None,
        };
        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(routed.plan.execution_driver().as_deref(), Some("native"));
        assert_eq!(routed.plan.selected_target_label(), "desktop");
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("dist/MyApp.AppImage")
        );
    }

    #[test]
    fn native_delivery_build_derive_only_appears_in_resolve_phase() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"build":"npm run tauri build","tauri":"tauri"}}"#,
        )
        .expect("write package json");
        fs::write(dir.path().join("package-lock.json"), "{}\n").expect("write package lock");
        fs::create_dir_all(dir.path().join("src-tauri")).expect("create src-tauri");
        fs::write(
            dir.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(dir.path().join("src-tauri/tauri.conf.json"), "{}\n")
            .expect("write tauri config");
        write_macos_app_bundle(
            &dir.path()
                .join("src-tauri/target/release/bundle/macos/MyApp.app"),
        );
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "pnpm"
cmd = ["build"]

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/MyApp.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/MyApp.app"]
"#,
        )
        .expect("write manifest");
        fs::write(dir.path().join("Cargo.lock"), "version = 3\n").expect("write cargo lock");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };
        let (draft_input, _) =
            draft_lock_input_from_compatibility(&project).expect("compile compatibility draft");

        let inferred = infer_phase(SourceInferenceInput::DraftLock(draft_input)).expect("infer");
        assert_eq!(
            inferred
                .result
                .lock
                .contract
                .entries
                .get("delivery")
                .and_then(|value| value.get("mode"))
                .and_then(Value::as_str),
            Some("source-draft")
        );
        assert_eq!(
            inferred
                .result
                .lock
                .resolution
                .entries
                .get("closure")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str),
            Some("metadata_only")
        );

        let resolved = resolve_phase(inferred).expect("resolve");
        assert!(resolved.build_derive_involved);
        assert_eq!(
            resolved
                .result
                .lock
                .resolution
                .entries
                .get("closure")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str),
            Some("build_closure")
        );
    }

    #[test]
    fn single_script_workspace_adapter_is_materialization_scoped() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.ts");
        fs::write(&script_path, "console.log('hello');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::TypeScript,
            }),
        };

        let adapter = prepare_run_materialization_adapter(&source, None).expect("adapter");
        assert_ne!(adapter.project_root, source.project_root);
        assert!(adapter.project_root.join("deno.json").exists());
    }

    #[test]
    fn parse_pep723_python_metadata_extracts_dependencies_and_requires_python() {
        let metadata = parse_pep723_python_metadata(
            r#"# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "rich",
# ]
# ///
print('ok')
"#,
        )
        .expect("parse pep723");

        assert_eq!(metadata.requires_python.as_deref(), Some(">=3.11"));
        assert_eq!(metadata.dependencies, vec!["httpx>=0.27", "rich"]);
    }

    #[test]
    fn typescript_single_script_virtual_workspace_generates_deno_lock() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.ts");
        fs::write(&script_path, "console.log('hello from ts');\n").expect("write script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::TypeScript,
            }),
        };

        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(routed.plan.execution_runtime().as_deref(), Some("source"));
        assert_eq!(routed.plan.execution_driver().as_deref(), Some("deno"));
        assert_eq!(
            routed
                .plan
                .typed_manifest()
                .expect("typed manifest")
                .capsule_type,
            capsule_core::types::CapsuleType::Job
        );
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("main.ts")
        );
        assert!(materialized.project_root.join("deno.lock").exists());
    }

    #[test]
    fn tsx_single_script_virtual_workspace_writes_jsx_compiler_options() {
        if std::process::Command::new("deno")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("hello.tsx");
        fs::write(
            &script_path,
            "/** @jsxImportSource npm:preact */\nexport const App = <div>hello</div>;\n",
        )
        .expect("write tsx script");

        let source = ResolvedSourceOnly {
            project_root: dir.path().to_path_buf(),
            single_script: Some(ResolvedSingleScript {
                path: script_path,
                language: SingleScriptLanguage::TypeScript,
            }),
        };

        let materialized = materialize_run_from_source_only(&source, None, reporter(), true)
            .expect("materialize run");
        let deno_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(materialized.project_root.join("deno.json"))
                .expect("read deno json"),
        )
        .expect("parse deno json");
        let routed = capsule_core::router::route_lock(
            &materialized.lock_path,
            &materialized.lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("main.tsx")
        );
        assert_eq!(
            deno_json
                .get("compilerOptions")
                .and_then(|value| value.get("jsx"))
                .and_then(serde_json::Value::as_str),
            Some("react-jsx")
        );
        assert_eq!(
            deno_json
                .get("compilerOptions")
                .and_then(|value| value.get("jsxImportSource"))
                .and_then(serde_json::Value::as_str),
            Some("npm:preact")
        );
    }

    #[test]
    fn compatibility_run_materialization_fails_closed_when_process_unresolved() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"

[targets.worker]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "worker.ts"

[services.main]
target = "main"

[services.worker]
target = "worker"
"#,
        )
        .expect("write manifest");

        let resolved = resolve_authoritative_input(dir.path(), ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };

        let error = materialize_run_from_compatibility(&project, None, reporter(), true)
            .expect_err("compatibility run must fail closed when process is unresolved");

        assert!(error.to_string().contains("ATO_ERR_AMBIGUOUS_ENTRYPOINT"));
    }

    #[test]
    fn draft_lock_run_fails_closed_when_runtime_promotion_cannot_resolve() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "web", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"},
                {"label": "worker", "runtime": "source", "driver": "deno", "entrypoint": "worker.ts"}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete", "observed_lockfiles": []}),
        );

        let error = execute_shared_engine(
            SourceInferenceInput::DraftLock(DraftLockInput {
                project_root: PathBuf::from("."),
                draft_lock: lock,
                provenance: Vec::new(),
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect_err("draft lock without a resolvable target/runtime must fail closed");

        assert!(error.to_string().contains("ATO_ERR_RUNTIME_NOT_RESOLVED"));
    }

    #[test]
    fn draft_lock_normalizes_legacy_complete_closure_shape() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "web", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"status": "complete", "inputs": []}),
        );

        let result = execute_shared_engine(
            SourceInferenceInput::DraftLock(DraftLockInput {
                project_root: PathBuf::from("."),
                draft_lock: lock,
                provenance: Vec::new(),
            }),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("draft lock engine");

        assert_eq!(
            result.lock.resolution.entries.get("closure"),
            Some(&json!({
                "kind": "runtime_closure",
                "status": "complete",
                "inputs": [],
            }))
        );
    }

    #[test]
    fn canonical_run_fails_closed_when_resolved_targets_missing() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete", "observed_lockfiles": []}),
        );

        let error = execute_shared_engine(
            SourceInferenceInput::CanonicalLock(CanonicalLockInput {
                project_root: PathBuf::from("."),
                canonical_path: PathBuf::from("ato.lock.json"),
                lock,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect_err("canonical lock without resolved targets must fail closed");

        assert!(error
            .to_string()
            .contains("ATO_ERR_EXECUTION_CONTRACT_INVALID"));
        assert!(error.to_string().contains("resolved target-compatible"));
    }

    #[test]
    fn canonical_run_fails_closed_when_closure_missing() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "web", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"}
            ]),
        );

        let error = execute_shared_engine(
            SourceInferenceInput::CanonicalLock(CanonicalLockInput {
                project_root: PathBuf::from("."),
                canonical_path: PathBuf::from("ato.lock.json"),
                lock,
            }),
            MaterializationMode::RunAttempt,
            true,
            reporter(),
        )
        .expect_err("canonical lock without closure must fail closed");

        assert!(error.to_string().contains("dependency closure state"));
    }

    #[test]
    fn canonical_lock_normalizes_legacy_complete_closure_shape() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "main.ts", "cmd": []}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "web", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"status": "complete", "inputs": []}),
        );

        let result = execute_shared_engine(
            SourceInferenceInput::CanonicalLock(CanonicalLockInput {
                project_root: PathBuf::from("."),
                canonical_path: PathBuf::from("ato.lock.json"),
                lock,
            }),
            MaterializationMode::InitWorkspace,
            true,
            reporter(),
        )
        .expect("canonical lock engine");

        assert_eq!(
            result.lock.resolution.entries.get("closure"),
            Some(&json!({
                "kind": "runtime_closure",
                "status": "complete",
                "inputs": [],
            }))
        );
    }
}
