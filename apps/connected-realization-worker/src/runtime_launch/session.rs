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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use ato_ipc::runtime_launch::{
        EndpointAllocationV1, EndpointV1, LaunchRealizationV1, ProcessRealizationV1, ReadinessV1,
        RuntimeLaunchSpecV1, StateAccessV1, StateAttachmentV1,
    };

    use super::super::lease::{ActiveRun, ActiveWorkload, ResolvedRun};
    use super::super::process_executor::{ProcessLaunchHost, ReadinessProbe};
    use super::super::resolved::{ResolvedStateAttachment, allocate_endpoint};
    use super::super::state_artifact::{StateArtifact, state_artifact_digest};
    use super::*;

    // Exercise the Hosted production lifecycle, not a second start/finish
    // implementation maintained solely for these state tests.
    fn start_run(
        spec: &RuntimeLaunchSpecV1,
        context: ResolvedRuntimeLaunchContext,
        transport: &dyn StateArtifactTransport,
        probe: &dyn ReadinessProbe,
    ) -> Result<ActiveRun> {
        let prepared = prepare_run(
            &spec.state_attachments,
            &context,
            transport,
            &BTreeMap::new(),
        )?;
        let host = ProcessLaunchHost {
            shim: std::env::var_os("ATO_TEST_WORKER_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_exe().expect("test executable")),
            runtime_root: context.workspace_root().join(".ato/test-runtime"),
            output: None,
        };
        let endpoint_ports = context
            .endpoints()
            .iter()
            .map(|p| (p.name.clone(), p.host_port))
            .collect();
        super::super::lease::start(
            &ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::V1(spec.clone()),
            ResolvedRun {
                context,
                prepared,
                endpoint_ports,
            },
            transport,
            probe,
            &ato_adapter_oci::OciOwner {
                runner_id: "test-runner".into(),
                slot_id: "test-slot".into(),
                lease_id: spec.context.run_id.clone(),
                run_id: spec.context.run_id.clone(),
                incarnation: "test-incarnation".into(),
            },
            None,
            &host,
        )
        .map_err(anyhow::Error::new)
    }

    fn finish_run(
        spec: &RuntimeLaunchSpecV1,
        transport: &dyn StateArtifactTransport,
        active: ActiveRun,
        request: &str,
    ) -> Result<Vec<RunStateOutcome>> {
        let (stop, result) = super::super::lease::finish(
            &ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::V1(spec.clone()),
            active,
            transport,
            request,
        );
        assert!(stop.overall.is_confirmed(), "{stop:?}");
        result
    }

    /// A control plane, reduced to the two rules that matter: revisions are
    /// immutable, and a stale fence cannot commit.
    #[derive(Default)]
    struct FakeControlPlane {
        inner: Mutex<Plane>,
    }

    #[derive(Default)]
    struct Plane {
        fence: u64,
        head: Option<String>,
        head_digest: Option<String>,
        artifacts: BTreeMap<String, Vec<u8>>,
        revisions: Vec<String>,
        /// Who currently holds the slot. `None` means free — which is what
        /// every failure path has to restore.
        held_by_fence: Option<u64>,
        releases: Vec<(u64, &'static str)>,
        quarantined: Vec<u64>,
        /// The volume the plane grants with the writer, for a volume-backed
        /// slot.
        volume: Option<super::super::state_artifact::GrantedVolume>,
        volume_reports: Vec<VolumeReport>,
    }

    impl StateArtifactTransport for FakeControlPlane {
        fn acquire_writer(&self, _state_key: &str) -> Result<StateWriterGrant> {
            let mut plane = self.inner.lock().expect("lock");
            anyhow::ensure!(
                plane.held_by_fence.is_none(),
                "the slot is already held; a previous Run never gave it back"
            );
            plane.fence += 1;
            plane.held_by_fence = Some(plane.fence);
            Ok(StateWriterGrant {
                revision_ref: plane.head.clone(),
                artifact_digest: plane.head_digest.clone(),
                writer_fence: plane.fence,
                volume: plane.volume.clone(),
            })
        }

        fn download(&self, artifact_digest: &str) -> Result<Vec<u8>> {
            let plane = self.inner.lock().expect("lock");
            plane
                .artifacts
                .get(artifact_digest)
                .cloned()
                .context("artifact is not in the store")
        }

        fn commit(
            &self,
            _state_key: &str,
            writer_fence: u64,
            parent_revision_ref: Option<&str>,
            _commit_request_id: &str,
            artifact: &StateArtifact,
        ) -> Result<String> {
            let mut plane = self.inner.lock().expect("lock");
            anyhow::ensure!(writer_fence == plane.fence, "stale writer fence");
            anyhow::ensure!(
                parent_revision_ref == plane.head.as_deref(),
                "commit does not descend from the head"
            );
            let revision = format!("isrev_{}", plane.revisions.len() + 1);
            plane
                .artifacts
                .insert(artifact.digest().to_owned(), artifact.bytes().to_vec());
            plane.head = Some(revision.clone());
            plane.head_digest = Some(artifact.digest().to_owned());
            plane.revisions.push(revision.clone());
            Ok(revision)
        }

        fn release_writer(&self, _state_key: &str, writer_fence: u64) -> Result<()> {
            let mut plane = self.inner.lock().expect("lock");
            plane.releases.push((writer_fence, "released"));
            if plane.held_by_fence == Some(writer_fence) {
                plane.held_by_fence = None;
            }
            Ok(())
        }

        fn abort_writer(&self, _state_key: &str, writer_fence: u64) -> Result<()> {
            let mut plane = self.inner.lock().expect("lock");
            plane.releases.push((writer_fence, "aborted"));
            if plane.held_by_fence == Some(writer_fence) {
                plane.held_by_fence = None;
            }
            Ok(())
        }

        fn quarantine_writer(
            &self,
            _state_key: &str,
            writer_fence: u64,
            _reason: &str,
        ) -> Result<()> {
            // The slot stays held: quarantine never frees it.
            self.inner
                .lock()
                .expect("lock")
                .quarantined
                .push(writer_fence);
            Ok(())
        }

        fn report_volume(
            &self,
            _state_key: &str,
            _writer_fence: u64,
            report: &VolumeReport,
        ) -> Result<()> {
            let mut plane = self.inner.lock().expect("lock");
            if let (VolumeReport::Ready { .. }, Some(volume)) = (report, plane.volume.as_mut()) {
                volume.status = VolumeStatus::Ready;
            }
            plane.volume_reports.push(report.clone());
            Ok(())
        }
    }

    struct AlwaysReady;
    impl ReadinessProbe for AlwaysReady {
        fn probe(&self, _port: u16, _path: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// A real Python program, using only the standard library.
    ///
    /// Stdlib on purpose: P3.0 established that installed dependencies cannot
    /// be carried as a Formation artifact yet, so a fixture that needed pip
    /// would be testing a lane that does not exist. `sqlite3` is what the
    /// product claim is actually about — a real database file that has to
    /// survive.
    ///
    /// It writes its row and only THEN starts listening. That ordering is what
    /// makes readiness meaningful: the Runner stops a workload once it is
    /// ready, so a workload that became ready before its write landed would
    /// lose it. Readiness is the workload's own claim that it is up, and this
    /// fixture makes that claim honestly.
    const APPEND_ROW_THEN_SERVE: &str = r#"
import os, socket, sqlite3, sys
path = os.path.join(os.environ["ATO_STATE_PATH_APP_DATA"], "app.sqlite")
connection = sqlite3.connect(path)
connection.execute("CREATE TABLE IF NOT EXISTS notes (body TEXT)")
connection.execute("INSERT INTO notes VALUES (?)", (sys.argv[1],))
connection.commit()
connection.close()
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", int(sys.argv[2])))
listener.listen(8)
while True:
    listener.accept()[0].close()
"#;

    fn spec_for(
        run_id: &str,
        writer_fence: Option<u64>,
        note: &str,
        port: u16,
    ) -> RuntimeLaunchSpecV1 {
        RuntimeLaunchSpecV1 {
            protocol: "ato.runtime-launch-spec.v1".to_owned(),
            context: ato_ipc::runtime_launch::LaunchContextV1 {
                run_id: run_id.to_owned(),
                // The App is the SAME across both Runs; only the Run differs.
                compute_id: "cmp_sleepwake".to_owned(),
                compute_schema_id: "csch_sleepwake".to_owned(),
                compute_instance_id: "cinst_sleepwake".to_owned(),
            },
            workspace: ato_ipc::runtime_launch::LaunchWorkspaceV1 {
                materialization_ref: format!("sha256:{}", "ab".repeat(32)),
                cwd_relative: String::new(),
            },
            realization: LaunchRealizationV1::Process(ProcessRealizationV1 {
                argv: vec![
                    "python3".to_owned(),
                    "-c".to_owned(),
                    APPEND_ROW_THEN_SERVE.to_owned(),
                    note.to_owned(),
                    port.to_string(),
                ],
                executable: None,
            }),
            public_env: Vec::new(),
            secret_grants: Vec::new(),
            state_attachments: vec![StateAttachmentV1 {
                state_key: "app_data".to_owned(),
                revision_ref: None,
                mount_target: "/data".to_owned(),
                access: StateAccessV1::ReadWrite,
                writer_fence,
            }],
            endpoints: vec![EndpointV1 {
                name: "http".to_owned(),
                protocol: "http".to_owned(),
                guest_port: Some(8000),
                allocation: EndpointAllocationV1::Automatic,
                preferred_port: None,
            }],
            // TCP, not `process`: "the process is up" would be true before
            // the row was written.
            readiness: ReadinessV1::Tcp {
                endpoint_name: "http".to_owned(),
                timeout_ms: 15_000,
            },
            lifecycle: ato_ipc::runtime_launch::LifecycleV1 {
                graceful_shutdown_ms: 5_000,
                force_kill_after_ms: 10_000,
            },
        }
    }

    fn context_for(workspace: &Path, port: u16) -> ResolvedRuntimeLaunchContext {
        ResolvedRuntimeLaunchContext::new(
            workspace.to_path_buf(),
            "",
            BTreeMap::new(),
            Vec::new(),
            vec![ResolvedStateAttachment::new(
                "app_data",
                None,
                state_working_copy(workspace, "app_data"),
                "/data",
                StateAccessV1::ReadWrite,
            )],
            vec![allocate_endpoint(
                &EndpointV1 {
                    name: "http".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8000),
                    allocation: EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                port,
            )],
        )
        .expect("context resolves")
    }

    /// One Run, in a workspace of its own — a woken App does NOT get the
    /// previous Run's directory back. That is the whole point: if the second
    /// Run reused the first one's disk, the test would pass without any state
    /// ever being committed or restored.
    fn run_once(plane: &FakeControlPlane, run_id: &str, note: &str) -> (Vec<RunStateOutcome>, u32) {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("dedicated test port")
            .local_addr()
            .unwrap()
            .port();
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), port);
        let grant_fence = {
            let plane = plane.inner.lock().expect("lock");
            plane.fence + 1
        };
        let spec = spec_for(run_id, Some(grant_fence), note, port);

        let active = start_run(
            &spec,
            context,
            plane,
            &super::super::process_executor::LoopbackReadinessProbe::new(
                reqwest::blocking::Client::new(),
            ),
        )
        .expect("run starts");
        let pid = match &active.launched {
            ActiveWorkload::Process(p) => p.pid(),
            _ => panic!("process"),
        };
        let outcomes =
            finish_run(&spec, plane, active, &format!("commit_{run_id}")).expect("run commits");
        (outcomes, pid)
    }

    fn python3_is_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    #[test]
    fn an_app_woken_a_second_time_continues_from_its_own_state() {
        if !python3_is_available() || !super::super::sandbox::containment_available() {
            eprintln!("skipping: needs python3 and a Runner that can contain a workload");
            return;
        }
        let plane = FakeControlPlane::default();

        let (first, first_pid) = run_once(&plane, "run_first", "from-run-1");
        assert_eq!(
            first[0].parent_revision_ref, None,
            "a first Run has no parent"
        );
        assert_eq!(first[0].revision_ref.as_deref(), Some("isrev_1"));
        assert_eq!(first[0].writer_fence, 1);

        let (second, second_pid) = run_once(&plane, "run_second", "from-run-2");
        // Different Run, different process...
        assert_ne!(first_pid, second_pid);
        // ...advanced fence...
        assert_eq!(second[0].writer_fence, 2);
        // ...and a revision that descends from the first, rather than
        // replacing it.
        assert_eq!(second[0].parent_revision_ref.as_deref(), Some("isrev_1"));
        assert_eq!(second[0].revision_ref.as_deref(), Some("isrev_2"));

        // The product claim: the row Run 1 wrote is still there in Run 2's
        // database, in a workspace Run 1 never touched.
        let restored = tempfile::tempdir().expect("tempdir");
        let head = {
            let inner = plane.inner.lock().expect("lock");
            inner.head_digest.clone().expect("head digest")
        };
        let bytes = plane.download(&head).expect("download");
        super::super::state_artifact::unpack_state_tree(&bytes, &head, restored.path())
            .expect("unpacks");

        let rows = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import sqlite3,sys;print(','.join(r[0] for r in \
                 sqlite3.connect(sys.argv[1]).execute('SELECT body FROM notes ORDER BY rowid')))",
            )
            .arg(restored.path().join("app.sqlite"))
            .output()
            .expect("query runs");
        let listed = String::from_utf8_lossy(&rows.stdout);
        assert_eq!(listed.trim(), "from-run-1,from-run-2");
    }

    #[test]
    fn an_unconfirmed_stop_quarantines_and_never_releases() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_109);
        let plane = FakeControlPlane::default();
        let spec = spec_for("run_quarantine", Some(1), "unused", 39_109);
        let prepared = prepare_run(&spec.state_attachments, &context, &plane, &BTreeMap::new())
            .expect("prepared");
        assert_eq!(prepared.writer_fences().get("app_data"), Some(&1));

        let unconfirmed = ato_adapter_oci::StopOutcome::Unconfirmed {
            reason: "process group survived".into(),
        };
        let (_, committed) = super::super::lease::settle_stopped_run(
            super::super::lease::FinishedStop {
                overall: unconfirmed.clone(),
                services: vec![("process".into(), unconfirmed)],
            },
            ResolvedRun {
                context,
                prepared,
                endpoint_ports: BTreeMap::new(),
            },
            &plane,
            "must-not-commit",
        );
        assert!(committed.is_err());

        let inner = plane.inner.lock().expect("lock");
        // Held, quarantined, and neither released nor aborted: a new writer
        // cannot take it and nothing was committed.
        assert_eq!(inner.held_by_fence, Some(1));
        assert_eq!(inner.quarantined, vec![1]);
        assert!(inner.releases.is_empty());
        assert!(inner.revisions.is_empty());
        drop(inner);
        assert!(plane.acquire_writer("app_data").is_err());
    }

    #[test]
    fn a_run_whose_slot_was_reassigned_never_starts() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_104);
        let plane = FakeControlPlane::default();
        // The spec was projected while the slot was at generation 7; by the
        // time this Run acquires, the control plane hands out 1. Starting
        // anyway would mean running a workload that believes it holds a
        // generation it does not.
        let spec = spec_for("run_stale", Some(7), "unused", 39_104);
        let error =
            prepare_run(&spec.state_attachments, &context, &plane, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("re-assigned"), "{error}");
    }

    #[test]
    fn a_run_that_changed_nothing_does_not_mint_a_revision() {
        if !python3_is_available() || !super::super::sandbox::containment_available() {
            eprintln!("skipping: needs python3 and a Runner that can contain a workload");
            return;
        }
        let plane = FakeControlPlane::default();
        run_once(&plane, "run_write", "only-row");

        // A Run that touches nothing: same App, same state, no new bytes.
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_105);
        let mut spec = spec_for("run_noop", Some(2), "unused", 39_105);
        spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "true".to_owned()],
            executable: None,
        });
        // Nothing to serve, so readiness is the weakest form.
        spec.readiness = ReadinessV1::Process { timeout_ms: 5_000 };
        let active = start_run(&spec, context, &plane, &AlwaysReady).expect("starts");
        let outcomes = finish_run(&spec, &plane, active, "commit_noop").expect("commits");
        // Committing anyway would grow the history with a revision that
        // restores to exactly what came before.
        assert_eq!(outcomes[0].revision_ref, None);
        assert_eq!(outcomes[0].parent_revision_ref.as_deref(), Some("isrev_1"));
    }

    #[test]
    fn a_restored_working_copy_is_byte_identical_to_what_was_committed() {
        let plane = FakeControlPlane::default();
        let workspace = tempfile::tempdir().expect("tempdir");
        let working = state_working_copy(workspace.path(), "app_data");
        std::fs::create_dir_all(&working).expect("mkdir");
        std::fs::write(working.join("app.sqlite"), b"not-really-sqlite").expect("write");
        let artifact = pack_state_tree(&working).expect("packs");
        plane
            .commit("app_data", 0, None, "c1", &artifact)
            .expect("commits");

        let restored = tempfile::tempdir().expect("tempdir");
        let target = restored.path().join("working");
        materialize_working_copy(
            &plane,
            &StateWriterGrant {
                revision_ref: Some("isrev_1".to_owned()),
                artifact_digest: Some(artifact.digest().to_owned()),
                writer_fence: 1,
                volume: None,
            },
            &target,
        )
        .expect("materializes");
        assert_eq!(
            std::fs::read(target.join("app.sqlite")).expect("read"),
            b"not-really-sqlite"
        );
        assert_eq!(
            state_artifact_digest(pack_state_tree(&target).expect("repacks").bytes()),
            state_artifact_digest(artifact.bytes())
        );
    }

    #[test]
    fn a_spawn_failure_gives_the_slot_straight_back() {
        // The failure that matters most: the Run never existed, so nothing
        // else would ever release it, and the App would be permanently stuck.
        let plane = FakeControlPlane::default();
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_106);
        let mut spec = spec_for("run_nonexistent", Some(1), "unused", 39_106);
        spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
            argv: Vec::new(),
            executable: None,
        });
        assert!(start_run(&spec, context, &plane, &AlwaysReady).is_err());

        let inner = plane.inner.lock().expect("lock");
        assert_eq!(inner.held_by_fence, None, "the slot is still held");
        assert_eq!(inner.releases, vec![(1, "aborted")]);
    }

    #[test]
    fn a_failed_run_does_not_block_the_next_one() {
        let plane = FakeControlPlane::default();
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_107);
        let mut spec = spec_for("run_doomed", Some(1), "unused", 39_107);
        spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
            argv: Vec::new(),
            executable: None,
        });
        assert!(start_run(&spec, context, &plane, &AlwaysReady).is_err());

        // The acceptance criterion: a DIFFERENT Run can take the same
        // state_key immediately, with an advanced fence.
        let grant = plane.acquire_writer("app_data").expect("the slot is free");
        assert_eq!(grant.writer_fence, 2);
    }

    #[test]
    fn a_stale_projection_releases_what_it_already_took() {
        let plane = FakeControlPlane::default();
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_108);
        // Projected at generation 7, granted 1 — the slot moved underneath it.
        let spec = spec_for("run_stale_release", Some(7), "unused", 39_108);
        prepare_run(&spec.state_attachments, &context, &plane, &BTreeMap::new()).unwrap_err();

        let inner = plane.inner.lock().expect("lock");
        assert_eq!(inner.held_by_fence, None);
        assert_eq!(inner.releases, vec![(1, "aborted")]);
    }

    #[test]
    fn a_no_op_run_releases_the_slot_too() {
        if !python3_is_available() || !super::super::sandbox::containment_available() {
            eprintln!("skipping: needs python3 and a Runner that can contain a workload");
            return;
        }
        let plane = FakeControlPlane::default();
        run_once(&plane, "run_seed", "only-row");
        let inner = plane.inner.lock().expect("lock");
        // Committed or not, the slot goes back — otherwise a wake that changed
        // nothing would be indistinguishable from a crash.
        assert_eq!(inner.held_by_fence, None);
        assert!(inner.releases.iter().any(|(_, why)| *why == "released"));
    }

    const VOLUME_REF: &str = "svol_01M2SVCGR0VP000000000000V1";

    fn volume_backing(seed: Option<&str>) -> RunnerVolumeBackingV3 {
        RunnerVolumeBackingV3 {
            volume_ref: VOLUME_REF.to_owned(),
            capacity_bytes: 1024 * 1024,
            initialize_from_revision_ref: seed.map(str::to_owned),
        }
    }

    fn volume_plane(status: VolumeStatus) -> FakeControlPlane {
        FakeControlPlane {
            inner: Mutex::new(Plane {
                volume: Some(super::super::state_artifact::GrantedVolume {
                    volume_ref: VOLUME_REF.to_owned(),
                    status,
                }),
                ..Plane::default()
            }),
        }
    }

    /// A context whose `app_data` is mounted from the volume, as `resolve_run`
    /// builds it.
    fn volume_context(workspace: &Path, store: &VolumeStore) -> ResolvedRuntimeLaunchContext {
        ResolvedRuntimeLaunchContext::new(
            workspace.to_path_buf(),
            "",
            BTreeMap::new(),
            Vec::new(),
            vec![ResolvedStateAttachment::new(
                "app_data",
                None,
                store.data_dir(VOLUME_REF).unwrap(),
                "/data",
                StateAccessV1::ReadWrite,
            )],
            Vec::new(),
        )
        .expect("context resolves")
    }

    fn fence_attachment(fence: u64) -> Vec<StateAttachmentV1> {
        vec![StateAttachmentV1 {
            state_key: "app_data".to_owned(),
            revision_ref: None,
            mount_target: "/data".to_owned(),
            access: StateAccessV1::ReadWrite,
            writer_fence: Some(fence),
        }]
    }

    #[test]
    fn a_volume_backed_run_writes_in_place_and_never_commits_a_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(root.path(), "runner-a").unwrap();
        let plane = volume_plane(VolumeStatus::Provisioning);
        let backing = volume_backing(None);
        let volumes: VolumeAttachments<'_> =
            BTreeMap::from([("app_data".to_owned(), (&store, &backing))]);

        for (run, value) in [(1u64, "A"), (2, "B")] {
            let workspace = tempfile::tempdir().unwrap();
            let context = volume_context(workspace.path(), &store);
            let prepared =
                prepare_run(&fence_attachment(run), &context, &plane, &volumes).expect("prepared");
            let data = prepared
                .volume("app_data")
                .unwrap()
                .data_dir()
                .to_path_buf();
            if run == 2 {
                // The next Run sees the previous Run's bytes: same volume.
                assert_eq!(std::fs::read_to_string(data.join("value")).unwrap(), "A");
            }
            std::fs::write(data.join("value"), value).unwrap();
            let outcomes = commit_run(&context, &plane, &prepared, "commit").unwrap();
            assert_eq!(outcomes[0].revision_ref, None);
        }

        let plane = plane.inner.lock().unwrap();
        assert!(plane.revisions.is_empty(), "a volume Run never commits");
        assert_eq!(plane.held_by_fence, None, "the writer is given back");
        assert_eq!(
            plane
                .volume_reports
                .iter()
                .filter(|report| matches!(report, VolumeReport::Ready { .. }))
                .count(),
            1,
            "provisioned once, reported ready once"
        );
        assert!(
            plane
                .volume_reports
                .iter()
                .any(|report| matches!(report, VolumeReport::Usage { .. }))
        );
        assert_eq!(
            std::fs::read_to_string(store.data_dir(VOLUME_REF).unwrap().join("value")).unwrap(),
            "B"
        );
    }

    #[test]
    fn a_ready_volume_that_is_gone_is_reported_missing_and_the_writer_returned() {
        let root = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(root.path(), "runner-a").unwrap();
        let plane = volume_plane(VolumeStatus::Ready);
        let backing = volume_backing(None);
        let volumes: VolumeAttachments<'_> =
            BTreeMap::from([("app_data".to_owned(), (&store, &backing))]);
        let workspace = tempfile::tempdir().unwrap();
        let context = volume_context(workspace.path(), &store);

        let error = prepare_run(&fence_attachment(1), &context, &plane, &volumes).unwrap_err();
        assert!(format!("{error:#}").contains("state_volume_missing"));
        let plane = plane.inner.lock().unwrap();
        assert!(matches!(
            plane.volume_reports.as_slice(),
            [VolumeReport::Missing { .. }]
        ));
        assert_eq!(plane.held_by_fence, None);
        assert!(!store.data_dir(VOLUME_REF).unwrap().exists());
    }

    #[test]
    fn a_grant_that_disagrees_with_the_launch_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(root.path(), "runner-a").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let context = volume_context(workspace.path(), &store);

        // A different volume than the spec names.
        let plane = volume_plane(VolumeStatus::Provisioning);
        plane
            .inner
            .lock()
            .unwrap()
            .volume
            .as_mut()
            .unwrap()
            .volume_ref = "svol_01M2SVCGR0VP000000000000V2".to_owned();
        let backing = volume_backing(None);
        let volumes: VolumeAttachments<'_> =
            BTreeMap::from([("app_data".to_owned(), (&store, &backing))]);
        prepare_run(&fence_attachment(1), &context, &plane, &volumes).unwrap_err();
        assert_eq!(plane.inner.lock().unwrap().held_by_fence, None);

        // A volume-backed slot the launch attaches as a revision.
        let plane = volume_plane(VolumeStatus::Ready);
        let revision_context = context_for(workspace.path(), 0);
        prepare_run(
            &fence_attachment(1),
            &revision_context,
            &plane,
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(plane.inner.lock().unwrap().held_by_fence, None);
        assert!(!store.data_dir(VOLUME_REF).unwrap().exists());
    }

    #[test]
    fn an_unconfirmed_stop_keeps_the_volume_and_quarantines_the_writer() {
        let root = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(root.path(), "runner-a").unwrap();
        let plane = volume_plane(VolumeStatus::Provisioning);
        let backing = volume_backing(None);
        let volumes: VolumeAttachments<'_> =
            BTreeMap::from([("app_data".to_owned(), (&store, &backing))]);
        let workspace = tempfile::tempdir().unwrap();
        let context = volume_context(workspace.path(), &store);
        let prepared = prepare_run(&fence_attachment(1), &context, &plane, &volumes).unwrap();
        std::fs::write(
            prepared.volume("app_data").unwrap().data_dir().join("v"),
            "A",
        )
        .unwrap();

        quarantine_run(&plane, &prepared, "stop unconfirmed");
        drop(prepared);

        let inner = plane.inner.lock().unwrap();
        assert_eq!(inner.quarantined, vec![1]);
        assert_eq!(
            inner.held_by_fence,
            Some(1),
            "quarantine never frees the slot"
        );
        assert!(inner.revisions.is_empty());
        assert_eq!(
            std::fs::read_to_string(store.data_dir(VOLUME_REF).unwrap().join("v")).unwrap(),
            "A"
        );
    }

    // ── Hosted OCI on a native Linux Docker host ────────────────────────────
    //
    // The same production `lease::start` → `lease::finish` as the process
    // tests above, for a single container and for a service group, with real
    // pinned images. `ATO_OCI_OWNERSHIP_FIXTURE_OUT=<dir>` writes each case
    // normalized, to compare two builds.

    const WHOAMI: &str = "docker.io/traefik/whoami@sha256:4f90b33ddca9c4d4f06527070d6e503b16d71016edea036842be2a84e60c91cb";
    const NGINX: &str = "docker.io/nginxinc/nginx-unprivileged@sha256:28d91bdce70ad09025ea901458fdd149259d8e05982ade79d4ef2c0d9470eb48";

    fn oci_owner(slot: &str, run_id: &str) -> ato_adapter_oci::OciOwner {
        ato_adapter_oci::OciOwner {
            runner_id: "itest-2e-c".into(),
            slot_id: slot.into(),
            lease_id: format!("lease-{slot}"),
            run_id: run_id.to_owned(),
            incarnation: "itest".into(),
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Start and finish one Run through the Hosted lease, and describe what
    /// happened with every volatile identifier replaced.
    fn oci_run(
        case: &str,
        spec: ato_ipc::runtime_launch_v2::RuntimeLaunchSpec,
        context: ResolvedRuntimeLaunchContext,
        attachments: &[StateAttachmentV1],
        surface_port: u16,
        path: &str,
    ) -> serde_json::Value {
        let plane = FakeControlPlane::default();
        let prepared = prepare_run(attachments, &context, &plane, &BTreeMap::new()).unwrap();
        let endpoint_ports = context
            .endpoints()
            .iter()
            .map(|endpoint| (endpoint.name.clone(), endpoint.host_port))
            .collect();
        let run_id = spec.context().run_id.clone();
        let owner = oci_owner(case, &run_id);
        let host = ProcessLaunchHost {
            shim: std::env::current_exe().unwrap(),
            runtime_root: context.workspace_root().join(".ato/test-runtime"),
            output: None,
        };
        let active = super::super::lease::start(
            &spec,
            ResolvedRun {
                context,
                prepared,
                endpoint_ports,
            },
            &plane,
            &super::super::process_executor::LoopbackReadinessProbe::new(
                reqwest::blocking::Client::new(),
            ),
            &owner,
            None,
            &host,
        )
        .expect("the OCI Run starts");
        let evidence = serde_json::to_value(active.execution_evidence()).unwrap();
        let subject = active.execution_subject();
        let served = reqwest::blocking::get(format!("http://127.0.0.1:{surface_port}{path}"))
            .expect("the Surface answers")
            .status()
            .as_u16();
        let running = ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
            .unwrap()
            .scan()
            .unwrap();
        let (stop, committed) =
            super::super::lease::finish(&spec, active, &plane, &format!("commit_{case}"));
        let committed = committed.expect("the confirmed stop commits");
        let left = ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
            .unwrap()
            .scan()
            .unwrap();
        assert!(stop.overall.is_confirmed(), "{stop:?}");
        assert!(left.containers.is_empty(), "{left:?}");
        assert!(left.networks.is_empty(), "{left:?}");
        let plane = plane.inner.lock().unwrap();
        let mut document = serde_json::json!({
            "evidence": evidence,
            "subject_parts": subject.split(',').count(),
            "served_status": served,
            "running_containers": running.containers.len(),
            "running_networks": running.networks.len(),
            "stop": stop
                .services
                .iter()
                .map(|(name, outcome)| (name.clone(), serde_json::to_value(outcome).unwrap()))
                .collect::<Vec<_>>(),
            "stop_confirmed": stop.overall.is_confirmed(),
            "state": committed
                .iter()
                .map(|outcome| serde_json::json!({
                    "state_key": outcome.state_key,
                    "revision_ref": outcome.revision_ref,
                    "writer_fence": outcome.writer_fence,
                }))
                .collect::<Vec<_>>(),
            "releases": plane.releases.iter().map(|(fence, kind)| format!("{fence}:{kind}")).collect::<Vec<_>>(),
            "quarantined": plane.quarantined,
            "left_containers": left.containers.len(),
            "left_networks": left.networks.len(),
        });
        normalize_container_ids(&mut document["evidence"]);
        if let Some(dir) = std::env::var_os("ATO_OCI_OWNERSHIP_FIXTURE_OUT") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{case}.json")),
                serde_json::to_vec_pretty(&document).unwrap(),
            )
            .unwrap();
        }
        document
    }

    fn normalize_container_ids(evidence: &mut serde_json::Value) {
        if evidence["container_id"].is_string() {
            evidence["container_id"] = serde_json::json!("<container>");
        }
        if let Some(services) = evidence["services"].as_array_mut() {
            for service in services {
                service["container_id"] = serde_json::json!("<container>");
            }
        }
    }

    #[test]
    #[ignore = "needs a native Linux Docker host"]
    fn a_hosted_oci_container_starts_serves_stops_and_commits() {
        let workspace = tempfile::tempdir().unwrap();
        let port = free_port();
        let mut spec = spec_for("run_2ec_container", Some(1), "unused", port);
        spec.realization = LaunchRealizationV1::Oci(ato_ipc::runtime_launch::OciRealizationV1 {
            image_digest_ref: WHOAMI.rsplit_once('@').unwrap().1.to_owned(),
            image_reference: Some(WHOAMI.to_owned()),
            platform: Some("linux/amd64".to_owned()),
            resource_limits: Some(ato_ipc::runtime_launch::OciResourceLimitsV1 {
                memory_bytes: 134_217_728,
                cpu_limit_millis: 500,
                pids_limit: 64,
            }),
            entrypoint: None,
            argv: Some(vec!["--port".to_owned(), "8000".to_owned()]),
            working_dir: None,
            workspace_mount_path: None,
        });
        spec.readiness = ReadinessV1::Http {
            endpoint_name: "http".to_owned(),
            path: "/".to_owned(),
            timeout_ms: 60_000,
        };
        let attachments = spec.state_attachments.clone();
        let document = oci_run(
            "container",
            ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::V1(spec),
            context_for(workspace.path(), port),
            &attachments,
            port,
            "/",
        );
        assert_eq!(document["evidence"]["realization"], "oci");
        assert_eq!(document["served_status"], 200);
        assert_eq!(document["running_containers"], 1);
    }

    #[test]
    #[ignore = "needs a native Linux Docker host"]
    fn a_hosted_oci_service_group_starts_serves_stops_and_commits() {
        let mut raw: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json"
        ))
        .unwrap();
        raw["context"]["run_id"] = serde_json::json!("run_2ec_group");
        let services = raw["realization"]["services"].as_array_mut().unwrap();
        for (service, image) in services.iter_mut().zip([WHOAMI, NGINX]) {
            service["image_reference"] = serde_json::json!(image);
            service["image_digest_ref"] = serde_json::json!(image.rsplit_once('@').unwrap().1);
        }
        services[0]["argv"] = serde_json::json!(["--port", "8081"]);
        services[0]["endpoints"][0]["guest_port"] = serde_json::json!(8081);
        services[1]["readiness"]["path"] = serde_json::json!("/healthz");
        let spec = ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::parse(&raw.to_string())
            .expect("the group spec parses");
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("nginx.conf"),
            include_str!("../../../../samples/oci-service-group-proof/nginx.conf"),
        )
        .unwrap();
        let port = free_port();
        let context = ResolvedRuntimeLaunchContext::new(
            workspace.path().to_path_buf(),
            "",
            BTreeMap::new(),
            vec![super::super::resolved::ResolvedSecret::new(
                "ATO_BINDING_ADMIN_SECRET",
                "itest-secret",
            )],
            vec![ResolvedStateAttachment::new(
                "data",
                None,
                state_working_copy(workspace.path(), "data"),
                "/data",
                StateAccessV1::ReadWrite,
            )],
            vec![allocate_endpoint(
                &EndpointV1 {
                    name: "app.http".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8080),
                    allocation: EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                port,
            )],
        )
        .unwrap();
        let attachments = vec![StateAttachmentV1 {
            state_key: "data".to_owned(),
            revision_ref: None,
            mount_target: "/data".to_owned(),
            access: StateAccessV1::ReadWrite,
            writer_fence: Some(1),
        }];
        let document = oci_run("group", spec, context, &attachments, port, "/");
        assert_eq!(document["evidence"]["realization"], "oci_service_group");
        assert_eq!(document["served_status"], 200);
        assert_eq!(document["running_containers"], 2);
        assert_eq!(document["subject_parts"], 2);
    }
}
