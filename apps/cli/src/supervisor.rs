use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::{
    AdapterAttachContext, AdapterContext, AdapterError, AdapterObservation, IgnoreObservations,
    ObservationEffect, ObservationSink, PresentationAsset, PresentationHint,
    PresentationKeyframeCapture, PresentationKind,
};
use ato_adapter_process::terminate_process_tree;
use ato_adapter_workspace::restore_workspace;
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_materializer_api::{
    MaterializerContext, MaterializerError, Realization, RealizationDriver,
    RealizationVerification, ReplayRuntime,
};
use ato_objects::{
    ActiveRun, Direction, LocalCapsuleRepository, ObjectStore, RecordEnvelope, RecordId,
};
use serde::{Deserialize, Serialize};

use crate::{
    activity_executor_gateway::{
        ACTIVITY_INPUT_REQUEST, ACTIVITY_INPUT_RESPONSE, ActivityInputReceipt, ActivityInputRequest,
    },
    adapter_registry,
    authoring::{
        adapter_instances, evolve_observation, evolve_workspace_active, load_runtime_state,
        workspace_policy,
    },
    materializer_registry,
};

const STOP_REQUEST: &str = "runs/stop.request";
const STOP_ACK: &str = "runs/stop.ack";
const SUPERVISOR_STOP_TIMEOUT_SECONDS: u64 = 5;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const _: () = assert!(
    ato_adapter_browser::BROWSER_LIFECYCLE_TIMEOUT_SECONDS < SUPERVISOR_STOP_TIMEOUT_SECONDS
);
const CAPTURE_REQUEST: &str = "runs/capture.request.json";
const CAPTURE_ACK: &str = "runs/capture.ack.json";
const CAPTURE_RELEASE: &str = "runs/capture.release.json";
static RUN_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);
const MAX_ARCHIVE_FRAMES: usize = 24;
const MAX_ARCHIVE_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_PRESENTATION_ASSET_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct PresentationArchive {
    assets: Vec<PresentationAsset>,
    digests: BTreeSet<String>,
    total_bytes: usize,
}

impl PresentationArchive {
    fn ingest(&mut self, asset: PresentationAsset) {
        self.ingest_with_policy(asset, false);
    }

    fn ingest_final_keyframe(&mut self, asset: PresentationAsset) {
        self.ingest_with_policy(asset, true);
    }

    fn ingest_with_policy(&mut self, asset: PresentationAsset, retain_duplicate: bool) {
        if asset.kind != PresentationKind::ArchiveKeyframe
            || asset.bytes.is_empty()
            || asset.bytes.len() > MAX_PRESENTATION_ASSET_BYTES
            || asset.bytes.len() > MAX_ARCHIVE_TOTAL_BYTES
        {
            return;
        }
        let digest = blake3::hash(&asset.bytes).to_hex().to_string();
        if !retain_duplicate && self.digests.contains(&digest) {
            return;
        }
        while !self.assets.is_empty()
            && (self.assets.len() >= MAX_ARCHIVE_FRAMES
                || self.total_bytes.saturating_add(asset.bytes.len()) > MAX_ARCHIVE_TOTAL_BYTES)
        {
            let remove_index = usize::from(self.assets.len() > 1);
            let removed = self.assets.remove(remove_index);
            self.total_bytes = self.total_bytes.saturating_sub(removed.bytes.len());
            let removed_digest = blake3::hash(&removed.bytes).to_hex().to_string();
            if !self
                .assets
                .iter()
                .any(|asset| blake3::hash(&asset.bytes).to_hex().as_str() == removed_digest)
            {
                self.digests.remove(&removed_digest);
            }
        }
        self.total_bytes = self.total_bytes.saturating_add(asset.bytes.len());
        self.digests.insert(digest);
        self.assets.push(asset);
    }

    fn capture_assets(
        &self,
        final_assets: Vec<PresentationAsset>,
        sequence: u32,
    ) -> Vec<PresentationAsset> {
        let mut archive = Self {
            assets: self.assets.clone(),
            digests: self.digests.clone(),
            total_bytes: self.total_bytes,
        };
        let mut finals = Vec::with_capacity(final_assets.len());
        for mut asset in final_assets {
            if asset.kind == PresentationKind::FinalState {
                asset.sequence = sequence;
                let mut keyframe = asset.clone();
                keyframe.kind = PresentationKind::ArchiveKeyframe;
                archive.ingest_final_keyframe(keyframe);
            }
            finals.push(asset);
        }
        archive.assets.extend(finals);
        archive.assets
    }
}

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
    #[serde(default)]
    presentation_assets: Vec<CapturePresentationAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturePresentationAsset {
    pub(crate) kind: String,
    pub(crate) content_type: String,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) sequence: u32,
    pub(crate) path: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresentationCaptureReceipt {
    pub(crate) root_computation_ref: String,
    pub(crate) record_sequence: u64,
    pub(crate) assets: Vec<CapturePresentationAsset>,
}

pub(crate) struct CaptureLease {
    repository_root: std::path::PathBuf,
    token: String,
    pub(crate) branch: String,
    pub(crate) target: ComputationRef,
    pub(crate) record_seq: u64,
    pub(crate) presentation_assets: Vec<CapturePresentationAsset>,
    presentation_dir: Option<std::path::PathBuf>,
    released: bool,
}

impl CaptureLease {
    pub(crate) fn release(mut self) -> Result<()> {
        self.signal_release()?;
        let request = self.repository_root.join(CAPTURE_REQUEST);
        let ack = self.repository_root.join(CAPTURE_ACK);
        for _ in 0..250 {
            if !request.exists() && !ack.exists() {
                if let Some(directory) = self.presentation_dir.take() {
                    let _ = fs::remove_dir_all(directory);
                }
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
            if let Some(directory) = self.presentation_dir.take() {
                let _ = fs::remove_dir_all(directory);
            }
        }
    }
}

pub(crate) fn export_capture_presentations(lease: &CaptureLease, output: &Path) -> Result<()> {
    if output.exists() {
        bail!("presentation output already exists")
    }
    fs::create_dir_all(output)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o700))?;
    }
    let mut exported = Vec::with_capacity(lease.presentation_assets.len());
    for (index, asset) in lease.presentation_assets.iter().enumerate() {
        let extension = asset
            .path
            .extension()
            .and_then(|value| value.to_str())
            .context("presentation asset has no safe extension")?;
        let file = format!("{index:03}-{}.{}", asset.kind, extension);
        let target = output.join(&file);
        fs::copy(&asset.path, &target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        }
        exported.push(CapturePresentationAsset {
            kind: asset.kind.clone(),
            content_type: asset.content_type.clone(),
            width: asset.width,
            height: asset.height,
            sequence: asset.sequence,
            path: std::path::PathBuf::from(file),
        });
    }
    atomic_control_write(
        &output.join("receipt.json"),
        &serde_jcs::to_vec(&PresentationCaptureReceipt {
            root_computation_ref: lease.target.to_string(),
            record_sequence: lease.record_seq,
            assets: exported,
        })?,
    )
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
            let presentation_dir = repository
                .root()
                .join("runs")
                .join(format!("presentation-{token}"));
            let presentation_assets = resolve_capture_asset_paths(
                repository.root(),
                &token,
                response.presentation_assets,
            )?;
            return Ok(CaptureLease {
                repository_root: repository.root().to_path_buf(),
                token,
                branch: response.branch,
                target: ComputationRef::parse(response.target.context("capture target missing")?)?,
                record_seq: response
                    .record_seq
                    .context("capture Record frontier missing")?,
                presentation_assets,
                presentation_dir: Some(presentation_dir),
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
    let descriptor = replay_records
        .map(|records| encode_replay(repository, head, records))
        .transpose()?;
    start_durable_with_descriptor(repository, branch, head, bindings, descriptor.as_ref())
}

pub(crate) fn start_durable_with_descriptor(
    repository: &LocalCapsuleRepository,
    branch: &str,
    head: &ComputationRef,
    bindings: &BTreeMap<String, String>,
    descriptor: Option<&ContentRef>,
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
    let result = start_claimed(repository, branch, head, bindings, &token, descriptor);
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
    let keyframe_requests = Arc::new(Mutex::new(VecDeque::new()));
    let presentation_archive = Arc::new(Mutex::new(PresentationArchive::default()));
    let keyframe_captures = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<dyn ObservationSink> = Arc::new(RepositoryObservationSink {
        project: repository.project().to_path_buf(),
        branch: branch.to_owned(),
        token: token.to_owned(),
        head: Arc::clone(&live_head),
        keyframe_requests: Arc::clone(&keyframe_requests),
        presentation_archive: Arc::clone(&presentation_archive),
        keyframe_captures: Arc::clone(&keyframe_captures),
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
    *keyframe_captures
        .lock()
        .map_err(|_| anyhow::anyhow!("presentation keyframe handles were poisoned"))? =
        match restored.as_deref() {
            Some(realization) => realization.presentation_keyframe_captures(),
            None => sessions
                .iter()
                .filter_map(|session| session.presentation_keyframe_capture())
                .collect(),
        };
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
    if let Ok(sequence) = u32::try_from(active.record_seq)
        && let Ok(mut archive) = presentation_archive.lock()
    {
        capture_archive_keyframe(
            sequence,
            &mut archive,
            &mut restored,
            &mut sessions,
            &AdapterContext {
                workspace: repository.project(),
                objects: repository.objects(),
            },
        );
    }

    let stop_request = repository.root().join(STOP_REQUEST);
    let stop_ack = repository.root().join(STOP_ACK);
    let mut activity_receipts = BTreeMap::<String, ActivityInputReceipt>::new();
    let mut activity_run_sequence = 0_u64;
    loop {
        if let Ok(mut archive) = presentation_archive.lock() {
            drain_archive_keyframes(
                &keyframe_requests,
                &mut archive,
                &mut restored,
                &mut sessions,
                &AdapterContext {
                    workspace: repository.project(),
                    objects: repository.objects(),
                },
            );
        }
        if repository.root().join(CAPTURE_REQUEST).exists() {
            handle_capture_request(
                &repository,
                branch,
                token,
                &live_head,
                &mut restored,
                &mut sessions,
                &presentation_archive,
            )?;
        }
        if repository.root().join(ACTIVITY_INPUT_REQUEST).exists() {
            handle_activity_input_request(
                &repository,
                branch,
                &live_head,
                &mut sessions,
                sink.as_ref(),
                &mut activity_receipts,
                &mut activity_run_sequence,
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

fn handle_activity_input_request(
    repository: &LocalCapsuleRepository,
    branch: &str,
    live_head: &Arc<Mutex<ComputationRef>>,
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    sink: &dyn ObservationSink,
    receipts: &mut BTreeMap<String, ActivityInputReceipt>,
    run_sequence: &mut u64,
) -> Result<()> {
    let request_path = repository.root().join(ACTIVITY_INPUT_REQUEST);
    let response_path = repository.root().join(ACTIVITY_INPUT_RESPONSE);
    let bytes = fs::read(&request_path).context("read Activity input gateway request")?;
    fs::remove_file(&request_path).context("consume Activity input gateway request")?;
    let request: ActivityInputRequest =
        serde_json::from_slice(&bytes).context("decode Activity input gateway request")?;
    if request.adapter_id != ato_adapter_browser::BROWSER_ADAPTER_ID
        || request.protocol_id != ato_adapter_browser::BROWSER_PROTOCOL_ID
        || request.request_id.len() > 128
        || request.operation_id.len() > 128
        || request.actor_participant_id.len() > 128
    {
        bail!("Activity input gateway request is outside its Run authority");
    }
    if let Some(previous) = receipts.get(&request.operation_id) {
        let mut duplicate = previous.clone();
        duplicate.request_id = request.request_id;
        duplicate.result = "duplicate".to_owned();
        write_activity_input_response(&response_path, &duplicate)?;
        return Ok(());
    }
    let payload =
        serde_jcs::to_vec(&request.event).context("canonicalize Activity Browser event")?;
    let payload_ref = repository.objects().put(&payload)?;
    let head = live_head
        .lock()
        .map_err(|_| anyhow::anyhow!("Run head lock was poisoned"))?
        .clone();
    let port_id = PortId::parse("activity.browser")?;
    let protocol_id = ProtocolId::parse(ato_adapter_browser::BROWSER_PROTOCOL_ID)?;
    let candidate = RecordEnvelope {
        id: RecordId::new(branch, 0),
        adapter_id: request.adapter_id.clone(),
        protocol_id: protocol_id.clone(),
        port_id: port_id.clone(),
        direction: Direction::Inbound,
        payload_ref,
        head_before: head.clone(),
        head_after: head,
        caused_by: Vec::new(),
        observed_at: "0".to_owned(),
    };
    let mut matches = sessions
        .iter_mut()
        .filter(|session| session.accepts(&candidate));
    let session = matches
        .next()
        .context("Activity Run has no Browser Adapter for activity.browser")?;
    if matches.next().is_some() {
        bail!("Activity Run has multiple Browser Adapters for activity.browser");
    }
    session.apply(
        &candidate,
        &AdapterContext {
            workspace: repository.project(),
            objects: repository.objects(),
        },
    )?;
    sink.emit(AdapterObservation {
        adapter_id: request.adapter_id.clone(),
        protocol_id,
        port_id,
        direction: Direction::Inbound,
        payload,
        caused_by: Vec::new(),
        effect: ObservationEffect::Evolution,
        presentation_hint: PresentationHint::None,
    })?;
    *run_sequence = run_sequence
        .checked_add(1)
        .context("Activity Run sequence exhausted")?;
    let committed = repository
        .records_for_stream(branch, None)?
        .last()
        .cloned()
        .context("Activity Browser event was applied without a committed Record")?;
    let receipt = ActivityInputReceipt {
        request_id: request.request_id,
        operation_id: request.operation_id.clone(),
        actor_participant_id: request.actor_participant_id,
        client_sequence: request.client_sequence,
        run_sequence: *run_sequence,
        result: "applied".to_owned(),
        adapter_id: request.adapter_id,
        record_ref: Some(format!("{}:{}", committed.id.stream, committed.id.seq)),
        error: None,
    };
    receipts.insert(request.operation_id, receipt.clone());
    write_activity_input_response(&response_path, &receipt)
}

fn write_activity_input_response(path: &Path, receipt: &ActivityInputReceipt) -> Result<()> {
    let pending = path.with_extension("pending");
    fs::write(
        &pending,
        serde_json::to_vec(receipt).context("encode Activity input gateway response")?,
    )
    .context("write Activity input gateway response")?;
    fs::rename(pending, path).context("publish Activity input gateway response")
}

fn handle_capture_request(
    repository: &LocalCapsuleRepository,
    branch: &str,
    run_token: &str,
    live_head: &Arc<Mutex<ComputationRef>>,
    realization: &mut Option<Box<dyn Realization>>,
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    presentation_archive: &Arc<Mutex<PresentationArchive>>,
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
    eprintln!("current-point capture: pausing live Adapter boundaries");
    let paused = match realization.as_deref_mut() {
        Some(realization) => realization.pause_for_capture().map_err(Into::into),
        None => pause_for_capture(sessions, &context),
    };
    let result = paused.and_then(|()| {
        eprintln!("current-point capture: Adapter boundaries paused");
        let run = repository
            .active_run()?
            .context("active Run disappeared during capture")?;
        if run.token != run_token || run.branch != branch || run.status != "active" {
            bail!("active Run lease changed during capture")
        }
        let target = evolve_workspace_active(repository, branch, run_token, &run.head)?;
        eprintln!("current-point capture: workspace frontier reconciled");
        let frontier = repository
            .active_run()?
            .context("active Run disappeared after workspace reconciliation")?;
        if frontier.head != target {
            bail!("capture frontier did not commit atomically")
        }
        *live_head
            .lock()
            .map_err(|_| anyhow::anyhow!("Run head lock was poisoned"))? = target.clone();
        let final_assets = match realization.as_deref_mut() {
            Some(realization) => realization
                .capture_final_presentation()
                .map_err(anyhow::Error::from)?,
            None => capture_final_presentation(sessions, &context)?,
        };
        let sequence = u32::try_from(frontier.record_seq)
            .context("presentation sequence exceeds the bounded projection contract")?;
        let assets = presentation_archive
            .lock()
            .map_err(|_| anyhow::anyhow!("presentation archive was poisoned"))?
            .capture_assets(final_assets, sequence);
        eprintln!(
            "current-point capture: collected {} bounded presentation asset(s)",
            assets.len()
        );
        let persisted = persist_presentation_assets(repository, &request.token, &assets)?;
        Ok((target, frontier.record_seq, persisted))
    });

    let ack = match &result {
        Ok((target, record_seq, presentation_assets)) => CaptureAck {
            token: request.token.clone(),
            branch: branch.to_owned(),
            target: Some(target.to_string()),
            record_seq: Some(*record_seq),
            error: None,
            presentation_assets: presentation_assets.clone(),
        },
        Err(error) => CaptureAck {
            token: request.token.clone(),
            branch: branch.to_owned(),
            target: None,
            record_seq: None,
            error: Some(format!("{error:#}")),
            presentation_assets: Vec::new(),
        },
    };
    eprintln!("current-point capture: publishing acknowledgement");
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
    } else {
        // Keep a failure acknowledgement observable until the requesting CLI
        // consumes it. Without this bounded handshake a fast worker can remove
        // the receipt between two polling intervals and turn a concrete error
        // into a misleading timeout.
        for _ in 0..500 {
            if !request_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
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

fn capture_final_presentation(
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) -> Result<Vec<ato_adapter_api::PresentationAsset>> {
    let mut assets = Vec::new();
    for session in sessions {
        let Some(capture) = session.presentation_capture() else {
            continue;
        };
        capture.attach(context)?;
        let result = capture.capture_final(context);
        let detach = capture.detach(context);
        assets.extend(result?);
        detach?;
    }
    Ok(assets)
}

fn capture_presentation_keyframe(
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    sequence: u32,
    context: &AdapterContext<'_>,
) -> Result<Vec<PresentationAsset>> {
    let mut assets = Vec::new();
    for session in sessions {
        let Some(capture) = session.presentation_capture() else {
            continue;
        };
        capture.attach(context)?;
        let result = capture.capture_keyframe(sequence, context);
        let detach = capture.detach(context);
        assets.extend(result?);
        detach?;
    }
    Ok(assets)
}

fn capture_archive_keyframe(
    sequence: u32,
    archive: &mut PresentationArchive,
    realization: &mut Option<Box<dyn Realization>>,
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) {
    let result = match realization.as_deref_mut() {
        Some(realization) => realization
            .capture_presentation_keyframe(sequence)
            .map_err(anyhow::Error::from),
        None => capture_presentation_keyframe(sessions, sequence, context),
    };
    match result {
        Ok(assets) => {
            eprintln!(
                "presentation keyframe {sequence}: collected {} bounded asset(s)",
                assets.len()
            );
            for asset in assets {
                archive.ingest(asset);
            }
        }
        Err(error) => {
            eprintln!("presentation keyframe {sequence} unavailable: {error:#}");
        }
    }
}

fn drain_archive_keyframes(
    requests: &Arc<Mutex<VecDeque<u32>>>,
    archive: &mut PresentationArchive,
    realization: &mut Option<Box<dyn Realization>>,
    sessions: &mut [Box<dyn ato_adapter_api::AttachedAdapter>],
    context: &AdapterContext<'_>,
) {
    loop {
        let sequence = match requests.lock() {
            Ok(mut requests) => requests.pop_front(),
            Err(_) => {
                eprintln!("presentation keyframe queue was poisoned");
                return;
            }
        };
        let Some(sequence) = sequence else {
            return;
        };
        capture_archive_keyframe(sequence, archive, realization, sessions, context);
    }
}

fn persist_presentation_assets(
    repository: &LocalCapsuleRepository,
    token: &str,
    assets: &[ato_adapter_api::PresentationAsset],
) -> Result<Vec<CapturePresentationAsset>> {
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let relative_directory = PathBuf::from("runs").join(format!("presentation-{token}"));
    let directory = repository.root().join(&relative_directory);
    fs::create_dir_all(&directory)?;
    let mut persisted = Vec::with_capacity(assets.len());
    for (index, asset) in assets.iter().enumerate() {
        if asset.bytes.is_empty() || asset.bytes.len() > MAX_PRESENTATION_ASSET_BYTES {
            bail!("presentation asset is outside the bounded size contract")
        }
        let kind = match asset.kind {
            ato_adapter_api::PresentationKind::FinalState => "final_state",
            ato_adapter_api::PresentationKind::ArchiveKeyframe => "archive_keyframe",
            ato_adapter_api::PresentationKind::TerminalFinal => "terminal_final",
        };
        let extension = match asset.content_type.as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            "application/vnd.ato.terminal-screen+json" => "json",
            _ => bail!("unsupported presentation content type"),
        };
        let relative_path = relative_directory.join(format!("{index:03}-{kind}.{extension}"));
        atomic_control_write(&repository.root().join(&relative_path), &asset.bytes)?;
        persisted.push(CapturePresentationAsset {
            kind: kind.to_owned(),
            content_type: asset.content_type.clone(),
            width: asset.width,
            height: asset.height,
            sequence: asset.sequence,
            path: relative_path,
        });
    }
    Ok(persisted)
}

fn resolve_capture_asset_paths(
    repository_root: &Path,
    token: &str,
    assets: Vec<CapturePresentationAsset>,
) -> Result<Vec<CapturePresentationAsset>> {
    let expected_directory = format!("presentation-{token}");
    assets
        .into_iter()
        .map(|mut asset| {
            let components: Vec<_> = asset.path.components().collect();
            let safe = components.len() == 3
                && matches!(components[0], std::path::Component::Normal(value) if value == "runs")
                && matches!(components[1], std::path::Component::Normal(value) if value == expected_directory.as_str())
                && matches!(components[2], std::path::Component::Normal(_));
            if !safe {
                bail!("capture worker returned an unsafe presentation asset path")
            }
            asset.path = repository_root.join(&asset.path);
            Ok(asset)
        })
        .collect()
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
    activation_gate: Option<Arc<GatedObservationSink>>,
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
            activation_gate: None,
            isolated_processes,
        }
    }
}

pub(crate) struct PortableContinuationCapture {
    driver: CliRealizationDriver,
    head: Arc<Mutex<ComputationRef>>,
    project: std::path::PathBuf,
    token: String,
}

impl PortableContinuationCapture {
    pub(crate) const BRANCH: &'static str = "continued";

    pub(crate) fn begin(
        repository: &LocalCapsuleRepository,
        head: &ComputationRef,
        bindings: &BTreeMap<String, String>,
    ) -> Result<Self> {
        repository.create_branch(Self::BRANCH, head, None)?;
        let token = format!("portable-{}-{}", std::process::id(), observed_nanos());
        let starting = ActiveRun {
            token: token.clone(),
            branch: Self::BRANCH.to_owned(),
            branch_base: head.clone(),
            head: head.clone(),
            record_seq: 0,
            pid: std::process::id(),
            process_start_time: "portable-foreground".to_owned(),
            process_group: 0,
            boot_session: String::new(),
            status: "starting".to_owned(),
        };
        repository.claim_active_run(&starting)?;
        let mut active = starting;
        active.status = "active".to_owned();
        if let Err(error) = repository.activate_run(&token, &active) {
            let _ = repository.release_active_run(&token);
            return Err(error.into());
        }

        let live_head = Arc::new(Mutex::new(head.clone()));
        let keyframe_requests = Arc::new(Mutex::new(VecDeque::new()));
        let sink: Arc<dyn ObservationSink> = Arc::new(RepositoryObservationSink {
            project: repository.project().to_path_buf(),
            branch: Self::BRANCH.to_owned(),
            token: token.clone(),
            head: Arc::clone(&live_head),
            keyframe_requests,
            presentation_archive: Arc::new(Mutex::new(PresentationArchive::default())),
            keyframe_captures: Arc::new(Mutex::new(Vec::new())),
        });
        let gate = Arc::new(GatedObservationSink {
            enabled: AtomicBool::new(false),
            inner: sink,
        });
        let driver = CliRealizationDriver {
            project: repository.project().to_path_buf(),
            bindings: bindings.clone(),
            observations: gate.clone(),
            activation_gate: Some(gate),
            isolated_processes: true,
        };
        Ok(Self {
            driver,
            head: live_head,
            project: repository.project().to_path_buf(),
            token,
        })
    }

    pub(crate) fn driver(&self) -> &CliRealizationDriver {
        &self.driver
    }

    pub(crate) fn finish(self) -> Result<ComputationRef> {
        let head = self
            .head
            .lock()
            .map_err(|_| anyhow::anyhow!("portable continuation head lock was poisoned"))?
            .clone();
        LocalCapsuleRepository::open(&self.project)?.release_active_run(&self.token)?;
        Ok(head)
    }
}

impl Drop for PortableContinuationCapture {
    fn drop(&mut self) {
        if let Ok(repository) = LocalCapsuleRepository::open(&self.project) {
            let _ = repository.release_active_run(&self.token);
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
            activation_gate: self.activation_gate.clone(),
        }))
    }
}

struct CliReplayRuntime {
    project: std::path::PathBuf,
    sessions: Vec<Box<dyn ato_adapter_api::AttachedAdapter>>,
    activation_gate: Option<Arc<GatedObservationSink>>,
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

    fn abort(&mut self) -> Result<(), MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        quiesce_and_detach(
            &mut self.sessions,
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
            activation_gate: self.activation_gate,
        }))
    }
}

struct CliRealization {
    project: std::path::PathBuf,
    sessions: Vec<Box<dyn ato_adapter_api::AttachedAdapter>>,
    target: ComputationRef,
    activation_gate: Option<Arc<GatedObservationSink>>,
}

impl Realization for CliRealization {
    fn target(&self) -> &ComputationRef {
        &self.target
    }

    fn verification(&self) -> RealizationVerification {
        RealizationVerification::AppliedUnverified
    }

    fn activate(&mut self) -> Result<(), MaterializerError> {
        if let Some(gate) = &self.activation_gate {
            gate.enabled.store(true, Ordering::Release);
        }
        for session in &mut self.sessions {
            if let Err(error) = session.activate() {
                if let Some(gate) = &self.activation_gate {
                    gate.enabled.store(false, Ordering::Release);
                }
                return Err(materializer_operation(error));
            }
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
        let result = quiesce_and_detach(&mut self.sessions, &context);
        if let Some(gate) = &self.activation_gate {
            gate.enabled.store(false, Ordering::Release);
        }
        result.map_err(materializer_operation)
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

    fn capture_final_presentation(
        &mut self,
    ) -> Result<Vec<ato_adapter_api::PresentationAsset>, MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        capture_final_presentation(
            &mut self.sessions,
            &AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
        )
        .map_err(materializer_operation)
    }

    fn capture_presentation_keyframe(
        &mut self,
        sequence: u32,
    ) -> Result<Vec<PresentationAsset>, MaterializerError> {
        let repository =
            LocalCapsuleRepository::open(&self.project).map_err(materializer_operation)?;
        capture_presentation_keyframe(
            &mut self.sessions,
            sequence,
            &AdapterContext {
                workspace: &self.project,
                objects: repository.objects(),
            },
        )
        .map_err(materializer_operation)
    }

    fn presentation_keyframe_captures(&self) -> Vec<Arc<dyn PresentationKeyframeCapture>> {
        self.sessions
            .iter()
            .filter_map(|session| session.presentation_keyframe_capture())
            .collect()
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
    if let Some(session) = sessions.iter().find(|session| {
        session.capabilities().capture_consistency
            == ato_adapter_api::CaptureConsistency::Unsupported
    }) {
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
    keyframe_requests: Arc<Mutex<VecDeque<u32>>>,
    presentation_archive: Arc<Mutex<PresentationArchive>>,
    keyframe_captures: Arc<Mutex<Vec<Arc<dyn PresentationKeyframeCapture>>>>,
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
        let presentation_hint = observation.presentation_hint;
        let committed = repository
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
        drop(head);
        if presentation_hint == PresentationHint::Keyframe
            && let Ok(sequence) = u32::try_from(committed.id.seq)
        {
            let captures = self
                .keyframe_captures
                .lock()
                .map_err(|_| {
                    AdapterError::Operation(
                        "presentation keyframe handles were poisoned".to_owned(),
                    )
                })?
                .clone();
            let mut captured_any = false;
            for capture in captures {
                match capture.capture_keyframe(sequence) {
                    Ok(assets) => {
                        captured_any |= !assets.is_empty();
                        let mut archive = self.presentation_archive.lock().map_err(|_| {
                            AdapterError::Operation("presentation archive was poisoned".to_owned())
                        })?;
                        for asset in assets {
                            archive.ingest(asset);
                        }
                    }
                    Err(error) => {
                        eprintln!("presentation keyframe {sequence} unavailable: {error}");
                    }
                }
            }
            if !captured_any {
                let mut requests = self.keyframe_requests.lock().map_err(|_| {
                    AdapterError::Operation("presentation keyframe queue was poisoned".to_owned())
                })?;
                if requests.len() < MAX_ARCHIVE_FRAMES.saturating_mul(2) {
                    requests.push_back(sequence);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u32, byte: u8) -> PresentationAsset {
        PresentationAsset {
            kind: PresentationKind::ArchiveKeyframe,
            content_type: "image/png".to_owned(),
            width: Some(100),
            height: Some(100),
            sequence,
            bytes: vec![byte],
        }
    }

    #[test]
    fn visual_archive_is_deduplicated_bounded_and_retains_initial_and_latest() {
        let mut archive = PresentationArchive::default();
        archive.ingest(frame(0, 0));
        archive.ingest(frame(1, 0));
        for sequence in 1..=30 {
            archive.ingest(frame(sequence, sequence as u8));
        }
        assert_eq!(archive.assets.len(), MAX_ARCHIVE_FRAMES);
        assert_eq!(archive.assets.first().map(|asset| asset.sequence), Some(0));
        assert_eq!(archive.assets.last().map(|asset| asset.sequence), Some(30));
    }

    #[test]
    fn final_browser_state_is_also_an_archive_frontier_without_changing_bytes() {
        let mut archive = PresentationArchive::default();
        archive.ingest(frame(0, 7));
        let mut final_asset = frame(0, 7);
        final_asset.kind = PresentationKind::FinalState;
        let assets = archive.capture_assets(vec![final_asset], 42);
        assert_eq!(assets.len(), 3);
        assert_eq!(assets[0].kind, PresentationKind::ArchiveKeyframe);
        assert_eq!(assets[1].kind, PresentationKind::ArchiveKeyframe);
        assert_eq!(assets[2].kind, PresentationKind::FinalState);
        assert_eq!(assets[0].sequence, 0);
        assert_eq!(assets[1].sequence, 42);
        assert_eq!(assets[2].sequence, 42);
        assert_eq!(assets[0].bytes, assets[1].bytes);
        assert_eq!(assets[1].bytes, assets[2].bytes);
        assert_eq!(archive.digests.len(), 1);
    }

    #[test]
    fn capture_asset_locator_is_namespace_neutral_and_host_resolved() {
        let asset = CapturePresentationAsset {
            kind: "terminal_final".to_owned(),
            content_type: "application/vnd.ato.terminal-screen+json".to_owned(),
            width: None,
            height: None,
            sequence: 4,
            path: PathBuf::from("runs/presentation-token/000-terminal_final.json"),
        };
        let [resolved] = resolve_capture_asset_paths(
            Path::new("/host/repository/.capsule"),
            "token",
            vec![asset.clone()],
        )
        .unwrap()
        .try_into()
        .unwrap();
        assert_eq!(
            resolved.path,
            Path::new("/host/repository/.capsule/runs/presentation-token/000-terminal_final.json")
        );

        for unsafe_path in [
            "/workspace/.capsule/runs/presentation-token/000-terminal_final.json",
            "runs/presentation-token/../credential",
            "runs/presentation-other/000-terminal_final.json",
        ] {
            let mut unsafe_asset = asset.clone();
            unsafe_asset.path = PathBuf::from(unsafe_path);
            assert!(
                resolve_capture_asset_paths(
                    Path::new("/host/repository/.capsule"),
                    "token",
                    vec![unsafe_asset],
                )
                .is_err()
            );
        }
    }
}
