use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child as ProcessChild, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use capsule::protocol_bundle::{
    PortableCapsule, SpoolBundle, StreamingBundleReader, capture_local_workspace_checkpoint,
    normalize_v1_spool, restore_workspace_state,
};
use capsule_protocol::{
    ComputationRef, ConnectorId, ContentRef, Direction, IoRecord, LEGACY_STATE_IO_COMPUTATION_TYPE,
    Payload, RecordKindId,
};
use capsule_session_runtime::{
    BoundaryCoordinator, BoundaryDriver, BoundaryOperationId, CapsuleProtocolSessionStore,
    DurableFrontier, JournalLsn, NewStoredProtocolSession, NewSupervisorIdentity, RecordFrontier,
    SessionId, SharedSessionWal, StoredComputationOrigin, StoredConnectorCheckpoint,
    StoredLocalCheckpoint, StoredProtocolSession, StoredReplayVerification, StoredRuntimeProfile,
    SupervisorIdentity,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size};
use fs2::FileExt;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use snapshot::capsule_state::{
    READY_STATE_STATE_TYPE, ReadyStateStateObjectV1, import_ready_state,
    select_backend_for_ready_state,
};
use snapshot::{
    RestoreContainment, RestoreReadyStateInput, RestoredSession, SnapshotBackend, SnapshotError,
};

const CONTROL_FRAME_LIMIT: usize = 16 * 1024 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_QUEUE_DEPTH: usize = 64;
const PTY_DRAIN_QUEUE_DEPTH: usize = 64;
const PTY_CONNECTOR_ID: &str = "terminal.main";
const PTY_PROTOCOL_ID: &str = "ato.io.pty@1";
const PTY_CHECKPOINT_FORMAT: &str = "ato.io.pty.local-checkpoint@1";
const READY_MARKER: &[u8] = b"__ATO_SESSION_READY__";
const WATCHDOG_DISARM: &[u8] = b"DISARM\n";
const PUBLIC_METADATA_SCHEMA_VERSION: u16 = 1;
const PUBLIC_NAME_MAX_LEN: usize = 64;
const CONTAINMENT_RECEIPT: &[u8] = b"revoked\n";

pub(crate) fn start(bundle: &Path, into: &Path, no_attach: bool) -> Result<()> {
    let bundle = bundle
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", bundle.display()))?;
    let into = absolute_path(into)?;
    let session_id = random_session_id()?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    StreamingBundleReader::read_into(&bundle, &paths.directory.join("validation-spool"))
        .context("invalid Capsule bundle")?;
    import_session_seed(&bundle, &paths.seed_capsule)?;
    write_bootstrap_metadata(&paths, &SessionBootstrapMetadata::default())?;
    launch_session(&session_id, &paths, &into, no_attach, true)
}

pub(crate) fn start_public(
    bundle: &Path,
    requested_name: Option<&str>,
    detach: bool,
) -> Result<()> {
    let bundle = bundle
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", bundle.display()))?;
    let requested_name = requested_name
        .map(validate_public_name)
        .transpose()?
        .unwrap_or_else(|| public_name_from_bundle(&bundle));
    let session_id = random_session_id()?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    let spool =
        StreamingBundleReader::read_into(&bundle, &paths.directory.join("validation-spool"))
            .context("invalid Capsule bundle")?;
    let attachable = spool
        .descriptor
        .connectors
        .values()
        .any(|connector| connector.protocol.as_str() == PTY_PROTOCOL_ID);
    import_session_seed(&bundle, &paths.seed_capsule)?;
    write_bootstrap_metadata(&paths, &SessionBootstrapMetadata::default())?;

    let name_lock = public_name_lock()?;
    let name = available_public_name(&requested_name)?;
    let reservation_owner_pid = std::process::id();
    let reservation = PublicSessionMetadata {
        schema_version: PUBLIC_METADATA_SCHEMA_VERSION,
        name: name.clone(),
        source: bundle.to_string_lossy().into_owned(),
        created_at_unix_ms: now_unix_ms(),
        state: PublicMetadataState::Reserved,
        reservation_owner: Some(ProcessIdentity {
            pid: reservation_owner_pid,
            start_identity: workload_process_start_identity(reservation_owner_pid)
                .context("failed to establish public Session alias reservation identity")?,
        }),
    };
    write_public_metadata(&paths, &reservation)?;
    FileExt::unlock(&name_lock)?;

    println!("Starting {name}...");
    let workspace = paths.directory.join("workspace");
    if let Err(error) = launch_session(&session_id, &paths, &workspace, true, false) {
        release_public_reservation_if_reclaimable(&session_id, &paths)?;
        return Err(error);
    }
    if std::env::var_os("ATO_TEST_PAUSE_BEFORE_PUBLIC_METADATA_COMMIT").is_some() {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    let active = PublicSessionMetadata {
        state: PublicMetadataState::Active,
        reservation_owner: None,
        ..reservation
    };
    if let Err(error) = write_public_metadata(&paths, &active) {
        let cleanup = stop_started_session_after_metadata_failure(&session_id, &paths);
        return match cleanup {
            Ok(()) => {
                release_public_reservation(&paths);
                Err(error.context("failed to commit public Session metadata; Session stopped"))
            }
            Err(cleanup) => Err(error.context(format!(
                "failed to commit public Session metadata and failed to stop Session: {cleanup}"
            ))),
        };
    }
    println!("Ready.");
    if attachable && !detach {
        attach(session_id.as_str(), false)
    } else {
        Ok(())
    }
}

fn stop_started_session_after_metadata_failure(
    session_id: &SessionId,
    paths: &SessionPaths,
) -> Result<()> {
    if std::env::var_os("ATO_TEST_FAIL_PUBLIC_METADATA_CLEANUP").is_some() {
        bail!("injected public Session cleanup failure");
    }
    match kill(session_id.as_str()) {
        Ok(()) => Ok(()),
        Err(error) => {
            let store = CapsuleProtocolSessionStore::open(&paths.root)?;
            let stored = store.read(session_id)?;
            if stored.lifecycle == "stopped"
                && !paths.directory.join("ready-state-overlay").exists()
            {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

fn launch_session(
    session_id: &SessionId,
    paths: &SessionPaths,
    into: &Path,
    no_attach: bool,
    announce_id: bool,
) -> Result<()> {
    let executable = std::env::current_exe().context("failed to locate ato executable")?;
    let log = owner_only_log(&paths.supervisor_log)?;
    let stderr = log.try_clone().context("failed to clone Supervisor log")?;
    let mut command = Command::new(executable);
    command
        .args(["internal", "capsule-session", "serve", "--session"])
        .arg(session_id.as_str())
        .arg("--bundle")
        .arg(&paths.seed_capsule)
        .arg("--into")
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .context("failed to spawn Session Supervisor")?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll Session Supervisor")?
        {
            let log = fs::read_to_string(&paths.supervisor_log).unwrap_or_default();
            bail!("Session Supervisor exited during startup ({status}): {log}");
        }
        match request(session_id, ControlAction::Status) {
            Ok(ControlMessage::Status { lifecycle, .. }) if lifecycle == "running" => break,
            Ok(ControlMessage::Error { message }) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Session Supervisor failed readiness: {message}");
            }
            Ok(_) | Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(_) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Session Supervisor did not become ready within {READY_TIMEOUT:?}");
            }
        }
    }

    if announce_id {
        println!("{session_id}");
    }
    if no_attach {
        Ok(())
    } else {
        attach(session_id.as_str(), false)
    }
}

pub(crate) fn serve(session: &str, bundle: &Path, into: &Path) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    let spool = StreamingBundleReader::read_into(bundle, &paths.root.join("bundle-spool"))
        .context("failed to read Capsule bundle")?;
    let normalized = normalize_v1_spool(&spool, &paths.directory.join("computation-cas"))
        .context("failed to normalize Capsule v1 as a computation")?;
    let root = normalized.descriptor.root;
    match LegacyComputationEvaluator::select(&root, &spool.descriptor.base_state.state_type)? {
        LegacyRuntimeProfile::WorkspacePty => {
            let capsule = PortableCapsule {
                records: spool.records.materialize(&spool.descriptor)?,
                objects: spool.objects.materialize()?,
                descriptor: spool.descriptor,
            };
            serve_workspace_pty(session, into, capsule, root)
        }
        LegacyRuntimeProfile::ReadyState => serve_ready_state(session, into, spool, root),
    }
}

enum LegacyRuntimeProfile {
    WorkspacePty,
    ReadyState,
}

struct LegacyComputationEvaluator;

impl LegacyComputationEvaluator {
    fn select(
        computation: &ComputationRef,
        state_type: &capsule_protocol::StateTypeId,
    ) -> Result<LegacyRuntimeProfile> {
        if computation.computation_type.as_str() != LEGACY_STATE_IO_COMPUTATION_TYPE {
            bail!(
                "UnsupportedComputationType: {}",
                computation.computation_type
            );
        }
        match state_type.as_str() {
            "ato.state.workspace-posix-host@1" => Ok(LegacyRuntimeProfile::WorkspacePty),
            READY_STATE_STATE_TYPE => Ok(LegacyRuntimeProfile::ReadyState),
            other => bail!("UnsupportedStateType: {other}"),
        }
    }
}

fn serve_workspace_pty(
    session: &str,
    into: &Path,
    capsule: PortableCapsule,
    base_computation: ComputationRef,
) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    let bootstrap: SessionBootstrapMetadata =
        serde_json::from_slice(&fs::read(&paths.bootstrap).context("missing Session bootstrap")?)?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)
        .context("failed to open Supervisor lock")?;
    lock.try_lock_exclusive()
        .context("another Supervisor already owns this Session")?;

    let state = &capsule.descriptor.base_state;
    if bootstrap.resume_checkpoint.is_none() {
        let object = capsule
            .objects
            .get(&state.state_ref)
            .ok_or_else(|| anyhow!("base State object is missing"))?;
        restore_workspace_state(state, object, into)
            .context("failed to restore workspace State")?;
    }

    let mut computation = PtyComputation::spawn(into, &paths.containment_receipt)?;
    initialize_terminal(&mut computation.reader, &computation.writer)?;
    if let Some(checkpoint) = bootstrap.base_connector_checkpoints.get(PTY_CONNECTOR_ID) {
        restore_pty_checkpoint(&computation, checkpoint)?;
    }
    let historical_frontier = bootstrap.resume_checkpoint.as_ref().map_or_else(
        || {
            capsule
                .records
                .last()
                .map_or(RecordFrontier::Origin, |record| {
                    RecordFrontier::Through(record.seq)
                })
        },
        |checkpoint| checkpoint.captured_at.records_through,
    );
    let next_seq = match historical_frontier {
        RecordFrontier::Origin => 1,
        RecordFrontier::Through(seq) => {
            seq.checked_add(1).ok_or_else(|| anyhow!("seq exhausted"))?
        }
    };
    let generated = NewSupervisorIdentity::generate(
        bootstrap.generation,
        std::process::id(),
        process_start_identity(std::process::id()),
    );
    replace_secret(&paths.token, generated.secret())?;
    let identity = generated.identity;
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    let resume_rollback = if bootstrap.resume_checkpoint.is_some() {
        let mut suspended = store.read(&session_id)?;
        suspended.lifecycle = "suspended".to_owned();
        Some(ResumeStartupRollback::new(store.clone(), suspended))
    } else {
        None
    };
    let mut stored = if let Some(checkpoint) = &bootstrap.resume_checkpoint {
        let store = CapsuleProtocolSessionStore::open(&paths.root)?;
        let mut stored = store.read(&session_id)?;
        stored.lifecycle = "starting".to_owned();
        let checkpoint_state = ContentRef::parse(&checkpoint.state_ref)
            .map_err(|error| anyhow!("invalid checkpoint State: {error}"))?;
        stored.replace_with_legacy_state(&state.state_type, &checkpoint_state);
        stored.base_frontier = checkpoint.captured_at.records_through;
        stored.base_connector_checkpoints = checkpoint.connector_checkpoints.clone();
        stored.durable_frontier = SharedSessionWal::open(&paths.wal)?.durable_frontier()?;
        stored.latest_consistent_frontier = Some(checkpoint.captured_at);
        stored.active_checkpoint = Some(checkpoint.clone());
        stored.historical_replay = None;
        stored.supervisor = identity.clone();
        stored
    } else {
        let mut stored = StoredProtocolSession::new(NewStoredProtocolSession {
            session_id: session_id.clone(),
            lifecycle: "starting".to_owned(),
            base_computation: StoredComputationOrigin::Native {
                computation_type: base_computation.computation_type.to_string(),
                computation_ref: base_computation.computation_ref.to_string(),
            },
            base_frontier: bootstrap.base_frontier,
            durable_frontier: DurableFrontier {
                records_through: historical_frontier,
                journal_through: JournalLsn::ORIGIN,
            },
            runtime_profile: StoredRuntimeProfile::WorkspacePty {
                workspace: into.to_path_buf(),
            },
            supervisor: identity.clone(),
        });
        stored.base_connector_checkpoints = bootstrap.base_connector_checkpoints.clone();
        stored.source_session_id = bootstrap.source_session_id.clone();
        stored
    };
    store.write(&stored)?;
    if bootstrap.resume_checkpoint.is_some()
        && std::env::var_os("ATO_TEST_FAIL_RESUME_STARTUP").is_some()
    {
        bail!("injected resume startup failure");
    }

    if paths.socket.exists() {
        fs::remove_file(&paths.socket).context("failed to remove stale control socket")?;
    }
    let listener = UnixListener::bind(&paths.socket).context("failed to bind control socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    capsule_session_runtime::session_store::write_atomic_owner_only(
        &paths.socket_address,
        paths.socket.as_os_str().as_encoded_bytes(),
    )?;

    let (pty_tx, pty_rx) = mpsc::sync_channel(PTY_DRAIN_QUEUE_DEPTH);
    let reader = computation.take_reader()?;
    let drain_state = Arc::new(PtyDrainState::default());
    let drain_master_fd = computation.master_raw_fd()?;
    let reader_state = Arc::clone(&drain_state);
    thread::Builder::new()
        .name(format!("capsule-pty-drain-{session_id}"))
        .spawn(move || drain_pty(reader, drain_master_fd, reader_state, pty_tx))
        .context("failed to start PTY drain")?;
    if bootstrap.resume_checkpoint.is_none() {
        PtyHistoricalReplayer::new(&mut computation, &pty_rx, &drain_state).replay(
            &capsule.records,
            bootstrap.base_frontier,
            historical_frontier,
        )?;
        stored.historical_replay = Some(StoredReplayVerification {
            connector: PTY_CONNECTOR_ID.to_owned(),
            protocol: PTY_PROTOCOL_ID.to_owned(),
            from: bootstrap.base_frontier,
            through: historical_frontier,
        });
    }
    if let Some(expected) = bootstrap.expected_workspace_digest.as_deref() {
        let frozen = freeze_workload_tree(
            computation.pid,
            computation.pgid,
            &computation.process_start_identity,
        )
        .inspect_err(|_| {
            let root = ProcessTarget {
                pid: computation.pid,
                pgid: computation.pgid,
                process_start_identity: computation.process_start_identity.clone(),
            };
            let _ = checked_signal(&root, libc::SIGCONT);
        })?;
        let verification = capture_local_workspace_checkpoint(into)
            .context("failed to verify branched workspace State");
        thaw_workload_tree(&frozen)?;
        let verification = verification?;
        if verification.0.state_ref.to_string() != expected {
            bail!("BranchDiverged: replayed workspace State differs from source frontier");
        }
    }

    let wal = SharedSessionWal::open(&paths.wal)?;
    let driver = PtyBoundaryWriter {
        writer: Arc::clone(&computation.writer),
        master: Arc::clone(&computation.master),
        current_size: Arc::clone(&computation.current_size),
    };
    let mut coordinator = BoundaryCoordinator::new(wal.clone(), driver);
    let (command_tx, command_rx) = mpsc::channel();
    let auth = ControlAuth {
        session_id: session_id.clone(),
        identity: identity.clone(),
    };
    thread::Builder::new()
        .name(format!("capsule-control-{session_id}"))
        .spawn(move || accept_control(listener, auth, command_tx))
        .context("failed to start control listener")?;

    stored.lifecycle = "running".to_owned();
    store.write(&stored)?;
    if let Some(rollback) = resume_rollback {
        rollback.commit();
    }

    let result = supervisor_loop(
        &session_id,
        &store,
        &mut stored,
        &mut computation,
        &mut coordinator,
        &wal,
        command_rx,
        pty_rx,
        drain_state,
        next_seq,
    );
    if result.is_err() {
        stored.lifecycle = "failed".to_owned();
        let _ = store.write(&stored);
    }
    let _ = computation.terminate();
    let _ = fs::remove_file(&paths.socket);
    let _ = lock.unlock();
    result
}

fn serve_ready_state(
    session: &str,
    _into: &Path,
    spool: SpoolBundle,
    base_computation: ComputationRef,
) -> Result<()> {
    if !spool.descriptor.connectors.is_empty() {
        bail!("ReadyStateConnectorAttachmentUnsupported");
    }
    if spool.records.count() != 0 {
        bail!("ReadyStateHistoricalReplayUnsupported");
    }

    let session_id = SessionId::parse(session)?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)
        .context("failed to open Supervisor lock")?;
    lock.try_lock_exclusive()
        .context("another Supervisor already owns this Session")?;

    let state = spool.descriptor.base_state.clone();
    let cas_root = paths.directory.join("ready-state-cas");
    let imported = import_ready_state(&state, &spool.objects, &cas_root)
        .context("failed to import Ready-State State")?;
    drop(spool);
    let state_object = ReadyStateStateObjectV1 {
        schema: snapshot::capsule_state::READY_STATE_STATE_OBJECT_SCHEMA.to_string(),
        legacy_manifest: imported.legacy_manifest.clone(),
        snapshot_manifest: imported.snapshot_manifest.clone(),
        artifact_envelope: imported.artifact_envelope.clone(),
    };
    state_object
        .validate_f1_session_profile()
        .context("Ready-State is unsupported by the F1 Session profile")?;
    let backend = select_backend_for_ready_state(&state_object)
        .context("failed to select exact Ready-State backend")?;
    let host_runner_class = backend
        .host_runner_class()
        .context("failed to resolve actual host runner class")?;
    let overlay_root = paths.directory.join("ready-state-overlay");
    fs::create_dir_all(&overlay_root)?;
    fs::set_permissions(&overlay_root, fs::Permissions::from_mode(0o700))?;
    let containment =
        ReadyStateRestoreContainment::new(overlay_root.clone(), paths.containment_receipt.clone());
    let restore = backend
        .restore(RestoreReadyStateInput {
            store: &imported.cas_store,
            manifest: imported.legacy_manifest,
            overlay_root: overlay_root.clone(),
            host_runner_class: Some(host_runner_class),
            containment: Some(&containment),
            uffd_preview: false,
        })
        .context("Ready-State backend restore failed")?;
    let manifest_id = restore.ready_state_manifest_id.clone();
    let installed_containment = containment
        .take_for(restore.session.vmm_pid)
        .context("Ready-State restore containment mismatch")?;
    let vmm_identity = installed_containment
        .as_ref()
        .map(|installed| installed.process_start_identity.clone());
    let mut session_guard =
        ReadyStateSessionGuard::new(backend, restore.session, installed_containment);

    let generated = NewSupervisorIdentity::generate(
        1,
        std::process::id(),
        process_start_identity(std::process::id()),
    );
    replace_secret(&paths.token, generated.secret())?;
    let identity = generated.identity;
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    let mut stored = StoredProtocolSession::new(NewStoredProtocolSession {
        session_id: session_id.clone(),
        lifecycle: "starting".to_string(),
        base_computation: StoredComputationOrigin::Native {
            computation_type: base_computation.computation_type.to_string(),
            computation_ref: base_computation.computation_ref.to_string(),
        },
        base_frontier: RecordFrontier::Origin,
        durable_frontier: DurableFrontier {
            records_through: RecordFrontier::Origin,
            journal_through: JournalLsn::ORIGIN,
        },
        runtime_profile: StoredRuntimeProfile::ReadyState {
            backend_id: session_guard.session().backend_id.clone(),
            ready_state_manifest_id: manifest_id,
            cas_root,
            overlay_root: overlay_root.clone(),
            vmm_pid: session_guard.session().vmm_pid,
            vmm_process_start_identity: vmm_identity,
        },
        supervisor: identity.clone(),
    });
    store.write(&stored)?;

    if paths.socket.exists() {
        fs::remove_file(&paths.socket).context("failed to remove stale control socket")?;
    }
    let listener = UnixListener::bind(&paths.socket).context("failed to bind control socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    capsule_session_runtime::session_store::write_atomic_owner_only(
        &paths.socket_address,
        paths.socket.as_os_str().as_encoded_bytes(),
    )?;
    let (command_tx, command_rx) = mpsc::channel();
    let auth = ControlAuth {
        session_id: session_id.clone(),
        identity,
    };
    thread::Builder::new()
        .name(format!("capsule-control-{session_id}"))
        .spawn(move || accept_control(listener, auth, command_tx))
        .context("failed to start control listener")?;

    stored.lifecycle = "running".to_string();
    store.write(&stored)?;
    let result = ready_state_supervisor_loop(
        &session_id,
        &store,
        &mut stored,
        &mut session_guard,
        command_rx,
    );
    if result.is_err() {
        stored.lifecycle = "failed".to_string();
        let _ = store.write(&stored);
    }
    let _ = fs::remove_file(&paths.socket);
    let _ = lock.unlock();
    result
}

fn ready_state_supervisor_loop(
    session_id: &SessionId,
    store: &CapsuleProtocolSessionStore,
    stored: &mut StoredProtocolSession,
    session: &mut ReadyStateSessionGuard,
    commands: Receiver<SupervisorCommand>,
) -> Result<()> {
    loop {
        match commands.recv()? {
            SupervisorCommand::Status { reply } => {
                let _ = reply.send(ControlMessage::Status {
                    session_id: session_id.to_string(),
                    lifecycle: stored.lifecycle.clone(),
                    pid: stored.supervisor.pid,
                    writer_attached: false,
                    observers: 0,
                    frontier: stored.durable_frontier,
                });
            }
            SupervisorCommand::Kill { reply } => {
                stored.lifecycle = "terminating".to_string();
                store.write(stored)?;
                session.stop()?;
                stored.lifecycle = "stopped".to_string();
                if let StoredRuntimeProfile::ReadyState {
                    vmm_pid,
                    vmm_process_start_identity,
                    ..
                } = &mut stored.runtime_profile
                {
                    *vmm_pid = None;
                    *vmm_process_start_identity = None;
                }
                store.write(stored)?;
                let _ = reply.send(ControlMessage::Killed);
                return Ok(());
            }
            SupervisorCommand::Attach { reply, .. } => {
                let _ = reply.send(Err("ReadyStateConnectorAttachmentUnsupported".to_string()));
            }
            SupervisorCommand::CreateFrontier { reply } => {
                let _ = reply.send(Err("ReadyStateBranchUnsupported".to_string()));
            }
            SupervisorCommand::Suspend { reply } => {
                let _ = reply.send(Err("ReadyStateSuspendUnsupported".to_string()));
            }
            SupervisorCommand::Input { .. }
            | SupervisorCommand::Resize { .. }
            | SupervisorCommand::Detach { .. } => {}
        }
    }
}

struct ReadyStateSessionGuard {
    backend: Box<dyn SnapshotBackend>,
    restored: Option<RestoredSession>,
    containment: Option<InstalledRestoreContainment>,
}

impl ReadyStateSessionGuard {
    fn new(
        backend: Box<dyn SnapshotBackend>,
        restored: RestoredSession,
        containment: Option<InstalledRestoreContainment>,
    ) -> Self {
        Self {
            backend,
            restored: Some(restored),
            containment,
        }
    }

    fn session(&self) -> &RestoredSession {
        self.restored.as_ref().expect("restored session is present")
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(restored) = self.restored.take() {
            self.backend.stop(restored)?;
        }
        if let Some(mut containment) = self.containment.take() {
            containment.lease.write_all(WATCHDOG_DISARM)?;
            containment.lease.flush()?;
            containment.watchdog.wait()?;
        }
        Ok(())
    }
}

struct ReadyStateRestoreContainment {
    overlay_root: PathBuf,
    receipt: PathBuf,
    installed: Mutex<Option<InstalledRestoreContainment>>,
}

struct InstalledRestoreContainment {
    pid: u32,
    process_start_identity: String,
    lease: File,
    watchdog: ProcessChild,
}

impl ReadyStateRestoreContainment {
    fn new(overlay_root: PathBuf, receipt: PathBuf) -> Self {
        Self {
            overlay_root,
            receipt,
            installed: Mutex::new(None),
        }
    }

    fn take_for(&self, restored_pid: Option<i32>) -> Result<Option<InstalledRestoreContainment>> {
        let mut installed = self
            .installed
            .lock()
            .map_err(|_| anyhow!("Ready-State containment lock poisoned"))?;
        match (restored_pid, installed.as_ref()) {
            (None, None) => Ok(None),
            (Some(pid), Some(authority)) if pid > 0 && authority.pid == pid as u32 => {
                Ok(installed.take())
            }
            (Some(_), None) => bail!("backend returned a VMM without installing containment"),
            (None, Some(_)) => bail!("backend installed containment but returned no VMM"),
            (Some(pid), Some(authority)) => bail!(
                "backend returned VMM pid {pid} but installed containment for {}",
                authority.pid
            ),
        }
    }
}

impl RestoreContainment for ReadyStateRestoreContainment {
    fn install(&self, vmm_pid: u32) -> Result<(), SnapshotError> {
        let identity =
            workload_process_start_identity(vmm_pid).map_err(|error| SnapshotError::Backend {
                backend: "restore-containment".to_string(),
                reason: error.to_string(),
            })?;
        let (lease, watchdog) = spawn_watchdog(
            vmm_pid,
            0,
            &identity,
            Some(&self.overlay_root),
            Some(&self.receipt),
        )
        .map_err(|error| SnapshotError::Backend {
            backend: "restore-containment".to_string(),
            reason: error.to_string(),
        })?;
        let mut installed = self.installed.lock().map_err(|_| SnapshotError::Backend {
            backend: "restore-containment".to_string(),
            reason: "containment lock poisoned".to_string(),
        })?;
        if installed.is_some() {
            return Err(SnapshotError::Backend {
                backend: "restore-containment".to_string(),
                reason: "containment was installed more than once".to_string(),
            });
        }
        *installed = Some(InstalledRestoreContainment {
            pid: vmm_pid,
            process_start_identity: identity,
            lease,
            watchdog,
        });
        Ok(())
    }
}

impl Drop for ReadyStateSessionGuard {
    fn drop(&mut self) {
        if self.restored.is_some() {
            let _ = self.stop();
        }
    }
}

struct ResumeStartupRollback {
    store: CapsuleProtocolSessionStore,
    suspended: StoredProtocolSession,
    armed: bool,
}

impl ResumeStartupRollback {
    fn new(store: CapsuleProtocolSessionStore, suspended: StoredProtocolSession) -> Self {
        Self {
            store,
            suspended,
            armed: true,
        }
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for ResumeStartupRollback {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.store.write(&self.suspended);
        }
    }
}

pub(crate) fn attach(session: &str, observe: bool) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    let (mut stream, auth) = connect_authenticated(&session_id)?;
    write_frame(
        &mut stream,
        &ControlEnvelope {
            auth: auth.clone(),
            action: ControlAction::Attach { observe },
        },
    )?;
    match read_frame::<ControlMessage>(&mut stream)? {
        ControlMessage::Attached { writer } => {
            if !observe && !writer {
                bail!("interactive writer lease is already held");
            }
        }
        ControlMessage::Error { message } => bail!("attach rejected: {message}"),
        other => bail!("unexpected attach response: {other:?}"),
    }

    let raw = !observe && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if raw {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
    }
    let _raw_guard = RawModeGuard(raw);

    if !observe {
        if raw && let Ok((cols, rows)) = terminal_size() {
            send_action(&mut stream, &auth, ControlAction::Resize { rows, cols })?;
        }
        let mut input_stream = stream.try_clone()?;
        let input_auth = auth.clone();
        thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut byte = [0_u8; 1];
            let mut escape = false;
            while input.read_exact(&mut byte).is_ok() {
                if escape {
                    escape = false;
                    if byte[0] == 0x04 {
                        let _ = send_action(&mut input_stream, &input_auth, ControlAction::Detach);
                        break;
                    }
                    let _ = send_action(
                        &mut input_stream,
                        &input_auth,
                        ControlAction::Input {
                            bytes: vec![0x1c, byte[0]],
                        },
                    );
                } else if byte[0] == 0x1c {
                    escape = true;
                } else if send_action(
                    &mut input_stream,
                    &input_auth,
                    ControlAction::Input {
                        bytes: byte.to_vec(),
                    },
                )
                .is_err()
                {
                    break;
                }
            }
            let _ = send_action(&mut input_stream, &input_auth, ControlAction::Detach);
        });
    }

    let mut output = std::io::stdout().lock();
    loop {
        match read_frame::<ControlMessage>(&mut stream) {
            Ok(ControlMessage::Output { bytes }) => {
                output.write_all(&bytes)?;
                output.flush()?;
            }
            Ok(ControlMessage::Detached) | Err(_) => break,
            Ok(ControlMessage::Error { message }) => bail!("Session failed: {message}"),
            Ok(_) => {}
        }
    }
    Ok(())
}

pub(crate) fn status(session: &str) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    let response = request(&session_id, ControlAction::Status)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub(crate) fn kill(session: &str) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    match request(&session_id, ControlAction::Kill)? {
        ControlMessage::Killed => {
            let paths = SessionPaths::new(&session_id)?;
            let store = CapsuleProtocolSessionStore::open(&paths.root)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let stored = store.read(&session_id)?;
                if stored.lifecycle == "stopped"
                    && !capsule::state::session::process::pid_is_alive(stored.supervisor.pid)
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    bail!("Session Supervisor did not terminate after kill acknowledgement");
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        ControlMessage::Error { message } => bail!("kill rejected: {message}"),
        other => bail!("unexpected kill response: {other:?}"),
    }
}

pub(crate) fn attach_public(name: &str) -> Result<()> {
    let session = resolve_public_session(name)?;
    attach(session.session_id.as_str(), false)
}

pub(crate) fn stop_public(name: &str) -> Result<()> {
    let session = resolve_public_session(name)?;
    let metadata = public_metadata_for(&session.session_id)?;
    if session.lifecycle != "stopped" {
        if supervisor_identity_is_live(&session.supervisor) {
            kill(session.session_id.as_str())?;
        } else {
            reconcile_orphaned_session(session)?;
        }
    }
    println!("Stopped {}.", metadata.name);
    Ok(())
}

fn reconcile_orphaned_session(mut session: StoredProtocolSession) -> Result<()> {
    let paths = SessionPaths::new(&session.session_id)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)
        .context("failed to open orphaned Supervisor lock")?;
    lock.try_lock_exclusive()
        .context("Session Supervisor is still releasing its authority")?;
    if supervisor_identity_is_live(&session.supervisor) {
        bail!("Session Supervisor became live during reconciliation");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let containment_complete = match &session.runtime_profile {
            StoredRuntimeProfile::WorkspacePty { .. } => {
                has_containment_receipt(&paths.containment_receipt)
            }
            StoredRuntimeProfile::ReadyState {
                vmm_pid,
                vmm_process_start_identity,
                ..
            } => match (vmm_pid, vmm_process_start_identity) {
                (Some(pid), Some(identity)) => match workload_process_start_identity(*pid as u32) {
                    Ok(actual) => actual != *identity,
                    Err(_) => !capsule::state::session::process::pid_is_alive(*pid as u32),
                },
                (None, None) => true,
                _ => false,
            },
        };
        if containment_complete {
            break;
        }
        if Instant::now() >= deadline {
            bail!("orphaned Session containment has not proven computation termination");
        }
        thread::sleep(Duration::from_millis(25));
    }

    if let StoredRuntimeProfile::ReadyState {
        vmm_pid,
        vmm_process_start_identity,
        overlay_root,
        ..
    } = &mut session.runtime_profile
    {
        *vmm_pid = None;
        *vmm_process_start_identity = None;
        let _ = fs::remove_dir_all(overlay_root);
    }
    session.lifecycle = "stopped".to_owned();
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    store.write(&session)?;
    let _ = fs::remove_file(&paths.socket);
    let _ = fs::remove_file(&paths.socket_address);
    FileExt::unlock(&lock)?;
    Ok(())
}

fn has_containment_receipt(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| bytes == CONTAINMENT_RECEIPT)
}

pub(crate) fn branch(session: &str, into: &Path, no_attach: bool) -> Result<()> {
    eprintln!(
        "warning: workspace-posix-host + ato.io.pty@1 branch is experimental; only PTY boundary history is verified, and unmediated host external effects may be re-executed"
    );
    let source_id = SessionId::parse(session)?;
    let checkpoint = match request(&source_id, ControlAction::CreateFrontier)? {
        ControlMessage::FrontierCreated { checkpoint } => checkpoint,
        ControlMessage::Error { message } => bail!("failed to create branch frontier: {message}"),
        _ => bail!("unexpected CreateFrontier response"),
    };
    let source_paths = SessionPaths::new(&source_id)?;
    let store = CapsuleProtocolSessionStore::open(&source_paths.root)?;
    let source = store.read(&source_id)?;
    if source.latest_consistent_frontier != Some(checkpoint.captured_at) {
        bail!("source Session did not commit the requested consistent frontier");
    }

    let mut seed = PortableCapsule::read(&source_paths.seed_capsule)
        .context("failed to read source Session seed")?;
    let existing_through = seed.records.last().map_or(0, |record| record.seq);
    let through = checkpoint.captured_at.records_through;
    let recovered = capsule_session_runtime::SessionWal::open(&source_paths.wal)?.recover()?;
    for entry in recovered.entries {
        let capsule_session_runtime::WalEntry::RecordCandidate { record, .. } = entry else {
            continue;
        };
        if record.seq > existing_through
            && !source.base_frontier.contains(record.seq)
            && through.contains(record.seq)
        {
            seed.records.push(record.try_into()?);
        }
    }
    seed.records.sort_by_key(|record| record.seq);
    seed.validate().context("invalid child recovery seed")?;

    let child_id = random_session_id()?;
    let child_paths = SessionPaths::new(&child_id)?;
    child_paths.create()?;
    write_capsule_seed(&seed, &child_paths.seed_capsule)?;
    write_bootstrap_metadata(
        &child_paths,
        &SessionBootstrapMetadata {
            source_session_id: Some(source_id),
            expected_workspace_digest: Some(checkpoint.workspace_digest),
            base_frontier: source.base_frontier,
            base_connector_checkpoints: source.base_connector_checkpoints,
            ..SessionBootstrapMetadata::default()
        },
    )?;
    launch_session(
        &child_id,
        &child_paths,
        &absolute_path(into)?,
        no_attach,
        true,
    )
}

pub(crate) fn suspend(session: &str) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    match request(&session_id, ControlAction::Suspend)? {
        ControlMessage::Suspended => Ok(()),
        ControlMessage::Error { message } => bail!("failed to suspend Session: {message}"),
        _ => bail!("unexpected Suspend response"),
    }
}

pub(crate) fn resume(session: &str) -> Result<()> {
    let session_id = SessionId::parse(session)?;
    let paths = SessionPaths::new(&session_id)?;
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    let stored = store.read(&session_id)?;
    if matches!(
        &stored.runtime_profile,
        StoredRuntimeProfile::ReadyState { .. }
    ) {
        bail!("ReadyStateResumeUnsupported");
    }
    if stored.lifecycle != "suspended" {
        bail!("Session is not suspended");
    }
    let checkpoint = stored
        .active_checkpoint
        .clone()
        .ok_or_else(|| anyhow!("suspended Session has no active checkpoint"))?;
    if stored.latest_consistent_frontier != Some(checkpoint.captured_at) {
        bail!("suspended Session checkpoint is not a consistent frontier");
    }
    let (current_state, current_object) = capture_local_workspace_checkpoint(stored.workspace()?)
        .context("failed to hash suspended workspace")?;
    if current_state.state_ref.to_string() != checkpoint.workspace_digest {
        bail!("WorkspaceDrift: suspended workspace differs from its checkpoint");
    }
    let checkpoint_object = checkpoint_object_path(&paths, &checkpoint.state_ref)?;
    if fs::read(&checkpoint_object).context("failed to read local checkpoint object")?
        != current_object
    {
        bail!("local checkpoint object does not match suspended workspace");
    }

    let mut seed = PortableCapsule::read(&paths.seed_capsule)?;
    seed.descriptor.base_state.state_ref =
        capsule_protocol::ContentRef::parse(&checkpoint.state_ref)?;
    seed.records.clear();
    seed.objects
        .insert(seed.descriptor.base_state.state_ref.clone(), current_object);
    seed.prune_unreachable_objects();
    seed.validate()?;
    let previous_seed = fs::read(&paths.seed_capsule)?;
    let previous_bootstrap = fs::read(&paths.bootstrap)?;
    let previous_token = fs::read(&paths.token)?;
    replace_capsule_seed(&seed, &paths.seed_capsule)?;
    let generation = stored
        .supervisor
        .generation
        .checked_add(1)
        .ok_or_else(|| anyhow!("Supervisor generation exhausted"))?;
    replace_bootstrap_metadata(
        &paths,
        &SessionBootstrapMetadata {
            source_session_id: stored.source_session_id.clone(),
            expected_workspace_digest: None,
            generation,
            base_frontier: checkpoint.captured_at.records_through,
            base_connector_checkpoints: checkpoint.connector_checkpoints.clone(),
            resume_checkpoint: Some(checkpoint),
        },
    )?;
    match launch_session(&session_id, &paths, stored.workspace()?, true, true) {
        Ok(()) => Ok(()),
        Err(error) => {
            capsule_session_runtime::session_store::write_atomic_owner_only(
                &paths.seed_capsule,
                &previous_seed,
            )?;
            fs::set_permissions(&paths.seed_capsule, fs::Permissions::from_mode(0o400))?;
            capsule_session_runtime::session_store::write_atomic_owner_only(
                &paths.bootstrap,
                &previous_bootstrap,
            )?;
            capsule_session_runtime::session_store::write_atomic_owner_only(
                &paths.token,
                &previous_token,
            )?;
            let _ = fs::remove_file(&paths.socket);
            let _ = fs::remove_file(&paths.socket_address);
            let mut suspended = stored;
            suspended.lifecycle = "suspended".to_owned();
            store.write(&suspended)?;
            Err(error)
        }
    }
}

pub(crate) fn list() -> Result<()> {
    let root = session_root()?;
    let store = CapsuleProtocolSessionStore::open(root)?;
    for session in store.list()? {
        let lifecycle = if matches!(
            session.lifecycle.as_str(),
            "starting" | "running" | "terminating"
        ) && !supervisor_identity_is_live(&session.supervisor)
        {
            "orphaned"
        } else {
            &session.lifecycle
        };
        println!(
            "{}\t{}\t{}",
            session.session_id, lifecycle, session.supervisor.pid
        );
    }
    Ok(())
}

pub(crate) fn list_public(json: bool) -> Result<()> {
    let root = session_root()?;
    let store = CapsuleProtocolSessionStore::open(root)?;
    let now = now_unix_ms();
    let mut rows = Vec::new();
    for session in store.list()? {
        if session.lifecycle == "stopped" {
            continue;
        }
        let Ok(metadata) = reconciled_public_metadata(&session.session_id, &store) else {
            continue;
        };
        if metadata.state != PublicMetadataState::Active {
            continue;
        }
        let lifecycle = displayed_lifecycle(&session);
        rows.push(PublicSessionRow {
            name: metadata.name,
            state: lifecycle.to_owned(),
            age_seconds: now.saturating_sub(metadata.created_at_unix_ms) / 1_000,
            source: metadata.source,
            session_id: session.session_id.to_string(),
        });
    }
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("NAME\tSTATE\tAGE\tSOURCE");
    for row in rows {
        println!(
            "{}\t{}\t{}\t{}",
            sanitize_terminal_text(&row.name),
            row.state,
            format_age(row.age_seconds),
            sanitize_terminal_text(display_source(&row.source))
        );
    }
    Ok(())
}

pub(crate) fn watchdog(
    pid: u32,
    pgid: i32,
    expected_start_identity: &str,
    lease_fd: i32,
    overlay_root: Option<&Path>,
    receipt: Option<&Path>,
) -> Result<()> {
    if lease_fd < 0 {
        bail!("invalid watchdog lease fd");
    }
    let mut lease = unsafe { File::from_raw_fd(lease_fd as RawFd) };
    let mut message = Vec::new();
    lease.read_to_end(&mut message)?;
    if message == WATCHDOG_DISARM {
        return Ok(());
    }
    let targets = if pgid > 0 && workload_identity_matches(pid, pgid, expected_start_identity) {
        let targets = workload_tree(pid, pgid, expected_start_identity);
        signal_workload_tree(&targets, libc::SIGTERM);
        thread::sleep(Duration::from_millis(500));
        signal_workload_tree(&targets, libc::SIGKILL);
        targets
    } else if pgid <= 0
        && workload_process_start_identity(pid)
            .is_ok_and(|identity| identity == expected_start_identity)
    {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        thread::sleep(Duration::from_millis(500));
        if workload_process_start_identity(pid)
            .is_ok_and(|identity| identity == expected_start_identity)
        {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        vec![ProcessTarget {
            pid,
            pgid,
            process_start_identity: expected_start_identity.to_owned(),
        }]
    } else {
        Vec::new()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while targets.iter().any(|target| {
        workload_process_start_identity(target.pid)
            .is_ok_and(|identity| identity == target.process_start_identity)
    }) {
        if Instant::now() >= deadline {
            bail!("containment could not prove workload termination");
        }
        thread::sleep(Duration::from_millis(25));
    }
    if let Some(overlay_root) = overlay_root {
        let _ = fs::remove_dir_all(overlay_root);
    }
    if let Some(receipt) = receipt {
        capsule_session_runtime::session_store::write_atomic_owner_only(
            receipt,
            CONTAINMENT_RECEIPT,
        )?;
    }
    Ok(())
}

// Each argument is a distinct runtime authority. Keeping them explicit makes
// it harder to accidentally let Control IPC own the computation or WAL.
#[allow(clippy::too_many_arguments)]
fn supervisor_loop<J, D>(
    session_id: &SessionId,
    store: &CapsuleProtocolSessionStore,
    stored: &mut StoredProtocolSession,
    computation: &mut PtyComputation,
    coordinator: &mut BoundaryCoordinator<J, D>,
    wal: &SharedSessionWal,
    commands: Receiver<SupervisorCommand>,
    pty_events: Receiver<PtyEvent>,
    drain_state: Arc<PtyDrainState>,
    mut next_seq: u64,
) -> Result<()>
where
    J: capsule_session_runtime::supervisor::JournalCommit,
    D: BoundaryDriver,
{
    let mut clients: Vec<ClientRegistration> = Vec::new();
    let mut writer_client: Option<u64> = None;
    let mut next_client_id = 1_u64;
    let mut termination: Option<TerminationReason> = None;
    let mut pty_closed = false;
    let mut child_status: Option<ExitStatus> = None;
    let mut exit_committed = false;
    let mut kill_replies = Vec::new();
    let mut suspend_replies = Vec::new();

    loop {
        if let Ok(event) = pty_events.try_recv() {
            match event {
                PtyEvent::Output(mut bytes) => {
                    let mut close_after_commit = false;
                    while bytes.len() < 256 * 1024 {
                        match pty_events.recv_timeout(Duration::from_millis(1)) {
                            Ok(PtyEvent::Output(more)) => bytes.extend_from_slice(&more),
                            Ok(PtyEvent::Closed) => {
                                close_after_commit = true;
                                break;
                            }
                            Ok(PtyEvent::Failed(message)) => {
                                bail!("PTY drain failed: {message}")
                            }
                            Err(mpsc::RecvTimeoutError::Timeout)
                            | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    commit_output_bytes(
                        coordinator,
                        store,
                        stored,
                        &mut clients,
                        &mut writer_client,
                        &mut next_seq,
                        bytes,
                    )?;
                    if close_after_commit {
                        pty_closed = true;
                        termination.get_or_insert(TerminationReason::Natural);
                    }
                }
                PtyEvent::Closed => {
                    pty_closed = true;
                    termination.get_or_insert(TerminationReason::Natural);
                }
                PtyEvent::Failed(message) => bail!("PTY drain failed: {message}"),
            }
        }

        match commands.recv_timeout(Duration::from_millis(25)) {
            Ok(SupervisorCommand::Attach {
                observe,
                sender,
                reply,
            }) => {
                if termination.is_some() {
                    let _ = reply.send(Err("Session is terminating".to_owned()));
                    continue;
                }
                let writer = !observe && writer_client.is_none();
                if !observe && !writer {
                    let _ = reply.send(Err("interactive writer lease is already held".to_owned()));
                    continue;
                }
                let id = next_client_id;
                next_client_id += 1;
                if writer {
                    writer_client = Some(id);
                }
                clients.push(ClientRegistration { id, sender });
                let _ = reply.send(Ok(AttachGrant { id, writer }));
            }
            Ok(SupervisorCommand::Input { client_id, bytes }) => {
                if termination.is_some() || writer_client != Some(client_id) {
                    continue;
                }
                let record = pty_record(next_seq, Direction::Ingress, "stdin", bytes);
                coordinator
                    .deliver_ingress(random_operation_id("pty-input")?, &record)
                    .map_err(|error| anyhow!(error.to_string()))?;
                stored.durable_frontier = wal.durable_frontier()?;
                next_seq = next_seq
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("seq exhausted"))?;
                store.write(stored)?;
            }
            Ok(SupervisorCommand::Resize {
                client_id,
                rows,
                cols,
            }) => {
                if termination.is_some() || writer_client != Some(client_id) {
                    continue;
                }
                let payload = serde_json::to_vec(&ResizePayload { rows, cols })?;
                let record = pty_record(next_seq, Direction::Ingress, "resize", payload);
                let operation_id = random_operation_id("pty-resize")?;
                coordinator
                    .deliver_ingress(operation_id, &record)
                    .map_err(|error| anyhow!(error.to_string()))?;
                stored.durable_frontier = wal.durable_frontier()?;
                next_seq = next_seq
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("seq exhausted"))?;
                store.write(stored)?;
            }
            Ok(SupervisorCommand::Detach { client_id }) => {
                clients.retain(|client| client.id != client_id);
                if writer_client == Some(client_id) {
                    writer_client = None;
                }
            }
            Ok(SupervisorCommand::Status { reply }) => {
                let _ = reply.send(ControlMessage::Status {
                    session_id: session_id.to_string(),
                    lifecycle: stored.lifecycle.clone(),
                    pid: stored.supervisor.pid,
                    writer_attached: writer_client.is_some(),
                    observers: clients.len(),
                    frontier: stored.durable_frontier,
                });
            }
            Ok(SupervisorCommand::Kill { reply }) => {
                kill_replies.push(reply);
                if termination.is_none() {
                    termination = Some(TerminationReason::ControlKill);
                    stored.lifecycle = "terminating".to_owned();
                    store.write(stored)?;
                    computation.request_termination();
                }
            }
            Ok(SupervisorCommand::CreateFrontier { reply }) => {
                if termination.is_some() {
                    let _ = reply.send(Err("Session is terminating".to_owned()));
                    continue;
                }
                let result = create_consistent_checkpoint(
                    session_id,
                    computation,
                    coordinator,
                    wal,
                    store,
                    stored,
                    &pty_events,
                    &drain_state,
                    &mut clients,
                    &mut writer_client,
                    &mut next_seq,
                    true,
                )
                .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Ok(SupervisorCommand::Suspend { reply }) => {
                if termination.is_some() {
                    let _ = reply.send(Err("Session is terminating".to_owned()));
                    continue;
                }
                stored.lifecycle = "suspending".to_owned();
                store.write(stored)?;
                match create_consistent_checkpoint(
                    session_id,
                    computation,
                    coordinator,
                    wal,
                    store,
                    stored,
                    &pty_events,
                    &drain_state,
                    &mut clients,
                    &mut writer_client,
                    &mut next_seq,
                    false,
                ) {
                    Ok(_) => {
                        termination = Some(TerminationReason::Suspend);
                        suspend_replies.push(reply);
                        computation.force_termination();
                    }
                    Err(error) => {
                        stored.lifecycle = "running".to_owned();
                        store.write(stored)?;
                        let _ = reply.send(Err(error.to_string()));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if termination.is_none() {
                    termination = Some(TerminationReason::ControlChannelClosed);
                    stored.lifecycle = "terminating".to_owned();
                    store.write(stored)?;
                    computation.request_termination();
                }
            }
        }

        if child_status.is_none()
            && let Some(status) = computation.try_wait()?
        {
            child_status = Some(status);
            if termination.is_none() {
                termination = Some(TerminationReason::Natural);
                stored.lifecycle = "terminating".to_owned();
                store.write(stored)?;
            }
        }

        if pty_closed && let (Some(reason), Some(status)) = (termination, child_status.as_ref()) {
            if reason == TerminationReason::Natural && !exit_committed {
                commit_terminal_exit(coordinator, wal, store, stored, next_seq, status)?;
                exit_committed = true;
            }
            if reason != TerminationReason::Natural || exit_committed {
                break;
            }
        }
    }

    computation.disarm_watchdog()?;
    stored.lifecycle = if termination == Some(TerminationReason::Suspend) {
        "suspended".to_owned()
    } else {
        "stopped".to_owned()
    };
    store.write(stored)?;
    for reply in kill_replies {
        let _ = reply.send(ControlMessage::Killed);
    }
    for client in clients {
        let _ = client.sender.try_send(ControlMessage::Detached);
    }
    for reply in suspend_replies {
        let _ = reply.send(Ok(()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn commit_output_bytes<J, D>(
    coordinator: &mut BoundaryCoordinator<J, D>,
    store: &CapsuleProtocolSessionStore,
    stored: &mut StoredProtocolSession,
    clients: &mut Vec<ClientRegistration>,
    writer_client: &mut Option<u64>,
    next_seq: &mut u64,
    bytes: Vec<u8>,
) -> Result<()>
where
    J: capsule_session_runtime::supervisor::JournalCommit,
    D: BoundaryDriver,
{
    let record = pty_record(*next_seq, Direction::Egress, "output", bytes.clone());
    stored.durable_frontier = coordinator
        .commit_egress(random_operation_id("pty-output")?, &record)
        .map_err(|error| anyhow!(error.to_string()))?;
    *next_seq = (*next_seq)
        .checked_add(1)
        .ok_or_else(|| anyhow!("seq exhausted"))?;
    store.write(stored)?;
    clients.retain(|client| {
        match client.sender.try_send(ControlMessage::Output {
            bytes: bytes.clone(),
        }) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if *writer_client == Some(client.id) {
                    *writer_client = None;
                }
                false
            }
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_consistent_checkpoint<J, D>(
    session_id: &SessionId,
    computation: &mut PtyComputation,
    coordinator: &mut BoundaryCoordinator<J, D>,
    wal: &SharedSessionWal,
    store: &CapsuleProtocolSessionStore,
    stored: &mut StoredProtocolSession,
    pty_events: &Receiver<PtyEvent>,
    drain_state: &PtyDrainState,
    clients: &mut Vec<ClientRegistration>,
    writer_client: &mut Option<u64>,
    next_seq: &mut u64,
    resume_workload: bool,
) -> Result<StoredLocalCheckpoint>
where
    J: capsule_session_runtime::supervisor::JournalCommit,
    D: BoundaryDriver,
{
    let frozen = freeze_workload_tree(
        computation.pid,
        computation.pgid,
        &computation.process_start_identity,
    )?;
    let result = (|| {
        let master_fd = computation.master_raw_fd()?;
        loop {
            let mut observed = false;
            while let Ok(event) = pty_events.try_recv() {
                observed = true;
                match event {
                    PtyEvent::Output(bytes) => commit_output_bytes(
                        coordinator,
                        store,
                        stored,
                        clients,
                        writer_client,
                        next_seq,
                        bytes,
                    )?,
                    PtyEvent::Closed => bail!("PTY closed while creating a consistent frontier"),
                    PtyEvent::Failed(message) => bail!("PTY drain failed: {message}"),
                }
            }
            let reader_idle = !drain_state.read_in_flight.load(Ordering::SeqCst);
            if !observed && reader_idle && !poll_readable(master_fd, 10)? {
                // A second zero-time observation closes the race between the
                // reader consuming kernel bytes and publishing its queue item.
                match pty_events.try_recv() {
                    Ok(PtyEvent::Output(bytes)) => commit_output_bytes(
                        coordinator,
                        store,
                        stored,
                        clients,
                        writer_client,
                        next_seq,
                        bytes,
                    )?,
                    Ok(PtyEvent::Closed) => {
                        bail!("PTY closed while creating a consistent frontier")
                    }
                    Ok(PtyEvent::Failed(message)) => bail!("PTY drain failed: {message}"),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        bail!("PTY drain disconnected while creating a consistent frontier")
                    }
                    Err(mpsc::TryRecvError::Empty)
                        if !drain_state.read_in_flight.load(Ordering::SeqCst)
                            && !poll_readable(master_fd, 0)? =>
                    {
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }

        let frontier = wal.durable_frontier()?;
        let (state, object) = capture_local_workspace_checkpoint(stored.workspace()?)
            .context("failed to capture local workspace checkpoint")?;
        frozen.verify_root()?;
        persist_checkpoint_object(session_id, &state.state_ref.to_string(), &object)?;
        let checkpoint = StoredLocalCheckpoint {
            state_ref: state.state_ref.to_string(),
            captured_at: frontier,
            workspace_digest: state.state_ref.to_string(),
            resume_fidelity: "filesystem_restart".to_owned(),
            connector_checkpoints: BTreeMap::from([(
                PTY_CONNECTOR_ID.to_owned(),
                pty_connector_checkpoint(frontier, computation.current_terminal_size()?),
            )]),
        };
        stored.durable_frontier = frontier;
        stored.latest_consistent_frontier = Some(frontier);
        stored.active_checkpoint = Some(checkpoint.clone());
        store.write(stored)?;
        Ok(checkpoint)
    })();
    if resume_workload || result.is_err() {
        thaw_workload_tree(&frozen)?;
    }
    result
}

fn persist_checkpoint_object(session_id: &SessionId, reference: &str, bytes: &[u8]) -> Result<()> {
    let paths = SessionPaths::new(session_id)?;
    let path = checkpoint_object_path(&paths, reference)?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("checkpoint object path has no parent"))?;
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        let existing = fs::read(&path)?;
        if existing != bytes {
            bail!("checkpoint object collision for {reference}");
        }
        return Ok(());
    }
    write_owner_only(&path, bytes)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn checkpoint_object_path(paths: &SessionPaths, reference: &str) -> Result<PathBuf> {
    let (algorithm, digest) = reference
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid checkpoint ContentRef"))?;
    Ok(paths.directory.join("objects").join(algorithm).join(digest))
}

fn commit_terminal_exit<J, D>(
    coordinator: &mut BoundaryCoordinator<J, D>,
    wal: &SharedSessionWal,
    store: &CapsuleProtocolSessionStore,
    stored: &mut StoredProtocolSession,
    seq: u64,
    status: &ExitStatus,
) -> Result<()>
where
    J: capsule_session_runtime::supervisor::JournalCommit,
    D: BoundaryDriver,
{
    let payload = serde_json::to_vec(&ExitPayload {
        exit_code: status.exit_code(),
        signal: status.signal().map(str::to_owned),
        reason: "natural".to_owned(),
    })?;
    let record = pty_record(seq, Direction::Egress, "exit", payload);
    coordinator
        .commit_egress(random_operation_id("pty-exit")?, &record)
        .map_err(|error| anyhow!(error.to_string()))?;
    stored.durable_frontier = wal.durable_frontier()?;
    store.write(stored)?;
    Ok(())
}

struct PtyBoundaryWriter {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    current_size: Arc<Mutex<TerminalSize>>,
}

impl BoundaryDriver for PtyBoundaryWriter {
    type Error = std::io::Error;

    fn deliver_ingress(
        &mut self,
        _operation_id: &BoundaryOperationId,
        record: &IoRecord,
    ) -> Result<(), Self::Error> {
        let Payload::Inline(bytes) = &record.payload else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "PTY payload must be inline",
            ));
        };
        if record.kind.as_str() == "resize" {
            let size: ResizePayload = serde_json::from_slice(bytes).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
            })?;
            let master = self
                .master
                .lock()
                .map_err(|_| std::io::Error::other("PTY master lock poisoned"))?;
            master
                .resize(PtySize {
                    rows: size.rows,
                    cols: size.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            *self
                .current_size
                .lock()
                .map_err(|_| std::io::Error::other("PTY size lock poisoned"))? = size;
            return Ok(());
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("PTY writer lock poisoned"))?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    fn dispatch_effect(
        &mut self,
        _operation_id: &BoundaryOperationId,
        _intent: &capsule_session_runtime::EffectIntent,
    ) -> Result<(), Self::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "PTY has no external-effect dispatch",
        ))
    }
}

struct PtyComputation {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    current_size: Arc<Mutex<TerminalSize>>,
    pid: u32,
    pgid: i32,
    process_start_identity: String,
    lease: Option<File>,
    watchdog: Option<ProcessChild>,
}

impl PtyComputation {
    fn spawn(workspace: &Path, containment_receipt: &Path) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| anyhow!("failed to open PTY: {error}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| anyhow!("failed to clone PTY reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| anyhow!("failed to take PTY writer: {error}"))?;
        let mut command = CommandBuilder::new("/bin/sh");
        command.cwd(workspace);
        command.env("TERM", "dumb");
        command.env("NO_COLOR", "1");
        command.env("CLICOLOR", "0");
        command.env("PS1", "");
        command.env("ENV", "");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| anyhow!("failed to spawn shell: {error}"))?;
        drop(pair.slave);
        let pid = child
            .process_id()
            .ok_or_else(|| anyhow!("PTY child did not expose a process id"))?;
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        if pgid < 1 {
            bail!("PTY child did not expose a valid process group");
        }
        let process_start_identity = workload_process_start_identity(pid)?;
        let killer = child.clone_killer();
        let (lease, watchdog) = spawn_watchdog(
            pid,
            pgid,
            &process_start_identity,
            None,
            Some(containment_receipt),
        )?;
        Ok(Self {
            master: Arc::new(Mutex::new(pair.master)),
            child,
            killer,
            reader: Some(reader),
            writer: Arc::new(Mutex::new(writer)),
            current_size: Arc::new(Mutex::new(TerminalSize {
                rows: 24,
                cols: 120,
            })),
            pid,
            pgid,
            process_start_identity,
            lease: Some(lease),
            watchdog: Some(watchdog),
        })
    }

    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .ok_or_else(|| anyhow!("PTY reader already taken"))
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().context("failed to poll shell")
    }

    fn write_input(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("PTY writer lock poisoned"))?;
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let master = self
            .master
            .lock()
            .map_err(|_| anyhow!("PTY master lock poisoned"))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| anyhow!("failed to resize PTY: {error}"))?;
        *self
            .current_size
            .lock()
            .map_err(|_| anyhow!("PTY size lock poisoned"))? = TerminalSize { rows, cols };
        Ok(())
    }

    fn master_raw_fd(&self) -> Result<RawFd> {
        let master = self
            .master
            .lock()
            .map_err(|_| anyhow!("PTY master lock poisoned"))?;
        master
            .as_raw_fd()
            .ok_or_else(|| anyhow!("PTY master does not expose a Unix file descriptor"))
    }

    fn current_terminal_size(&self) -> Result<TerminalSize> {
        self.current_size
            .lock()
            .map(|size| *size)
            .map_err(|_| anyhow!("PTY size lock poisoned"))
    }

    fn request_termination(&mut self) {
        if workload_identity_matches(self.pid, self.pgid, &self.process_start_identity) {
            signal_workload(self.pid, self.pgid, libc::SIGTERM);
        }
        let _ = self.killer.kill();
    }

    fn force_termination(&mut self) {
        let targets = workload_tree(self.pid, self.pgid, &self.process_start_identity);
        signal_workload_tree(&targets, libc::SIGKILL);
        let _ = self.killer.kill();
    }

    fn disarm_watchdog(&mut self) -> Result<()> {
        if let Some(mut lease) = self.lease.take() {
            lease
                .write_all(WATCHDOG_DISARM)
                .context("failed to disarm workload watchdog")?;
            lease.flush().context("failed to flush watchdog DISARM")?;
            drop(lease);
        }
        if let Some(mut watchdog) = self.watchdog.take() {
            watchdog
                .wait()
                .context("failed to reap workload watchdog")?;
        }
        Ok(())
    }

    fn terminate(&mut self) -> Result<()> {
        self.request_termination();
        let _ = self.child.wait();
        self.disarm_watchdog()?;
        Ok(())
    }
}

fn pty_connector_checkpoint(
    applied_at: DurableFrontier,
    size: TerminalSize,
) -> StoredConnectorCheckpoint {
    StoredConnectorCheckpoint {
        protocol_id: PTY_PROTOCOL_ID.to_owned(),
        applied_at,
        format: PTY_CHECKPOINT_FORMAT.to_owned(),
        payload: serde_json::to_value(size).expect("TerminalSize is JSON serializable"),
    }
}

fn restore_pty_checkpoint(
    computation: &PtyComputation,
    checkpoint: &StoredConnectorCheckpoint,
) -> Result<()> {
    if checkpoint.protocol_id != PTY_PROTOCOL_ID || checkpoint.format != PTY_CHECKPOINT_FORMAT {
        bail!("unsupported PTY Connector checkpoint");
    }
    let size: TerminalSize = serde_json::from_value(checkpoint.payload.clone())?;
    computation.resize(size.rows, size.cols)
}

fn spawn_watchdog(
    pid: u32,
    pgid: i32,
    process_start_identity: &str,
    overlay_root: Option<&Path>,
    receipt: Option<&Path>,
) -> Result<(File, ProcessChild)> {
    let mut fds = [0_i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(std::io::Error::last_os_error()).context("failed to create watchdog pipe");
    }
    let read_fd = fds[0];
    let write_fd = fds[1];
    if unsafe { libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(std::io::Error::last_os_error()).context("failed to protect watchdog lease");
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.args([
        "internal",
        "capsule-session",
        "watchdog",
        "--pid",
        &pid.to_string(),
        "--pgid",
        &pgid.to_string(),
        "--process-start-identity",
        process_start_identity,
        "--lease-fd",
        &read_fd.to_string(),
    ]);
    if let Some(overlay_root) = overlay_root {
        command.arg("--overlay-root").arg(overlay_root);
    }
    if let Some(receipt) = receipt {
        match fs::remove_file(receipt) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to clear containment receipt"),
        }
        command.arg("--receipt").arg(receipt);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn workload watchdog")?;
    unsafe { libc::close(read_fd) };
    let lease = unsafe { File::from_raw_fd(write_fd) };
    Ok((lease, child))
}

fn initialize_terminal(
    reader: &mut Option<Box<dyn Read + Send>>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
) -> Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| anyhow!("PTY writer lock poisoned"))?;
    writer.write_all(b"stty -echo\nprintf '__ATO_SESSION_%s__\\n' 'READY'\n")?;
    writer.flush()?;
    drop(writer);
    let reader = reader
        .as_mut()
        .ok_or_else(|| anyhow!("PTY reader missing"))?;
    let _ = read_until_marker(reader, READY_MARKER)?;
    Ok(())
}

struct PtyHistoricalReplayer<'a> {
    computation: &'a mut PtyComputation,
    events: &'a Receiver<PtyEvent>,
    drain_state: &'a PtyDrainState,
    output: PtyOutputVerifier,
}

impl<'a> PtyHistoricalReplayer<'a> {
    fn new(
        computation: &'a mut PtyComputation,
        events: &'a Receiver<PtyEvent>,
        drain_state: &'a PtyDrainState,
    ) -> Self {
        Self {
            computation,
            events,
            drain_state,
            output: PtyOutputVerifier::default(),
        }
    }

    fn replay(
        mut self,
        records: &[IoRecord],
        from: RecordFrontier,
        through: RecordFrontier,
    ) -> Result<()> {
        for record in records
            .iter()
            .filter(|record| from.replay_contains(through, record.seq))
        {
            if record.connector.as_str() != PTY_CONNECTOR_ID {
                bail!("unsupported historical Connector at seq {}", record.seq);
            }
            let Payload::Inline(payload) = &record.payload else {
                bail!(
                    "historical PTY payload must be inline at seq {}",
                    record.seq
                );
            };
            match (record.direction, record.kind.as_str()) {
                (Direction::Ingress, "stdin") => self.computation.write_input(payload)?,
                (Direction::Ingress, "resize") => {
                    let resize: ResizePayload = serde_json::from_slice(payload)?;
                    self.computation.resize(resize.rows, resize.cols)?;
                }
                (Direction::Egress, "output") => {
                    while self.output.available() < payload.len() {
                        match self.events.recv_timeout(Duration::from_secs(30)) {
                            Ok(PtyEvent::Output(bytes)) => self.output.push(&bytes),
                            Ok(PtyEvent::Closed) => {
                                bail!("PTY closed during historical output at seq {}", record.seq)
                            }
                            Ok(PtyEvent::Failed(message)) => {
                                bail!("PTY replay drain failed: {message}")
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                bail!("historical PTY replay timed out at seq {}", record.seq)
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                bail!("PTY replay drain disconnected at seq {}", record.seq)
                            }
                        }
                    }
                    self.output.consume(record.seq, payload)?;
                }
                (Direction::Egress, "exit") => {
                    let expected: ExitPayload = serde_json::from_slice(payload)?;
                    let status = self.wait_for_exit(record.seq)?;
                    if status.exit_code() != expected.exit_code
                        || status.signal() != expected.signal.as_deref()
                    {
                        bail!("historical PTY replay diverged at seq {}", record.seq);
                    }
                }
                _ => bail!("unsupported historical PTY record at seq {}", record.seq),
            }
        }
        self.drain_actual_output_to_quiescence()?;
        if self.output.available() != 0 {
            let seq = match through {
                RecordFrontier::Origin => 0,
                RecordFrontier::Through(seq) => seq,
            };
            bail!("historical PTY replay diverged after seq {seq}");
        }
        Ok(())
    }

    fn drain_actual_output_to_quiescence(&mut self) -> Result<()> {
        let master_fd = self.computation.master_raw_fd()?;
        loop {
            while let Ok(event) = self.events.try_recv() {
                match event {
                    PtyEvent::Output(bytes) => self.output.push(&bytes),
                    PtyEvent::Closed => {}
                    PtyEvent::Failed(message) => bail!("PTY replay drain failed: {message}"),
                }
            }
            if !self.drain_state.read_in_flight.load(Ordering::SeqCst)
                && !poll_readable(master_fd, 10)?
            {
                match self.events.try_recv() {
                    Ok(PtyEvent::Output(bytes)) => self.output.push(&bytes),
                    Ok(PtyEvent::Closed) => {}
                    Ok(PtyEvent::Failed(message)) => bail!("PTY replay drain failed: {message}"),
                    Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty)
                        if !self.drain_state.read_in_flight.load(Ordering::SeqCst)
                            && !poll_readable(master_fd, 0)? =>
                    {
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
        Ok(())
    }

    fn wait_for_exit(&mut self, seq: u64) -> Result<ExitStatus> {
        loop {
            if let Some(status) = self.computation.try_wait()? {
                return Ok(status);
            }
            match self.events.recv_timeout(Duration::from_secs(30)) {
                Ok(PtyEvent::Output(bytes)) => self.output.push(&bytes),
                Ok(PtyEvent::Closed) => {}
                Ok(PtyEvent::Failed(message)) => bail!("PTY replay drain failed: {message}"),
                Err(_) => bail!("historical PTY exit timed out at seq {seq}"),
            }
            if self.output.available() != 0 {
                bail!("historical PTY replay diverged at seq {seq}");
            }
        }
    }
}

#[derive(Default)]
struct PtyOutputVerifier {
    buffered: Vec<u8>,
    consumed: usize,
}

impl PtyOutputVerifier {
    fn push(&mut self, bytes: &[u8]) {
        self.buffered.extend_from_slice(bytes);
    }

    fn available(&self) -> usize {
        self.buffered.len().saturating_sub(self.consumed)
    }

    fn consume(&mut self, seq: u64, expected: &[u8]) -> Result<()> {
        let end = self
            .consumed
            .checked_add(expected.len())
            .ok_or_else(|| anyhow!("PTY replay buffer overflow"))?;
        if self.buffered.get(self.consumed..end) != Some(expected) {
            bail!("historical PTY replay diverged at seq {seq}");
        }
        self.consumed = end;
        if self.consumed == self.buffered.len() {
            self.buffered.clear();
            self.consumed = 0;
        }
        Ok(())
    }
}

fn read_until_marker(reader: &mut dyn Read, marker: &[u8]) -> Result<Vec<u8>> {
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            bail!("PTY closed before replay marker");
        }
        collected.extend_from_slice(&buffer[..count]);
        if let Some(index) = find_subslice(&collected, marker) {
            collected.truncate(index);
            return Ok(collected);
        }
    }
}

fn find_subslice(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

fn drain_pty(
    mut reader: Box<dyn Read + Send>,
    master_fd: RawFd,
    state: Arc<PtyDrainState>,
    sender: SyncSender<PtyEvent>,
) {
    let mut buffer = [0_u8; 8192];
    loop {
        match poll_readable(master_fd, 100) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                let _ = sender.send(PtyEvent::Failed(error.to_string()));
                return;
            }
        }
        state.read_in_flight.store(true, Ordering::SeqCst);
        match reader.read(&mut buffer) {
            Ok(0) => {
                state.read_in_flight.store(false, Ordering::SeqCst);
                let _ = sender.send(PtyEvent::Closed);
                return;
            }
            Ok(count) => {
                if sender
                    .send(PtyEvent::Output(buffer[..count].to_vec()))
                    .is_err()
                {
                    state.read_in_flight.store(false, Ordering::SeqCst);
                    return;
                }
                state.read_in_flight.store(false, Ordering::SeqCst);
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                state.read_in_flight.store(false, Ordering::SeqCst);
                let _ = sender.send(PtyEvent::Closed);
                return;
            }
            Err(error) => {
                state.read_in_flight.store(false, Ordering::SeqCst);
                let _ = sender.send(PtyEvent::Failed(error.to_string()));
                return;
            }
        }
    }
}

fn poll_readable(fd: RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}

fn accept_control(
    listener: UnixListener,
    auth: ControlAuth,
    sender: mpsc::Sender<SupervisorCommand>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let auth = auth.clone();
        let sender = sender.clone();
        thread::spawn(move || {
            let _ = handle_control(stream, &auth, &sender);
        });
    }
}

fn handle_control(
    mut stream: UnixStream,
    auth: &ControlAuth,
    sender: &mpsc::Sender<SupervisorCommand>,
) -> Result<()> {
    let envelope: ControlEnvelope = read_frame(&mut stream)?;
    authorize(auth, &envelope.auth)?;
    match envelope.action {
        ControlAction::Status => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            sender.send(SupervisorCommand::Status { reply: reply_tx })?;
            write_frame(&mut stream, &reply_rx.recv()?)?;
        }
        ControlAction::Kill => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            sender.send(SupervisorCommand::Kill { reply: reply_tx })?;
            write_frame(&mut stream, &reply_rx.recv()?)?;
        }
        ControlAction::CreateFrontier => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            sender.send(SupervisorCommand::CreateFrontier { reply: reply_tx })?;
            match reply_rx.recv()? {
                Ok(checkpoint) => {
                    write_frame(&mut stream, &ControlMessage::FrontierCreated { checkpoint })?
                }
                Err(message) => write_frame(&mut stream, &ControlMessage::Error { message })?,
            }
        }
        ControlAction::Suspend => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            sender.send(SupervisorCommand::Suspend { reply: reply_tx })?;
            match reply_rx.recv()? {
                Ok(()) => write_frame(&mut stream, &ControlMessage::Suspended)?,
                Err(message) => write_frame(&mut stream, &ControlMessage::Error { message })?,
            }
        }
        ControlAction::Attach { observe } => {
            let (out_tx, out_rx) = mpsc::sync_channel(CLIENT_QUEUE_DEPTH);
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            sender.send(SupervisorCommand::Attach {
                observe,
                sender: out_tx,
                reply: reply_tx,
            })?;
            let grant = match reply_rx.recv()? {
                Ok(grant) => grant,
                Err(message) => {
                    write_frame(&mut stream, &ControlMessage::Error { message })?;
                    return Ok(());
                }
            };
            write_frame(
                &mut stream,
                &ControlMessage::Attached {
                    writer: grant.writer,
                },
            )?;
            let mut output = stream.try_clone()?;
            thread::spawn(move || {
                while let Ok(message) = out_rx.recv() {
                    if write_frame(&mut output, &message).is_err() {
                        break;
                    }
                }
                let _ = output.shutdown(std::net::Shutdown::Write);
            });
            loop {
                let envelope = match read_frame::<ControlEnvelope>(&mut stream) {
                    Ok(envelope) => envelope,
                    Err(_) => break,
                };
                if authorize(auth, &envelope.auth).is_err() {
                    break;
                }
                match envelope.action {
                    ControlAction::Input { bytes } if grant.writer => {
                        sender.send(SupervisorCommand::Input {
                            client_id: grant.id,
                            bytes,
                        })?;
                    }
                    ControlAction::Resize { rows, cols } if grant.writer => {
                        sender.send(SupervisorCommand::Resize {
                            client_id: grant.id,
                            rows,
                            cols,
                        })?;
                    }
                    ControlAction::Detach => break,
                    _ => {}
                }
            }
            let _ = sender.send(SupervisorCommand::Detach {
                client_id: grant.id,
            });
        }
        _ => write_frame(
            &mut stream,
            &ControlMessage::Error {
                message: "action requires an attached channel".to_owned(),
            },
        )?,
    }
    Ok(())
}

fn authorize(expected: &ControlAuth, presented: &ControlCredentials) -> Result<()> {
    if presented.session_id != expected.session_id {
        bail!("control request targets another Session");
    }
    expected.identity.authorize(
        &presented.token,
        presented.generation,
        &presented.incarnation_nonce,
        presented.supervisor_pid,
        &presented.process_start_identity,
    )?;
    Ok(())
}

fn request(session_id: &SessionId, action: ControlAction) -> Result<ControlMessage> {
    let (mut stream, auth) = connect_authenticated(session_id)?;
    write_frame(&mut stream, &ControlEnvelope { auth, action })?;
    read_frame(&mut stream)
}

fn connect_authenticated(session_id: &SessionId) -> Result<(UnixStream, ControlCredentials)> {
    let paths = SessionPaths::new(session_id)?;
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    let stored = store.read(session_id)?;
    let token = fs::read(&paths.token).context("failed to read Session control token")?;
    let socket = fs::read(&paths.socket_address)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(PathBuf::from)
        .unwrap_or(paths.socket);
    let stream = UnixStream::connect(socket).context("failed to connect Session control")?;
    Ok((
        stream,
        ControlCredentials {
            session_id: session_id.clone(),
            generation: stored.supervisor.generation,
            incarnation_nonce: stored.supervisor.incarnation_nonce.clone(),
            supervisor_pid: stored.supervisor.pid,
            process_start_identity: stored.supervisor.process_start_identity.clone(),
            token,
        },
    ))
}

fn send_action(
    stream: &mut UnixStream,
    auth: &ControlCredentials,
    action: ControlAction,
) -> Result<()> {
    write_frame(
        stream,
        &ControlEnvelope {
            auth: auth.clone(),
            action,
        },
    )
}

fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > CONTROL_FRAME_LIMIT {
        bail!("invalid control frame size {}", bytes.len());
    }
    let length = u32::try_from(bytes.len()).context("control frame too large")?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > CONTROL_FRAME_LIMIT {
        bail!("invalid control frame length {length}");
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).context("malformed control frame")
}

#[derive(Clone, Serialize, Deserialize)]
struct ControlCredentials {
    session_id: SessionId,
    generation: u64,
    incarnation_nonce: String,
    supervisor_pid: u32,
    process_start_identity: String,
    token: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ControlEnvelope {
    auth: ControlCredentials,
    action: ControlAction,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method", content = "params")]
enum ControlAction {
    Status,
    CreateFrontier,
    Suspend,
    Attach { observe: bool },
    Input { bytes: Vec<u8> },
    Resize { rows: u16, cols: u16 },
    Detach,
    Kill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
enum ControlMessage {
    Status {
        session_id: String,
        lifecycle: String,
        pid: u32,
        writer_attached: bool,
        observers: usize,
        frontier: DurableFrontier,
    },
    Attached {
        writer: bool,
    },
    Output {
        bytes: Vec<u8>,
    },
    Detached,
    Killed,
    FrontierCreated {
        checkpoint: StoredLocalCheckpoint,
    },
    Suspended,
    Error {
        message: String,
    },
}

#[derive(Clone)]
struct ControlAuth {
    session_id: SessionId,
    identity: SupervisorIdentity,
}

enum SupervisorCommand {
    Attach {
        observe: bool,
        sender: SyncSender<ControlMessage>,
        reply: SyncSender<Result<AttachGrant, String>>,
    },
    Input {
        client_id: u64,
        bytes: Vec<u8>,
    },
    Resize {
        client_id: u64,
        rows: u16,
        cols: u16,
    },
    Detach {
        client_id: u64,
    },
    Status {
        reply: SyncSender<ControlMessage>,
    },
    Kill {
        reply: SyncSender<ControlMessage>,
    },
    CreateFrontier {
        reply: SyncSender<Result<StoredLocalCheckpoint, String>>,
    },
    Suspend {
        reply: SyncSender<Result<(), String>>,
    },
}

struct AttachGrant {
    id: u64,
    writer: bool,
}

struct ClientRegistration {
    id: u64,
    sender: SyncSender<ControlMessage>,
}

enum PtyEvent {
    Output(Vec<u8>),
    Closed,
    Failed(String),
}

#[derive(Default)]
struct PtyDrainState {
    read_in_flight: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminationReason {
    Natural,
    ControlKill,
    ControlChannelClosed,
    Suspend,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct TerminalSize {
    rows: u16,
    cols: u16,
}

type ResizePayload = TerminalSize;

#[derive(Serialize, Deserialize)]
struct ExitPayload {
    exit_code: u32,
    signal: Option<String>,
    reason: String,
}

struct RawModeGuard(bool);

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = disable_raw_mode();
        }
    }
}

struct SessionPaths {
    root: PathBuf,
    directory: PathBuf,
    control: PathBuf,
    socket: PathBuf,
    socket_address: PathBuf,
    token: PathBuf,
    lock: PathBuf,
    wal: PathBuf,
    seed_capsule: PathBuf,
    bootstrap: PathBuf,
    supervisor_log: PathBuf,
    containment_receipt: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct PublicSessionMetadata {
    schema_version: u16,
    name: String,
    source: String,
    created_at_unix_ms: u64,
    state: PublicMetadataState,
    reservation_owner: Option<ProcessIdentity>,
}

#[derive(Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PublicMetadataState {
    Reserved,
    Active,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProcessIdentity {
    pid: u32,
    start_identity: String,
}

#[derive(Serialize)]
struct PublicSessionRow {
    name: String,
    state: String,
    age_seconds: u64,
    source: String,
    session_id: String,
}

impl SessionPaths {
    fn new(session_id: &SessionId) -> Result<Self> {
        let root = session_root()?;
        let directory = root.join(session_id.as_str());
        let control = directory.join("control");
        let preferred_socket = control.join("control.sock");
        let socket = if preferred_socket.as_os_str().as_encoded_bytes().len() < 96 {
            preferred_socket
        } else {
            short_socket_path(&root, session_id)?
        };
        Ok(Self {
            root,
            socket,
            socket_address: control.join("socket-address"),
            token: control.join("token"),
            lock: control.join("supervisor.lock"),
            wal: directory.join("journal").join("wal-000001"),
            seed_capsule: directory.join("seed").join("source.capsule.local"),
            bootstrap: control.join("bootstrap.json"),
            supervisor_log: directory.join("logs").join("supervisor.log"),
            containment_receipt: control.join("containment-revoked"),
            directory,
            control,
        })
    }

    fn create(&self) -> Result<()> {
        for directory in [
            &self.root,
            &self.directory,
            &self.control,
            &self.directory.join("journal"),
            &self.directory.join("seed"),
            &self.directory.join("objects"),
            &self.directory.join("logs"),
        ] {
            fs::create_dir_all(directory)?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

fn write_public_metadata(paths: &SessionPaths, metadata: &PublicSessionMetadata) -> Result<()> {
    validate_public_metadata(metadata)?;
    if metadata.state == PublicMetadataState::Active
        && std::env::var_os("ATO_TEST_FAIL_PUBLIC_METADATA_COMMIT").is_some()
    {
        bail!("injected public Session metadata commit failure");
    }
    capsule_session_runtime::session_store::write_atomic_owner_only(
        &paths.directory.join("display.json"),
        &serde_json::to_vec_pretty(metadata)?,
    )?;
    Ok(())
}

fn public_metadata_for(session_id: &SessionId) -> Result<PublicSessionMetadata> {
    let paths = SessionPaths::new(session_id)?;
    let metadata: PublicSessionMetadata = serde_json::from_slice(
        &fs::read(paths.directory.join("display.json"))
            .context("failed to read public Session metadata")?,
    )
    .context("invalid public Session metadata")?;
    validate_public_metadata(&metadata)?;
    Ok(metadata)
}

fn validate_public_metadata(metadata: &PublicSessionMetadata) -> Result<()> {
    if metadata.schema_version != PUBLIC_METADATA_SCHEMA_VERSION {
        bail!(
            "unsupported public Session metadata schema {}",
            metadata.schema_version
        );
    }
    validate_public_name(&metadata.name)?;
    if metadata.source.trim().is_empty() {
        bail!("public Session source is empty");
    }
    match (metadata.state, &metadata.reservation_owner) {
        (PublicMetadataState::Reserved, Some(owner))
            if owner.pid > 0 && !owner.start_identity.is_empty() => {}
        (PublicMetadataState::Active, None) => {}
        _ => bail!("public Session reservation state is invalid"),
    }
    Ok(())
}

fn validate_public_name(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > PUBLIC_NAME_MAX_LEN
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("Session name must be 1..={PUBLIC_NAME_MAX_LEN} ASCII alphanumeric, '-' or '_'");
    }
    Ok(name.to_owned())
}

fn public_name_from_bundle(bundle: &Path) -> String {
    let stem = bundle
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("capsule");
    let mut name = String::new();
    let mut separator = false;
    for byte in stem.bytes() {
        let byte = byte.to_ascii_lowercase();
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            if separator && !name.is_empty() {
                name.push('-');
            }
            separator = false;
            name.push(char::from(byte));
        } else {
            separator = true;
        }
        if name.len() == PUBLIC_NAME_MAX_LEN {
            break;
        }
    }
    while name.ends_with('-') {
        name.pop();
    }
    if name.is_empty() {
        "capsule".to_owned()
    } else {
        name
    }
}

fn available_public_name(base: &str) -> Result<String> {
    let root = session_root()?;
    let store = CapsuleProtocolSessionStore::open(&root)?;
    let mut used = BTreeSet::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(session_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| SessionId::parse(name).ok())
        else {
            continue;
        };
        let display_path = entry.path().join("display.json");
        if !display_path.exists() {
            continue;
        }
        let metadata = reconciled_public_metadata(&session_id, &store)
            .with_context(|| format!("invalid public Session metadata for {session_id}"))?;
        let owns_alias = store
            .read(&session_id)
            .map_or(true, |session| session.lifecycle != "stopped");
        if owns_alias {
            used.insert(metadata.name);
        }
    }
    if !used.contains(base) {
        return Ok(base.to_owned());
    }
    for suffix in 2_u64.. {
        let suffix = format!("-{suffix}");
        let keep = PUBLIC_NAME_MAX_LEN.saturating_sub(suffix.len());
        let candidate = format!("{}{}", &base[..base.len().min(keep)], suffix);
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("the numeric Session name suffix space is unbounded")
}

fn public_name_lock() -> Result<File> {
    let root = session_root()?;
    CapsuleProtocolSessionStore::open(&root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root.join("public-names.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn resolve_public_session(name: &str) -> Result<StoredProtocolSession> {
    let root = session_root()?;
    let store = CapsuleProtocolSessionStore::open(root)?;
    for session in store.list()? {
        if session.lifecycle == "stopped" {
            continue;
        }
        if reconciled_public_metadata(&session.session_id, &store).is_ok_and(|metadata| {
            metadata.state == PublicMetadataState::Active && metadata.name == name
        }) {
            return Ok(session);
        }
    }
    bail!("Capsule Session not found: {name}")
}

fn displayed_lifecycle(session: &StoredProtocolSession) -> &str {
    if matches!(
        session.lifecycle.as_str(),
        "starting" | "running" | "terminating"
    ) && !supervisor_identity_is_live(&session.supervisor)
    {
        "orphaned"
    } else {
        &session.lifecycle
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn format_age(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn display_source(source: &str) -> &str {
    Path::new(source)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(source)
}

fn sanitize_terminal_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f) {
            sanitized.extend(character.escape_default());
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

fn reconciled_public_metadata(
    session_id: &SessionId,
    store: &CapsuleProtocolSessionStore,
) -> Result<PublicSessionMetadata> {
    let metadata = public_metadata_for(session_id)?;
    if metadata.state != PublicMetadataState::Reserved {
        return Ok(metadata);
    }
    let Ok(session) = store.read(session_id) else {
        // A dead launcher does not prove that a detached Supervisor was never
        // spawned. Preserve uncertain reservations until terminal state is
        // positively established.
        return Ok(metadata);
    };
    if session.lifecycle == "stopped" {
        return Ok(metadata);
    }
    let active = PublicSessionMetadata {
        state: PublicMetadataState::Active,
        reservation_owner: None,
        ..metadata
    };
    let paths = SessionPaths::new(session_id)?;
    // Recovery remains usable when the durable promotion cannot be written:
    // the on-disk Reserved record still owns the alias fail-closed, while
    // list/stop may expose and recover the associated non-terminal Session.
    let _ = write_public_metadata(&paths, &active);
    Ok(active)
}

fn release_public_reservation(paths: &SessionPaths) {
    let _ = fs::remove_file(paths.directory.join("display.json"));
}

fn release_public_reservation_if_reclaimable(
    session_id: &SessionId,
    paths: &SessionPaths,
) -> Result<()> {
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    match store.read(session_id) {
        Ok(session) if session.lifecycle != "stopped" => {}
        Ok(_) => {
            release_public_reservation(paths);
        }
        Err(capsule_session_runtime::SessionStoreError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            release_public_reservation(paths);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct SessionBootstrapMetadata {
    source_session_id: Option<SessionId>,
    expected_workspace_digest: Option<String>,
    #[serde(default = "initial_supervisor_generation")]
    generation: u64,
    resume_checkpoint: Option<StoredLocalCheckpoint>,
    #[serde(default)]
    base_frontier: RecordFrontier,
    #[serde(default = "default_pty_connector_checkpoints")]
    base_connector_checkpoints: BTreeMap<String, StoredConnectorCheckpoint>,
}

fn initial_supervisor_generation() -> u64 {
    1
}

impl Default for SessionBootstrapMetadata {
    fn default() -> Self {
        Self {
            source_session_id: None,
            expected_workspace_digest: None,
            generation: initial_supervisor_generation(),
            resume_checkpoint: None,
            base_frontier: RecordFrontier::Origin,
            base_connector_checkpoints: default_pty_connector_checkpoints(),
        }
    }
}

fn default_pty_connector_checkpoints() -> BTreeMap<String, StoredConnectorCheckpoint> {
    BTreeMap::from([(
        PTY_CONNECTOR_ID.to_owned(),
        pty_connector_checkpoint(
            DurableFrontier {
                records_through: RecordFrontier::Origin,
                journal_through: JournalLsn::ORIGIN,
            },
            TerminalSize {
                rows: 24,
                cols: 120,
            },
        ),
    )])
}

fn write_bootstrap_metadata(
    paths: &SessionPaths,
    metadata: &SessionBootstrapMetadata,
) -> Result<()> {
    write_owner_only(&paths.bootstrap, &serde_json::to_vec_pretty(metadata)?)
}

fn replace_bootstrap_metadata(
    paths: &SessionPaths,
    metadata: &SessionBootstrapMetadata,
) -> Result<()> {
    capsule_session_runtime::session_store::write_atomic_owner_only(
        &paths.bootstrap,
        &serde_json::to_vec_pretty(metadata)?,
    )
    .map_err(Into::into)
}

fn import_session_seed(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)
        .with_context(|| format!("failed to open Session seed {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .with_context(|| format!("failed to create Session seed {}", destination.display()))?;
    std::io::copy(&mut input, &mut output).context("failed to import Session seed")?;
    output
        .set_permissions(fs::Permissions::from_mode(0o400))
        .context("failed to make Session seed immutable")?;
    output.sync_all().context("failed to sync Session seed")?;
    let seed_directory = destination
        .parent()
        .ok_or_else(|| anyhow!("Session seed path has no parent"))?;
    File::open(seed_directory)
        .and_then(|directory| directory.sync_all())
        .context("failed to sync Session seed directory")?;
    Ok(())
}

fn write_capsule_seed(capsule: &PortableCapsule, destination: &Path) -> Result<()> {
    capsule
        .write(destination)
        .context("failed to write child Session seed")?;
    let file = File::open(destination)?;
    file.set_permissions(fs::Permissions::from_mode(0o400))?;
    file.sync_all()?;
    File::open(
        destination
            .parent()
            .ok_or_else(|| anyhow!("Session seed path has no parent"))?,
    )?
    .sync_all()?;
    Ok(())
}

fn replace_capsule_seed(capsule: &PortableCapsule, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("Session seed path has no parent"))?;
    let temporary = parent.join(".source.capsule.local.new");
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    write_capsule_seed(capsule, &temporary)?;
    fs::rename(&temporary, destination)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn session_root() -> Result<PathBuf> {
    Ok(capsule::config::config_dir()?.join("capsule-protocol-sessions"))
}

fn owner_only_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))
}

fn replace_secret(path: &Path, secret: &[u8]) -> Result<()> {
    capsule_session_runtime::session_store::write_atomic_owner_only(path, secret)
        .map_err(Into::into)
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create owner-only file {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn short_socket_path(root: &Path, session_id: &SessionId) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    let runtime_root = {
        let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        xdg.filter(|path| path.is_dir())
            .unwrap_or_else(|| PathBuf::from("/dev/shm"))
    };
    #[cfg(not(target_os = "linux"))]
    let runtime_root = std::env::temp_dir();

    if runtime_root == Path::new("/tmp") || runtime_root == Path::new("/var/tmp") {
        bail!("no safe short runtime directory is available for the control socket");
    }
    let directory = runtime_root.join(format!("ato-cs-{}", unsafe { libc::geteuid() }));
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!(
                "unsafe Capsule Session runtime directory: {}",
                directory.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::metadata(&directory)?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "Capsule Session runtime directory is not owned by the current user: {}",
            directory.display()
        );
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let identity = format!("{}:{}", root.display(), session_id);
    let digest = blake3::hash(identity.as_bytes()).to_hex();
    Ok(directory.join(format!("{}.sock", &digest.as_str()[..16])))
}

fn random_session_id() -> Result<SessionId> {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    SessionId::parse(format!("session-{}", hex::encode(bytes))).map_err(Into::into)
}

fn random_operation_id(prefix: &str) -> Result<BoundaryOperationId> {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    BoundaryOperationId::parse(format!("{prefix}-{}", hex::encode(bytes))).map_err(Into::into)
}

fn pty_record(seq: u64, direction: Direction, kind: &str, bytes: Vec<u8>) -> IoRecord {
    IoRecord {
        seq,
        offset_ns: None,
        observed_at_unix_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok()),
        connector: ConnectorId::parse(PTY_CONNECTOR_ID).expect("static ConnectorId"),
        direction,
        kind: RecordKindId::parse(kind).expect("static RecordKindId"),
        payload: Payload::Inline(bytes),
    }
}

fn process_start_identity(pid: u32) -> String {
    capsule::state::session::process::process_start_time_unix_ms(pid)
        .map_or_else(|| format!("pid-{pid}-unknown"), |value| value.to_string())
}

fn workload_process_start_identity(pid: u32) -> Result<String> {
    capsule::state::session::process::process_start_time_unix_ms(pid)
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow!("failed to read workload process start identity for PID {pid}"))
}

fn workload_identity_matches(pid: u32, pgid: i32, expected_start_identity: &str) -> bool {
    workload_process_start_identity(pid).is_ok_and(|actual| actual == expected_start_identity)
        && unsafe { libc::getpgid(pid as libc::pid_t) } == pgid
}

#[derive(Clone, Debug)]
struct ProcessTarget {
    pid: u32,
    pgid: i32,
    process_start_identity: String,
}

struct FrozenWorkload {
    root: ProcessTarget,
    targets: Vec<ProcessTarget>,
}

impl FrozenWorkload {
    fn verify_root(&self) -> Result<()> {
        if workload_identity_matches(
            self.root.pid,
            self.root.pgid,
            &self.root.process_start_identity,
        ) {
            Ok(())
        } else {
            bail!("root process disappeared while creating frontier")
        }
    }
}

struct FreezeInProgress {
    root: ProcessTarget,
    targets: BTreeMap<u32, ProcessTarget>,
    armed: bool,
}

impl FreezeInProgress {
    fn new(root: ProcessTarget) -> Self {
        Self {
            targets: BTreeMap::from([(root.pid, root.clone())]),
            root,
            armed: true,
        }
    }

    fn finish(mut self) -> FrozenWorkload {
        self.armed = false;
        FrozenWorkload {
            root: self.root.clone(),
            targets: self.targets.values().cloned().collect(),
        }
    }
}

impl Drop for FreezeInProgress {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for target in self.targets.values().rev() {
            if workload_identity_matches(target.pid, target.pgid, &target.process_start_identity) {
                // Roll back every stop already issued while preserving the
                // original fail-closed discovery error.
                unsafe {
                    libc::kill(-target.pgid, libc::SIGCONT);
                    libc::kill(target.pid as libc::pid_t, libc::SIGCONT);
                }
            }
        }
    }
}

fn discover_process_tree(root_pid: u32) -> Result<BTreeSet<u32>> {
    if std::env::var_os("ATO_TEST_FAIL_PROCESS_DISCOVERY").is_some() {
        bail!("injected process discovery failure");
    }
    let output = Command::new("ps")
        .args(["-e", "-o", "pid=", "-o", "ppid="])
        .output()
        .context("failed to execute process discovery")?;
    if !output.status.success() {
        bail!("process discovery exited with {}", output.status);
    }
    let mut processes = Vec::new();
    for line in String::from_utf8(output.stdout)
        .context("process discovery was not UTF-8")?
        .lines()
    {
        let mut fields = line.split_whitespace();
        let candidate = fields
            .next()
            .ok_or_else(|| anyhow!("process discovery omitted PID"))?
            .parse::<u32>()
            .context("process discovery returned invalid PID")?;
        let parent = fields
            .next()
            .ok_or_else(|| anyhow!("process discovery omitted PPID"))?
            .parse::<u32>()
            .context("process discovery returned invalid PPID")?;
        processes.push((candidate, parent));
    }
    let mut discovered = BTreeSet::from([root_pid]);
    loop {
        let before = discovered.len();
        for &(candidate, parent) in &processes {
            if discovered.contains(&parent) {
                discovered.insert(candidate);
            }
        }
        if discovered.len() == before {
            return Ok(discovered);
        }
    }
}

fn checked_signal(target: &ProcessTarget, signal: i32) -> Result<()> {
    if !workload_identity_matches(target.pid, target.pgid, &target.process_start_identity) {
        bail!("process identity changed for PID {}", target.pid);
    }
    if unsafe { libc::kill(-target.pgid, signal) } == -1 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to signal process group {}", target.pgid));
    }
    if unsafe { libc::kill(target.pid as libc::pid_t, signal) } == -1 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to signal PID {}", target.pid));
    }
    Ok(())
}

fn freeze_workload_tree(
    pid: u32,
    pgid: i32,
    process_start_identity: &str,
) -> Result<FrozenWorkload> {
    let root = ProcessTarget {
        pid,
        pgid,
        process_start_identity: process_start_identity.to_owned(),
    };
    checked_signal(&root, libc::SIGSTOP)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut freezing = FreezeInProgress::new(root);
    for _ in 0..32 {
        let discovered = discover_process_tree(pid)?;
        for descendant in discovered
            .iter()
            .copied()
            .filter(|candidate| *candidate != pid)
        {
            if freezing.targets.contains_key(&descendant) {
                continue;
            }
            let descendant_pgid = unsafe { libc::getpgid(descendant as libc::pid_t) };
            if descendant_pgid < 1 {
                if !discover_process_tree(pid)?.contains(&descendant) {
                    continue;
                }
                bail!("failed to read process group for live PID {descendant}");
            }
            let process_start_identity = match workload_process_start_identity(descendant) {
                Ok(identity) => identity,
                Err(_) if !discover_process_tree(pid)?.contains(&descendant) => continue,
                Err(error) => return Err(error),
            };
            let target = ProcessTarget {
                pid: descendant,
                pgid: descendant_pgid,
                process_start_identity,
            };
            match checked_signal(&target, libc::SIGSTOP) {
                Ok(()) => {
                    freezing.targets.insert(descendant, target);
                }
                Err(_) if !discover_process_tree(pid)?.contains(&descendant) => continue,
                Err(error) => return Err(error),
            }
        }
        let stable = discover_process_tree(pid)?;
        if stable
            .iter()
            .all(|candidate| freezing.targets.contains_key(candidate))
        {
            for target in freezing.targets.values() {
                if stable.contains(&target.pid)
                    && !workload_identity_matches(
                        target.pid,
                        target.pgid,
                        &target.process_start_identity,
                    )
                {
                    bail!("process identity changed while freezing PID {}", target.pid);
                }
            }
            if !workload_identity_matches(pid, pgid, process_start_identity) {
                bail!("root process disappeared while creating frontier");
            }
            return Ok(freezing.finish());
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    bail!("ProcessTreeDidNotQuiesce")
}

fn thaw_workload_tree(workload: &FrozenWorkload) -> Result<()> {
    let mut groups = BTreeSet::new();
    for target in workload.targets.iter().rev() {
        match workload_process_start_identity(target.pid) {
            Ok(identity) if identity == target.process_start_identity => {
                let current_pgid = unsafe { libc::getpgid(target.pid as libc::pid_t) };
                if current_pgid != target.pgid {
                    bail!("process group changed before thaw for PID {}", target.pid);
                }
                if groups.insert(target.pgid)
                    && unsafe { libc::kill(-target.pgid, libc::SIGCONT) } == -1
                {
                    return Err(std::io::Error::last_os_error())
                        .context("failed to thaw process group");
                }
                if unsafe { libc::kill(target.pid as libc::pid_t, libc::SIGCONT) } == -1 {
                    return Err(std::io::Error::last_os_error()).context("failed to thaw process");
                }
            }
            Ok(_) => bail!("PID reuse detected while thawing PID {}", target.pid),
            Err(_) => {}
        }
    }
    Ok(())
}

fn workload_tree(pid: u32, pgid: i32, process_start_identity: &str) -> Vec<ProcessTarget> {
    let mut targets = vec![ProcessTarget {
        pid,
        pgid,
        process_start_identity: process_start_identity.to_owned(),
    }];
    let Ok(output) = Command::new("ps")
        .args(["-e", "-o", "pid=", "-o", "ppid="])
        .output()
    else {
        return targets;
    };
    let processes: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut discovered = BTreeSet::from([pid]);
    loop {
        let mut added = false;
        for &(candidate, parent) in &processes {
            if discovered.contains(&parent) && discovered.insert(candidate) {
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    for descendant in discovered.into_iter().filter(|candidate| *candidate != pid) {
        let descendant_pgid = unsafe { libc::getpgid(descendant as libc::pid_t) };
        if descendant_pgid < 1 {
            continue;
        }
        let Ok(identity) = workload_process_start_identity(descendant) else {
            continue;
        };
        targets.push(ProcessTarget {
            pid: descendant,
            pgid: descendant_pgid,
            process_start_identity: identity,
        });
    }
    targets
}

fn signal_workload_tree(targets: &[ProcessTarget], signal: i32) {
    let mut signaled_groups = BTreeSet::new();
    for target in targets.iter().rev() {
        if !workload_identity_matches(target.pid, target.pgid, &target.process_start_identity) {
            continue;
        }
        if signaled_groups.insert(target.pgid) {
            unsafe { libc::kill(-target.pgid, signal) };
        }
        unsafe { libc::kill(target.pid as libc::pid_t, signal) };
    }
}

fn supervisor_identity_is_live(identity: &SupervisorIdentity) -> bool {
    let pid = identity.pid;
    (unsafe { libc::kill(pid as libc::pid_t, 0) }) == 0
        && capsule::state::session::process::process_start_time_unix_ms(pid)
            .is_some_and(|value| value.to_string() == identity.process_start_identity)
}

fn signal_workload(pid: u32, pgid: i32, signal: i32) {
    unsafe {
        libc::kill(-pgid, signal);
        libc::kill(pid as libc::pid_t, signal);
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_verification_ignores_os_read_segmentation() {
        let mut verifier = PtyOutputVerifier::default();
        verifier.push(b"hello\n");
        verifier.consume(2, b"hel").expect("first segment");
        verifier.consume(3, b"lo\n").expect("second segment");
        assert_eq!(verifier.available(), 0);
    }

    #[test]
    fn replay_verification_detects_one_byte_divergence() {
        let mut verifier = PtyOutputVerifier::default();
        verifier.push(b"hallo\n");
        let error = verifier.consume(2, b"hello\n").expect_err("must diverge");
        assert!(error.to_string().contains("seq 2"));
    }

    #[test]
    fn control_escape_is_not_a_pty_record_kind() {
        for kind in ["stdin", "output", "resize", "exit"] {
            assert_ne!(kind, "detach");
        }
    }

    #[test]
    fn public_name_is_derived_from_a_human_bundle_filename() {
        assert_eq!(
            public_name_from_bundle(Path::new("/issues/Login Refresh BUG.capsule")),
            "login-refresh-bug"
        );
        assert_eq!(
            public_name_from_bundle(Path::new("/issues/バグ再現.capsule")),
            "capsule"
        );
    }

    #[test]
    fn explicit_public_name_rejects_control_and_path_characters() {
        assert!(validate_public_name("login-bug").is_ok());
        for invalid in ["", "../login-bug", "login bug", "login\tbug"] {
            assert!(
                validate_public_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn public_age_uses_compact_units() {
        assert_eq!(format_age(12), "12s");
        assert_eq!(format_age(120), "2m");
        assert_eq!(format_age(7_200), "2h");
        assert_eq!(format_age(172_800), "2d");
    }

    #[test]
    fn human_output_escapes_terminal_control_sequences() {
        assert_eq!(
            sanitize_terminal_text("ログイン\n\u{1b}[31m\u{0085}.capsule"),
            "ログイン\\n\\u{1b}[31m\\u{85}.capsule"
        );
    }
}
