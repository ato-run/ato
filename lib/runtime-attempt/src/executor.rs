//! The execution seam a Formation attempt crosses once per candidate.
//!
//! One trait, one implementation, on purpose. Phase 1 runs every candidate on
//! the machine it was asked about — `LocalAttemptExecutor`. Phase 2 hands the
//! same call to a Runtime Network client without the search loop above it
//! changing a line.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::build::{
    BuildAttempt, NetworkRefusalObserver, control_policy_path, output_root,
    run_build_with_variables,
};
use crate::build_sandbox::{BuildSandbox, NetworkPolicy};
use crate::plan::{PlannedCandidate, stage_workspace};
use crate::static_lane::StaticFormationOutput;

/// Everything one attempt needs, already decided: the bound route, the
/// verified source tree, and a scratch directory it owns.
pub struct AttemptExecution<'a> {
    pub attempt_id: &'a str,
    pub candidate: &'a PlannedCandidate,
    pub source_root: &'a Path,
    pub attempt_root: &'a Path,
}

/// What an attempt produced, in the shape verification consumes.
pub enum ExecutedCandidate {
    /// A workspace the Contract's process can be launched from.
    Process { workspace_root: PathBuf },
    /// A static bundle whose manifest already states what it serves.
    StaticWeb { output: Box<StaticFormationOutput> },
}

pub trait AttemptExecutor {
    fn execute(&self, execution: &AttemptExecution<'_>) -> Result<ExecutedCandidate>;
}

/// Runs the candidate's build plan on this machine, under the same
/// containment the hosted worker uses.
pub struct LocalAttemptExecutor {
    /// The calling binary, re-exec'd inside the sandbox as `sandbox-exec`.
    /// Any host that runs a contained build must expose that entry point.
    pub shim: PathBuf,
    pub network: NetworkPolicy,
    pub limits: crate::build_sandbox::BuildLimits,
}

impl AttemptExecutor for LocalAttemptExecutor {
    fn execute(&self, execution: &AttemptExecution<'_>) -> Result<ExecutedCandidate> {
        self.execute_with_identity(
            execution,
            BuildAttempt {
                job_id: "local".to_owned(),
                attempt_id: execution.attempt_id.to_owned(),
                attempt_fence: 1,
            },
        )
    }
}

impl LocalAttemptExecutor {
    /// Hosted callers retain their job/fence identity while sharing the same
    /// build, materialization and containment implementation.
    pub fn execute_with_identity(
        &self,
        execution: &AttemptExecution<'_>,
        build_attempt: BuildAttempt,
    ) -> Result<ExecutedCandidate> {
        self.execute_with_scoped_network(
            execution,
            build_attempt,
            &std::collections::BTreeMap::new(),
        )
    }

    pub fn execute_with_scoped_network(
        &self,
        execution: &AttemptExecution<'_>,
        build_attempt: BuildAttempt,
        gates: &std::collections::BTreeMap<ato_formation::requirements::ExecutionPhase, PathBuf>,
    ) -> Result<ExecutedCandidate> {
        self.execute_with_observed_network(execution, build_attempt, gates, None)
    }

    pub fn execute_with_observed_network(
        &self,
        execution: &AttemptExecution<'_>,
        build_attempt: BuildAttempt,
        gates: &std::collections::BTreeMap<ato_formation::requirements::ExecutionPhase, PathBuf>,
        refusal: Option<&NetworkRefusalObserver<'_>>,
    ) -> Result<ExecutedCandidate> {
        self.execute_with_variables(execution, build_attempt, gates, refusal, &[])
    }
    pub fn execute_with_variables(
        &self,
        execution: &AttemptExecution<'_>,
        build_attempt: BuildAttempt,
        gates: &std::collections::BTreeMap<ato_formation::requirements::ExecutionPhase, PathBuf>,
        refusal: Option<&NetworkRefusalObserver<'_>>,
        variables: &[crate::variables::ResolvedVariable],
    ) -> Result<ExecutedCandidate> {
        anyhow::ensure!(
            build_attempt.attempt_id == execution.attempt_id,
            "build attempt identity mismatch"
        );
        let AttemptExecution {
            attempt_id,
            candidate,
            source_root,
            attempt_root,
        } = execution;
        let workspace_root = attempt_root.join("workspace");
        stage_workspace(source_root, &workspace_root)?;
        let cache_root = attempt_root.join("cache");
        std::fs::create_dir_all(&cache_root).context("cannot create the build cache")?;

        let built = run_build_with_variables(
            &candidate.plan,
            &candidate.derivation,
            build_attempt,
            &BuildSandbox {
                source_root,
                workspace_root: &workspace_root,
                cache_root: Some(&cache_root),
                shim: &self.shim,
                policy_host_path: &control_policy_path(attempt_root)?,
                network: self.network,
                broker_socket: None,
                limits: self.limits,
                toolchain: crate::build_sandbox::ToolchainAccess::ReadOnly,
            },
            gates,
            refusal,
            variables,
        )?;

        match candidate.plan.lane {
            ato_formation::intent::Lane::PythonProcess | ato_formation::intent::Lane::Process => {
                let root = output_root(&built, "")?;
                let secrets = variables
                    .iter()
                    .filter(|v| {
                        candidate
                            .derivation
                            .variable_bindings
                            .iter()
                            .any(|r| r.name == v.value.name() && r.secret)
                    })
                    .map(|v| v.value.expose_for_spawn().as_bytes())
                    .collect::<Vec<_>>();
                crate::variables::scan_artifact(&root, &secrets)?;
                Ok(ExecutedCandidate::Process {
                    workspace_root: root,
                })
            }
            ato_formation::intent::Lane::StaticWeb => {
                let produced = crate::static_lane::materialize_static(
                    &candidate.derivation,
                    &candidate.plan,
                    // The WORKSPACE root: the lane resolves
                    // `static.output_root` itself.
                    &built.workspace_root,
                    &attempt_root.join("bundle"),
                    &format!("swm_{attempt_id}"),
                    // Redeemed values may not become reusable artifact bytes.
                    &variables
                        .iter()
                        .filter(|v| {
                            candidate
                                .derivation
                                .variable_bindings
                                .iter()
                                .any(|r| r.name == v.value.name() && r.secret)
                        })
                        .map(|v| v.value.expose_for_spawn().as_bytes())
                        .collect::<Vec<_>>(),
                )?;
                Ok(ExecutedCandidate::StaticWeb {
                    output: Box::new(produced),
                })
            }
        }
    }
}
