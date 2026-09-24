//! The execution seam a Formation attempt crosses once per candidate.
//!
//! One trait, one implementation, on purpose. Phase 1 runs every candidate on
//! the machine it was asked about — `LocalAttemptExecutor`. Phase 2 hands the
//! same call to a Runtime Network client without the search loop above it
//! changing a line.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::build::{BuildAttempt, control_policy_path, output_root, run_build};
use crate::job::{PlannedCandidate, stage_workspace};
use crate::sandbox::{BuildSandbox, NetworkPolicy};
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
    pub limits: crate::sandbox::BuildLimits,
}

impl AttemptExecutor for LocalAttemptExecutor {
    fn execute(&self, execution: &AttemptExecution<'_>) -> Result<ExecutedCandidate> {
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

        let built = run_build(
            &candidate.plan,
            BuildAttempt {
                job_id: "local".to_owned(),
                attempt_id: (*attempt_id).to_owned(),
                // Local attempts are serial and owned by this process; there
                // is no newer attempt to fence against.
                attempt_fence: 1,
            },
            &BuildSandbox {
                source_root,
                workspace_root: &workspace_root,
                cache_root: Some(&cache_root),
                shim: &self.shim,
                policy_host_path: &control_policy_path(&attempt_root)?,
                network: self.network,
                limits: self.limits,
                toolchain: crate::sandbox::ToolchainAccess::ReadOnly,
            },
        )?;

        match candidate.intent.lane {
            ato_formation::intent::Lane::PythonProcess => {
                let root = output_root(&built, &candidate.plan)?;
                Ok(ExecutedCandidate::Process {
                    workspace_root: root,
                })
            }
            ato_formation::intent::Lane::StaticWeb => {
                let produced = crate::static_lane::materialize_static(
                    &candidate.intent,
                    &candidate.plan,
                    // The WORKSPACE root: the lane resolves
                    // `static.output_root` itself.
                    &built.workspace_root,
                    &attempt_root.join("bundle"),
                    &format!("swm_{attempt_id}"),
                    // No canaries: this build redeems no secrets, so there is
                    // nothing to scan for — and an empty list is NOT a claim
                    // that the output was scanned.
                    &[],
                )?;
                Ok(ExecutedCandidate::StaticWeb {
                    output: Box::new(produced),
                })
            }
        }
    }
}
