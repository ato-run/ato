//! The registered process adapter under a search-scoped grant. This reuses
//! the common builder, process launcher and Verifier, never a mock executor.
use crate::{
    build::BuildAttempt,
    build_sandbox::NetworkPolicy,
    executor::{AttemptExecution, LocalAttemptExecutor},
    formation_realizer::CandidateLauncher,
    plan::PlannedCandidate,
    realize::{CandidateRealizer, RealizeFailure, Realized},
};
use ato_formation::{
    request::{AttemptFailure, RuntimeProfile},
    requirements::{ExecutionPhase, ExecutionRequirements, ResourceOperation},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub struct ExplorationRealizer<'a> {
    pub planned: &'a PlannedCandidate,
    pub source_root: &'a Path,
    pub builder: &'a LocalAttemptExecutor,
    pub shim: &'a Path,
    pub ceiling: &'a ExecutionRequirements,
    pub gates: &'a BTreeMap<ExecutionPhase, PathBuf>,
    pub network_refusal: Option<&'a crate::build::NetworkRefusalObserver<'a>>,
}

fn refuse(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.into(),
        stage: "admission".into(),
        message: message.into(),
    })
}

/// Logical requirements the registered adapter actually consumes. No host
/// working-copy path, credential or pre-existing production revision enters D.
pub fn required_isolated_authority(
    d: &ato_formation::authoring::BoundDerivation,
) -> Vec<ato_formation::requirements::AuthorityRequirement> {
    use ato_formation::{authoring::StateAccess, requirements::AuthorityRequirement};
    let mut required: Vec<_> = d
        .ports
        .iter()
        .map(|p| AuthorityRequirement {
            phase: ExecutionPhase::Runtime,
            protocol: p.protocol.clone(),
            resource: p.id.clone(),
            operation: ResourceOperation::Bind,
        })
        .collect();
    for slot in &d.state {
        let mut operations = vec![ResourceOperation::Read];
        if slot.access == StateAccess::ReadWrite {
            operations.extend([ResourceOperation::Write, ResourceOperation::Create]);
        }
        for operation in operations {
            required.push(AuthorityRequirement {
                phase: ExecutionPhase::Runtime,
                protocol: slot.protocol.clone(),
                resource: slot.id.clone(),
                operation,
            });
        }
    }
    required.sort();
    required.dedup();
    required
}
impl CandidateRealizer for ExplorationRealizer<'_> {
    fn admit(&self, profile: &RuntimeProfile) -> Option<AttemptFailure> {
        let d = &self.planned.derivation;
        if let Err(error) = d.requirements.within(self.ceiling) {
            return refuse(
                error.0,
                "requirements exceed the frozen search-scoped ceiling",
            );
        }
        if profile.get("formation.containment") != Some("bwrap+landlock") {
            return refuse(
                "runtime_cannot_contain_candidate",
                "exploration requires the real contained process adapter",
            );
        }
        if self.builder.network != NetworkPolicy::Scoped {
            return refuse(
                "exploration_requirement_enforcement_unavailable",
                "exploration requires phase-scoped gate enforcement",
            );
        }
        if d.requirements.network.iter().any(|n| n.port != 443) {
            return refuse(
                "unsupported_plain_http",
                "the scoped adapter accepts HTTPS CONNECT destinations only",
            );
        }
        admit_isolated_authority(d, self.ceiling)
    }
    fn realize(&self, attempt_id: &str, root: &Path) -> Result<Realized, RealizeFailure> {
        let executed = self
            .builder
            .execute_with_observed_network(
                &AttemptExecution {
                    attempt_id,
                    candidate: self.planned,
                    source_root: self.source_root,
                    attempt_root: root,
                },
                BuildAttempt {
                    job_id: "exploration".into(),
                    attempt_id: attempt_id.into(),
                    attempt_fence: 1,
                },
                self.gates,
                self.network_refusal,
            )
            .map_err(
                |error| match error.downcast_ref::<crate::build::BuildStopUnconfirmed>() {
                    Some(stopped) => RealizeFailure::Abandoned {
                        cleanup: stopped.cleanup.clone(),
                        resources: vec![format!("build-process-group:{}", stopped.pid)],
                        error,
                    },
                    None => RealizeFailure::Execution(error),
                },
            )?;
        CandidateLauncher {
            planned: self.planned,
            shim: self.shim,
            network: NetworkPolicy::Scoped,
        }
        .realize_scoped(
            executed,
            attempt_id,
            root,
            self.gates
                .get(&ExecutionPhase::Runtime)
                .ok_or_else(|| RealizeFailure::Launch {
                    error: anyhow::anyhow!("runtime phase gate missing"),
                    evidence: None,
                })?,
        )
    }
}

pub fn admit_isolated_authority(
    d: &ato_formation::authoring::BoundDerivation,
    ceiling: &ExecutionRequirements,
) -> Option<AttemptFailure> {
    for required in required_isolated_authority(d) {
        // Existing canonical routes express bind through their port
        // declaration. Preserve those exact bytes while checking that
        // implicit authority against the SAME exploration ceiling.
        let legacy = d.source_oci.is_none()
            && d.requirements.is_empty()
            && d.steps.iter().all(|s| s.network.is_denied());
        if legacy {
            if !ceiling.authority.contains(&required) {
                return refuse(
                    "exploration_authority_exceeded",
                    "legacy declared port exceeds exploration ceiling",
                );
            }
            continue;
        }
        if !d.requirements.authority.contains(&required) {
            return refuse(
                "authority_denied",
                format!(
                    "runtime {:?} requirement missing for {} {}",
                    required.operation, required.protocol, required.resource
                ),
            );
        }
    }
    for a in &d.requirements.authority {
        let registered = a.phase == ExecutionPhase::Runtime
            && (d.ports.iter().any(|p| {
                p.id == a.resource
                    && p.protocol == a.protocol
                    && a.operation == ResourceOperation::Bind
            }) || d.state.iter().any(|s| {
                s.id == a.resource
                    && s.protocol == a.protocol
                    && matches!(
                        a.operation,
                        ResourceOperation::Read
                            | ResourceOperation::Write
                            | ResourceOperation::Create
                    )
            }));
        if !registered {
            return refuse(
                "unsupported_authority_resource",
                "no registered isolated resource adapter implements this requirement",
            );
        }
    }
    if d.state.iter().any(|s| {
        s.protocol != ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL
            || !ato_formation::proposal::isolated_state_id(&s.id)
            || !ato_formation::proposal::isolated_state_mount(&s.mount)
    }) {
        return refuse(
            "unsupported_state_requirement",
            "the state adapter accepts only isolated guest filesystem slots",
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::authoring::{BindingContext, StepNetwork, bind};
    #[test]
    fn generated_scoped_plan_requires_explicit_bind_while_legacy_uses_its_declared_port() {
        let draft = ato_formation::capsule_toml::parse_capsule_toml(include_str!(
            "../../formation/tests/fixtures/proposal-python.toml"
        ))
        .unwrap();
        let (_, mut d) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
            },
        )
        .unwrap();
        let ceiling = ExecutionRequirements {
            network: vec![],
            authority: required_isolated_authority(&d),
        };
        assert!(admit_isolated_authority(&d, &ceiling).is_none());
        d.steps[0].network = StepNetwork::ScopedDependencies;
        assert_eq!(
            admit_isolated_authority(&d, &ceiling).unwrap().code,
            "authority_denied"
        );
        d.requirements = ceiling.clone();
        assert!(admit_isolated_authority(&d, &ceiling).is_none());
        assert_eq!(
            d.requirements
                .within(&ExecutionRequirements::default())
                .unwrap_err()
                .0,
            "exploration_authority_exceeded"
        );
    }
}
