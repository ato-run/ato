//! The `state_volume_maintenance` lease: checkpoint, restore or delete a
//! Runner-local volume (Mail Step ②c).
//!
//! The control plane issues this lease only after the previous Run's stop was
//! confirmed and this operation took the next writer generation; it runs no
//! workload. Each operation leaves the volume consistent at every instant:
//!
//! - checkpoint reads `data/` and commits it as a State Revision;
//! - restore writes the target revision beside `data/` and swaps it in with
//!   one atomic exchange;
//! - delete renames the whole volume out of the store in one step, then
//!   removes it.
//!
//! The command names a `volume_ref`, never a path.

use anyhow::{Context, Result, ensure};
use ato_ipc::runtime_launch_v3::is_volume_ref;
use serde::Deserialize;

use super::state_artifact::{StateArtifactTransport, materialize_working_copy, pack_state_tree};
use super::volume::VolumeStore;

pub const STATE_VOLUME_MAINTENANCE_LEASE_KIND: &str = "state_volume_maintenance";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeOperationKind {
    Checkpoint,
    Restore,
    Delete,
}

/// What the control plane puts in `runner_leases.command_json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMaintenanceLeaseCommand {
    pub run_id: String,
    pub compute_instance_id: String,
    pub operation_id: String,
    pub operation: VolumeOperationKind,
    pub state_key: String,
    pub volume_ref: String,
    pub writer_fence: u64,
}

impl VolumeMaintenanceLeaseCommand {
    pub fn validate(&self, lease_run_id: &str) -> Result<()> {
        ensure!(
            self.run_id == lease_run_id,
            "volume operation Run identity mismatch"
        );
        ensure!(
            self.operation_id.starts_with("vop_")
                && self.operation_id.len() <= 64
                && self.operation_id[4..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric()),
            "volume operation id is invalid"
        );
        ensure!(
            is_volume_ref(&self.volume_ref),
            "volume operation names an invalid volume"
        );
        ensure!(
            !self.state_key.is_empty()
                && self.state_key.len() <= 64
                && self
                    .state_key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "volume operation names an invalid state key"
        );
        Ok(())
    }
}

/// Perform the operation. The caller reports the outcome.
pub fn perform(
    command: &VolumeMaintenanceLeaseCommand,
    store: &VolumeStore,
    transport: &dyn StateArtifactTransport,
) -> Result<()> {
    // The grant is what the control plane recorded for this operation's
    // writer; it must name this volume and this generation.
    let grant = transport
        .acquire_writer(&command.state_key)
        .context("failed to redeem the operation's writer")?;
    ensure!(
        grant.writer_fence == command.writer_fence,
        "the operation's writer generation changed (lease {}, grant {})",
        command.writer_fence,
        grant.writer_fence
    );
    let granted = grant
        .volume
        .as_ref()
        .context("the operation was granted without its volume")?;
    ensure!(
        granted.volume_ref == command.volume_ref,
        "the operation was granted a different volume than its lease names"
    );

    let volume = store
        .open_existing(&command.volume_ref)
        .map_err(anyhow::Error::new)?;
    match command.operation {
        VolumeOperationKind::Checkpoint => {
            let artifact = pack_state_tree(volume.data_dir())
                .context("failed to pack the volume for a checkpoint")?;
            transport
                .commit(
                    &command.state_key,
                    command.writer_fence,
                    grant.revision_ref.as_deref(),
                    &format!("vop:{}", command.operation_id),
                    &artifact,
                )
                .context("failed to commit the checkpoint")?;
        }
        VolumeOperationKind::Restore => {
            ensure!(
                grant.revision_ref.is_some() && grant.artifact_digest.is_some(),
                "a restore was granted without the revision to restore"
            );
            let fill =
                |staging: &std::path::Path| materialize_working_copy(transport, &grant, staging);
            store
                .replace_data(&volume, &fill)
                .map_err(anyhow::Error::new)
                .context("failed to restore the volume")?;
        }
        VolumeOperationKind::Delete => {
            store
                .delete(volume)
                .map_err(anyhow::Error::new)
                .context("failed to delete the volume")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
