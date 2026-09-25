//! Where the Capsule model meets the machinery this worker already has.
//!
//! ## What this is, and what it is not
//!
//! `ProgramIntent` and `EffectiveBuildPlan` are no longer authoring objects and
//! are never inputs to Capsule identity. They are an **execution plan**: the
//! projection of a Bound Derivation onto the compiler, sandbox and materializers
//! that exist today. A Capsule is identified by its Contract; a Derivation is
//! identified by its own canonical form; this file is how one particular
//! Derivation gets run on one particular worker, and it is replaceable without
//! touching either digest.
//!
//! Read the direction carefully. Nothing flows back: no field of a
//! `ProgramIntent` reaches a `ContractRef` or a `DerivationRef`.
//!
//! ## Readiness is derived from the Contract
//!
//! The run-time readiness gate is built HERE, from the Contract's own HTTP
//! observation, rather than declared beside it. That is deliberate: it is what
//! makes the promise in `verify::ObservationOutcome::Deferred` true by
//! construction. Two independent declarations of "the path that proves this is
//! up" drift, and when they drift the Capsule's identity rests on a probe of
//! some other path.
//!
//! ## Preparation steps
//!
//! A route this worker executes is `exec* serve`: zero or more
//! `ato.process@1` `exec` steps, in authored order, then exactly one serving
//! step. The execs are the build — each runs to completion before the next —
//! and the serve is the realization. An `exec` after the serve is refused:
//! the model here is build → materialize → realize, and a step after the
//! realization has started has no place in it that means what it said.
//!
//! Each exec becomes one build step carrying its argv as the array the author
//! wrote (never joined into a command line and split again), its cwd, its env
//! and its declared network. The steps land AFTER the platform's own
//! prerequisites (a provisioned interpreter) and REPLACE the application
//! build the platform would otherwise infer from the source: an author who
//! wrote the build is the authority on it, and a second, detected install or
//! `npm run build` beside it is a build that did something nobody wrote.

use std::collections::BTreeMap;

use crate::authoring::{
    BROWSER_PROTOCOL, BoundContract, BoundDerivation, BoundStep, HTTP_CONTRACT_VERIFIER,
    PROCESS_PROTOCOL, StateAccess, StepNetwork,
};
use crate::intent::{AuthoredOverrides, BuildStepV1};

/// The runtimes a Derivation may declare for this worker to provision.
pub const PROVISIONED_RUNTIMES: &[&str] = &["python", "node", "pnpm", "yarn"];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectionError {
    #[error("this build cannot execute {protocol} `{op}` yet: {detail}")]
    UnsupportedStep {
        protocol: String,
        op: String,
        detail: &'static str,
    },
    #[error("a route this build can execute has exactly one serving step; this one has {found}")]
    ServingSteps { found: usize },
    #[error(
        "step {step:?} is an `exec` after the serving step; a route this build can execute \
         prepares first and serves last"
    )]
    StepOrder { step: String },
    #[error("{detail}")]
    Unprojectable { detail: String },
}

impl ProjectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedStep { .. } => "projection_unsupported_step",
            Self::ServingSteps { .. } => "projection_serving_steps",
            Self::StepOrder { .. } => "projection_step_order",
            Self::Unprojectable { .. } => "projection_unprojectable",
        }
    }
}

/// The run-time gate a projected Derivation will be admitted by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessProjection {
    pub port_id: String,
    pub path: String,
}

/// A Derivation, expressed in the vocabulary the intent compiler reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationProjection {
    pub overrides: AuthoredOverrides,
    /// Present when the route serves a process. `None` for a browser surface,
    /// which is admitted by being served rather than by being probed.
    pub readiness: Option<ReadinessProjection>,
    /// The route's `exec` steps, in authored order, as build steps. They run
    /// after the platform's prerequisites and before materialization.
    pub build_steps: Vec<BuildStepV1>,
}

/// Project a bound Derivation, with its Contract, onto today's execution IR.
pub fn project(
    derivation: &BoundDerivation,
    contract: &BoundContract,
) -> Result<DerivationProjection, ProjectionError> {
    let mut overrides: BTreeMap<String, String> = BTreeMap::new();

    let serving: Vec<_> = derivation
        .steps
        .iter()
        .filter(|step| step.op == "serve")
        .collect();
    for step in &derivation.steps {
        let executable = match step.op.as_str() {
            "serve" => true,
            "exec" => step.protocol == PROCESS_PROTOCOL,
            _ => false,
        };
        if !executable {
            return Err(ProjectionError::UnsupportedStep {
                protocol: step.protocol.clone(),
                op: step.op.clone(),
                detail: "this worker executes `ato.process@1` `exec` steps and one serving step, \
                     and a step that is silently skipped is a route that did something else",
            });
        }
    }
    let [serve] = serving.as_slice() else {
        return Err(ProjectionError::ServingSteps {
            found: serving.len(),
        });
    };
    let serve_at = derivation
        .steps
        .iter()
        .position(|step| step.op == "serve")
        .expect("exactly one serving step");
    if let Some(late) = derivation.steps[serve_at + 1..].first() {
        return Err(ProjectionError::StepOrder {
            step: late.id.clone(),
        });
    }
    if !serve.network.is_denied() {
        return Err(ProjectionError::Unprojectable {
            detail: format!(
                "serving step {:?} declares a network; only an `exec` step does",
                serve.id
            ),
        });
    }
    let build_steps = derivation.steps[..serve_at]
        .iter()
        .map(project_exec)
        .collect::<Result<Vec<_>, _>>()?;

    // The Contract's HTTP observation, if it has one. Its path becomes the
    // readiness probe, so the gate and the identity cannot disagree.
    let http_observation = contract
        .requirements
        .iter()
        .find(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER);

    for name in derivation.runtimes.keys() {
        if !PROVISIONED_RUNTIMES.contains(&name.as_str()) {
            return Err(ProjectionError::Unprojectable {
                detail: format!(
                    "this build provisions {}; {name:?} was declared and would be silently \
                     absent at run time",
                    PROVISIONED_RUNTIMES.join(", ")
                ),
            });
        }
    }
    let python_only = derivation.runtimes.keys().all(|name| name == "python");

    let readiness = match serve.protocol.as_str() {
        BROWSER_PROTOCOL => {
            overrides.insert("lane".to_owned(), "static_web".to_owned());
            overrides.insert(
                "static.output_root".to_owned(),
                serve.root.clone().unwrap_or_default(),
            );
            overrides.insert(
                "static.entry_path".to_owned(),
                serve
                    .entry
                    .clone()
                    .unwrap_or_else(|| "index.html".to_owned()),
            );
            overrides.insert(
                "static.spa_fallback".to_owned(),
                serve.spa_fallback.unwrap_or(false).to_string(),
            );
            // A platform compiler and an author's own build are alternatives,
            // never both: one says Ato owns the toolchain, the other says the
            // package does. Emitting `static.build = required` alongside a
            // compiler would send a one-file upload down `npm ci`.
            //
            // Authored `exec` steps are a third authority, and the most
            // specific one: the author wrote the build. Nothing is inferred
            // beside it, and a route that also names another build has not
            // said which one runs.
            if !build_steps.is_empty() {
                if derivation.workspace_compiler.is_some() || derivation.workspace_build.is_some() {
                    return Err(ProjectionError::Unprojectable {
                        detail: "this route has authored `exec` steps and also names a platform \
                                 or package build; one of them is the build"
                            .to_owned(),
                    });
                }
                overrides.insert("static.build".to_owned(), "none".to_owned());
            } else if let Some(compiler) = derivation.workspace_compiler.as_ref() {
                overrides.insert("static.compile".to_owned(), compiler.clone());
            } else {
                overrides.insert(
                    "static.build".to_owned(),
                    if derivation.workspace_build.is_some() {
                        "required".to_owned()
                    } else {
                        "none".to_owned()
                    },
                );
            }
            None
        }
        PROCESS_PROTOCOL => {
            // A route that declares no runtime but Python is the v1 Python
            // lane, unchanged: same intent, same plan, same formation key.
            // Anything else is a generic process whose runtimes are exactly
            // the ones declared — never ones read off the argv.
            overrides.insert(
                "lane".to_owned(),
                if python_only {
                    "python_process"
                } else {
                    "process"
                }
                .to_owned(),
            );
            // The author's execs prepare the workspace; no dependency install
            // is detected and added beside them.
            if !build_steps.is_empty() {
                overrides.insert("dependencies".to_owned(), "authored".to_owned());
            }
            // Re-quoted for the existing splitter. An element carrying a quote
            // would not survive the round trip, so it is refused rather than
            // mangled into a different argv than the author wrote.
            let mut argv = Vec::with_capacity(serve.argv.len());
            for word in &serve.argv {
                if word.contains('"') || word.contains('\'') {
                    return Err(ProjectionError::Unprojectable {
                        detail: format!(
                            "argv element {word:?} contains a quote, which this build's \
                             execution plan cannot carry without changing it"
                        ),
                    });
                }
                argv.push(if word.contains(char::is_whitespace) {
                    format!("\"{word}\"")
                } else {
                    word.clone()
                });
            }
            overrides.insert("launch.argv".to_owned(), argv.join(" "));
            if !serve.cwd.is_empty() {
                overrides.insert("launch.cwd".to_owned(), serve.cwd.clone());
            }
            for (name, value) in &serve.env {
                overrides.insert(format!("env.{name}"), value.clone());
            }

            let port = derivation
                .ports
                .iter()
                .find(|port| port.from == serve.id)
                .ok_or_else(|| ProjectionError::Unprojectable {
                    detail: format!("no port is exported from step {:?}", serve.id),
                })?;
            let guest_port = port
                .guest_port
                .ok_or_else(|| ProjectionError::Unprojectable {
                    detail: format!(
                        "port {:?} declares no guest_port; the port a workload listens on is \
                     declared, never read from a framework",
                        port.id
                    ),
                })?;
            overrides.insert("port.http".to_owned(), guest_port.to_string());

            let path = http_observation
                .and_then(|requirement| requirement.path.clone())
                .unwrap_or_else(|| "/".to_owned());
            overrides.insert("readiness.http_path".to_owned(), path.clone());
            Some(ReadinessProjection {
                port_id: port.id.clone(),
                path,
            })
        }
        other => {
            return Err(ProjectionError::UnsupportedStep {
                protocol: other.to_owned(),
                op: serve.op.clone(),
                detail: "no serving lane on this worker evaluates that protocol",
            });
        }
    };

    // Where the declared runtimes go. The v1 keys for the v1 lanes; the
    // declared-toolchain keys for a generic process, and for a static route
    // whose authored build runs on them.
    let declared_toolchains = match serve.protocol.as_str() {
        PROCESS_PROTOCOL => !python_only,
        _ => !build_steps.is_empty() && !derivation.runtimes.is_empty(),
    };
    if !declared_toolchains && !python_only {
        return Err(ProjectionError::Unprojectable {
            detail: "a runtime on a static route is a build toolchain: declare the build as \
                     `exec` steps that use it"
                .to_owned(),
        });
    }
    for (name, version) in &derivation.runtimes {
        let key = if declared_toolchains {
            format!("toolchain.{name}")
        } else {
            format!("runtime.{name}")
        };
        overrides.insert(key, version.clone());
    }

    for slot in &derivation.state {
        if slot.access != StateAccess::ReadWrite {
            return Err(ProjectionError::Unprojectable {
                detail: format!(
                    "state slot {:?} is read-only, and this build carries writable state only",
                    slot.id
                ),
            });
        }
        overrides.insert(format!("state.{}.mount", slot.id), slot.mount.clone());
    }

    Ok(DerivationProjection {
        overrides: AuthoredOverrides(overrides),
        readiness,
        build_steps,
    })
}

/// One authored `exec`, as the build step that runs it.
///
/// Everything is carried as written. What is refused here is what could not
/// be run as written: an argv or env a process cannot receive, and a cwd that
/// names somewhere other than inside the workspace. The cwd is checked again,
/// against the real directory, immediately before the step runs — an earlier
/// step can create it, or a link in its place.
pub fn project_exec(step: &BoundStep) -> Result<BuildStepV1, ProjectionError> {
    let refuse = |detail: String| ProjectionError::Unprojectable {
        detail: format!("exec step {:?}: {detail}", step.id),
    };
    if step.argv.is_empty() {
        return Err(refuse("declares no argv".to_owned()));
    }
    if step.argv.iter().any(|word| word.contains('\0')) {
        return Err(refuse("an argv element contains NUL".to_owned()));
    }
    for (name, value) in &step.env {
        let valid_name = name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid_name {
            return Err(refuse(format!("env name {name:?} is not a variable name")));
        }
        if value.contains('\0') {
            return Err(refuse(format!("env {name} contains NUL")));
        }
    }
    Ok(BuildStepV1 {
        name: step.id.clone(),
        argv: step.argv.clone(),
        needs_network: step.network == StepNetwork::DependencyResolution,
        cwd_relative: workspace_relative_cwd(&step.cwd).map_err(refuse)?,
        env: step.env.clone(),
        toolchain_access: crate::intent::ToolchainAccess::ReadOnly,
    })
}

/// `""` / `"."` is the workspace root; otherwise plain names only. Absolute
/// paths and `..` anywhere are refused, even when they would land inside.
pub fn workspace_relative_cwd(cwd: &str) -> Result<String, String> {
    if cwd.contains('\0') || cwd.contains('\\') {
        return Err(format!("cwd {cwd:?} is not a workspace path"));
    }
    if cwd.starts_with('/') {
        return Err(format!(
            "cwd {cwd:?} is absolute; a step runs inside the workspace"
        ));
    }
    let mut parts = Vec::new();
    for part in cwd.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "cwd {cwd:?} climbs with `..`; a step runs inside the workspace"
                ));
            }
            name => parts.push(name),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::{AuthoringProvenance, BindingContext, bind};
    use crate::capsule_toml::parse_capsule_toml;
    use crate::preset::{AppPreset, synthesize_authoring};

    fn project_text(text: &str) -> Result<DerivationProjection, ProjectionError> {
        let draft = parse_capsule_toml(text).expect("parses");
        let (contract, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: "sha256:aa",
            },
        )
        .expect("binds");
        project(&derivation, &contract)
    }

    const FIXTURE: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"

[[runtime]]
name = "python"
version = "3.12.7"

[[state]]
id = "app_data"
use = "ato.state.filesystem@1"
mount = "/data"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "main.py"]

[derive.step.env]
APP_DB_PATH = "/data/app.sqlite"

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "app-responds"
use = "ato.contract.http@1"
port = "app.http"
path = "/health"

[contract.require.expect]
status = 200
"#;

    #[test]
    fn the_readiness_gate_is_built_from_the_contract_and_not_beside_it() {
        // The promise `verify` makes when it defers an observation: whatever
        // path the Contract observes IS the path the gate probes. Declaring
        // them separately is how a Capsule's identity comes to rest on a probe
        // of some other path.
        let projected = project_text(FIXTURE).expect("projects");
        assert_eq!(
            projected.readiness,
            Some(ReadinessProjection {
                port_id: "app.http".to_owned(),
                path: "/health".to_owned(),
            })
        );
        assert_eq!(
            projected.overrides.get("readiness.http_path"),
            Some("/health")
        );
    }

    #[test]
    fn a_process_route_projects_onto_the_existing_override_vocabulary() {
        let projected = project_text(FIXTURE).expect("projects");
        let o = &projected.overrides;
        assert_eq!(o.get("lane"), Some("python_process"));
        assert_eq!(o.get("runtime.python"), Some("3.12.7"));
        assert_eq!(
            o.get("launch.argv"),
            Some("/opt/ato/toolchains/python/3.12.7/bin/python3 -B main.py")
        );
        assert_eq!(o.get("port.http"), Some("8000"));
        assert_eq!(o.get("state.app_data.mount"), Some("/data"));
        assert_eq!(o.get("env.APP_DB_PATH"), Some("/data/app.sqlite"));
    }

    #[test]
    fn the_static_preset_projects_onto_the_same_vocabulary() {
        let draft = synthesize_authoring(AppPreset::SingleHtml);
        assert!(matches!(
            draft.provenance,
            AuthoringProvenance::PresetSynthesized { .. }
        ));
        let (contract, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: "sha256:aa",
            },
        )
        .expect("binds");
        let projected = project(&derivation, &contract).expect("projects");
        let o = &projected.overrides;
        assert_eq!(o.get("lane"), Some("static_web"));
        assert_eq!(o.get("static.entry_path"), Some("index.html"));
        assert_eq!(o.get("static.build"), Some("none"));
        assert_eq!(o.get("static.spa_fallback"), Some("false"));
        // A served surface is admitted by being served, not by being probed.
        assert_eq!(projected.readiness, None);
    }

    #[test]
    fn the_node_static_preset_still_asks_for_its_build() {
        let draft = synthesize_authoring(AppPreset::NodeStatic);
        let (contract, derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: "sha256:aa",
            },
        )
        .expect("binds");
        let projected = project(&derivation, &contract).expect("projects");
        assert_eq!(projected.overrides.get("static.build"), Some("required"));
        assert_eq!(projected.overrides.get("static.output_root"), Some("dist"));
    }

    #[test]
    fn a_preparation_step_before_the_serve_becomes_a_build_step() {
        let projected = project_text(&FIXTURE.replace(
            "[[derive.step]]\nid = \"app\"",
            "[[derive.step]]\nid = \"deps\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"uv\", \"sync\"]\n\n[[derive.step]]\nid = \"app\"",
        ))
        .expect("projects");
        assert_eq!(projected.build_steps.len(), 1);
        assert_eq!(projected.build_steps[0].name, "deps");
        assert_eq!(projected.build_steps[0].argv, ["uv", "sync"]);
        assert_eq!(projected.overrides.get("dependencies"), Some("authored"));
    }

    #[test]
    fn a_preparation_step_after_the_serve_is_refused_by_name() {
        let error = project_text(&FIXTURE.replace(
            "[[port]]",
            "[[derive.step]]\nid = \"late\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"true\"]\n\n[[port]]",
        ))
        .unwrap_err();
        assert_eq!(error.code(), "projection_step_order");
    }

    #[test]
    fn a_port_with_no_declared_guest_port_is_refused() {
        let error = project_text(&FIXTURE.replace("guest_port = 8000", "")).unwrap_err();
        assert!(
            format!("{error}").contains("never read from a framework"),
            "{error}"
        );
    }

    #[test]
    fn a_runtime_this_build_cannot_provision_is_refused_rather_than_dropped() {
        let error =
            project_text(&FIXTURE.replace("name = \"python\"", "name = \"ruby\"")).unwrap_err();
        assert!(format!("{error}").contains("silently absent"), "{error}");
    }
}
