//! Build materialization for `ato run` (RFC: BUILD_MATERIALIZATION, v0).
//!
//! This module is responsible for:
//! 1. Resolving a build spec from `capsule.toml [build]` (canonical) or
//!    framework heuristic fallback.
//! 2. Computing a deterministic blake3 input digest over the spec, source
//!    files, lockfiles, env values, and toolchain fingerprint.
//! 3. Persisting / reading `.ato/state/materializations.json` so subsequent
//!    runs can skip the build executor when input digest + outputs match.
//! 4. Resolving the build policy (`--rebuild` / `--no-build` / default) into
//!    a concrete decision and surfacing fine-grained `result_kind`s for
//!    diagnostic emission.
//!
//! See: `apps/ato/docs/rfcs/draft/BUILD_MATERIALIZATION.md`

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::application::source_inventory::{
    collect_source_files, native_lockfiles, normalize_outputs,
};

/// Marker version for the digest layout. Bump if the digest composition
/// changes so previously-recorded materializations invalidate naturally.
const MATERIALIZATION_DIGEST_VERSION: &str = "ato-build-materialization-v1";

/// User-controlled build policy. v0 supports three policies; default is
/// `IfStale`, which skips the build executor when an existing materialization
/// record matches the current input digest and outputs are still on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BuildPolicy {
    /// Default: skip if record + outputs match, build otherwise.
    #[default]
    IfStale,
    /// `--rebuild`: ignore record, always run the build executor.
    Rebuild,
    /// `--no-build`: never run the build executor; fail if no usable
    /// materialization exists.
    NoBuild,
}

impl BuildPolicy {
    pub fn from_flags(rebuild: bool, no_build: bool) -> Self {
        match (rebuild, no_build) {
            (false, false) => Self::IfStale,
            (true, false) => Self::Rebuild,
            (false, true) => Self::NoBuild,
            (true, true) => {
                // clap's `conflicts_with` should already prevent this, but
                // we degrade gracefully to no-build (the more conservative
                // option) rather than panic.
                Self::NoBuild
            }
        }
    }
}

/// Origin of the build spec used for materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuildSpecSource {
    /// `capsule.toml [build]` declared the spec explicitly.
    Declared,
    /// No `[build]` was declared; the spec was inferred for a known framework.
    Heuristic {
        name: &'static str,
        version: &'static str,
    },
}

impl BuildSpecSource {
    /// Render as `declared` or `heuristic:<name>:<version>` for digest /
    /// state-file consumption.
    pub(crate) fn fingerprint_label(&self) -> String {
        match self {
            Self::Declared => "declared".to_string(),
            Self::Heuristic { name, version } => format!("heuristic:{}:{}", name, version),
        }
    }

    /// Render as `declared` or `heuristic` for PHASE-TIMING `source=` extra.
    pub(crate) fn timing_label(&self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Heuristic { .. } => "heuristic",
        }
    }

    /// Render as `<name>:<version>` for PHASE-TIMING `heuristic=` extra.
    pub(crate) fn heuristic_label(&self) -> Option<String> {
        match self {
            Self::Declared => None,
            Self::Heuristic { name, version } => Some(format!("{}:{}", name, version)),
        }
    }
}

/// A resolved build spec ready for digest computation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // PR-C reads `inputs` / `env_include` for state-file persistence.
pub(crate) struct BuildSpec {
    pub(crate) command: String,
    pub(crate) inputs: Vec<String>,
    pub(crate) outputs: Vec<String>,
    pub(crate) env_include: Vec<String>,
    pub(crate) source: BuildSpecSource,
}

/// Diagnostic record produced by [`observe`]. Carries enough information to
/// surface as a `PhaseAnnotation` and (in a future PR) to be persisted into
/// `materializations.json`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // PR-C reads `command`, `outputs`, `working_dir_relative` for state-file records.
pub(crate) struct BuildObservation {
    pub(crate) source: BuildSpecSource,
    pub(crate) command: String,
    pub(crate) input_digest: String,
    pub(crate) outputs: Vec<String>,
    pub(crate) target: String,
    pub(crate) working_dir_relative: String,
}

/// Compute a build observation for the currently selected target of a
/// `ManifestData`. Wraps [`observe`] with the working-dir resolution that
/// `preflight.rs::run_v03_lifecycle_steps` uses, so the digest reflects the
/// same source/lockfile set the build executor sees.
pub(crate) fn observe_for_plan(
    plan: &capsule_core::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
) -> Result<Option<BuildObservation>> {
    let working_dir = resolve_lifecycle_working_dir(plan);
    let toolchain_fp = derive_toolchain_fingerprint(plan, launch_ctx);
    observe(
        &plan.manifest,
        plan.selected_target_label(),
        &plan.workspace_root,
        &working_dir,
        plan.build_lifecycle_build().as_deref(),
        &toolchain_fp,
        |key| std::env::var(key).ok(),
    )
}

/// Mirror of `preflight::resolve_provision_working_dir`. GitHub-installed
/// capsules sometimes lay out their project files under `source/` while the
/// outer manifest dir does not contain `package.json`. The build digest must
/// hash the same directory the build executor will shell into.
fn resolve_lifecycle_working_dir(plan: &capsule_core::router::ManifestData) -> PathBuf {
    let source_dir = plan.manifest_dir.join("source");
    if source_dir.join("package.json").exists() {
        return source_dir;
    }
    plan.execution_working_directory()
}

/// Public toolchain fingerprint helper for callers that need to record the
/// same value on the materialization record after a successful build.
pub(crate) fn toolchain_fingerprint_for_plan(plan: &capsule_core::router::ManifestData) -> String {
    let runtime = plan
        .execution_runtime()
        .unwrap_or_else(|| "unknown".to_string());
    let driver = plan
        .execution_driver()
        .unwrap_or_else(|| "unknown".to_string());
    let target = plan.selected_target_label();
    let working = plan
        .execution_working_dir()
        .unwrap_or_else(|| ".".to_string());
    let os_arch = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    format!(
        "runtime:{}|driver:{}|target:{}|workdir:{}|os:{}|schema:{}",
        runtime, driver, target, working, os_arch, MATERIALIZATION_DIGEST_VERSION
    )
}

/// Compose a deterministic, host-aware fingerprint for the toolchain in use.
/// v0 stays conservative: read whatever is available cheaply from the
/// `RuntimeLaunchContext` and the resolved manifest plan; mark unavailable
/// fields as `unknown` so the digest still changes when resolution improves
/// later.
fn derive_toolchain_fingerprint(
    plan: &capsule_core::router::ManifestData,
    _launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
) -> String {
    let runtime = plan
        .execution_runtime()
        .unwrap_or_else(|| "unknown".to_string());
    let driver = plan
        .execution_driver()
        .unwrap_or_else(|| "unknown".to_string());
    let target = plan.selected_target_label();
    let working = plan
        .execution_working_dir()
        .unwrap_or_else(|| ".".to_string());
    let os_arch = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    format!(
        "runtime:{}|driver:{}|target:{}|workdir:{}|os:{}|schema:{}",
        runtime, driver, target, working, os_arch, MATERIALIZATION_DIGEST_VERSION
    )
}

/// Compute a build observation from a manifest + resolved working directory.
///
/// Returns `Ok(None)` when no build is applicable (no declaration, no
/// heuristic match, and no legacy `build_lifecycle_build()` command). Returns
/// `Err` for spec parse failures (e.g. invalid outputs path).
pub(crate) fn observe(
    manifest: &toml::Value,
    selected_target: &str,
    workspace_root: &Path,
    working_dir_absolute: &Path,
    legacy_build_command: Option<&str>,
    toolchain_fingerprint: &str,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<BuildObservation>> {
    let Some(spec) = resolve_build_spec(manifest, working_dir_absolute, legacy_build_command)?
    else {
        return Ok(None);
    };

    let working_dir_relative = working_dir_absolute
        .strip_prefix(workspace_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| working_dir_absolute.display().to_string());

    let input_digest = compute_input_digest(
        &spec,
        selected_target,
        &working_dir_relative,
        working_dir_absolute,
        toolchain_fingerprint,
        &env_lookup,
    )?;

    Ok(Some(BuildObservation {
        source: spec.source,
        command: spec.command,
        input_digest,
        outputs: spec.outputs,
        target: selected_target.to_string(),
        working_dir_relative,
    }))
}

/// Resolve a [`BuildSpec`] from declared `[build]` first, then framework
/// heuristic, then legacy `build_lifecycle_build()` (heuristic is required
/// even for legacy commands so the caller knows what files compose the digest).
pub(crate) fn resolve_build_spec(
    manifest: &toml::Value,
    working_dir: &Path,
    legacy_build_command: Option<&str>,
) -> Result<Option<BuildSpec>> {
    if let Some(declared) = read_declared_build(manifest)? {
        return Ok(Some(declared));
    }

    let Some(command) = legacy_build_command
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };

    let Some(heuristic) = detect_heuristic(working_dir) else {
        return Ok(None);
    };

    Ok(Some(BuildSpec {
        command,
        inputs: heuristic.inputs.iter().map(|&s| s.to_string()).collect(),
        outputs: heuristic.outputs.iter().map(|&s| s.to_string()).collect(),
        env_include: Vec::new(),
        source: BuildSpecSource::Heuristic {
            name: heuristic.name,
            version: heuristic.version,
        },
    }))
}

/// Read declared `[build]` from manifest if present. Returns `Ok(None)` if
/// the section is absent. Returns `Err` if the section is malformed.
fn read_declared_build(manifest: &toml::Value) -> Result<Option<BuildSpec>> {
    let Some(section) = manifest.get("build").and_then(|v| v.as_table()) else {
        return Ok(None);
    };
    let Some(command) = section
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        // command is required when [build] is declared
        return Ok(None);
    };

    let inputs = section
        .get("inputs")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let outputs = section
        .get("outputs")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let env_include = section
        .get("env")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("include"))
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Ok(Some(BuildSpec {
        command,
        inputs,
        outputs,
        env_include,
        source: BuildSpecSource::Declared,
    }))
}

/// A framework heuristic: which name + version, plus the inputs and outputs
/// the framework's build artifact depends on.
struct HeuristicHint {
    name: &'static str,
    version: &'static str,
    /// Glob include list. Empty means "everything under working_dir minus
    /// excludes" (which is what the source walker already produces — see
    /// [`crate::application::source_inventory`]). v0 always uses the empty
    /// include + exclusions baked into the walker; this field is reserved
    /// for future declarative heuristics.
    inputs: &'static [&'static str],
    outputs: &'static [&'static str],
}

fn detect_heuristic(working_dir: &Path) -> Option<HeuristicHint> {
    if is_nextjs_project(working_dir) {
        return Some(HeuristicHint {
            name: "nextjs",
            version: "v1",
            inputs: &[],
            outputs: &[".next"],
        });
    }
    if is_vite_project(working_dir) {
        return Some(HeuristicHint {
            name: "vite",
            version: "v1",
            inputs: &[],
            outputs: &["dist"],
        });
    }
    None
}

fn is_nextjs_project(working_dir: &Path) -> bool {
    if first_existing_with_extensions(working_dir, "next.config", &["js", "mjs", "cjs", "ts"])
        .is_some()
    {
        return true;
    }
    if let Ok(text) = std::fs::read_to_string(working_dir.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
            for section in ["dependencies", "devDependencies", "peerDependencies"] {
                if pkg
                    .get(section)
                    .and_then(|v| v.as_object())
                    .is_some_and(|map| map.contains_key("next"))
                {
                    return true;
                }
            }
        }
    }
    false
}

fn is_vite_project(working_dir: &Path) -> bool {
    first_existing_with_extensions(working_dir, "vite.config", &["js", "mjs", "cjs", "ts"])
        .is_some()
}

fn first_existing_with_extensions(
    working_dir: &Path,
    stem: &str,
    extensions: &[&str],
) -> Option<PathBuf> {
    for ext in extensions {
        let path = working_dir.join(format!("{}.{}", stem, ext));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Compute the blake3 input digest over the build spec, source files,
/// lockfiles, env values, and toolchain fingerprint. See
/// `BUILD_MATERIALIZATION.md` §2.2 for the canonical composition.
fn compute_input_digest(
    spec: &BuildSpec,
    selected_target: &str,
    working_dir_relative: &str,
    working_dir_absolute: &Path,
    toolchain_fingerprint: &str,
    env_lookup: &impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let mut hasher = Hasher::new();

    update_text(&mut hasher, MATERIALIZATION_DIGEST_VERSION);
    update_text(&mut hasher, selected_target);
    update_text(&mut hasher, working_dir_relative);
    update_text(&mut hasher, &spec.command);
    update_text(&mut hasher, &spec.source.fingerprint_label());
    update_text(&mut hasher, toolchain_fingerprint);

    // outputs (normalized + sorted for determinism)
    let normalized_outputs = normalize_outputs(&spec.outputs)
        .with_context(|| "Failed to normalize build outputs for materialization digest")?;
    let mut output_paths: Vec<String> = normalized_outputs
        .iter()
        .map(|o| o.relative_path.display().to_string())
        .collect();
    output_paths.sort();
    update_text(&mut hasher, "outputs");
    for path in &output_paths {
        update_text(&mut hasher, path);
    }

    // env.include: sorted (key, blake3(value)). Raw values are NEVER recorded
    // anywhere; only their hashes participate in the digest.
    let mut env_keys = expand_env_globs(&spec.env_include, env_lookup);
    env_keys.sort();
    env_keys.dedup();
    update_text(&mut hasher, "env");
    for key in &env_keys {
        update_text(&mut hasher, key);
        match env_lookup(key) {
            Some(value) => {
                let value_hash = blake3::hash(value.as_bytes());
                update_text(&mut hasher, value_hash.to_hex().as_str());
            }
            None => update_text(&mut hasher, "<missing>"),
        }
    }

    // lockfiles (delegated to existing source_inventory helper)
    update_text(&mut hasher, "lockfiles");
    for lockfile in native_lockfiles(working_dir_absolute) {
        update_text(&mut hasher, &lockfile.display().to_string());
        hash_file(&mut hasher, &lockfile)?;
    }

    // source files: walker already excludes node_modules, .git, declared
    // outputs, etc. (DEFAULT_IGNORED_DIRS in source_inventory.rs).
    update_text(&mut hasher, "sources");
    for relative_path in collect_source_files(working_dir_absolute, &normalized_outputs)? {
        update_text(&mut hasher, &relative_path.display().to_string());
        hash_file(&mut hasher, &working_dir_absolute.join(&relative_path))?;
    }

    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn update_text(hasher: &mut Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_file(hasher: &mut Hasher, path: &Path) -> Result<()> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to read materialization input: {}", path.display()))?;
    let mut buffer = [0u8; 8192];
    loop {
        use std::io::Read as _;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(())
}

/// Expand `env.include` patterns. v0 supports literal keys and a single
/// trailing `*` wildcard (e.g. `NEXT_PUBLIC_*`). The wildcard is matched
/// against the host process environment via `env_lookup`'s caller, but
/// since we cannot enumerate env from the closure, this expansion uses
/// `std::env::vars` for wildcards. Literal keys are always included
/// regardless of presence (missing keys hash to "<missing>").
fn expand_env_globs(
    include: &[String],
    _env_lookup: &impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut keys = Vec::new();
    for pattern in include {
        if let Some(prefix) = pattern.strip_suffix('*') {
            for (key, _) in std::env::vars() {
                if key.starts_with(prefix) {
                    keys.push(key);
                }
            }
        } else {
            keys.push(pattern.clone());
        }
    }
    keys
}

// ---------------------------------------------------------------------------
// State file: .ato/state/materializations.json
// ---------------------------------------------------------------------------

const STATE_SCHEMA_VERSION: u32 = 1;
const STATE_RELATIVE_PATH: &str = ".ato/state/materializations.json";

/// Persisted state file. Project-local; never carries host-portable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MaterializationFile {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) artifacts: Vec<MaterializationRecord>,
    #[serde(default)]
    pub(crate) recommendations_emitted: Vec<RecommendationRecord>,
}

impl Default for MaterializationFile {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            artifacts: Vec::new(),
            recommendations_emitted: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Some fields are diagnostic-only for v0; PR-D / future may read them.
pub(crate) struct MaterializationRecord {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) working_dir: String,
    pub(crate) input_digest: String,
    pub(crate) command: String,
    pub(crate) outputs: Vec<String>,
    pub(crate) source: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) heuristic: Option<String>,
    pub(crate) toolchain_fingerprint: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) env_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) env_fingerprint: Option<String>,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RecommendationRecord {
    pub(crate) kind: String,
    pub(crate) input_digest: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) heuristic: Option<String>,
}

/// Outcome of attempting to load the state file.
pub(crate) enum LoadOutcome {
    Loaded(MaterializationFile),
    Missing,
    Invalid(()),
}

/// Read state with non-fatal error semantics: a missing file yields
/// `Missing`, a corrupted file yields `Invalid(())`. Both are recoverable —
/// callers should treat them as "no usable record". The reason for an
/// `Invalid` outcome is logged via `tracing::warn!` so operators can
/// diagnose corrupted state without the variant carrying a payload no
/// caller currently inspects.
pub(crate) fn load_state(workspace_root: &Path) -> LoadOutcome {
    let path = workspace_root.join(STATE_RELATIVE_PATH);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::Missing,
        Err(err) => {
            warn!(path = %path.display(), %err, "materialization state read failed");
            return LoadOutcome::Invalid(());
        }
    };
    match serde_json::from_slice::<MaterializationFile>(&bytes) {
        Ok(file) if file.schema_version == STATE_SCHEMA_VERSION => LoadOutcome::Loaded(file),
        Ok(file) => {
            warn!(
                path = %path.display(),
                schema_version = file.schema_version,
                "materialization state has unsupported schema_version"
            );
            LoadOutcome::Invalid(())
        }
        Err(err) => {
            warn!(path = %path.display(), %err, "materialization state JSON parse failed");
            LoadOutcome::Invalid(())
        }
    }
}

/// Atomically persist the state file. Creates the parent directory if
/// missing. v0 uses a temp-file + rename; concurrent writes from the same
/// workspace may race but the file is at most a few KB and parsing failures
/// fall back to "no record" (callers rebuild and overwrite).
pub(crate) fn save_state(workspace_root: &Path, file: &MaterializationFile) -> Result<()> {
    let path = workspace_root.join(STATE_RELATIVE_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(file).context("failed to serialize materializations.json")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Locate an existing record by `(target, working_dir, name)` triple.
pub(crate) fn find_record<'a>(
    file: &'a MaterializationFile,
    target: &str,
    working_dir: &str,
    name: &str,
) -> Option<&'a MaterializationRecord> {
    file.artifacts
        .iter()
        .find(|r| r.target == target && r.working_dir == working_dir && r.name == name)
}

/// Insert or update a record matched by `(target, working_dir, name)`.
pub(crate) fn upsert_record(file: &mut MaterializationFile, record: MaterializationRecord) {
    if let Some(existing) = file.artifacts.iter_mut().find(|r| {
        r.target == record.target && r.working_dir == record.working_dir && r.name == record.name
    }) {
        *existing = record;
    } else {
        file.artifacts.push(record);
    }
}

// ---------------------------------------------------------------------------
// Decision: build vs materialize vs fail (--no-build)
// ---------------------------------------------------------------------------

/// Fine-grained result_kind values surfaced to PHASE-TIMING.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildResultKind {
    Materialized,
    Executed,
    NotApplicable,
    MissingMaterialization,
    StaleMaterialization,
    MissingOutputs,
    InvalidMaterializationState,
}

impl BuildResultKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Materialized => "materialized",
            Self::Executed => "executed",
            Self::NotApplicable => "not-applicable",
            Self::MissingMaterialization => "missing-materialization",
            Self::StaleMaterialization => "stale-materialization",
            Self::MissingOutputs => "missing-outputs",
            Self::InvalidMaterializationState => "invalid-materialization-state",
        }
    }
}

/// What `decide` recommends doing, given policy + state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecisionAction {
    /// Skip the build executor; outputs are already on disk and current.
    Skip,
    /// Run the build executor.
    Execute,
    /// `--no-build` policy refused materialization. Caller should fail with
    /// `ATO_ERR_MISSING_MATERIALIZATION`.
    Fail,
}

/// The result of consulting the policy + state for a given observation.
#[derive(Debug, Clone)]
pub(crate) struct BuildDecision {
    pub(crate) action: DecisionAction,
    pub(crate) result_kind: BuildResultKind,
}

/// Decide whether to build / skip / fail based on policy and recorded state.
///
/// Returns the action plus the canonical `result_kind` for downstream
/// emission. Callers map `Fail` to `ATO_ERR_MISSING_MATERIALIZATION`.
pub(crate) fn decide(
    policy: BuildPolicy,
    observation: &BuildObservation,
    workspace_root: &Path,
) -> BuildDecision {
    match policy {
        BuildPolicy::Rebuild => BuildDecision {
            action: DecisionAction::Execute,
            result_kind: BuildResultKind::Executed,
        },
        BuildPolicy::IfStale | BuildPolicy::NoBuild => {
            let load = load_state(workspace_root);
            match load {
                LoadOutcome::Invalid(_) => BuildDecision {
                    action: if matches!(policy, BuildPolicy::NoBuild) {
                        DecisionAction::Fail
                    } else {
                        DecisionAction::Execute
                    },
                    result_kind: BuildResultKind::InvalidMaterializationState,
                },
                LoadOutcome::Missing => BuildDecision {
                    action: if matches!(policy, BuildPolicy::NoBuild) {
                        DecisionAction::Fail
                    } else {
                        DecisionAction::Execute
                    },
                    result_kind: BuildResultKind::MissingMaterialization,
                },
                LoadOutcome::Loaded(file) => {
                    let record = find_record(
                        &file,
                        &observation.target,
                        &observation.working_dir_relative,
                        "build",
                    );
                    let Some(record) = record else {
                        return BuildDecision {
                            action: if matches!(policy, BuildPolicy::NoBuild) {
                                DecisionAction::Fail
                            } else {
                                DecisionAction::Execute
                            },
                            result_kind: BuildResultKind::MissingMaterialization,
                        };
                    };
                    if record.input_digest != observation.input_digest {
                        return BuildDecision {
                            action: if matches!(policy, BuildPolicy::NoBuild) {
                                DecisionAction::Fail
                            } else {
                                DecisionAction::Execute
                            },
                            result_kind: BuildResultKind::StaleMaterialization,
                        };
                    }
                    if !outputs_present(
                        workspace_root,
                        &observation.working_dir_relative,
                        &record.outputs,
                    ) {
                        return BuildDecision {
                            action: if matches!(policy, BuildPolicy::NoBuild) {
                                DecisionAction::Fail
                            } else {
                                DecisionAction::Execute
                            },
                            result_kind: BuildResultKind::MissingOutputs,
                        };
                    }
                    BuildDecision {
                        action: DecisionAction::Skip,
                        result_kind: BuildResultKind::Materialized,
                    }
                }
            }
        }
    }
}

fn outputs_present(workspace_root: &Path, working_dir_relative: &str, outputs: &[String]) -> bool {
    let working = if working_dir_relative.is_empty() || working_dir_relative == "." {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(working_dir_relative)
    };
    let Ok(specs) = normalize_outputs(outputs) else {
        return false;
    };
    if specs.is_empty() {
        return false;
    }
    for spec in &specs {
        let path = working.join(&spec.relative_path);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                if meta.is_file() && meta.len() == 0 {
                    return false;
                }
                if meta.is_dir() {
                    let mut iter = match std::fs::read_dir(&path) {
                        Ok(i) => i,
                        Err(_) => return false,
                    };
                    if iter.next().is_none() {
                        return false;
                    }
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// Build a fresh `MaterializationRecord` from an observation. Used after a
/// successful build to upsert into state.
pub(crate) fn record_for(
    observation: &BuildObservation,
    toolchain_fingerprint: &str,
) -> MaterializationRecord {
    MaterializationRecord {
        name: "build".to_string(),
        target: observation.target.clone(),
        working_dir: observation.working_dir_relative.clone(),
        input_digest: observation.input_digest.clone(),
        command: observation.command.clone(),
        outputs: observation.outputs.clone(),
        source: observation.source.timing_label().to_string(),
        heuristic: observation.source.heuristic_label(),
        toolchain_fingerprint: toolchain_fingerprint.to_string(),
        env_keys: Vec::new(),
        env_fingerprint: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Whether a recommendation should be emitted for this `(input_digest,
/// heuristic)` pair given prior emissions. Mutates the file to record the
/// emission when returning true.
pub(crate) fn maybe_record_recommendation(
    file: &mut MaterializationFile,
    input_digest: &str,
    heuristic: Option<&str>,
) -> bool {
    let already = file
        .recommendations_emitted
        .iter()
        .any(|r| r.input_digest == input_digest && r.heuristic.as_deref() == heuristic);
    if already {
        return false;
    }
    file.recommendations_emitted.push(RecommendationRecord {
        kind: "declare-build".to_string(),
        input_digest: input_digest.to_string(),
        heuristic: heuristic.map(str::to_string),
    });
    true
}

// ---------------------------------------------------------------------------
// Caller-friendly composite helpers (used by both `ato run`'s build phase and
// `ato app session start`'s session-start phase runner).
// ---------------------------------------------------------------------------

/// What `prepare_decision` resolved: an observation (if a build spec is
/// applicable) and the policy decision derived from it. Callers use the
/// `decision.action` to choose between skipping, executing, or failing the
/// build executor; the `observation` is what they should pass back to
/// [`persist_after_execute`] after a successful executor invocation.
#[derive(Debug, Clone)]
pub(crate) struct PreparedBuildDecision {
    pub(crate) observation: Option<BuildObservation>,
    pub(crate) decision: BuildDecision,
}

/// Observe the plan and consult the policy + state. Idempotent (no side
/// effects on disk); safe to call before every build invocation.
pub(crate) fn prepare_decision(
    plan: &capsule_core::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    policy: BuildPolicy,
    workspace_root: &Path,
) -> PreparedBuildDecision {
    let observation = observe_for_plan(plan, launch_ctx).ok().flatten();
    let decision = match observation.as_ref() {
        Some(obs) => decide(policy, obs, workspace_root),
        None => BuildDecision {
            action: DecisionAction::Execute,
            result_kind: BuildResultKind::NotApplicable,
        },
    };
    PreparedBuildDecision {
        observation,
        decision,
    }
}

/// After a successful build executor invocation, upsert the materialization
/// record and emit a one-shot heuristic recommendation. Failures to persist
/// are logged but never surfaced (the build itself succeeded; missing the
/// record only hurts the next run, which will fall back to `Execute`).
pub(crate) fn persist_after_execute(
    plan: &capsule_core::router::ManifestData,
    workspace_root: &Path,
    observation: &BuildObservation,
    suppress_recommendation: bool,
) {
    let toolchain_fp = toolchain_fingerprint_for_plan(plan);
    let mut file = match load_state(workspace_root) {
        LoadOutcome::Loaded(f) => f,
        LoadOutcome::Missing | LoadOutcome::Invalid(_) => MaterializationFile::default(),
    };
    upsert_record(&mut file, record_for(observation, &toolchain_fp));

    let heuristic_label = observation.source.heuristic_label();
    if heuristic_label.is_some() && !suppress_recommendation {
        let emitted = maybe_record_recommendation(
            &mut file,
            &observation.input_digest,
            heuristic_label.as_deref(),
        );
        if emitted {
            eprintln!(
                "ATO-RECOMMEND build inputs were inferred for \"{}\" framework. \
                 Declare [build] inputs/outputs in capsule.toml for stable \
                 materialization. See: docs/rfcs/draft/BUILD_MATERIALIZATION.md",
                heuristic_label.as_deref().unwrap_or("(unknown)")
            );
        }
    }

    if let Err(err) = save_state(workspace_root, &file) {
        eprintln!(
            "ATO-WARN failed to persist build materialization state: {}",
            err
        );
    }
}

/// Build the user-facing error returned when policy=NoBuild cannot be
/// satisfied. The decision's `result_kind` carries the granular reason
/// (`missing-materialization` / `stale-materialization` / etc.).
pub(crate) fn no_build_error(decision: &BuildDecision) -> anyhow::Error {
    anyhow::anyhow!(
        "{}: {} (policy=no-build)",
        crate::utils::error::ATO_ERR_MISSING_MATERIALIZATION,
        decision.result_kind.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn manifest_with_build(toml_str: &str) -> toml::Value {
        toml::from_str(toml_str).expect("parse toml")
    }

    fn no_env(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn declared_build_takes_priority_over_legacy() {
        let manifest = manifest_with_build(
            r#"
            [build]
            command = "npm run build"
            inputs = ["src/**"]
            outputs = [".next"]
            "#,
        );
        let dir = tempfile::tempdir().expect("tmpdir");
        let spec = resolve_build_spec(&manifest, dir.path(), Some("legacy build"))
            .expect("resolve")
            .expect("spec");
        assert_eq!(spec.command, "npm run build");
        assert!(matches!(spec.source, BuildSpecSource::Declared));
        assert_eq!(spec.outputs, vec![".next".to_string()]);
    }

    #[test]
    fn legacy_command_falls_back_to_heuristic_for_nextjs() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {};")
            .expect("write next.config");
        let manifest = manifest_with_build("");
        let spec = resolve_build_spec(&manifest, dir.path(), Some("npm run build"))
            .expect("resolve")
            .expect("spec");
        assert_eq!(spec.command, "npm run build");
        assert!(matches!(
            spec.source,
            BuildSpecSource::Heuristic {
                name: "nextjs",
                version: "v1"
            }
        ));
        assert_eq!(spec.outputs, vec![".next".to_string()]);
    }

    #[test]
    fn no_legacy_command_and_no_build_section_yields_none() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let manifest = manifest_with_build("");
        let spec = resolve_build_spec(&manifest, dir.path(), None).expect("resolve");
        assert!(spec.is_none());
    }

    #[test]
    fn missing_heuristic_with_legacy_command_yields_none() {
        let dir = tempfile::tempdir().expect("tmpdir");
        // No next.config, no vite.config, no package.json with `next` dep.
        let manifest = manifest_with_build("");
        let spec =
            resolve_build_spec(&manifest, dir.path(), Some("npm run build")).expect("resolve");
        assert!(spec.is_none());
    }

    #[test]
    fn nextjs_detected_via_package_json_dependency() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "demo", "dependencies": {"next": "15.0.0"}}"#,
        )
        .expect("write package.json");
        let manifest = manifest_with_build("");
        let spec = resolve_build_spec(&manifest, dir.path(), Some("npm run build"))
            .expect("resolve")
            .expect("spec");
        assert!(matches!(
            spec.source,
            BuildSpecSource::Heuristic { name: "nextjs", .. }
        ));
    }

    #[test]
    fn vite_detected_via_config_extension_variants() {
        for ext in ["js", "ts", "mjs", "cjs"] {
            let dir = tempfile::tempdir().expect("tmpdir");
            std::fs::write(
                dir.path().join(format!("vite.config.{}", ext)),
                "export default {}",
            )
            .expect("write vite.config");
            let manifest = manifest_with_build("");
            let spec = resolve_build_spec(&manifest, dir.path(), Some("npm run build"))
                .expect("resolve")
                .expect("spec");
            assert!(matches!(
                spec.source,
                BuildSpecSource::Heuristic { name: "vite", .. }
            ));
            assert_eq!(spec.outputs, vec!["dist".to_string()]);
        }
    }

    #[test]
    fn digest_changes_when_command_changes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("file.txt"), "hello").expect("write");
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {};")
            .expect("write next.config");
        let manifest = manifest_with_build("");

        let observation_a = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            Some("npm run build"),
            "tc",
            no_env,
        )
        .expect("observe a")
        .expect("some");
        let observation_b = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            Some("npm run build:prod"),
            "tc",
            no_env,
        )
        .expect("observe b")
        .expect("some");

        assert_ne!(observation_a.input_digest, observation_b.input_digest);
    }

    #[test]
    fn digest_changes_when_source_file_changes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {};")
            .expect("write next.config");
        std::fs::write(dir.path().join("a.txt"), "v1").expect("write");
        let manifest = manifest_with_build("");

        let digest_v1 = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            Some("npm run build"),
            "tc",
            no_env,
        )
        .expect("observe v1")
        .expect("some")
        .input_digest;

        std::fs::write(dir.path().join("a.txt"), "v2").expect("rewrite");

        let digest_v2 = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            Some("npm run build"),
            "tc",
            no_env,
        )
        .expect("observe v2")
        .expect("some")
        .input_digest;

        assert_ne!(digest_v1, digest_v2);
    }

    #[test]
    fn digest_changes_when_target_or_working_dir_changes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {};")
            .expect("write next.config");
        let manifest = manifest_with_build("");

        let app_app = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            Some("npm run build"),
            "tc",
            no_env,
        )
        .expect("ok")
        .expect("some")
        .input_digest;
        let app_other = observe(
            &manifest,
            "other",
            dir.path(),
            dir.path(),
            Some("npm run build"),
            "tc",
            no_env,
        )
        .expect("ok")
        .expect("some")
        .input_digest;
        assert_ne!(app_app, app_other);
    }

    #[test]
    fn digest_includes_env_value_hash_not_raw() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {};")
            .expect("write next.config");
        let manifest = manifest_with_build(
            r#"
            [build]
            command = "npm run build"
            inputs = []
            outputs = [".next"]
            [build.env]
            include = ["MY_VAR"]
            "#,
        );

        let mut env: HashMap<&str, &str> = HashMap::new();
        env.insert("MY_VAR", "value-1");
        let lookup_a = |key: &str| env.get(key).map(|v| v.to_string());
        let digest_a = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            None,
            "tc",
            lookup_a,
        )
        .expect("ok")
        .expect("some")
        .input_digest;

        let mut env_b: HashMap<&str, &str> = HashMap::new();
        env_b.insert("MY_VAR", "value-2");
        let lookup_b = |key: &str| env_b.get(key).map(|v| v.to_string());
        let digest_b = observe(
            &manifest,
            "app",
            dir.path(),
            dir.path(),
            None,
            "tc",
            lookup_b,
        )
        .expect("ok")
        .expect("some")
        .input_digest;

        assert_ne!(digest_a, digest_b);
    }

    #[test]
    fn build_spec_source_labels_round_trip() {
        let declared = BuildSpecSource::Declared;
        assert_eq!(declared.fingerprint_label(), "declared");
        assert_eq!(declared.timing_label(), "declared");
        assert_eq!(declared.heuristic_label(), None);

        let heuristic = BuildSpecSource::Heuristic {
            name: "nextjs",
            version: "v1",
        };
        assert_eq!(heuristic.fingerprint_label(), "heuristic:nextjs:v1");
        assert_eq!(heuristic.timing_label(), "heuristic");
        assert_eq!(heuristic.heuristic_label().as_deref(), Some("nextjs:v1"));
    }
}
