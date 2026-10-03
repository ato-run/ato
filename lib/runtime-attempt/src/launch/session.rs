//! One Dynamic Compute Run, start to finish.
//!
//! This is the module that makes the sleep/wake claim true, so it is worth
//! being explicit about what "the same App" means across two Runs:
//!
//! - the ComputeInstance and its schema are the SAME
//! - the Run id and the OS process are DIFFERENT
//! - the writer fence has ADVANCED
//! - the state revision has moved `null -> R1 -> R2`
//! - the bytes the app wrote in Run 1 are the bytes it reads in Run 2
//!
//! The last point is the product claim; the four above it are what make it
//! survive a second writer, a crash, and a retry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_ipc::runtime_launch::StateAttachmentV1;
use ato_ipc::runtime_launch_v3::RunnerVolumeBackingV3;

use super::process_executor::{state_working_copy, writable_state_keys};
use super::resolved::ResolvedRuntimeLaunchContext;
use super::state_artifact::{
    StateArtifactTransport, StateWriterGrant, VolumeReport, materialize_working_copy,
    pack_state_tree,
};
use super::volume::{AttachedVolume, VolumeError, VolumeStatus, VolumeStore};

/// State keys backed by a Runner-local volume, with the store that holds them.
/// Every other writable key is revision-backed.
pub type VolumeAttachments<'a> = BTreeMap<String, (&'a VolumeStore, &'a RunnerVolumeBackingV3)>;

/// What a finished Run committed. Safe to record on a receipt: identities only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStateOutcome {
    pub state_key: String,
    /// The revision this Run started from, or `None` for a first Run.
    pub parent_revision_ref: Option<String>,
    /// The revision this Run produced, or `None` when it changed nothing.
    pub revision_ref: Option<String>,
    pub writer_fence: u64,
}

/// A Run that has been prepared but not yet started.

#[derive(Debug)]
pub struct PreparedRun {
    grants: Vec<(String, StateWriterGrant)>,
    /// Attached volumes, held (and host-locked) for the whole Run.
    volumes: Vec<(String, AttachedVolume)>,
}

/// Materialize every writable attachment from the revision the control plane
/// says the slot holds.
///
/// Done BEFORE the workload starts, never lazily: an app that finds its state
/// path missing does not wait for it, it either fails or writes somewhere else.
pub fn prepare_run(
    state_attachments: &[StateAttachmentV1],
    context: &ResolvedRuntimeLaunchContext,
    transport: &dyn StateArtifactTransport,
    volume_attachments: &VolumeAttachments<'_>,
) -> Result<PreparedRun> {
    let mut grants: Vec<(String, StateWriterGrant)> = Vec::new();
    let mut volumes: Vec<(String, AttachedVolume)> = Vec::new();
    for state_key in writable_state_keys(context) {
        // Every failure past the FIRST successful acquisition has to give back
        // what it already took. A partially-prepared Run that keeps its grants
        // leaves those slots held by a Run that will never exist, and no later
        // Run can have them.
        let step = (|| -> Result<()> {
            let grant = transport
                .acquire_writer(state_key)
                .with_context(|| format!("failed to acquire the writer for state `{state_key}`"))?;
            if let Some((store, backing)) = volume_attachments.get(state_key) {
                return match attach_volume(transport, store, backing, &grant, state_key) {
                    Ok(volume) => {
                        grants.push((state_key.to_owned(), grant));
                        volumes.push((state_key.to_owned(), volume));
                        Ok(())
                    }
                    Err(error) => {
                        // Nothing has run against the volume: give the
                        // writer back.
                        let _ = transport.abort_writer(state_key, grant.writer_fence);
                        Err(error)
                    }
                };
            }
            if grant.volume.is_some() {
                // The control plane thinks this slot lives in a volume and
                // the spec does not. Restoring a working copy here would run
                // the App against stale state.
                let _ = transport.abort_writer(state_key, grant.writer_fence);
                anyhow::bail!(
                    "state `{state_key}` is volume-backed, but this launch attaches it as a revision"
                );
            }
            let working = state_working_copy(context.workspace_root(), state_key);
            match materialize_working_copy(transport, &grant, &working) {
                Ok(()) => {
                    grants.push((state_key.to_owned(), grant));
                    Ok(())
                }
                Err(error) => {
                    // This grant is not in `grants` yet, so release it here or
                    // it is lost.
                    let _ = transport.abort_writer(state_key, grant.writer_fence);
                    Err(error).with_context(|| {
                        format!("failed to materialize the working copy for state `{state_key}`")
                    })
                }
            }
        })();
        if let Err(error) = step {
            release_all(transport, &grants, WriterRelease::Aborted);
            return Err(error);
        }
    }
    if let Some(key) = volume_attachments
        .keys()
        .find(|key| !volumes.iter().any(|(attached, _)| attached == *key))
    {
        // A volume in the spec that no writable attachment took would leave
        // the App running without the state it was promised.
        release_all(transport, &grants, WriterRelease::Aborted);
        anyhow::bail!("volume-backed state `{key}` is not a writable attachment of this Run");
    }

    // The spec's fence and the grant's fence must agree, or the control plane
    // handed out the slot between projection and acquisition. Refusing here
    // means a Run never starts believing it holds a generation it does not.
    for attachment in state_attachments {
        if let (Some(expected), Some((_, grant))) = (
            attachment.writer_fence,
            grants.iter().find(|(key, _)| key == &attachment.state_key),
        ) && expected != grant.writer_fence
        {
            release_all(transport, &grants, WriterRelease::Aborted);
            anyhow::bail!(
                "state `{}` was re-assigned between projection and acquisition (spec fence {}, \
                 grant fence {})",
                attachment.state_key,
                expected,
                grant.writer_fence
            );
        }
    }
    Ok(PreparedRun { grants, volumes })
}

/// Attach the volume a grant names, provisioning it once if the control plane
/// says it is new. Never restores a revision over a volume that exists.
fn attach_volume(
    transport: &dyn StateArtifactTransport,
    store: &VolumeStore,
    backing: &RunnerVolumeBackingV3,
    grant: &StateWriterGrant,
    state_key: &str,
) -> Result<AttachedVolume> {
    let granted = grant
        .volume
        .as_ref()
        .with_context(|| format!("state `{state_key}` was granted without its volume"))?;
    anyhow::ensure!(
        granted.volume_ref == backing.volume_ref,
        "state `{state_key}` was granted a different volume than the launch names"
    );
    let seed_revision = if granted.status == VolumeStatus::Provisioning {
        anyhow::ensure!(
            grant.revision_ref == backing.initialize_from_revision_ref,
            "state `{state_key}` was granted a different seed than the launch names"
        );
        grant.revision_ref.as_deref()
    } else {
        None
    };
    let seed = |data: &Path| materialize_working_copy(transport, grant, data);
    match store.attach(
        &backing.volume_ref,
        backing.capacity_bytes,
        granted.status,
        seed_revision,
        &seed,
    ) {
        Ok(volume) => {
            if granted.status == VolumeStatus::Provisioning {
                // Idempotent: a retry that finds the volume already made
                // reports it again.
                transport.report_volume(
                    state_key,
                    grant.writer_fence,
                    &VolumeReport::Ready {
                        volume_ref: backing.volume_ref.clone(),
                    },
                )?;
            }
            Ok(volume)
        }
        Err(error) => {
            if let VolumeError::Missing { reason } = &error
                && granted.status != VolumeStatus::Missing
            {
                let _ = transport.report_volume(
                    state_key,
                    grant.writer_fence,
                    &VolumeReport::Missing {
                        volume_ref: backing.volume_ref.clone(),
                        reason: reason.clone(),
                    },
                );
            }
            Err(anyhow::Error::new(error))
        }
    }
}

/// Why a slot is being given back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterRelease {
    /// The Run finished normally.
    Released,
    /// The Run failed, or never started.
    Aborted,
}

/// Give every held slot back, best effort.
///
/// Best effort on purpose, and never fatal: this runs on the failure path, and
/// turning "could not release" into a hard error would replace a recoverable
/// stuck slot with a lost one. A slot the control plane never hears about is
/// still recoverable — the lease expires and the fence advances — but only if
/// the Runner does not abandon the rest of its cleanup first.
pub fn release_all(
    transport: &dyn StateArtifactTransport,
    grants: &[(String, StateWriterGrant)],
    reason: WriterRelease,
) {
    for (state_key, grant) in grants {
        let outcome = match reason {
            WriterRelease::Released => transport.release_writer(state_key, grant.writer_fence),
            WriterRelease::Aborted => transport.abort_writer(state_key, grant.writer_fence),
        };
        if let Err(error) = outcome {
            tracing::warn!(
                state_key = %state_key,
                writer_fence = grant.writer_fence,
                %error,
                "failed to give back a state writer; the slot stays held until the fence advances"
            );
        }
    }
}

/// Give back every slot this Run holds, without committing anything.
///
/// Only for a Run whose workload is confirmed stopped (or never started). A
/// Run whose stop could not be confirmed is quarantined instead.
pub fn abort_run(transport: &dyn StateArtifactTransport, prepared: &PreparedRun) {
    release_all(transport, &prepared.grants, WriterRelease::Aborted);
}

/// Hold every slot this Run has, and mark it quarantined: the workload could
/// not be confirmed stopped, so a new writer must not start. Nothing here
/// releases or commits. Best effort per slot — a quarantine that fails to
/// reach the control plane still leaves the slot held, which is the safe
/// side; reclamation will not free it either.
pub fn quarantine_run(
    transport: &dyn StateArtifactTransport,
    prepared: &PreparedRun,
    reason: &str,
) {
    for (state_key, grant) in &prepared.grants {
        if let Err(error) = transport.quarantine_writer(state_key, grant.writer_fence, reason) {
            tracing::warn!(
                state_key = %state_key,
                writer_fence = grant.writer_fence,
                %error,
                "failed to report a quarantined state writer; the slot stays held"
            );
        }
    }
}

impl PreparedRun {
    /// The volume attached for `state_key`, when it is volume-backed.
    pub fn volume(&self, state_key: &str) -> Option<&AttachedVolume> {
        self.volumes
            .iter()
            .find(|(key, _)| key == state_key)
            .map(|(_, volume)| volume)
    }

    /// `state_key -> writer_fence` for the slots this Run holds. Non-secret;
    /// recorded in the Runner's run journal.
    pub fn writer_fences(&self) -> std::collections::BTreeMap<String, u64> {
        self.grants
            .iter()
            .map(|(key, grant)| (key.clone(), grant.writer_fence))
            .collect()
    }
}

/// Commit whatever the workload wrote, once it has stopped.
///
/// Runs AFTER the process is gone on purpose. Packing a directory a live
/// process is still writing to would commit a torn SQLite file, and the digest
/// would make that corruption permanent.
pub fn commit_run(
    context: &ResolvedRuntimeLaunchContext,
    transport: &dyn StateArtifactTransport,
    prepared: &PreparedRun,
    commit_request_id: &str,
) -> Result<Vec<RunStateOutcome>> {
    let mut outcomes = Vec::new();
    for (state_key, grant) in &prepared.grants {
        // A volume IS the state: nothing to pack, nothing to commit. The
        // workload is already confirmed stopped, so its writes are final.
        if let Some(volume) = prepared.volume(state_key) {
            match volume.usage() {
                Ok(usage_bytes) => {
                    if let Err(error) = transport.report_volume(
                        state_key,
                        grant.writer_fence,
                        &VolumeReport::Usage {
                            volume_ref: volume.volume_ref().to_owned(),
                            usage_bytes,
                        },
                    ) {
                        tracing::warn!(state_key = %state_key, %error, "failed to report volume usage");
                    }
                }
                Err(error) => {
                    tracing::warn!(state_key = %state_key, %error, "failed to measure volume usage")
                }
            }
            outcomes.push(RunStateOutcome {
                state_key: state_key.clone(),
                parent_revision_ref: None,
                revision_ref: None,
                writer_fence: grant.writer_fence,
            });
            continue;
        }
        // Whatever happens below, this slot is given back before the function
        // returns — see the release at the end and the abort on the error
        // path.
        let working = state_working_copy(context.workspace_root(), state_key);
        let artifact = match pack_state_tree(&working) {
            Ok(artifact) => artifact,
            Err(error) => {
                release_all(transport, &prepared.grants, WriterRelease::Aborted);
                return Err(error).with_context(|| format!("failed to pack state `{state_key}`"));
            }
        };

        // An unchanged tree is not a new revision. Committing one anyway would
        // grow the history with rows that restore to exactly what came before.
        if grant.artifact_digest.as_deref() == Some(artifact.digest()) {
            outcomes.push(RunStateOutcome {
                state_key: state_key.clone(),
                parent_revision_ref: grant.revision_ref.clone(),
                revision_ref: None,
                writer_fence: grant.writer_fence,
            });
            continue;
        }

        let revision = match transport.commit(
            state_key,
            grant.writer_fence,
            grant.revision_ref.as_deref(),
            &format!("{commit_request_id}:{state_key}"),
            &artifact,
        ) {
            Ok(revision) => revision,
            Err(error) => {
                // A refused commit is not a reason to keep the slot. The bytes
                // are lost either way; holding the slot only adds a second
                // failure for the next Run.
                release_all(transport, &prepared.grants, WriterRelease::Aborted);
                return Err(error).with_context(|| format!("failed to commit state `{state_key}`"));
            }
        };
        outcomes.push(RunStateOutcome {
            state_key: state_key.clone(),
            parent_revision_ref: grant.revision_ref.clone(),
            revision_ref: Some(revision),
            writer_fence: grant.writer_fence,
        });
    }
    // Success, no-op and everything in between end here: the slot goes back so
    // the next Run can take it immediately.
    release_all(transport, &prepared.grants, WriterRelease::Released);
    Ok(outcomes)
}

/// The working copy of a state key, for a caller that needs to inspect it.
pub fn working_copy(workspace_root: &Path, state_key: &str) -> PathBuf {
    state_working_copy(workspace_root, state_key)
}
