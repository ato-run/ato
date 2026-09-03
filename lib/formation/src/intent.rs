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

use crate::detect::{DetectorEvidence, FieldOrigin, FieldOrigins};

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
#[derive(Debug, Clone, Default)]
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

fn compile_static(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    // A `dist/` that happens to exist does NOT select the Static lane's output.
    // A repository can carry a checked-in build directory, a vendored example
    // or a stale artifact, and publishing one because it looked like an output
    // is how the wrong bytes get served.
    let output_root = match overrides.get("static.output_root") {
        Some(declared) => {
            origins.insert("static.output_root".to_owned(), FieldOrigin::Authored);
            declared.trim_matches('/').to_owned()
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

    Ok(ProgramIntentV1 {
        schema: PROGRAM_INTENT_V1_SCHEMA.to_owned(),
        lane: Lane::StaticWeb,
        runtime: BTreeMap::new(),
        dependencies: DependencyPlan::None,
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
    })
}

fn compile_python(
    evidence: &DetectorEvidence,
    overrides: &AuthoredOverrides,
    workspace_guest_root: &str,
    origins: &mut FieldOrigins,
) -> Result<ProgramIntentV1, IntentError> {
    let python = evidence
        .python
        .as_ref()
        .ok_or_else(|| IntentError::NoLane {
            detail: "the Python lane was selected but the source carries no Python marker"
                .to_owned(),
        })?;

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
        (Lane::StaticWeb, _) => {}
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
