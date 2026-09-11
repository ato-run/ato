//! Evidence + authored intent → Program Intent → Effective Build Plan.
//!
//! Pure. Given the same evidence and the same overrides it produces the same
//! digests, which is what makes a formation key mean anything.
//!
//! ## Authored outranks inferred, always
//!
//! An override is what the author said. Inference is what a heuristic guessed.
//! Where they disagree the author wins, and the provenance records which field
//! came from where — so "why is it running that?" has an answer that does not
//! require re-running the detector.
//!
//! ## What this refuses to guess
//!
//! A launch argv, a state slot, and a Python runtime it cannot resolve
//! exactly. Each refusal is deliberate:
//!
//! - a guessed argv launches something the author never asked to launch;
//! - a guessed state slot means an app writes somewhere that is silently
//!   discarded on the next Run, which looks like data loss and is;
//! - a guessed interpreter means the build and the Run can disagree about what
//!   "python" is, and the failure surfaces at import time, far from its cause.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::detect::{
    DetectorEvidence, FieldOrigin, FieldOrigins, NodeEvidence, PythonEvidence, ViteOutDir,
};
use crate::preset::{AppPreset, preset_overrides};

/// Python versions this build knows how to provision, newest first.
///
/// A catalog rather than "whatever the host has": an implicit host interpreter
/// makes the build unreproducible and makes the Runner's interpreter a
/// coincidence.
pub const SUPPORTED_PYTHON: &[&str] = &["3.13.1", "3.12.7", "3.11.11"];

/// Used when nothing pins a version. Named so a reader can see the choice.
pub const DEFAULT_PYTHON: &str = "3.12.7";

/// Where a provisioned interpreter lives, inside the build sandbox AND inside
/// the runtime sandbox.
///
/// One deterministic path, because a venv created during the build records the
/// absolute path of the interpreter that made it. A build that used a
/// temporary directory produces a `pyvenv.cfg` and shebangs pointing somewhere
/// that does not exist at runtime, and the failure arrives as an import error
/// far from its cause.
pub const TOOLCHAIN_ROOT: &str = "/opt/ato/toolchains";

/// The release of python-build-standalone this catalog is pinned to.
///
/// Pinned rather than "latest": a moving upstream would silently change what a
/// formation key means, and two builds of the same commit could differ.
pub const PYTHON_BUILD_TAG: &str = "20241016";

/// The absolute path of a provisioned interpreter.
pub fn python_home(version: &str) -> String {
    format!("{TOOLCHAIN_ROOT}/python/{version}")
}

/// Where python-build-standalone publishes a relocatable build.
///
/// `install_only_stripped`, not `install_only`. The unstripped build ships a
/// **207 MB** `libpython3.12.so.1.0`, and the workspace vendors that library —
/// which took the acceptance artifact to 471 MB before anyone noticed. Debug
/// symbols in a published workspace are pure weight: nothing downstream can
/// use them, and every instance pays to move them.
///
/// The `+` in the asset name is percent-encoded. GitHub serves the asset under
/// `%2B`, and a literal `+` returns an HTML error page that `tar` then fails to
/// read — seen first as "gzip: unexpected end of file".
pub fn python_download_url(version: &str, triple: &str) -> String {
    format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/\
{PYTHON_BUILD_TAG}/cpython-{version}%2B{PYTHON_BUILD_TAG}-{triple}-install_only_stripped.tar.gz"
    )
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IntentError {
    #[error("no lane matched this source: {detail}")]
    NoLane { detail: String },
    #[error("{field} must be authored; this build does not infer it ({why})")]
    RequiresAuthoring {
        field: &'static str,
        why: &'static str,
    },
    #[error("ambiguous dependency lockfiles: {found}")]
    AmbiguousLockfiles { found: String },
    #[error("python {requested} is not in the supported runtime catalog")]
    UnsupportedPython { requested: String },
    #[error("node {requested} is not satisfied by any version this resolver provisions")]
    UnsupportedNode { requested: String },
    #[error("{field} is malformed: {detail}")]
    Malformed { field: &'static str, detail: String },
}

impl IntentError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoLane { .. } => "intent_no_lane",
            Self::RequiresAuthoring { .. } => "intent_requires_authoring",
            Self::AmbiguousLockfiles { .. } => "intent_ambiguous_lockfiles",
            Self::UnsupportedPython { .. } => "intent_unsupported_python",
            Self::UnsupportedNode { .. } => "intent_unsupported_node",
            Self::Malformed { .. } => "intent_malformed",
        }
    }
}

/// Which lane a source is built through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    StaticWeb,
    PythonProcess,
}

/// How dependencies are resolved, and how strong the reproducibility is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyPlan {
    /// A lockfile pins every version. Reproducible.
    UvFrozen,
    /// A requirements file pins what it pins and no more. Recorded as weaker,
    /// because a build that silently resolves differently on Tuesday is not
    /// reproducible and should not claim to be.
    PipRequirements { reproducibility: String },
    /// Nothing to install.
    None,
}

/// How a generated Static site is built.
///
/// Every field is resolved from the source's own declarations. None of it is a
/// runtime realization: a ComputeSchema formed from this still says "browser",
/// and nothing here ever becomes a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticBuildProfileV1 {
    pub package_manager: PackageManager,
    /// Exact. A range never survives this far.
    pub node_version: String,
    /// The package script to run, e.g. `build`.
    pub build_script: String,
    /// Where the build writes the site, relative to the workspace root.
    pub output_root: String,
    /// Whether a lockfile pins the dependency graph. Recorded because a build
    /// without one is not reproducible, and a plan that implied otherwise
    /// would be lying about what it did.
    pub lockfile_pinned: bool,
}

/// The normalized intent. Digestible, and the input to the build plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramIntentV1 {
    pub schema: String,
    pub lane: Lane,
    /// Exact versions. A range never survives this far.
    pub runtime: BTreeMap<String, String>,
    pub dependencies: DependencyPlan,
    /// The launch argv, exactly as authored.
    pub launch_argv: Vec<String>,
    #[serde(default)]
    pub cwd_relative: String,
    #[serde(default)]
    pub public_env: BTreeMap<String, String>,
    pub exported_ports: Vec<(String, u16)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_http_path: Option<String>,
    pub state_slots: Vec<(String, String)>,
    /// For the Static lane: the declared output root, never a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_output_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_entry_path: Option<String>,
    #[serde(default)]
    pub static_spa_fallback: bool,
    /// Present only when the Static site is GENERATED. Omitted for a
    /// source-static intent, so those digests are unchanged by this addition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_build: Option<StaticBuildProfileV1>,
}

pub const PROGRAM_INTENT_V1_SCHEMA: &str = "ato.program-intent.v1";

impl ProgramIntentV1 {
    pub fn canonical_digest(&self) -> Result<String, IntentError> {
        let bytes = serde_jcs::to_vec(self).map_err(|error| IntentError::Malformed {
            field: "program_intent",
            detail: error.to_string(),
        })?;
        Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
    }
}

/// One step of a build, as an argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildStepV1 {
    pub name: String,
    pub argv: Vec<String>,
    /// Whether this step needs the network. Declared per step, so a plan can
    /// say "fetch, then build offline" instead of opening the network for the
    /// whole build.
    pub needs_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveBuildPlanV1 {
    pub schema: String,
    pub lane: Lane,
    /// Where the workspace lives during the build AND at runtime. One string,
    /// because a venv records absolute paths and a build that used a temporary
    /// directory produces a workspace whose interpreter does not exist later.
    pub workspace_guest_root: String,
    pub runtime: BTreeMap<String, String>,
    pub steps: Vec<BuildStepV1>,
    /// The subtree that becomes the materialization, relative to the workspace.
    /// `""` is the whole workspace.
    #[serde(default)]
    pub output_root: String,
}

pub const EFFECTIVE_BUILD_PLAN_V1_SCHEMA: &str = "ato.effective-build-plan.v1";

impl EffectiveBuildPlanV1 {
    pub fn canonical_digest(&self) -> Result<String, IntentError> {
        let bytes = serde_jcs::to_vec(self).map_err(|error| IntentError::Malformed {
            field: "effective_build_plan",
            detail: error.to_string(),
        })?;
        Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
    }
}

/// What the author declared, flat, so the wire form stays a string map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthoredOverrides(pub BTreeMap<String, String>);

impl AuthoredOverrides {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// Compile evidence plus authored intent into a normalized intent.
///
/// An App Preset, when one is selected, is expanded into the SAME authored
/// overrides a person could have written by hand, and then compiled by the
/// path below. That is what stops a preset from becoming a second way to
/// describe an App: it is a short name for a set of decisions, not a parallel
/// language, and the intent it produces has no memory of which door it came
/// through.
pub fn compile_intent_for_preset(
    preset: AppPreset,
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    workspace_guest_root: &str,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    let mut merged = overrides.0.clone();
    for (key, value) in preset_overrides(preset) {
        // An explicit override wins. A preset is a default set, and a person
        // who states something meant it.
        merged.entry(key.to_owned()).or_insert(value);
        origins
            .entry(key.to_owned())
            .or_insert(FieldOrigin::PolicyDefault);
    }
    compile_intent(
        evidence,
        &AuthoredOverrides(merged),
        workspace_guest_root,
        origins,
    )
}

/// Compile evidence plus authored intent into a normalized intent.
pub fn compile_intent(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    workspace_guest_root: &str,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    let lane = choose_lane(evidence, overrides, origins)?;
    match lane {
        Lane::StaticWeb => compile_static(evidence, overrides, origins),
        Lane::PythonProcess => compile_python(evidence, overrides, workspace_guest_root, origins),
    }
}

fn choose_lane(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    origins: &mut FieldOrigins,
) -> Result<Lane, IntentError> {
    if let Some(declared) = overrides.get("lane") {
        origins.insert("lane".to_owned(), FieldOrigin::Authored);
        return match declared {
            "static_web" => Ok(Lane::StaticWeb),
            "python_process" => Ok(Lane::PythonProcess),
            other => Err(IntentError::Malformed {
                field: "lane",
                detail: format!("unknown lane {other:?}"),
            }),
        };
    }
    origins.insert("lane".to_owned(), FieldOrigin::DetectedFromSource);
    if evidence.python.is_some() {
        return Ok(Lane::PythonProcess);
    }
    if evidence
        .static_web
        .as_ref()
        .is_some_and(|static_web| static_web.has_root_index_html)
    {
        return Ok(Lane::StaticWeb);
    }
    Err(IntentError::NoLane {
        detail: "no Python marker and no root index.html; declare `lane` to build this source"
            .to_owned(),
    })
}

// ─── Static Build Profile v1 ────────────────────────────────────────────────
//
// A Static Compute is evaluated by the browser, but that says nothing about
// whether its files EXIST in the source. Two shapes reach the same
// realization:
//
//   source-static   source tree → selection → Static output
//   built-static    source tree → build     → generated output → Static output
//
// Not two product lanes: the realization is identical, and only Formation
// differs. Treating Static as "never needs a build" was measured wrong by
// B1-S — `ato-e2e-static-spa@1e1be10` published its stale checked-in `dist/`
// (STATIC_FIXTURE_V2) where the existing path published a real Vite build of
// the source (STATIC_FIXTURE_V1). Different application code, reported as
// success.
//
// Node here is a BUILD TOOL and nothing else. This profile never yields a Node
// process, a Node runtime realization, or a Node RuntimeLaunchSpec. "Built
// with Node" is not "runs on Node".

/// Node versions this resolver will provision, lowest first.
///
/// Carried over verbatim from the existing Builder's ladder so that a source
/// which resolved to a given Node there resolves to the same one here. All
/// four are published on nodejs.org as `linux-x64` tarballs (verified).
pub const NODE_VERSION_LADDER: &[&str] = &["18.20.4", "20.20.2", "22.14.0", "24.18.0"];

/// The version used when the source declares nothing. Also the existing
/// Builder's default, for the same reason.
pub const DEFAULT_NODE: &str = "20.20.2";

/// Packages whose presence means the repository serves itself.
///
/// A dependency set containing one of these is a server, and a server is not a
/// Static Compute however plainly its build script reads.
const SERVER_FRAMEWORKS: &[&str] = &[
    "express",
    "fastify",
    "koa",
    "@hapi/hapi",
    "next",
    "nuxt",
    "@remix-run/node",
    "@sveltejs/kit",
    "socket.io",
    "ws",
];

pub fn node_home(version: &str) -> String {
    format!("{TOOLCHAIN_ROOT}/node/{version}")
}

/// The official Node distribution. `.tar.gz`, so the sandbox needs no `xz`.
pub fn node_download_url(version: &str, triple: &str) -> String {
    format!("https://nodejs.org/dist/v{version}/node-v{version}-{triple}.tar.gz")
}

fn node_triple(target_triple: &str) -> &'static str {
    if target_triple.starts_with("aarch64") {
        "linux-arm64"
    } else {
        "linux-x64"
    }
}

/// Which package manager runs the build, and how reproducibly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub fn program(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "npm" => Some(Self::Npm),
            "pnpm" => Some(Self::Pnpm),
            "yarn" => Some(Self::Yarn),
            "bun" => Some(Self::Bun),
            _ => None,
        }
    }
}

/// Split a package script into argv, or refuse.
///
/// Refuses anything that is not a plain invocation: shell operators, quotes,
/// redirection, substitution. `npm run build:app && cp -r x y` is a program
/// this reader must not pretend to understand.
fn plain_task_argv(script: &str) -> Option<Vec<String>> {
    let script = script.trim();
    if script.is_empty()
        || script.chars().any(|c| {
            matches!(
                c,
                '\'' | '"' | '\\' | ';' | '|' | '&' | '$' | '<' | '>' | '(' | ')' | '`'
            ) || c.is_control()
        })
    {
        return None;
    }
    Some(script.split_ascii_whitespace().map(str::to_owned).collect())
}

fn tail_is(argv: &[String], tail: [&str; 2]) -> bool {
    argv.len() >= 2 && argv[argv.len() - 2] == tail[0] && argv[argv.len() - 1] == tail[1]
}

/// The package manager, from the source's own declarations.
///
/// `packageManager` wins because it is the author saying so. Otherwise a
/// single lockfile decides. Several lockfiles naming DIFFERENT managers is
/// ambiguous and fails closed: picking one would silently resolve a dependency
/// graph the author never chose.
fn resolve_package_manager(node: &NodeEvidence) -> Result<PackageManager, IntentError> {
    if let Some(declared) = node.package_manager.as_deref() {
        let name = declared.split('@').next().unwrap_or_default();
        return PackageManager::parse(name).ok_or_else(|| IntentError::Malformed {
            field: "package.json packageManager",
            detail: format!("unsupported package manager {name:?}"),
        });
    }
    let mut found: Vec<PackageManager> = Vec::new();
    for (present, manager) in [
        (
            node.has_package_lock || node.has_npm_shrinkwrap,
            PackageManager::Npm,
        ),
        (node.has_pnpm_lock, PackageManager::Pnpm),
        (node.has_yarn_lock, PackageManager::Yarn),
        (node.has_bun_lock, PackageManager::Bun),
    ] {
        if present && !found.contains(&manager) {
            found.push(manager);
        }
    }
    match found.as_slice() {
        // No lockfile at all: npm is available wherever package.json is
        // honored. Recorded as the weakest reproducibility below — nothing is
        // pinned, and the plan must say so rather than imply a lock.
        [] => Ok(PackageManager::Npm),
        [single] => Ok(*single),
        multiple => Err(IntentError::AmbiguousLockfiles {
            found: multiple
                .iter()
                .map(|manager| manager.program())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// The exact Node version, from the source's own declarations.
///
/// The host's `node` is never the answer. Which Node a build used is a
/// property of the source, not of whichever machine claimed the job — the same
/// lesson the Python lane learned when a host's 3.14 silently changed what
/// `pydantic-core` did.
fn resolve_node_version(node: &NodeEvidence) -> Result<String, IntentError> {
    let exact = |raw: &str| -> Result<String, IntentError> {
        let trimmed = raw.trim().trim_start_matches('v');
        let version = semver::Version::parse(trimmed).map_err(|error| IntentError::Malformed {
            field: "node version",
            detail: format!("{raw:?} is not an exact version: {error}"),
        })?;
        Ok(version.to_string())
    };
    if let Some(raw) = node.node_version_file.as_deref() {
        return exact(raw);
    }
    if let Some(raw) = node.node_version_alt_file.as_deref() {
        return exact(raw);
    }
    if let Some(raw) = node.volta_node.as_deref() {
        return exact(raw);
    }
    if let Some(raw) = node.engines_node.as_deref() {
        let requirement =
            semver::VersionReq::parse(raw).map_err(|error| IntentError::Malformed {
                field: "package.json engines.node",
                detail: format!("{raw:?} is not a parseable range: {error}"),
            })?;
        let default = semver::Version::parse(DEFAULT_NODE).expect("DEFAULT_NODE is exact");
        if requirement.matches(&default) {
            return Ok(DEFAULT_NODE.to_owned());
        }
        return NODE_VERSION_LADDER
            .iter()
            .filter_map(|candidate| semver::Version::parse(candidate).ok())
            .find(|candidate| requirement.matches(candidate))
            .map(|version| version.to_string())
            .ok_or_else(|| IntentError::UnsupportedNode {
                requested: raw.to_owned(),
            });
    }
    Ok(DEFAULT_NODE.to_owned())
}

/// What the source says about building itself, when it says it unambiguously.
///
/// `None` is "this is a source-static site", never "build it anyway and hope".
/// Every refusal below is deliberate: the alternative to a clear answer is a
/// wrong artifact that reports success.
///
/// Note what is NOT consulted: whether a `dist/` exists. Fixture B ships a
/// checked-in `dist/` that is a DIFFERENT VERSION of the app from its source,
/// so "the output directory is already here" is evidence of nothing.
fn detect_static_build(
    evidence: &DetectorEvidence,
) -> Result<Option<StaticBuildProfileV1>, IntentError> {
    let Some(node) = evidence.node.as_ref() else {
        return Ok(None);
    };
    let (Some(build), Some(preview)) = (
        node.script_build.as_deref().and_then(plain_task_argv),
        node.script_preview.as_deref().and_then(plain_task_argv),
    ) else {
        return Ok(None);
    };
    // The same gate the existing path used: `vite build` + `vite preview`, both
    // plain. A compound build script is a program, and this is not its
    // interpreter.
    if !tail_is(&build, ["vite", "build"]) || !tail_is(&preview, ["vite", "preview"]) {
        return Ok(None);
    }
    if node
        .dependency_names
        .iter()
        .any(|name| SERVER_FRAMEWORKS.contains(&name.as_str()))
    {
        return Ok(None);
    }
    let output_root = match &node.vite_out_dir {
        ViteOutDir::Unset => "dist".to_owned(),
        ViteOutDir::Literal(literal) => literal.trim_matches('/').to_owned(),
        // An override that exists but cannot be read is the one case where
        // guessing is worst: `dist` would be published while the build wrote
        // somewhere else, and the artifact would be stale-or-empty rather than
        // wrong-and-obvious.
        ViteOutDir::Unreadable => {
            return Err(IntentError::RequiresAuthoring {
                field: "static.output_root",
                why: "vite.config declares a build.outDir this reader cannot resolve to a \
                      literal; declare the output root rather than have one guessed",
            });
        }
    };
    Ok(Some(StaticBuildProfileV1 {
        package_manager: resolve_package_manager(node)?,
        node_version: resolve_node_version(node)?,
        // The SCRIPT name, not the command. `<pm> run build` is what the
        // existing path ran, and running the script keeps the package's own
        // definition authoritative.
        build_script: "build".to_owned(),
        output_root,
        // Weakest honest statement: only a lockfile pins a graph.
        lockfile_pinned: node.has_package_lock
            || node.has_npm_shrinkwrap
            || node.has_pnpm_lock
            || node.has_yarn_lock
            || node.has_bun_lock,
    }))
}

/// `node-static/v1`, as a fixed contract rather than an inference.
///
/// The preset selector has already established that a `package.json`, a
/// lockfile and a `build` script exist; this turns them into the plan. What it
/// does NOT do is read the build script's text to decide whether it is "really"
/// Vite. A project whose build writes somewhere other than `dist/` fails later,
/// at the output root, with a sentence that says so — a better failure than a
/// heuristic quietly picking a different directory.
fn fixed_node_static_build(
    evidence: &DetectorEvidence,
) -> Result<StaticBuildProfileV1, IntentError> {
    let node = evidence
        .node
        .as_ref()
        .ok_or(IntentError::RequiresAuthoring {
            field: "static.build",
            why: "a built web app needs a package.json; this source has none",
        })?;
    if node.script_build.is_none() {
        return Err(IntentError::RequiresAuthoring {
            field: "static.build",
            why: "a built web app needs a `build` script in package.json — Ato runs \
                  `npm run build` and publishes what it writes to `dist/`",
        });
    }
    Ok(StaticBuildProfileV1 {
        package_manager: resolve_package_manager(node)?,
        node_version: resolve_node_version(node)?,
        build_script: "build".to_owned(),
        output_root: crate::preset::NODE_STATIC_OUTPUT_ROOT.to_owned(),
        lockfile_pinned: node.has_package_lock
            || node.has_npm_shrinkwrap
            || node.has_pnpm_lock
            || node.has_yarn_lock
            || node.has_bun_lock,
    })
}

fn compile_static(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    // Is the site generated, or is it the source tree?
    //
    // Decided from the source's own declarations, before anything looks at
    // which directories exist. `static.build` overrides it — an author saying
    // "this needs no build" or "this does" outranks inference — and
    // `"required"` with nothing to infer from is a refusal, not a guess.
    let detected_build = detect_static_build(evidence)?;
    let static_build = match overrides.get("static.build").map(str::trim) {
        Some("none") => {
            origins.insert("static.build".to_owned(), FieldOrigin::Authored);
            None
        }
        Some("required") => {
            origins.insert("static.build".to_owned(), FieldOrigin::Authored);
            // `node-static/v1`'s contract, and the whole of it:
            //
            //     npm ci  ->  npm run build  ->  dist/
            //
            // Not "whatever this project seems to do". The detected profile is
            // used when it agrees; otherwise the fixed contract is applied and
            // the project is expected to meet it. That is the trade this preset
            // makes deliberately: Ato grows no build-command, output-directory
            // or package-manager settings, and a person has one rule to
            // remember.
            Some(match detected_build {
                Some(build) => build,
                None => fixed_node_static_build(evidence)?,
            })
        }
        Some(other) => {
            return Err(IntentError::Malformed {
                field: "static.build",
                detail: format!("expected `required` or `none`, got {other:?}"),
            });
        }
        None => {
            if detected_build.is_some() {
                origins.insert("static.build".to_owned(), FieldOrigin::DetectedFromSource);
            }
            detected_build
        }
    };

    // A `dist/` that happens to exist does NOT select the Static lane's output.
    // A repository can carry a checked-in build directory, a vendored example
    // or a stale artifact, and publishing one because it looked like an output
    // is how the wrong bytes get served.
    //
    // A built-static site answers this differently: its output root is where
    // the BUILD writes, taken from the same declaration that established there
    // is a build at all — still never from a directory that merely exists.
    let output_root = match overrides.get("static.output_root") {
        Some(declared) => {
            origins.insert("static.output_root".to_owned(), FieldOrigin::Authored);
            declared.trim_matches('/').to_owned()
        }
        None if static_build.is_some() => {
            origins.insert(
                "static.output_root".to_owned(),
                FieldOrigin::DetectedFromSource,
            );
            static_build
                .as_ref()
                .expect("just matched Some")
                .output_root
                .clone()
        }
        None => {
            let has_root_html = evidence
                .static_web
                .as_ref()
                .is_some_and(|static_web| static_web.has_root_index_html);
            if !has_root_html {
                return Err(IntentError::RequiresAuthoring {
                    field: "static.output_root",
                    why: "a build output directory is never selected because it exists; declare \
                          which directory is the site",
                });
            }
            origins.insert(
                "static.output_root".to_owned(),
                FieldOrigin::DetectedFromSource,
            );
            String::new()
        }
    };

    let entry_path = overrides
        .get("static.entry_path")
        .map(|value| {
            origins.insert("static.entry_path".to_owned(), FieldOrigin::Authored);
            value.to_owned()
        })
        .unwrap_or_else(|| {
            origins.insert("static.entry_path".to_owned(), FieldOrigin::PolicyDefault);
            "index.html".to_owned()
        });

    let spa_fallback = match overrides.get("static.spa_fallback") {
        Some(value) => {
            origins.insert("static.spa_fallback".to_owned(), FieldOrigin::Authored);
            value == "true"
        }
        None => {
            origins.insert("static.spa_fallback".to_owned(), FieldOrigin::PolicyDefault);
            false
        }
    };

    // Node lands in `runtime` for a BUILT static site, exactly as Python does
    // for a process — a toolchain the build provisions. It is not a runtime the
    // Compute runs on: this intent has no argv, no port and no state, and a
    // browser evaluates what it produces.
    let mut runtime = BTreeMap::new();
    if let Some(build) = static_build.as_ref() {
        runtime.insert("node".to_owned(), build.node_version.clone());
    }
    let dependencies = match static_build.as_ref() {
        None => DependencyPlan::None,
        Some(build) if build.lockfile_pinned => DependencyPlan::UvFrozen,
        Some(_) => DependencyPlan::PipRequirements {
            // No lockfile: the package manager resolves the graph at build
            // time, so two builds of this same pinned commit can differ. Said
            // plainly rather than implied.
            reproducibility: "unpinned-package-manager-resolution".to_owned(),
        },
    };

    Ok(ProgramIntentV1 {
        schema: PROGRAM_INTENT_V1_SCHEMA.to_owned(),
        lane: Lane::StaticWeb,
        runtime,
        dependencies,
        // A Static Compute is evaluated by the browser. It has no process, so
        // it has no argv, and inventing one would imply a Runner it must not
        // need.
        launch_argv: Vec::new(),
        cwd_relative: String::new(),
        public_env: BTreeMap::new(),
        exported_ports: Vec::new(),
        readiness_http_path: None,
        state_slots: Vec::new(),
        static_output_root: Some(output_root),
        static_entry_path: Some(entry_path),
        static_spa_fallback: spa_fallback,
        static_build,
    })
}

fn compile_python(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    workspace_guest_root: &str,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    // A Python program need not declare dependencies.
    //
    // `ato-e2e-compute-server@4f442f1` is one: a stdlib-only `server.py` with
    // no `requirements.txt`, no `pyproject.toml` and no `.python-version`. The
    // existing path formed it from an authored capsule.toml, and B1-S measured
    // that Formation refused it — not for want of authoring, which was
    // present, but for want of a DEPENDENCY marker. Those are different
    // things, and requiring the second is a rule that says every Python app
    // must have dependencies.
    //
    // Only the AUTHORED lane gets this. `choose_lane` is untouched, so a
    // static repository carrying a stray `build.py` is still a static
    // repository — auto-detection keeps needing a marker, and nothing here
    // makes a Python lane the answer to a question nobody asked.
    let marker_less;
    let python = match evidence.python.as_ref() {
        Some(python) => python,
        None => {
            if origins.get("lane") != Some(&FieldOrigin::Authored) {
                return Err(IntentError::NoLane {
                    detail: "the Python lane was selected but the source carries no Python marker"
                        .to_owned(),
                });
            }
            let modules: Vec<String> = evidence
                .present_files
                .iter()
                .filter(|name| name.ends_with(".py"))
                .cloned()
                .collect();
            if modules.is_empty() {
                return Err(IntentError::NoLane {
                    detail: "the Python lane was authored, but the source root holds no Python                              module and no dependency marker — there is nothing here to run"
                        .to_owned(),
                });
            }
            // Everything else stays absent, which is the accurate reading:
            // no lockfile, no requirements, no declared version. The runtime
            // falls to the policy default and dependencies resolve to None.
            marker_less = PythonEvidence {
                top_level_modules: modules,
                ..PythonEvidence::default()
            };
            &marker_less
        }
    };

    // ── runtime ─────────────────────────────────────────────────────────────
    // Priority is fixed and stated: authored, then .python-version, then
    // requires-python, then the policy default. A range is resolved to an EXACT
    // catalog version here; nothing downstream sees a constraint.
    let (requested, origin) = if let Some(value) = overrides.get("runtime.python") {
        (value.to_owned(), FieldOrigin::Authored)
    } else if let Some(value) = &python.python_version_file {
        (value.clone(), FieldOrigin::DetectedFromSource)
    } else if let Some(value) = &python.requires_python {
        (value.clone(), FieldOrigin::DetectedFromSource)
    } else {
        (DEFAULT_PYTHON.to_owned(), FieldOrigin::PolicyDefault)
    };
    let resolved = resolve_python(&requested)?;
    origins.insert("runtime.python".to_owned(), origin);

    // ── dependencies ────────────────────────────────────────────────────────
    let dependencies = resolve_dependencies(python, overrides, origins)?;

    // ── launch ──────────────────────────────────────────────────────────────
    // Not inferred, at all. A guessed entrypoint launches something the author
    // never asked to launch, and the failure looks like the app's fault.
    let launch_argv = split_argv(overrides.get("launch.argv").ok_or(
        IntentError::RequiresAuthoring {
            field: "launch.argv",
            why: "an entrypoint is not guessed from a framework or a filename",
        },
    )?)?;
    origins.insert("launch.argv".to_owned(), FieldOrigin::Authored);

    // ── ports and readiness ─────────────────────────────────────────────────
    let port = match overrides.get("port.http") {
        Some(value) => {
            origins.insert("port.http".to_owned(), FieldOrigin::Authored);
            value.parse::<u16>().map_err(|_| IntentError::Malformed {
                field: "port.http",
                detail: format!("{value:?} is not a port"),
            })?
        }
        None => {
            return Err(IntentError::RequiresAuthoring {
                field: "port.http",
                why: "the port a workload listens on is declared, not read from a framework",
            });
        }
    };
    let readiness = overrides
        .get("readiness.http_path")
        .map(|value| {
            origins.insert("readiness.http_path".to_owned(), FieldOrigin::Authored);
            value.to_owned()
        })
        .or_else(|| {
            origins.insert("readiness.http_path".to_owned(), FieldOrigin::PolicyDefault);
            Some("/".to_owned())
        });

    // ── state ───────────────────────────────────────────────────────────────
    // Declared or absent. A guessed slot means an app writes somewhere that is
    // silently discarded on the next Run — which looks like data loss, and is.
    let mut state_slots = Vec::new();
    for (key, value) in &overrides.0 {
        if let Some(state_key) = key
            .strip_prefix("state.")
            .and_then(|rest| rest.strip_suffix(".mount"))
        {
            if !value.starts_with('/') || value.contains("/../") || value.ends_with("/..") {
                return Err(IntentError::Malformed {
                    field: "state.<key>.mount",
                    detail: format!("{value:?} is not an absolute, traversal-free guest path"),
                });
            }
            state_slots.push((state_key.to_owned(), value.clone()));
            origins.insert(format!("state.{state_key}"), FieldOrigin::Authored);
        }
    }
    state_slots.sort();

    let mut public_env = BTreeMap::new();
    for (key, value) in &overrides.0 {
        if let Some(name) = key.strip_prefix("env.") {
            public_env.insert(name.to_owned(), value.clone());
            origins.insert(format!("env.{name}"), FieldOrigin::Authored);
        }
    }
    // Where the workspace's installed dependencies are. The interpreter comes
    // from the provisioned toolchain and knows nothing about this workspace, so
    // it is told rather than expected to guess.
    let minor = resolved
        .rsplit_once('.')
        .map(|(head, _)| head)
        .unwrap_or(resolved.as_str());
    public_env.insert(
        "PYTHONPATH".to_owned(),
        format!(
            "{}/.venv/lib/python{minor}/site-packages",
            workspace_guest_root.trim_end_matches('/')
        ),
    );
    origins.insert("env.PYTHONPATH".to_owned(), FieldOrigin::PolicyDefault);

    let mut runtime = BTreeMap::new();
    runtime.insert("python".to_owned(), resolved);

    ProgramIntentV1 {
        schema: PROGRAM_INTENT_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        runtime,
        dependencies,
        launch_argv,
        cwd_relative: overrides.get("launch.cwd").unwrap_or("").to_owned(),
        public_env,
        exported_ports: vec![("http".to_owned(), port)],
        readiness_http_path: readiness,
        state_slots,
        static_output_root: None,
        static_entry_path: None,
        static_spa_fallback: false,
        // A Python process is never a built static site.
        static_build: None,
        // `workspace_guest_root` shapes the build plan, not the intent: the
        // intent says WHAT to run, the plan says where. It is still checked
        // here, because an argv that cannot resolve under it is not a launch.
    }
    .with_guest_root_checked(workspace_guest_root)
}

impl ProgramIntentV1 {
    /// Refuse an intent whose launch cannot work under the declared root.
    ///
    /// An absolute argv[0] that does not live under the workspace root will not
    /// exist inside the sandbox, and the failure arrives as "no such file"
    /// after a build has already run.
    fn with_guest_root_checked(self, workspace_guest_root: &str) -> Result<Self, IntentError> {
        if let Some(program) = self.launch_argv.first()
            && program.starts_with('/')
            && !program.starts_with(&format!("{}/", workspace_guest_root.trim_end_matches('/')))
            // A launch may name the PROVISIONED interpreter, which lives
            // outside the workspace on purpose: it is a runtime requirement
            // shared across every app on the host, not part of any one of them.
            && !program.starts_with(&format!("{TOOLCHAIN_ROOT}/"))
        {
            return Err(IntentError::Malformed {
                field: "launch.argv",
                detail: format!(
                    "{program:?} is an absolute path outside the workspace root \
                     {workspace_guest_root:?}, so it will not exist inside the sandbox"
                ),
            });
        }
        Ok(self)
    }
}

fn resolve_dependencies(
    python: &crate::detect::PythonEvidence,
    overrides: &AuthoredOverrides,
    origins: &mut FieldOrigins,
) -> Result<DependencyPlan, IntentError> {
    // Two lockfiles are two answers. Picking one silently means the build uses
    // versions the author did not choose.
    let mut found = Vec::new();
    if python.has_uv_lock {
        found.push("uv.lock");
    }
    if python.has_poetry_lock {
        found.push("poetry.lock");
    }
    if python.has_pipfile_lock {
        found.push("Pipfile.lock");
    }
    if found.len() > 1 && overrides.get("dependencies.lockfile").is_none() {
        return Err(IntentError::AmbiguousLockfiles {
            found: found.join(", "),
        });
    }

    if python.has_uv_lock && python.has_pyproject {
        origins.insert("dependencies".to_owned(), FieldOrigin::DetectedFromSource);
        return Ok(DependencyPlan::UvFrozen);
    }
    if python.has_requirements_txt {
        origins.insert("dependencies".to_owned(), FieldOrigin::DetectedFromSource);
        // Honest about strength: a requirements file pins what it pins. Calling
        // that reproducible would make a weaker guarantee look like the strong
        // one.
        return Ok(DependencyPlan::PipRequirements {
            reproducibility: "pinned-where-declared".to_owned(),
        });
    }
    if python.has_pyproject {
        // A pyproject with no lock resolves at build time, so two builds of the
        // same commit can differ. Refused rather than silently accepted.
        return Err(IntentError::RequiresAuthoring {
            field: "dependencies",
            why: "pyproject.toml without a lockfile resolves differently over time; add uv.lock \
                  or declare dependencies.lockfile",
        });
    }
    origins.insert("dependencies".to_owned(), FieldOrigin::DetectedFromSource);
    Ok(DependencyPlan::None)
}

/// Resolve a requested version — exact or a range — to a catalog version.
fn resolve_python(requested: &str) -> Result<String, IntentError> {
    let trimmed = requested.trim();
    if SUPPORTED_PYTHON.contains(&trimmed) {
        return Ok(trimmed.to_owned());
    }
    // `3.12` selects the newest supported 3.12.x. A range is resolved here or
    // not at all; nothing downstream ever sees a constraint.
    let bare = trimmed.trim_start_matches(['>', '=', '~', '^', ' ']);
    if let Some(matched) = SUPPORTED_PYTHON
        .iter()
        .find(|candidate| candidate.starts_with(&format!("{bare}.")) || **candidate == bare)
    {
        return Ok((*matched).to_owned());
    }
    Err(IntentError::UnsupportedPython {
        requested: requested.to_owned(),
    })
}

/// Split an authored argv on whitespace, honouring simple quoting.
fn split_argv(raw: &str) -> Result<Vec<String>, IntentError> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in raw.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => current.push(c),
            (None, c @ ('"' | '\'')) => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    argv.push(std::mem::take(&mut current));
                }
            }
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return Err(IntentError::Malformed {
            field: "launch.argv",
            detail: "unterminated quote".to_owned(),
        });
    }
    if !current.is_empty() {
        argv.push(current);
    }
    if argv.is_empty() {
        return Err(IntentError::Malformed {
            field: "launch.argv",
            detail: "empty".to_owned(),
        });
    }
    Ok(argv)
}

/// Lower a normalized intent into the steps a worker executes.
pub fn compile_build_plan(
    intent: &ProgramIntentV1,
    workspace_guest_root: &str,
    target_triple: &str,
) -> Result<EffectiveBuildPlanV1, IntentError> {
    let root = workspace_guest_root.trim_end_matches('/');
    // The interpreter is PROVISIONED, never taken from the host.
    //
    // Measured, not assumed: the acceptance host runs Python 3.14, for which
    // `pydantic-core` publishes no wheel, so a plan that said `python3` fell
    // back to building a Rust extension from source and failed on a missing
    // linker. Which interpreter a build uses cannot be a property of whichever
    // machine claimed the job.
    let python = intent.runtime.get("python").cloned();
    let interpreter = python
        .as_deref()
        .map(|version| format!("{}/bin/python3", python_home(version)));

    let mut steps = Vec::new();

    // The Static build toolchain, provisioned exactly like the Python one and
    // for exactly the same reason: the host's `node` is whatever the host
    // happens to have, and which Node built an artifact must be a property of
    // the source. `node` and `npm` both live under `{home}/bin`.
    if let (Some(build), Lane::StaticWeb) = (intent.static_build.as_ref(), intent.lane) {
        let home = node_home(&build.node_version);
        steps.push(BuildStepV1 {
            name: "provision-node".to_owned(),
            argv: vec![
                "/bin/sh".to_owned(),
                "-euc".to_owned(),
                format!(
                    "if [ ! -x {home}/bin/node ]; then mkdir -p {home} && \
                     curl -fsSL '{url}' | tar -xz --strip-components=1 -C {home}; fi; \
                     {home}/bin/node -v",
                    url = node_download_url(&build.node_version, node_triple(target_triple)),
                ),
            ],
            needs_network: true,
        });
    }

    if let (Some(version), Lane::PythonProcess) = (python.as_deref(), intent.lane) {
        let home = python_home(version);
        steps.push(BuildStepV1 {
            name: "provision-python".to_owned(),
            // Idempotent: an already-provisioned toolchain is reused, so a
            // second build on the same host does not re-download it.
            argv: vec![
                "/bin/sh".to_owned(),
                "-euc".to_owned(),
                format!(
                    "if [ ! -x {home}/bin/python3 ]; then mkdir -p {home} && \
                     curl -fsSL '{url}' | tar -xz --strip-components=1 -C {home}; fi; \
                     {home}/bin/python3 -V",
                    url = python_download_url(version, &python_triple(target_triple)),
                ),
            ],
            needs_network: true,
        });
    }

    match (&intent.lane, &intent.dependencies) {
        (Lane::StaticWeb, _) => {
            if let Some(build) = intent.static_build.as_ref() {
                let home = node_home(&build.node_version);
                let manager = build.package_manager;
                // Every command runs through the provisioned toolchain's own
                // bin directory. Prepending it to PATH rather than calling an
                // absolute path is deliberate: `npm` re-invokes `node`, and a
                // build whose PATH still found the host's would provision one
                // Node and build with another.
                let with_toolchain = |command: String| -> Vec<String> {
                    vec![
                        "/bin/sh".to_owned(),
                        "-euc".to_owned(),
                        format!("export PATH={home}/bin:$PATH; cd {root}; {command}"),
                    ]
                };

                // Install, in the most reproducible mode the lockfile allows.
                // `npm ci` REQUIRES a lock and deletes node_modules first; with
                // no lock it cannot run at all, so `npm install` is the honest
                // command and the intent already recorded that the graph is
                // unpinned.
                let install = match (manager, build.lockfile_pinned) {
                    (PackageManager::Npm, true) => "npm ci".to_owned(),
                    (PackageManager::Npm, false) => "npm install".to_owned(),
                    (PackageManager::Pnpm, _) => {
                        "corepack enable && pnpm install --frozen-lockfile".to_owned()
                    }
                    (PackageManager::Yarn, _) => {
                        "corepack enable && yarn install --frozen-lockfile".to_owned()
                    }
                    (PackageManager::Bun, _) => "bun install --frozen-lockfile".to_owned(),
                };
                steps.push(BuildStepV1 {
                    name: "install-node-dependencies".to_owned(),
                    argv: with_toolchain(install),
                    needs_network: true,
                });

                // Run the package's OWN build script. Reconstructing the
                // command from the script's text would make this the authority
                // on what the build is; running the script keeps the package
                // the authority.
                steps.push(BuildStepV1 {
                    name: "static-build".to_owned(),
                    argv: with_toolchain(format!(
                        "{} run {}",
                        manager.program(),
                        build.build_script
                    )),
                    // The build itself resolves nothing from the network. A
                    // build that reached out here would be fetching something
                    // the install step did not pin.
                    needs_network: false,
                });
            }
        }
        (Lane::PythonProcess, DependencyPlan::UvFrozen) => {
            steps.push(BuildStepV1 {
                name: "uv-sync".to_owned(),
                // `--frozen`: the lock is the answer. Without it `uv` may
                // update the lock and the build silently resolves something
                // other than what the author committed.
                argv: vec![
                    "uv".to_owned(),
                    "sync".to_owned(),
                    "--frozen".to_owned(),
                    "--project".to_owned(),
                    root.to_owned(),
                    "--python".to_owned(),
                    interpreter.clone().unwrap_or_else(|| "python3".to_owned()),
                ],
                needs_network: true,
            });
        }
        (Lane::PythonProcess, DependencyPlan::PipRequirements { .. }) => {
            // Dependencies only. The INTERPRETER is a runtime requirement,
            // not part of the app, and vendoring it was a mistake with a long
            // tail: it took the artifact past the control plane's 100 MB
            // request cap and past a Worker's memory, which then needed a
            // multipart upload and a streaming download to work around — all
            // of it caused by putting a shared, reusable runtime inside a
            // per-app artifact.
            //
            // `runtime_requirements` already says which interpreter this needs
            // and the Runner already provisions it at a fixed path, shared
            // across every app on the host. `--without-pip` because pip is a
            // build-time tool, and `python -m venv` only to get a
            // site-packages layout the launch can point at; nothing from the
            // venv's `bin/` survives into the artifact.
            steps.push(BuildStepV1 {
                name: "create-site-packages".to_owned(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-euc".to_owned(),
                    format!(
                        "{interp} -m venv --without-pip {root}/.venv && \
                         rm -rf {root}/.venv/bin {root}/.venv/lib64 {root}/.venv/pyvenv.cfg",
                        interp = interpreter.clone().unwrap_or_else(|| "python3".to_owned()),
                    ),
                ],
                needs_network: false,
            });
            let site_packages = python
                .as_deref()
                .map(|version| {
                    let minor = version
                        .rsplit_once('.')
                        .map(|(head, _)| head)
                        .unwrap_or(version);
                    format!("{root}/.venv/lib/python{minor}/site-packages")
                })
                .unwrap_or_else(|| format!("{root}/.venv/lib/site-packages"));
            steps.push(BuildStepV1 {
                name: "pip-install".to_owned(),
                // Installed WITH the provisioned interpreter and targeted at
                // the workspace's site-packages. `--no-compile` because
                // bytecode is derived, regenerates on first import, and would
                // otherwise double the artifact for nothing.
                argv: vec![
                    interpreter.clone().unwrap_or_else(|| "python3".to_owned()),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "install".to_owned(),
                    "--no-input".to_owned(),
                    "--no-compile".to_owned(),
                    "--target".to_owned(),
                    site_packages,
                    "-r".to_owned(),
                    format!("{root}/requirements.txt"),
                ],
                needs_network: true,
            });
        }
        (Lane::PythonProcess, DependencyPlan::None) => {}
    }

    Ok(EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: intent.lane,
        workspace_guest_root: root.to_owned(),
        runtime: intent.runtime.clone(),
        steps,
        output_root: intent.static_output_root.clone().unwrap_or_default(),
    })
}

/// The python-build-standalone triple for an Ato target triple.
///
/// Distinct vocabularies, mapped explicitly rather than by string surgery: an
/// unmapped target falls back to the common Linux build, and a wrong guess here
/// produces an interpreter that cannot run.
fn python_triple(target_triple: &str) -> String {
    match target_triple {
        "x86_64-linux-gnu" => "x86_64-unknown-linux-gnu".to_owned(),
        "aarch64-linux-gnu" => "aarch64-unknown-linux-gnu".to_owned(),
        other => other.to_owned(),
    }
}
