//! Sealing a finished execution.
//!
//! Stopping a durable run is FIVE steps, not one: quiesce the worker, evolve
//! the workspace into a new Computation, seal the Run/RecordFrontier
//! association, advance the branch head, and release the lease. Calling only
//! the first leaves a run that reports itself active forever — which is exactly
//! what happened when the local runtime called `stop_active` alone.
//!
//! `evolve_workspace` is where the head actually moves: it snapshots the
//! workspace into a new Computation. A run whose process wrote nothing produces
//! an identical snapshot and therefore an unchanged head — correct behaviour,
//! and worth knowing before reading a flat head as a failure.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ato_computation::{ComputationRef, ContentRef};
use ato_objects::{ActiveRun, LocalCapsuleRepository};
use ato_record_writer::load_frontier;

use crate::authoring::evolve_workspace;
use crate::supervisor::stop_active;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedRunRecordFrontier {
    pub version: u32,
    pub run_id: String,
    pub branch: String,
    pub anchor_computation_ref: String,
    pub target_computation_ref: String,
    pub record_frontier_ref: String,
}

pub fn seal_run_record_frontier(
    repository: &LocalCapsuleRepository,
    run: &ato_objects::ActiveRun,
    target: &ComputationRef,
) -> Result<()> {
    let frontier_ref_path = repository
        .root()
        .join("runs")
        .join(format!("{}.record-frontier", run.token));
    let record_frontier_ref = fs::read_to_string(&frontier_ref_path).with_context(|| {
        format!(
            "missing Capture Barrier receipt at {}",
            frontier_ref_path.display()
        )
    })?;
    let record_frontier_ref = ContentRef::parse(record_frontier_ref.trim())?;
    let frontier = load_frontier(
        &repository.root().join("records"),
        &run.token,
        &record_frontier_ref,
    )?;
    if frontier.frontier_digest != record_frontier_ref {
        bail!("Capture Barrier returned a different RecordFrontier identity");
    }
    let association = SealedRunRecordFrontier {
        version: 1,
        run_id: run.token.clone(),
        branch: run.branch.clone(),
        anchor_computation_ref: run.branch_base.to_string(),
        target_computation_ref: target.to_string(),
        record_frontier_ref: record_frontier_ref.to_string(),
    };
    atomic_write(
        &repository
            .root()
            .join("runs")
            .join(format!("{}.sealed-record-frontier.json", run.token)),
        &serde_jcs::to_vec(&association)?,
    )
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.new",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("capsule"),
        std::process::id()
    ));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// A stopped execution, after the branch has been advanced and sealed.
pub struct SealedExecution {
    pub run: ActiveRun,
    /// The branch head AFTER evolution. Equal to the run's head when the
    /// workspace did not change.
    pub head: ComputationRef,
}

/// Stop the active run and seal it.
///
/// The whole five-step sequence, in one place, so every host performs it
/// identically. Returns `None` when there was no active run.
pub fn stop_and_seal(repository: &LocalCapsuleRepository) -> Result<Option<SealedExecution>> {
    if repository.active_run()?.is_none() {
        return Ok(None);
    }
    let Some(stopped) = stop_active(repository)? else {
        return Ok(None);
    };
    let head = evolve_workspace(repository, &stopped.branch, &stopped.head)?;
    seal_run_record_frontier(repository, &stopped, &head)?;
    repository.update_head(&stopped.branch, Some(&stopped.branch_base), &head)?;
    repository.release_active_run(&stopped.token)?;
    Ok(Some(SealedExecution { run: stopped, head }))
}
