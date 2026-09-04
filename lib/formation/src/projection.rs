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
//! ## What is not projected yet
//!
//! A preparation step (`ato.process@1` / `exec`). The grammar accepts one
//! because it is a real thing to write and the model has room for it; this
//! projection refuses it by name rather than dropping it, because a route step
//! that is silently not executed is a build that did something other than what
//! it was told.

use std::collections::BTreeMap;

use crate::authoring::{
    BROWSER_PROTOCOL, BoundContract, BoundDerivation, HTTP_CONTRACT_VERIFIER, PROCESS_PROTOCOL,
    StateAccess,
};
use crate::intent::AuthoredOverrides;

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
    #[error("{detail}")]
    Unprojectable { detail: String },
}

impl ProjectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedStep { .. } => "projection_unsupported_step",
            Self::ServingSteps { .. } => "projection_serving_steps",
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
        if step.op != "serve" {
            return Err(ProjectionError::UnsupportedStep {
                protocol: step.protocol.clone(),
                op: step.op.clone(),
                detail: "a preparation step is not projected onto this worker yet, and a step that \
                     is silently skipped is a route that did something else",
            });
        }
    }
    let [serve] = serving.as_slice() else {
        return Err(ProjectionError::ServingSteps {
            found: serving.len(),
        });
    };

    // The Contract's HTTP observation, if it has one. Its path becomes the
    // readiness probe, so the gate and the identity cannot disagree.
    let http_observation = contract
        .requirements
        .iter()
        .find(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER);

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
            overrides.insert(
                "static.build".to_owned(),
                if derivation.workspace_build.is_some() {
                    "required".to_owned()
                } else {
                    "none".to_owned()
                },
            );
            None
        }
        PROCESS_PROTOCOL => {
            overrides.insert("lane".to_owned(), "python_process".to_owned());
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

    for (name, version) in &derivation.runtimes {
        if name != "python" {
            return Err(ProjectionError::Unprojectable {
                detail: format!(
                    "this build provisions only `python`; {name:?} was declared and would be \
                     silently absent at run time"
                ),
            });
        }
        overrides.insert("runtime.python".to_owned(), version.clone());
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
    })
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
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-m", "uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8000"]

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
            Some(
                "/opt/ato/toolchains/python/3.12.7/bin/python3 -m uvicorn main:app --host 0.0.0.0 --port 8000"
            )
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
    fn a_preparation_step_is_refused_by_name_rather_than_skipped() {
        let error = project_text(&FIXTURE.replace(
            "[[derive.step]]\nid = \"app\"",
            "[[derive.step]]\nid = \"deps\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"uv\", \"sync\"]\n\n[[derive.step]]\nid = \"app\"",
        ))
        .unwrap_err();
        assert_eq!(error.code(), "projection_unsupported_step");
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
