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
}

fn refuse(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.into(),
        stage: "admission".into(),
        message: message.into(),
    })
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
        if d.requirements
            .network
            .iter()
            .any(|n| n.phase == ExecutionPhase::Runtime)
        {
            return refuse(
                "unsupported_runtime_network",
                "this process realization currently enforces no runtime egress; the requirement is saved, not silently granted",
            );
        }
        for port in &d.ports {
            if !d.requirements.authority.iter().any(|a| {
                a.phase == ExecutionPhase::Runtime
                    && a.protocol == port.protocol
                    && a.resource == port.id
                    && a.operation == ResourceOperation::Bind
            }) {
                return refuse(
                    "authority_denied",
                    format!(
                        "runtime bind requirement missing for {} {}",
                        port.protocol, port.id
                    ),
                );
            }
        }
        for a in &d.requirements.authority {
            if a.phase != ExecutionPhase::Runtime
                || a.operation != ResourceOperation::Bind
                || !d
                    .ports
                    .iter()
                    .any(|p| p.protocol == a.protocol && p.id == a.resource)
            {
                return refuse(
                    "unsupported_authority_resource",
                    "no registered isolated resource adapter implements this requirement",
                );
            }
        }
        if !d.state.is_empty() {
            return refuse(
                "unsupported_state_requirement",
                "isolated state lowering is required before this D can execute",
            );
        }
        None
    }
    fn realize(&self, attempt_id: &str, root: &Path) -> Result<Realized, RealizeFailure> {
        let executed = self.builder.execute_with_scoped_network(
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
        )?;
        CandidateLauncher {
            planned: self.planned,
            shim: self.shim,
            network: NetworkPolicy::Scoped,
        }
        .realize(executed, attempt_id, root)
    }
}
