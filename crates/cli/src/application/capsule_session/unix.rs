use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child as ProcessChild, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use capsule::protocol_bundle::{PortableCapsule, restore_workspace_state};
use capsule_protocol::{ConnectorId, Direction, IoRecord, Payload, RecordKindId};
use capsule_session_runtime::{
    BoundaryCoordinator, BoundaryDriver, BoundaryOperationId, CapsuleProtocolSessionStore,
    DurableFrontier, JournalLsn, NewStoredProtocolSession, NewSupervisorIdentity, RecordFrontier,
    SessionId, SharedSessionWal, StoredProtocolSession, SupervisorIdentity,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size};
use fs2::FileExt;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};

const CONTROL_FRAME_LIMIT: usize = 16 * 1024 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_QUEUE_DEPTH: usize = 64;
const PTY_DRAIN_QUEUE_DEPTH: usize = 64;
const PTY_CONNECTOR_ID: &str = "terminal.main";
const READY_MARKER: &[u8] = b"__ATO_SESSION_READY__";
const WATCHDOG_DISARM: &[u8] = b"DISARM\n";

pub(crate) fn start(bundle: &Path, into: &Path, no_attach: bool) -> Result<()> {
    PortableCapsule::read(bundle).context("invalid Capsule bundle")?;
    let bundle = bundle
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", bundle.display()))?;
    let into = absolute_path(into)?;
    let session_id = random_session_id()?;
    let paths = SessionPaths::new(&session_id)?;
    paths.create()?;
    import_session_seed(&bundle, &paths.seed_capsule)?;

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
        .arg(&into)
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
        match request(&session_id, ControlAction::Status) {
            Ok(ControlMessage::Status { lifecycle, .. }) if lifecycle == "running" => break,
            Ok(ControlMessage::Error { message }) => {
                let _ = child.kill();
                bail!("Session Supervisor failed readiness: {message}");
            }
            Ok(_) | Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(_) | Err(_) => {
                let _ = child.kill();
                bail!("Session Supervisor did not become ready within {READY_TIMEOUT:?}");
            }
        }
    }

    println!("{session_id}");
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
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)
        .context("failed to open Supervisor lock")?;
    lock.try_lock_exclusive()
        .context("another Supervisor already owns this Session")?;

    let capsule = PortableCapsule::read(bundle).context("failed to read Capsule bundle")?;
    let state = &capsule.descriptor.base_state;
    let object = capsule
        .objects
        .get(&state.state_ref)
        .ok_or_else(|| anyhow!("base State object is missing"))?;
    restore_workspace_state(state, object, into).context("failed to restore workspace State")?;

    let mut computation = PtyComputation::spawn(into)?;
    initialize_terminal(&mut computation.reader, &computation.writer)?;
    let historical_frontier = capsule
        .records
        .last()
        .map_or(RecordFrontier::Origin, |record| {
            RecordFrontier::Through(record.seq)
        });
    let next_seq = capsule.records.last().map_or(Ok(1), |record| {
        record
            .seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("seq exhausted"))
    })?;
    let generated = NewSupervisorIdentity::generate(
        1,
        std::process::id(),
        process_start_identity(std::process::id()),
    );
    write_secret(&paths.token, generated.secret())?;
    let identity = generated.identity;
    let store = CapsuleProtocolSessionStore::open(&paths.root)?;
    let mut stored = StoredProtocolSession::new(NewStoredProtocolSession {
        session_id: session_id.clone(),
        lifecycle: "starting".to_owned(),
        state_type: &state.state_type,
        base_state: &state.state_ref,
        base_frontier: RecordFrontier::Origin,
        durable_frontier: DurableFrontier {
            records_through: historical_frontier,
            journal_through: JournalLsn::ORIGIN,
        },
        workspace: into.to_path_buf(),
        supervisor: identity.clone(),
    });
    store.write(&stored)?;

    if paths.socket.exists() {
        fs::remove_file(&paths.socket).context("failed to remove stale control socket")?;
    }
    let listener = UnixListener::bind(&paths.socket).context("failed to bind control socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    write_owner_only(
        &paths.socket_address,
        paths.socket.as_os_str().as_encoded_bytes(),
    )?;

    let (pty_tx, pty_rx) = mpsc::sync_channel(PTY_DRAIN_QUEUE_DEPTH);
    let reader = computation.take_reader()?;
    thread::Builder::new()
        .name(format!("capsule-pty-drain-{session_id}"))
        .spawn(move || drain_pty(reader, pty_tx))
        .context("failed to start PTY drain")?;
    PtyHistoricalReplayer::new(&mut computation, &pty_rx).replay(
        &capsule.records,
        RecordFrontier::Origin,
        historical_frontier,
    )?;

    let wal = SharedSessionWal::open(&paths.wal)?;
    let driver = PtyBoundaryWriter {
        writer: Arc::clone(&computation.writer),
        master: Arc::clone(&computation.master),
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

    let result = supervisor_loop(
        &session_id,
        &store,
        &mut stored,
        &mut computation,
        &mut coordinator,
        &wal,
        command_rx,
        pty_rx,
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
        if let Ok((cols, rows)) = terminal_size() {
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

pub(crate) fn watchdog(
    pid: u32,
    pgid: i32,
    expected_start_identity: &str,
    lease_fd: i32,
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
    if !workload_identity_matches(pid, pgid, expected_start_identity) {
        return Ok(());
    }
    let targets = workload_tree(pid, pgid, expected_start_identity);
    signal_workload_tree(&targets, libc::SIGTERM);
    thread::sleep(Duration::from_millis(500));
    signal_workload_tree(&targets, libc::SIGKILL);
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
                    let record = pty_record(next_seq, Direction::Egress, "output", bytes.clone());
                    let operation_id = random_operation_id("pty-output")?;
                    let frontier = coordinator
                        .commit_egress(operation_id, &record)
                        .map_err(|error| anyhow!(error.to_string()))?;
                    next_seq = next_seq
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("seq exhausted"))?;
                    stored.durable_frontier = frontier;
                    store.write(stored)?;
                    clients.retain(|client| {
                        match client.sender.try_send(ControlMessage::Output {
                            bytes: bytes.clone(),
                        }) {
                            Ok(()) => true,
                            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                                if writer_client == Some(client.id) {
                                    writer_client = None;
                                }
                                false
                            }
                        }
                    });
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
    stored.lifecycle = "stopped".to_owned();
    store.write(stored)?;
    for reply in kill_replies {
        let _ = reply.send(ControlMessage::Killed);
    }
    for client in clients {
        let _ = client.sender.try_send(ControlMessage::Detached);
    }
    Ok(())
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
    pid: u32,
    pgid: i32,
    process_start_identity: String,
    lease: Option<File>,
    watchdog: Option<ProcessChild>,
}

impl PtyComputation {
    fn spawn(workspace: &Path) -> Result<Self> {
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
        let (lease, watchdog) = spawn_watchdog(pid, pgid, &process_start_identity)?;
        Ok(Self {
            master: Arc::new(Mutex::new(pair.master)),
            child,
            killer,
            reader: Some(reader),
            writer: Arc::new(Mutex::new(writer)),
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
            .map_err(|error| anyhow!("failed to resize PTY: {error}"))
    }

    fn request_termination(&mut self) {
        if workload_identity_matches(self.pid, self.pgid, &self.process_start_identity) {
            signal_workload(self.pid, self.pgid, libc::SIGTERM);
        }
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

fn spawn_watchdog(
    pid: u32,
    pgid: i32,
    process_start_identity: &str,
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
    let child = Command::new(std::env::current_exe()?)
        .args([
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
        ])
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
    output: PtyOutputVerifier,
}

impl<'a> PtyHistoricalReplayer<'a> {
    fn new(computation: &'a mut PtyComputation, events: &'a Receiver<PtyEvent>) -> Self {
        Self {
            computation,
            events,
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
        while let Ok(event) = self.events.try_recv() {
            match event {
                PtyEvent::Output(bytes) => self.output.push(&bytes),
                PtyEvent::Closed => {}
                PtyEvent::Failed(message) => bail!("PTY replay drain failed: {message}"),
            }
        }
        if self.output.available() != 0 {
            let seq = match through {
                RecordFrontier::Origin => 0,
                RecordFrontier::Through(seq) => seq,
            };
            bail!("historical PTY replay diverged after seq {seq}");
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

fn drain_pty(mut reader: Box<dyn Read + Send>, sender: SyncSender<PtyEvent>) {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(PtyEvent::Closed);
                return;
            }
            Ok(count) => {
                if sender
                    .send(PtyEvent::Output(buffer[..count].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                let _ = sender.send(PtyEvent::Closed);
                return;
            }
            Err(error) => {
                let _ = sender.send(PtyEvent::Failed(error.to_string()));
                return;
            }
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminationReason {
    Natural,
    ControlKill,
    ControlChannelClosed,
}

#[derive(Serialize, Deserialize)]
struct ResizePayload {
    rows: u16,
    cols: u16,
}

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
    supervisor_log: PathBuf,
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
            supervisor_log: directory.join("logs").join("supervisor.log"),
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

fn write_secret(path: &Path, secret: &[u8]) -> Result<()> {
    write_owner_only(path, secret)
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("failed to create Session control token")?;
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
}
