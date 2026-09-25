//! Canonical D → physical bindings. No intent compiler or string projection.
//!
//! The plan does not repeat semantic argv/cwd/env/ports/effects. Authored
//! actions and the serving step are indexes into the immutable D; only
//! platform prerequisites and local toolchain/environment bindings are owned.
use crate::authoring::{
    BROWSER_PROTOCOL, BoundDerivation, BoundStep, PROCESS_PROTOCOL, StateAccess,
};
use crate::detect::{DetectorEvidence, FieldOrigins, NodeEvidence, PythonEvidence};
use crate::intent::{
    self, AuthoredOverrides, BuildStepV1, DependencyPlan, IntentError, Lane, Provisioning,
    ResolvedPackageManager, StaticBuildProfileV1, StaticCompileProfileV1,
};
use crate::preset::{
    SINGLE_JSX_COMPILER, SINGLE_JSX_NODE_VERSION, SINGLE_JSX_OUTPUT_ROOT, SINGLE_JSX_REACT_VERSION,
};
use crate::projection::{ProjectionError, project_exec};
use std::{borrow::Cow, collections::BTreeMap};

/// Input facts captured before semantic binding, never a new D or its identity.
/// Failed optional readings are consulted only by the relevant workspace action.
pub struct InputFacts {
    python: Option<PythonEvidence>,
    node: Option<NodeEvidence>,
    python_modules: bool,
    static_build: Result<Option<StaticBuildProfileV1>, IntentError>,
    fixed_static_build: Result<StaticBuildProfileV1, IntentError>,
    jsx_entry: Result<String, IntentError>,
}
impl InputFacts {
    pub fn capture(evidence: &DetectorEvidence) -> Self {
        Self {
            python: evidence.python.clone(),
            node: evidence.node.clone(),
            python_modules: evidence.present_files.iter().any(|p| p.ends_with(".py")),
            static_build: intent::detect_static_build(evidence),
            fixed_static_build: intent::fixed_node_static_build(evidence),
            jsx_entry: intent::single_jsx_entry(evidence),
        }
    }
}

pub struct RuntimeBinding<'a> {
    pub workspace_guest_root: &'a str,
    pub target_triple: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildAction {
    /// Platform-owned command, never permission granted by source content.
    Prerequisite(BuildStepV1),
    /// The canonical D supplies argv/cwd/env/network at execution time.
    Authored { step: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub lane: Lane,
    pub serving_step: usize,
    pub workspace_guest_root: String,
    pub toolchains: BTreeMap<String, String>,
    pub package_manager: Option<ResolvedPackageManager>,
    pub actions: Vec<BuildAction>,
    pub toolchain_path: Vec<String>,
    /// Physical defaults only; never a copy of the D's authored environment.
    pub environment_bindings: BTreeMap<String, String>,
}
impl ExecutionPlan {
    pub fn serving<'a>(&self, derivation: &'a BoundDerivation) -> &'a BoundStep {
        &derivation.steps[self.serving_step]
    }
    pub fn process_environment(&self, derivation: &BoundDerivation) -> BTreeMap<String, String> {
        let mut env = self.environment_bindings.clone();
        env.extend(self.serving(derivation).env.clone());
        // The existing Python workspace convention is a physical binding.
        if self.lane == Lane::PythonProcess
            && let Some(path) = self.environment_bindings.get("PYTHONPATH")
        {
            env.insert("PYTHONPATH".to_owned(), path.clone());
        }
        env
    }
    pub fn steps<'a>(
        &'a self,
        derivation: &'a BoundDerivation,
    ) -> Result<Vec<Cow<'a, BuildStepV1>>, ProjectionError> {
        self.actions
            .iter()
            .map(|action| match action {
                BuildAction::Prerequisite(step) => Ok(Cow::Borrowed(step)),
                BuildAction::Authored { step } => {
                    project_exec(&derivation.steps[*step]).map(Cow::Owned)
                }
            })
            .collect()
    }
    pub fn needs_network(&self, derivation: &BoundDerivation) -> bool {
        self.actions.iter().any(|action| match action {
            BuildAction::Prerequisite(step) => step.needs_network,
            BuildAction::Authored { step } => !derivation.steps[*step].network.is_denied(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoweringError {
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error(transparent)]
    Binding(#[from] IntentError),
}
impl LoweringError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Projection(e) => e.code(),
            Self::Binding(e) => e.code(),
        }
    }
}

pub fn lower_execution(
    d: &BoundDerivation,
    facts: InputFacts,
    binding: RuntimeBinding<'_>,
) -> Result<ExecutionPlan, LoweringError> {
    let refuse = |detail: String| ProjectionError::Unprojectable { detail };
    let serves: Vec<_> = d
        .steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.op == "serve")
        .collect();
    let [(serving_step, serve)] = serves.as_slice() else {
        return Err(ProjectionError::ServingSteps {
            found: serves.len(),
        }
        .into());
    };
    let serving_step = *serving_step;
    if serving_step + 1 != d.steps.len() {
        return Err(ProjectionError::StepOrder {
            step: d.steps[serving_step + 1].id.clone(),
        }
        .into());
    }
    for step in &d.steps[..serving_step] {
        if step.op != "exec" || step.protocol != PROCESS_PROTOCOL {
            return Err(ProjectionError::UnsupportedStep {
                protocol: step.protocol.clone(),
                op: step.op.clone(),
                detail: "only process exec preparation is supported",
            }
            .into());
        }
        project_exec(step)?;
    }
    if !serve.network.is_denied() {
        return Err(refuse("serving steps cannot declare egress".into()).into());
    }
    for state in &d.state {
        if state.access != StateAccess::ReadWrite {
            return Err(refuse(
                "only writable state is supported by this Formation binding".into(),
            )
            .into());
        }
    }
    let python_only = d.runtimes.keys().all(|k| k == "python");
    let lane = match serve.protocol.as_str() {
        BROWSER_PROTOCOL => Lane::StaticWeb,
        PROCESS_PROTOCOL if python_only => Lane::PythonProcess,
        PROCESS_PROTOCOL => Lane::Process,
        _ => {
            return Err(ProjectionError::UnsupportedStep {
                protocol: serve.protocol.clone(),
                op: serve.op.clone(),
                detail: "no supported serving protocol",
            }
            .into());
        }
    };
    if lane.is_process() {
        project_exec(serve)?;
        if let Some(program) = serve.argv.first()
            && program.starts_with('/')
            && !program.starts_with(&format!(
                "{}/",
                binding.workspace_guest_root.trim_end_matches('/')
            ))
            && !program.starts_with(&format!("{}/", intent::TOOLCHAIN_ROOT))
        {
            return Err(IntentError::Malformed {
                field: "launch.argv",
                detail: format!("{program:?} is outside the workspace and provisioned toolchains"),
            }
            .into());
        }
        if !d
            .ports
            .iter()
            .any(|p| p.from == serve.id && p.guest_port.is_some())
        {
            return Err(refuse("serving process requires a declared guest port".into()).into());
        }
    }
    let mut dependencies = if serving_step > 0 {
        DependencyPlan::Authored
    } else {
        DependencyPlan::None
    };
    let mut build = None;
    let mut compiler = None;
    let (mut toolchains, package_manager) = if lane == Lane::PythonProcess {
        let python = facts.python.unwrap_or_default();
        if !facts.python_modules && python == PythonEvidence::default() {
            return Err(IntentError::NoLane {
                detail: "no Python source or dependency metadata".into(),
            }
            .into());
        }
        let requested = d
            .runtimes
            .get("python")
            .or(python.python_version_file.as_ref())
            .or(python.requires_python.as_ref())
            .map(String::as_str)
            .unwrap_or(intent::DEFAULT_PYTHON);
        let resolved = intent::resolve_python(requested)?;
        if serving_step == 0 {
            dependencies = intent::resolve_dependencies(
                &python,
                &AuthoredOverrides::default(),
                &mut FieldOrigins::new(),
            )?;
        }
        (BTreeMap::from([("python".to_owned(), resolved)]), None)
    } else if lane == Lane::Process || (serving_step > 0 && !d.runtimes.is_empty()) {
        intent::resolve_toolchains(facts.node.as_ref(), &d.runtimes)?
    } else {
        (BTreeMap::new(), None)
    };
    if lane == Lane::StaticWeb {
        if serving_step > 0 && (d.workspace_build.is_some() || d.workspace_compiler.is_some()) {
            return Err(refuse(
                "authored exec and inferred workspace build are mutually exclusive".into(),
            )
            .into());
        }
        if serving_step == 0 {
            if let Some(name) = &d.workspace_compiler {
                if name != SINGLE_JSX_COMPILER {
                    return Err(refuse(format!("unsupported compiler {name}")).into());
                }
                compiler = Some(StaticCompileProfileV1 {
                    compiler: name.clone(),
                    entry_source: facts.jsx_entry?,
                    node_version: SINGLE_JSX_NODE_VERSION.into(),
                    react_version: SINGLE_JSX_REACT_VERSION.into(),
                    output_root: SINGLE_JSX_OUTPUT_ROOT.into(),
                });
            } else if d.workspace_build.is_some() {
                build = Some(match facts.static_build? {
                    Some(build) => build,
                    None => facts.fixed_static_build?,
                });
            }
            if !python_only {
                return Err(refuse(
                    "static runtime declarations require authored exec steps".into(),
                )
                .into());
            }
        }
        if let Some(build) = &build {
            toolchains.insert("node".into(), build.node_version.clone());
        }
        if let Some(compiler) = &compiler {
            toolchains.insert("node".into(), compiler.node_version.clone());
            toolchains.insert("react".into(), compiler.react_version.clone());
        }
    }
    let (steps, toolchain_path) = intent::provision_steps(
        &Provisioning {
            lane,
            runtime: &toolchains,
            dependencies: &dependencies,
            static_build: build.as_ref(),
            static_compile: compiler.as_ref(),
            package_manager: package_manager.as_ref(),
        },
        binding.workspace_guest_root,
        binding.target_triple,
    )?;
    let mut actions: Vec<_> = steps.into_iter().map(BuildAction::Prerequisite).collect();
    actions.extend((0..serving_step).map(|step| BuildAction::Authored { step }));
    let mut environment_bindings = BTreeMap::new();
    if lane == Lane::PythonProcess {
        let version = &toolchains["python"];
        let minor = version.rsplit_once('.').map(|(m, _)| m).unwrap_or(version);
        environment_bindings.insert(
            "PYTHONPATH".into(),
            format!(
                "{}/.venv/lib/python{minor}/site-packages",
                binding.workspace_guest_root.trim_end_matches('/')
            ),
        );
    } else if lane == Lane::Process {
        environment_bindings.insert(
            "PATH".into(),
            intent::toolchain_bin_dirs(&toolchains, package_manager.as_ref())
                .into_iter()
                .chain([intent::SYSTEM_PATH.into()])
                .collect::<Vec<_>>()
                .join(":"),
        );
        environment_bindings.insert("HOME".into(), "/tmp".into());
        if toolchains.contains_key("node") {
            environment_bindings.insert("npm_config_cache".into(), "/tmp/.npm".into());
            environment_bindings.insert("npm_config_update_notifier".into(), "false".into());
        }
    }
    Ok(ExecutionPlan {
        lane,
        serving_step,
        workspace_guest_root: binding.workspace_guest_root.trim_end_matches('/').into(),
        toolchains,
        package_manager,
        actions,
        toolchain_path,
        environment_bindings,
    })
}
