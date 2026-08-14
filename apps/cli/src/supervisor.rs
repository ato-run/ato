use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
use ato_materializer_api::{MaterializerError, Realization, RealizationDriver, ReplayRuntime};
use ato_objects::{ActiveRun, LocalCapsuleRepository, ObjectStore, RecordEnvelope, RecordId};

use crate::{
    adapter_registry,
    authoring::{adapter_instances, evolve_observation, load_runtime_state},
};

const STOP_REQUEST: &str = "runs/stop.request";
const STOP_ACK: &str = "runs/stop.ack";
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
        pid: 0,
        process_start_time: String::new(),
        process_group: 0,
        boot_session: String::new(),
        status: "starting".to_owned(),
    };
    repository.claim_active_run(&lease)?;
    let result = start_claimed(repository, branch, head, bindings, &token);
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

pub(crate) fn worker(
    project: &Path,
    branch: &str,
    head: &ComputationRef,
    token: &str,
) -> Result<()> {
    let repository = LocalCapsuleRepository::open(project)?;
    enter(SupervisorState::Preparing);
    let config = load_runtime_state(head, repository.objects())?.config;
    let bindings: BTreeMap<String, String> = std::env::var("ATO_RUNTIME_BINDINGS")
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    let instances = adapter_instances(&config, &bindings, false, true)?;
    let registry = adapter_registry()?;
    let live_head = Arc::new(Mutex::new(head.clone()));
    let sink: Arc<dyn ObservationSink> = Arc::new(RepositoryObservationSink {
        project: repository.project().to_path_buf(),
        branch: branch.to_owned(),
        head: Arc::clone(&live_head),
    });
    let context = AdapterAttachContext {
        runtime: AdapterContext {
            workspace: repository.project(),
            objects: repository.objects(),
        },
        observations: sink,
    };
    enter(SupervisorState::Starting);
    let mut sessions = registry.attach_all(&instances, &context)?;
    let attached_head = live_head
        .lock()
        .map_err(|_| anyhow::anyhow!("Run head lock was poisoned"))?
        .clone();
    let active = ActiveRun {
        token: token.to_owned(),
        branch: branch.to_owned(),
        branch_base: head.clone(),
        head: attached_head,
        pid: std::process::id(),
        process_start_time: process_start_time(std::process::id())
            .context("worker process start time is unavailable")?,
        process_group: current_process_group()?,
        boot_session: boot_session_identity()?,
        status: "active".to_owned(),
    };
    repository.activate_run(token, &active)?;
    enter(SupervisorState::Active);
    for session in &mut sessions {
        session.activate()?;
    }

    let stop_request = repository.root().join(STOP_REQUEST);
    let stop_ack = repository.root().join(STOP_ACK);
    loop {
        if stop_request.exists() {
            enter(SupervisorState::Stopping);
            let result = quiesce_and_detach(&mut sessions, &context.runtime);
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
    for _ in 0..250 {
        if let Ok(value) = fs::read_to_string(&ack) {
            acknowledged = Some(value);
            break;
        }
        if process_start_time(run.pid).is_none() {
            bail!("active Run exited before Adapter quiesce acknowledgement");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let acknowledged = acknowledged.context("timed out waiting for live Adapters to quiesce")?;
    if let Some(error) = acknowledged.strip_prefix("error:") {
        bail!("live Adapter quiesce failed: {error}");
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
    Ok(Some(run))
}

fn observed_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos())
}

pub(crate) struct CliRealizationDriver {
    project: std::path::PathBuf,
    bindings: BTreeMap<String, String>,
}

impl CliRealizationDriver {
    pub(crate) fn new(project: &Path, bindings: &BTreeMap<String, String>) -> Self {
        Self {
            project: project.to_path_buf(),
            bindings: bindings.clone(),
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
        let instances = adapter_instances(&state.config, &self.bindings, true, false)
            .map_err(materializer_operation)?;
        let registry = adapter_registry().map_err(materializer_operation)?;
        let context = AdapterAttachContext {
            runtime: AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
            observations: Arc::new(IgnoreObservations),
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

    fn run(mut self: Box<Self>) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        let mut result = Ok(());
        for session in &mut self.sessions {
            session.activate().map_err(materializer_operation)?;
        }
        for session in &mut self.sessions {
            if let Err(error) = session.wait() {
                result = Err(MaterializerError::Operation(error.to_string()));
                break;
            }
        }
        let context = AdapterContext {
            workspace: &self.project,
            objects: repository.objects(),
        };
        let detach =
            quiesce_and_detach(&mut self.sessions, &context).map_err(materializer_operation);
        result.and(detach)
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
    head: Arc<Mutex<ComputationRef>>,
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
        let previous = repository
            .records_for_stream(&self.branch, None)
            .map_err(|error| AdapterError::Operation(error.to_string()))?
            .last()
            .map(|record| record.id.clone());
        repository
            .append_record(RecordEnvelope {
                id: RecordId::new(&self.branch, 0),
                adapter_id: observation.adapter_id,
                protocol_id: observation.protocol_id,
                port_id: observation.port_id,
                direction: observation.direction,
                payload_ref,
                head_before: head.clone(),
                head_after: next.clone(),
                caused_by: if observation.caused_by.is_empty() {
                    previous.into_iter().collect()
                } else {
                    observation.caused_by
                },
                observed_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string()),
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        if next != *head {
            if repository
                .active_run()
                .map_err(|error| AdapterError::Operation(error.to_string()))?
                .is_some()
            {
                repository
                    .update_active_head(&head, &next)
                    .map_err(|error| AdapterError::Operation(error.to_string()))?;
            }
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
