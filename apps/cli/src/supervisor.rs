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
    MaterializerContext, MaterializerError, Realization, RealizationDriver, ReplayRuntime,
};
use ato_objects::{ActiveRun, LocalCapsuleRepository, ObjectStore, RecordEnvelope, RecordId};
use serde::{Deserialize, Serialize};

use crate::{
    adapter_registry,
    authoring::{
        adapter_instances, evolve_observation, evolve_workspace_active, load_runtime_state,
        workspace_policy,
    },
    materializer_registry,
};

const STOP_REQUEST: &str = "runs/stop.request";
const STOP_ACK: &str = "runs/stop.ack";
const CAPTURE_REQUEST: &str = "runs/capture.request.json";
const CAPTURE_ACK: &str = "runs/capture.ack.json";
const CAPTURE_RELEASE: &str = "runs/capture.release.json";
static RUN_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureControl {
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureAck {
    token: String,
    branch: String,
    target: Option<String>,
    record_seq: Option<u64>,
    error: Option<String>,
}

pub(crate) struct CaptureLease {
    repository_root: std::path::PathBuf,
    token: String,
    pub(crate) branch: String,
    pub(crate) target: ComputationRef,
    pub(crate) record_seq: u64,
    released: bool,
}

impl CaptureLease {
    pub(crate) fn release(mut self) -> Result<()> {
        self.signal_release()?;
        let request = self.repository_root.join(CAPTURE_REQUEST);
        let ack = self.repository_root.join(CAPTURE_ACK);
        for _ in 0..250 {
            if !request.exists() && !ack.exists() {
                self.released = true;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!("timed out waiting for capture barrier release")
    }

    fn signal_release(&self) -> Result<()> {
        atomic_control_write(
            &self.repository_root.join(CAPTURE_RELEASE),
            &serde_jcs::to_vec(&CaptureControl {
                token: self.token.clone(),
            })?,
        )
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.signal_release();
        }
    }
}

pub(crate) fn capture_active(
    repository: &LocalCapsuleRepository,
    branch: &str,
) -> Result<CaptureLease> {
    let run = repository
        .active_run()?
        .context("Capsule has no active Run")?;
    if run.status != "active" || run.branch != branch {
        bail!("selected branch `{branch}` is not the active Run branch")
    }
    let token = format!(
        "capture-{}-{}-{}",
        std::process::id(),
        observed_nanos(),
        RUN_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let request = repository.root().join(CAPTURE_REQUEST);
    let ack = repository.root().join(CAPTURE_ACK);
    let release = repository.root().join(CAPTURE_RELEASE);
    let _ = fs::remove_file(&ack);
    let _ = fs::remove_file(&release);
    atomic_control_write(
        &request,
        &serde_jcs::to_vec(&CaptureControl {
            token: token.clone(),
        })?,
    )?;
    for _ in 0..500 {
        if let Ok(bytes) = fs::read(&ack) {
            let response: CaptureAck = serde_json::from_slice(&bytes)?;
            if serde_jcs::to_vec(&response)? != bytes || response.token != token {
                bail!("capture worker returned an invalid acknowledgement")
            }
            if let Some(error) = response.error {
                let _ = fs::remove_file(&ack);
                let _ = fs::remove_file(&request);
                bail!("current-point capture failed: {error}")
            }
            return Ok(CaptureLease {
                repository_root: repository.root().to_path_buf(),
                token,
                branch: response.branch,
                target: ComputationRef::parse(response.target.context("capture target missing")?)?,
                record_seq: response
                    .record_seq
                    .context("capture Record frontier missing")?,
                released: false,
            });
        }
        if process_start_time(run.pid).is_none() {
            bail!("active Run exited before capture acknowledgement")
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = fs::remove_file(request);
    bail!("timed out waiting for current-point capture barrier")
}

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
        if repository.root().join(CAPTURE_REQUEST).exists() {
            handle_capture_request(
                &repository,
                branch,
                token,
                &live_head,
                &mut restored,
                &mut sessions,
            )?;
        }
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

fn handle_capture_request(
    repository: &LocalCapsuleRepository,
    branch: &str,
    run_token: &str,
    live_head: &Arc<Mutex<ComputationRef>>,
    realization: &mut Option<Box<dyn Realization>>,
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
) -> Result<()> {
    let request_path = repository.root().join(CAPTURE_REQUEST);
    let ack_path = repository.root().join(CAPTURE_ACK);
    let release_path = repository.root().join(CAPTURE_RELEASE);
    let bytes = fs::read(&request_path)?;
    let request: CaptureControl = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&request)? != bytes || request.token.is_empty() {
        bail!("invalid capture request")
    }
    let context = AdapterContext {
        workspace: repository.project(),
        objects: repository.objects(),
    };
    let paused = match realization.as_deref_mut() {
        Some(realization) => realization.pause_for_capture().map_err(Into::into),
        None => pause_for_capture(sessions, &context),
    };
    let result = paused.and_then(|()| {
        let run = repository
            .active_run()?
            .context("active Run disappeared during capture")?;
        if run.token != run_token || run.branch != branch || run.status != "active" {
            bail!("active Run lease changed during capture")
        }
        let target = evolve_workspace_active(repository, branch, run_token, &run.head)?;
        let frontier = repository
            .active_run()?
            .context("active Run disappeared after workspace reconciliation")?;
        if frontier.head != target {
            bail!("capture frontier did not commit atomically")
        }
        *live_head
            .lock()
            .map_err(|_| anyhow::anyhow!("Run head lock was poisoned"))? = target.clone();
        Ok((target, frontier.record_seq))
    });

    let ack = match &result {
        Ok((target, record_seq)) => CaptureAck {
            token: request.token.clone(),
            branch: branch.to_owned(),
            target: Some(target.to_string()),
            record_seq: Some(*record_seq),
            error: None,
        },
        Err(error) => CaptureAck {
            token: request.token.clone(),
            branch: branch.to_owned(),
            target: None,
            record_seq: None,
            error: Some(format!("{error:#}")),
        },
    };
    atomic_control_write(&ack_path, &serde_jcs::to_vec(&ack)?)?;

    if result.is_ok() {
        let mut released = false;
        for _ in 0..15_000 {
            if let Ok(bytes) = fs::read(&release_path)
                && let Ok(control) = serde_json::from_slice::<CaptureControl>(&bytes)
                && control.token == request.token
            {
                released = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !released {
            // A dead CLI must not strand a durable Run behind a capture gate.
            eprintln!("capture lease expired; resuming live Adapters");
        }
    }

    let resume = match realization.as_deref_mut() {
        Some(realization) => realization.resume_after_capture().map_err(Into::into),
        None => resume_after_capture(sessions, &context),
    };
    let _ = fs::remove_file(&request_path);
    let _ = fs::remove_file(&ack_path);
    let _ = fs::remove_file(&release_path);
    match result {
        Ok(_) => resume,
        Err(_) => {
            resume?;
            Ok(())
        }
    }
}

fn atomic_control_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("control path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.new",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("control"),
        std::process::id()
    ));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
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

    fn pause_for_capture(&mut self) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        pause_for_capture(
            &mut self.sessions,
            &AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
        )
        .map_err(materializer_operation)
    }

    fn resume_after_capture(&mut self) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        resume_after_capture(
            &mut self.sessions,
            &AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
        )
        .map_err(materializer_operation)
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

fn pause_for_capture(
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) -> Result<()> {
    if let Some(session) = sessions
        .iter()
        .find(|session| !session.capabilities().capture_barrier)
    {
        bail!(
            "Adapter `{}` cannot establish a safe current-point capture barrier",
            session.adapter_id()
        );
    }
    for (paused, session) in sessions.iter_mut().enumerate() {
        if let Err(error) = session.pause_for_capture(context) {
            for session in sessions[..paused].iter_mut().rev() {
                let _ = session.resume_after_capture(context);
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn resume_after_capture(
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) -> Result<()> {
    let mut first_error = None;
    for session in sessions.iter_mut().rev() {
        if let Err(error) = session.resume_after_capture(context)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
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
