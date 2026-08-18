use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::{
    AdapterAttachContext, AdapterContext, AdapterError, AdapterObservation, IgnoreObservations,
    ObservationSink,
};
use ato_adapter_process::terminate_process_tree;
use ato_adapter_workspace::restore_workspace;
use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    MaterializerContext, MaterializerError, Realization, RealizationDriver,
    RealizationVerification, ReplayRuntime,
};
use ato_objects::{ActiveRun, LocalCapsuleRepository, ObjectStore, RecordEnvelope, RecordId};

use crate::{
    adapter_registry,
    authoring::{adapter_instances, evolve_observation, load_runtime_state, workspace_policy},
    materializer_registry,
};

const STOP_REQUEST: &str = "runs/stop.request";
const STOP_ACK: &str = "runs/stop.ack";
const SUPERVISOR_STOP_TIMEOUT_SECONDS: u64 = 5;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const _: () = assert!(
    ato_adapter_browser::BROWSER_LIFECYCLE_TIMEOUT_SECONDS < SUPERVISOR_STOP_TIMEOUT_SECONDS
);
static RUN_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisorState {
    Preparing,
    Starting,
    Active,
    Stopping,
    Sealed,
    Failed,
}

fn enter(state: SupervisorState) {
    let _ = state;
}

pub(crate) fn start_durable(
    repository: &LocalCapsuleRepository,
    branch: &str,
    head: &ComputationRef,
    bindings: &BTreeMap<String, String>,
    replay_records: Option<&[RecordEnvelope]>,
) -> Result<()> {
    let token = format!(
        "{}-{}-{}",
        std::process::id(),
        observed_nanos(),
        RUN_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let lease = ActiveRun {
        token: token.clone(),
        branch: branch.to_owned(),
        branch_base: head.clone(),
        head: head.clone(),
        record_seq: repository
            .records_for_stream(branch, None)?
            .last()
            .map_or(0, |record| record.id.seq),
        pid: 0,
        process_start_time: String::new(),
        process_group: 0,
        boot_session: String::new(),
        status: "starting".to_owned(),
    };
    repository.claim_active_run(&lease)?;
    let descriptor = match replay_records
        .map(|records| encode_replay(repository, head, records))
        .transpose()
    {
        Ok(descriptor) => descriptor,
        Err(error) => {
            let _ = repository.release_active_run(&token);
            return Err(error);
        }
    };
    let result = start_claimed(
        repository,
        branch,
        head,
        bindings,
        &token,
        descriptor.as_ref(),
    );
    if result.is_err() {
        let _ = repository.release_active_run(&token);
    }
    result
}

fn start_claimed(
    repository: &LocalCapsuleRepository,
    branch: &str,
    head: &ComputationRef,
    bindings: &BTreeMap<String, String>,
    token: &str,
    descriptor: Option<&ContentRef>,
) -> Result<()> {
    let log_path = repository.root().join("runs/output.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__worker")
        .arg(repository.project())
        .arg(branch)
        .arg(head.to_string())
        .arg(token)
        .env("ATO_RUNTIME_BINDINGS", serde_json::to_string(bindings)?)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stdout);
    if let Some(descriptor) = descriptor {
        command.arg(descriptor.to_string());
    }
    configure_detached_process(&mut command);
    let mut child = command.spawn()?;
    for _ in 0..100 {
        if repository
            .active_run()?
            .is_some_and(|run| run.token == token && run.status == "active")
        {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            bail!(
                "capsule worker exited before becoming active; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!(
        "capsule worker did not become active; see {}",
        log_path.display()
    )
}

fn encode_replay(
    repository: &LocalCapsuleRepository,
    target: &ComputationRef,
    records: &[RecordEnvelope],
) -> Result<ContentRef> {
    let state = load_runtime_state(target, repository.objects())?;
    let policy = workspace_policy(&state.config)?;
    let adapters = adapter_registry()?;
    let materializers = materializer_registry()?;
    let context = MaterializerContext {
        objects: repository.objects(),
        adapters: &adapters,
        records,
        workspace: repository.project(),
        workspace_policy: &policy,
        realization: None,
    };
    Ok(materializers
        .get("ato.replay@1")?
        .encode(target, &context)?)
}

pub(crate) fn worker(
    project: &Path,
    branch: &str,
    head: &ComputationRef,
    token: &str,
    descriptor: Option<&ContentRef>,
) -> Result<()> {
    let repository = LocalCapsuleRepository::open(project)?;
    enter(SupervisorState::Preparing);
    let config = load_runtime_state(head, repository.objects())?.config;
    let bindings: BTreeMap<String, String> = std::env::var("ATO_RUNTIME_BINDINGS")
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    let registry = adapter_registry()?;
    let live_head = Arc::new(Mutex::new(head.clone()));
    let sink: Arc<dyn ObservationSink> = Arc::new(RepositoryObservationSink {
        project: repository.project().to_path_buf(),
        branch: branch.to_owned(),
        token: token.to_owned(),
        head: Arc::clone(&live_head),
    });
    enter(SupervisorState::Starting);
    let mut sessions = Vec::new();
    let mut restored = None;
    let observation_gate = Arc::new(GatedObservationSink {
        enabled: AtomicBool::new(false),
        inner: Arc::clone(&sink),
    });
    if let Some(descriptor) = descriptor {
        let driver = CliRealizationDriver::with_observations(
            repository.project(),
            &bindings,
            observation_gate.clone(),
            false,
        );
        let policy = workspace_policy(&config)?;
        let materializers = materializer_registry()?;
        let context = MaterializerContext {
            objects: repository.objects(),
            adapters: &registry,
            records: &[],
            workspace: repository.project(),
            workspace_policy: &policy,
            realization: Some(&driver),
        };
        let realization = materializers
            .get("ato.replay@1")?
            .restore(descriptor, &context)?;
        if realization.target() != head {
            bail!(
                "Replay restored {} instead of selected {head}",
                realization.target()
            );
        }
        restored = Some(realization);
    } else {
        let instances = adapter_instances(&config, &bindings, false, true)?;
        let context = AdapterAttachContext {
            runtime: AdapterContext {
                workspace: repository.project(),
                objects: repository.objects(),
            },
            observations: Arc::clone(&sink),
        };
        sessions = registry.attach_all(&instances, &context)?;
    }
    let attached_head = live_head
        .lock()
        .map_err(|_| anyhow::anyhow!("Run head lock was poisoned"))?
        .clone();
    let active = ActiveRun {
        token: token.to_owned(),
        branch: branch.to_owned(),
        branch_base: head.clone(),
        head: attached_head,
        record_seq: repository
            .records_for_stream(branch, None)?
            .last()
            .map_or(0, |record| record.id.seq),
        pid: std::process::id(),
        process_start_time: process_start_time(std::process::id())
            .context("worker process start time is unavailable")?,
        process_group: current_process_group()?,
        boot_session: boot_session_identity()?,
        status: "active".to_owned(),
    };
    repository.activate_run(token, &active)?;
    observation_gate.enabled.store(true, Ordering::Release);
    enter(SupervisorState::Active);
    if let Some(realization) = &mut restored {
        realization.activate()?;
    } else {
        for session in &mut sessions {
            session.activate()?;
        }
    }

    let stop_request = repository.root().join(STOP_REQUEST);
    let stop_ack = repository.root().join(STOP_ACK);
    loop {
        if stop_request.exists() {
            enter(SupervisorState::Stopping);
            let result = if let Some(realization) = &mut restored {
                realization
                    .quiesce()
                    .map_err(|error| anyhow::anyhow!(error))
            } else {
                quiesce_and_detach(
                    &mut sessions,
                    &AdapterContext {
                        workspace: repository.project(),
                        objects: repository.objects(),
                    },
                )
            };
            observation_gate.enabled.store(false, Ordering::Release);
            let message = match &result {
                Ok(()) => "ok".to_owned(),
                Err(error) => format!("error:{error:#}"),
            };
            fs::write(&stop_ack, message)?;
            result?;
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn stop_active(repository: &LocalCapsuleRepository) -> Result<Option<ActiveRun>> {
    enter(SupervisorState::Stopping);
    let Some(run) = repository.active_run()? else {
        return Ok(None);
    };
    if run.status != "active" {
        bail!("Capsule Run is still preparing and cannot be stopped");
    }
    if boot_session_identity()? != run.boot_session
        || process_start_time(run.pid).as_deref() != Some(run.process_start_time.as_str())
    {
        enter(SupervisorState::Failed);
        bail!(
            "active Run process identity no longer matches; refusing to stop PID {}",
            run.pid
        );
    }
    let request = repository.root().join(STOP_REQUEST);
    let ack = repository.root().join(STOP_ACK);
    let _ = fs::remove_file(&ack);
    fs::write(&request, b"stop")?;
    let mut acknowledged = None;
    for _ in 0..(SUPERVISOR_STOP_TIMEOUT_SECONDS * 1_000 / STOP_POLL_INTERVAL.as_millis() as u64) {
        if let Ok(value) = fs::read_to_string(&ack) {
            acknowledged = Some(value);
            break;
        }
        if process_start_time(run.pid).is_none() {
            bail!("active Run exited before Adapter quiesce acknowledgement");
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    let acknowledged = acknowledged.context("timed out waiting for live Adapters to quiesce")?;
    if let Some(error) = acknowledged.strip_prefix("error:") {
        bail!("live Adapter quiesce failed: {error}");
    }
    let final_run = repository
        .active_run()?
        .context("active Run lease disappeared after quiesce")?;
    if final_run.token != run.token {
        bail!("active Run lease changed while quiescing");
    }
    terminate_process_tree(run.pid, run.process_group)?;
    for _ in 0..100 {
        if process_start_time(run.pid).is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = fs::remove_file(request);
    let _ = fs::remove_file(ack);
    enter(SupervisorState::Sealed);
    Ok(Some(final_run))
}

fn observed_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos())
}

pub(crate) struct CliRealizationDriver {
    project: std::path::PathBuf,
    bindings: BTreeMap<String, String>,
    observations: Arc<dyn ObservationSink>,
    isolated_processes: bool,
}

impl CliRealizationDriver {
    pub(crate) fn new(project: &Path, bindings: &BTreeMap<String, String>) -> Self {
        Self::with_observations(project, bindings, Arc::new(IgnoreObservations), true)
    }

    fn with_observations(
        project: &Path,
        bindings: &BTreeMap<String, String>,
        observations: Arc<dyn ObservationSink>,
        isolated_processes: bool,
    ) -> Self {
        Self {
            project: project.to_path_buf(),
            bindings: bindings.clone(),
            observations,
            isolated_processes,
        }
    }
}

impl RealizationDriver for CliRealizationDriver {
    fn begin(&self, anchor: &ComputationRef) -> Result<Box<dyn ReplayRuntime>, MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        let state =
            load_runtime_state(anchor, repository.objects()).map_err(materializer_operation)?;
        restore_workspace(
            &ContentRef::parse(&state.workspace_snapshot).map_err(materializer_operation)?,
            &self.project,
            repository.objects(),
        )
        .map_err(materializer_operation)?;
        let instances = adapter_instances(
            &state.config,
            &self.bindings,
            self.isolated_processes,
            false,
        )
        .map_err(materializer_operation)?;
        let registry = adapter_registry().map_err(materializer_operation)?;
        let context = AdapterAttachContext {
            runtime: AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
            observations: Arc::clone(&self.observations),
        };
        let sessions = registry
            .attach_all(&instances, &context)
            .map_err(materializer_operation)?;
        Ok(Box::new(CliReplayRuntime {
            project: self.project.clone(),
            sessions,
        }))
    }
}

struct CliReplayRuntime {
    project: std::path::PathBuf,
    sessions: Vec<Box<dyn ato_adapter_api::AttachedAdapter>>,
}

impl ReplayRuntime for CliReplayRuntime {
    fn apply(&mut self, record: &RecordEnvelope) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        let mut matches = self
            .sessions
            .iter_mut()
            .filter(|session| session.accepts(record));
        let session = matches.next().ok_or_else(|| {
            MaterializerError::Operation(format!(
                "no attached Adapter accepts record {:?} ({})",
                record.id, record.adapter_id
            ))
        })?;
        if matches.next().is_some() {
            return Err(MaterializerError::Operation(format!(
                "multiple attached Adapters accept record {:?} ({})",
                record.id, record.adapter_id
            )));
        }
        session
            .apply(
                record,
                &AdapterContext {
                    workspace: &self.project,
                    objects: repository.objects(),
                },
            )
            .map_err(materializer_operation)
    }

    fn finish(
        self: Box<Self>,
        target: &ComputationRef,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        Ok(Box::new(CliRealization {
            project: self.project,
            sessions: self.sessions,
            target: target.clone(),
        }))
    }
}

struct CliRealization {
    project: std::path::PathBuf,
    sessions: Vec<Box<dyn ato_adapter_api::AttachedAdapter>>,
    target: ComputationRef,
}

impl Realization for CliRealization {
    fn target(&self) -> &ComputationRef {
        &self.target
    }

    fn verification(&self) -> RealizationVerification {
        RealizationVerification::AppliedUnverified
    }

    fn activate(&mut self) -> Result<(), MaterializerError> {
        for session in &mut self.sessions {
            session.activate().map_err(materializer_operation)?;
        }
        Ok(())
    }

    fn wait(&mut self) -> Result<(), MaterializerError> {
        for session in &mut self.sessions {
            session.wait().map_err(materializer_operation)?;
        }
        Ok(())
    }

    fn quiesce(&mut self) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        let context = AdapterContext {
            workspace: &self.project,
            objects: repository.objects(),
        };
        quiesce_and_detach(&mut self.sessions, &context).map_err(materializer_operation)
    }
}

fn materializer_operation(error: impl std::fmt::Display) -> MaterializerError {
    MaterializerError::Operation(error.to_string())
}

fn quiesce_and_detach(
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) -> Result<()> {
    for session in sessions.iter_mut() {
        session.quiesce(context)?;
    }
    for session in sessions.iter_mut().rev() {
        session.detach(context)?;
    }
    Ok(())
}

struct RepositoryObservationSink {
    project: std::path::PathBuf,
    branch: String,
    token: String,
    head: Arc<Mutex<ComputationRef>>,
}

struct GatedObservationSink {
    enabled: AtomicBool,
    inner: Arc<dyn ObservationSink>,
}

impl ObservationSink for GatedObservationSink {
    fn emit(&self, observation: AdapterObservation) -> Result<(), AdapterError> {
        if self.enabled.load(Ordering::Acquire) {
            self.inner.emit(observation)
        } else {
            Ok(())
        }
    }
}

impl ObservationSink for RepositoryObservationSink {
    fn emit(&self, observation: AdapterObservation) -> Result<(), AdapterError> {
        let repository = LocalCapsuleRepository::open(&self.project)
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        let payload_ref = repository.objects().put(&observation.payload)?;
        let mut head = self
            .head
            .lock()
            .map_err(|_| AdapterError::Operation("Run head lock was poisoned".to_owned()))?;
        let next = match observation.effect {
            ato_adapter_api::ObservationEffect::Evidence => head.clone(),
            ato_adapter_api::ObservationEffect::Evolution => {
                evolve_observation(repository.objects(), &head, &observation, &payload_ref)
                    .map_err(|error| AdapterError::Operation(error.to_string()))?
            }
        };
        repository
            .commit_observation(
                &self.token,
                &head,
                RecordEnvelope {
                    id: RecordId::new(&self.branch, 0),
                    adapter_id: observation.adapter_id,
                    protocol_id: observation.protocol_id,
                    port_id: observation.port_id,
                    direction: observation.direction,
                    payload_ref,
                    head_before: head.clone(),
                    head_after: next.clone(),
                    caused_by: observation.caused_by,
                    observed_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string()),
                },
            )
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        if next != *head {
            *head = next;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19).map(|value| (*value).to_owned())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_start_time(pid: u32) -> Option<String> {
    command_output("ps", &["-o", "lstart=", "-p", &pid.to_string()])
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToUniversalTime().Ticks"),
        ],
    )
}

#[cfg(target_os = "linux")]
fn boot_session_identity() -> Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

#[cfg(target_os = "macos")]
fn boot_session_identity() -> Result<String> {
    command_output("sysctl", &["-n", "kern.boottime"])
        .context("kernel boot identity is unavailable")
}

#[cfg(windows)]
fn boot_session_identity() -> Result<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().Ticks",
        ],
    )
    .context("Windows boot identity is unavailable")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn boot_session_identity() -> Result<String> {
    bail!("boot/session identity is unavailable on this platform")
}

#[cfg(unix)]
fn current_process_group() -> Result<u32> {
    command_output(
        "ps",
        &["-o", "pgid=", "-p", &std::process::id().to_string()],
    )
    .and_then(|value| value.parse().ok())
    .context("current process group is unavailable")
}

#[cfg(windows)]
fn current_process_group() -> Result<u32> {
    Ok(std::process::id())
}

#[cfg(any(unix, windows))]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}
