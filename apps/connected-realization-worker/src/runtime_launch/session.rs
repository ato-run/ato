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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_ipc::runtime_launch::RuntimeLaunchSpecV1;

use super::process_executor::{
    LaunchedProcess, ReadinessProbe, launch_process, state_working_copy, wait_until_ready,
    writable_state_keys,
};
use super::resolved::ResolvedRuntimeLaunchContext;
use super::state_artifact::{
    StateArtifactTransport, StateWriterGrant, materialize_working_copy, pack_state_tree,
};

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
}

/// Materialize every writable attachment from the revision the control plane
/// says the slot holds.
///
/// Done BEFORE the workload starts, never lazily: an app that finds its state
/// path missing does not wait for it, it either fails or writes somewhere else.
pub fn prepare_run(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    transport: &dyn StateArtifactTransport,
) -> Result<PreparedRun> {
    let mut grants: Vec<(String, StateWriterGrant)> = Vec::new();
    for state_key in writable_state_keys(context) {
        // Every failure past the FIRST successful acquisition has to give back
        // what it already took. A partially-prepared Run that keeps its grants
        // leaves those slots held by a Run that will never exist, and no later
        // Run can have them.
        let step = (|| -> Result<()> {
            let grant = transport
                .acquire_writer(state_key)
                .with_context(|| format!("failed to acquire the writer for state `{state_key}`"))?;
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

    // The spec's fence and the grant's fence must agree, or the control plane
    // handed out the slot between projection and acquisition. Refusing here
    // means a Run never starts believing it holds a generation it does not.
    for attachment in &spec.state_attachments {
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
    Ok(PreparedRun { grants })
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
pub fn abort_run(transport: &dyn StateArtifactTransport, prepared: &PreparedRun) {
    release_all(transport, &prepared.grants, WriterRelease::Aborted);
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

/// Prepare, launch and wait for readiness.
pub fn start_run(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    transport: &dyn StateArtifactTransport,
    probe: &dyn ReadinessProbe,
) -> Result<(PreparedRun, LaunchedProcess)> {
    let prepared = prepare_run(spec, context, transport)?;
    let mut launched = match launch_process(spec, context) {
        Ok(launched) => launched,
        Err(error) => {
            // Spawn failure is the easiest path to a permanently stuck slot:
            // the Run never existed, so nothing else would ever release it.
            abort_run(transport, &prepared);
            return Err(error);
        }
    };
    match wait_until_ready(spec, context, &mut launched, probe) {
        Ok(()) => Ok((prepared, launched)),
        Err(error) => {
            // A workload that never became ready still gets stopped AND still
            // gives its slots back. Leaving either behind turns one failed Run
            // into an App that can never start again.
            let _ = launched.stop(&spec.lifecycle);
            abort_run(transport, &prepared);
            Err(error)
        }
    }
}

/// Stop the workload and commit what it wrote.
pub fn finish_run(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    transport: &dyn StateArtifactTransport,
    prepared: &PreparedRun,
    launched: LaunchedProcess,
    commit_request_id: &str,
) -> Result<Vec<RunStateOutcome>> {
    if let Err(error) = launched.stop(&spec.lifecycle) {
        // The subtree may still be alive, so packing is refused — but the
        // slot is still given back, because a stuck slot would outlive the
        // stuck process.
        abort_run(transport, prepared);
        return Err(error);
    }
    commit_run(context, transport, prepared, commit_request_id)
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
        StateAccessV1, StateAttachmentV1,
    };

    use super::super::resolved::{ResolvedStateAttachment, allocate_endpoint};
    use super::super::state_artifact::{StateArtifact, state_artifact_digest};
    use super::*;

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
    fn run_once(
        plane: &FakeControlPlane,
        run_id: &str,
        note: &str,
        port: u16,
    ) -> (Vec<RunStateOutcome>, u32) {
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), port);
        let grant_fence = {
            let plane = plane.inner.lock().expect("lock");
            plane.fence + 1
        };
        let spec = spec_for(run_id, Some(grant_fence), note, port);

        let (prepared, launched) = start_run(
            &spec,
            &context,
            plane,
            &super::super::process_executor::LoopbackReadinessProbe::new(
                reqwest::blocking::Client::new(),
            ),
        )
        .expect("run starts");
        let pid = launched.pid();
        let outcomes = finish_run(
            &spec,
            &context,
            plane,
            &prepared,
            launched,
            &format!("commit_{run_id}"),
        )
        .expect("run commits");
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

        let (first, first_pid) = run_once(&plane, "run_first", "from-run-1", 39_101);
        assert_eq!(
            first[0].parent_revision_ref, None,
            "a first Run has no parent"
        );
        assert_eq!(first[0].revision_ref.as_deref(), Some("isrev_1"));
        assert_eq!(first[0].writer_fence, 1);

        let (second, second_pid) = run_once(&plane, "run_second", "from-run-2", 39_102);
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
    fn a_run_whose_slot_was_reassigned_never_starts() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_104);
        let plane = FakeControlPlane::default();
        // The spec was projected while the slot was at generation 7; by the
        // time this Run acquires, the control plane hands out 1. Starting
        // anyway would mean running a workload that believes it holds a
        // generation it does not.
        let spec = spec_for("run_stale", Some(7), "unused", 39_104);
        let error = prepare_run(&spec, &context, &plane).unwrap_err();
        assert!(error.to_string().contains("re-assigned"), "{error}");
    }

    #[test]
    fn a_run_that_changed_nothing_does_not_mint_a_revision() {
        if !python3_is_available() || !super::super::sandbox::containment_available() {
            eprintln!("skipping: needs python3 and a Runner that can contain a workload");
            return;
        }
        let plane = FakeControlPlane::default();
        run_once(&plane, "run_write", "only-row", 39_103);

        // A Run that touches nothing: same App, same state, no new bytes.
        let workspace = tempfile::tempdir().expect("tempdir");
        let context = context_for(workspace.path(), 39_105);
        let mut spec = spec_for("run_noop", Some(2), "unused", 39_105);
        spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "true".to_owned()],
        });
        // Nothing to serve, so readiness is the weakest form.
        spec.readiness = ReadinessV1::Process { timeout_ms: 5_000 };
        let (prepared, launched) =
            start_run(&spec, &context, &plane, &AlwaysReady).expect("starts");
        let outcomes = finish_run(&spec, &context, &plane, &prepared, launched, "commit_noop")
            .expect("commits");
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
            argv: vec!["/nonexistent/program".to_owned()],
        });
        assert!(start_run(&spec, &context, &plane, &AlwaysReady).is_err());

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
            argv: vec!["/nonexistent/program".to_owned()],
        });
        assert!(start_run(&spec, &context, &plane, &AlwaysReady).is_err());

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
        prepare_run(&spec, &context, &plane).unwrap_err();

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
        run_once(&plane, "run_seed", "only-row", 39_109);
        let inner = plane.inner.lock().expect("lock");
        // Committed or not, the slot goes back — otherwise a wake that changed
        // nothing would be indistinguishable from a crash.
        assert_eq!(inner.held_by_fence, None);
        assert!(inner.releases.iter().any(|(_, why)| *why == "released"));
    }
}
