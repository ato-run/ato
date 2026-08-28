//! Activity product-runtime controller for the connected worker.
//!
//! This module owns Room/WebRTC/media orchestration only. Browser interaction
//! still crosses the generic `ato.browser@1` ingress and its Evolution/Record
//! ordering before an Activity receipt is emitted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use ato_adapter_browser::{
    BrowserEvent, BrowserSurfaceProjectionV1, OperationSource, SurfaceOperationDescriptorV1,
};
use ato_browser_semantics::BrowserOperationRetryStage;
use ato_kernel::{AcceptedOperation, EvolutionError};

use super::BrowserControlIngress;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECEIPTS: usize = 1024;
const MAX_ABORT_REQUESTS: usize = 1024;
const MAX_CONCURRENT_CONTROLLER_REQUESTS: usize = 32;
const OPERATION_JOURNAL_VERSION: u8 = 1;
const BROWSER_PROTOCOL: &str = "ato.browser@1";
const WEBMCP_PROTOCOL: &str = "ato.webmcp@1";
const CONTROLLER_HTML: &str = include_str!("activity_controller.html");

#[derive(Debug)]
pub(crate) enum ActivityControllerEvent {
    Ready,
    Ended,
    Failed,
    AbortRequested {
        operation_id: String,
        result: Sender<bool>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityControllerPageConfig {
    pub run_id: String,
    pub room_url: String,
    pub executor_credential: String,
    pub ice_servers: Value,
}

pub(crate) struct ActivityControllerServer {
    target_url: String,
    context: Arc<ActivityControllerContext>,
    events: Receiver<ActivityControllerEvent>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ActivityControllerServer {
    pub(crate) fn start(
        config: ActivityControllerPageConfig,
        ingress: Arc<dyn BrowserControlIngress>,
        journal_root: &Path,
    ) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .context("bind Activity controller")?;
        listener
            .set_nonblocking(true)
            .context("configure Activity controller")?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let secret = random_secret();
        let nonce = random_secret();
        let bootstrap_path = format!("/bootstrap/{}", random_secret());
        let target_url = format!("{origin}{bootstrap_path}");
        let room_origin = websocket_origin(&config.room_url)?;
        let html = controller_html(&config, &secret, &nonce)?;
        let csp = format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self' {room_origin}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        );
        let stopping = Arc::new(AtomicBool::new(false));
        let (event_tx, events) = mpsc::channel();
        let receipts = ReceiptCache::durable(journal_root, random_secret())?;
        let context = Arc::new(ActivityControllerContext {
            html,
            csp,
            secret,
            bootstrap_path,
            run_id: config.run_id,
            events: event_tx,
            frame: Mutex::new(None),
            receipts: Mutex::new(receipts),
            receipt_changed: Condvar::new(),
            surface: Mutex::new(None),
            abort_requests: Mutex::new(BTreeSet::new()),
            active_operations: Mutex::new(BTreeMap::new()),
            ingress,
        });
        let thread_context = Arc::clone(&context);
        let thread_stopping = Arc::clone(&stopping);
        let thread = thread::spawn(move || serve(listener, thread_context, thread_stopping));
        Ok(Self {
            target_url,
            context,
            events,
            stopping,
            thread: Some(thread),
        })
    }

    pub(crate) fn target_url(&self) -> &str {
        &self.target_url
    }

    pub(crate) fn publish_frame(&self, frame: Vec<u8>) -> Result<()> {
        ensure!(
            !frame.is_empty() && frame.len() <= MAX_FRAME_BYTES,
            "Activity presentation frame exceeds bound"
        );
        *self
            .context
            .frame
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity frame mutex poisoned"))? = Some(frame);
        Ok(())
    }

    pub(crate) fn publish_surface(
        &self,
        projection: BrowserSurfaceProjectionV1,
        registry_generation: u64,
        document_token: String,
    ) -> Result<()> {
        ensure!(
            projection.observation.target_run_id == self.context.run_id
                && projection.observation.surface_epoch > 0
                && registry_generation > 0
                && !document_token.is_empty()
                && document_token.len() <= 256,
            "Activity surface escaped its Run scope"
        );
        *self
            .context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))? =
            Some(PublishedSurface {
                projection,
                registry_generation,
                document_token,
            });
        Ok(())
    }

    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<ActivityControllerEvent, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub(crate) fn stop(mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Activity controller thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ActivityControllerServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostedRunInput {
    run_id: String,
    operation_id: String,
    client_seq: u64,
    adapter_id: String,
    protocol_id: String,
    event: Value,
    actor_participant_id: String,
    source_connection_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostedOperationInput {
    operation_id: String,
    descriptor_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    #[serde(default)]
    target_run_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    surface_id: String,
    surface_epoch: u64,
    protocol_id: String,
    operation_name: String,
    #[serde(default)]
    arguments: Value,
    client_sequence: u64,
    #[serde(default)]
    actor_participant_id: Option<String>,
}

impl HostedOperationInput {
    fn target_run_id(&self) -> Result<&str> {
        match (self.target_run_id.as_deref(), self.run_id.as_deref()) {
            (Some(target), None) | (None, Some(target)) => Ok(target),
            (Some(target), Some(run)) if target == run => Ok(target),
            _ => bail!("Activity operation target Run is ambiguous"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedAbortInput {
    operation_id: String,
    descriptor_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    #[serde(default)]
    target_run_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    surface_id: String,
    surface_epoch: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivityOperationReceipt {
    #[serde(skip_serializing_if = "Option::is_none")]
    run_sequence: Option<u64>,
    operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_epoch: Option<u64>,
    client_sequence: u64,
    result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    output: Value,
    applied_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct ActivityAbortReceipt {
    operation_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    target_run_id: String,
    surface_id: String,
    surface_epoch: u64,
    status: &'static str,
    best_effort_result: String,
    requested_at: String,
}

struct ReceiptCache {
    legacy_by_id: BTreeMap<String, (Vec<u8>, ActivityOperationReceipt)>,
    controller_by_id: BTreeMap<String, ControllerReceiptEntry>,
    insertion_order: VecDeque<String>,
    journal: Option<OperationJournal>,
    incarnation_id: String,
}

impl ReceiptCache {
    fn durable(root: &Path, incarnation_id: String) -> Result<Self> {
        let journal = OperationJournal::open(root)?;
        let mut controller_by_id = BTreeMap::new();
        let mut insertion_order = VecDeque::new();
        for entry in journal.load()? {
            ensure!(
                controller_by_id.len() < MAX_RECEIPTS,
                "Activity operation journal exceeds lease bound"
            );
            ensure!(
                controller_by_id
                    .insert(entry.operation_id.clone(), entry.clone().into_cache_entry())
                    .is_none(),
                "Activity operation journal contains duplicate ids"
            );
            insertion_order.push_back(entry.operation_id);
        }
        Ok(Self {
            legacy_by_id: BTreeMap::new(),
            controller_by_id,
            insertion_order,
            journal: Some(journal),
            incarnation_id,
        })
    }

    fn get(&self, operation_id: &str, payload: &[u8]) -> Result<Option<ActivityOperationReceipt>> {
        let Some((known, receipt)) = self.legacy_by_id.get(operation_id) else {
            return Ok(None);
        };
        ensure!(
            known == payload,
            "Activity operation id was reused with different input"
        );
        let mut duplicate = receipt.clone();
        duplicate.result = "duplicate".to_owned();
        Ok(Some(duplicate))
    }

    fn insert(
        &mut self,
        operation_id: String,
        payload: Vec<u8>,
        receipt: ActivityOperationReceipt,
    ) {
        if self.legacy_by_id.contains_key(&operation_id) {
            return;
        }
        self.insertion_order.push_back(operation_id.clone());
        self.legacy_by_id.insert(operation_id, (payload, receipt));
        while self.insertion_order.len() > MAX_RECEIPTS {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.legacy_by_id.remove(&oldest);
            }
        }
    }

    fn controller_lookup(
        &self,
        operation_id: &str,
        request_digest: &str,
        provenance: &ControllerOperationProvenance,
    ) -> Result<ControllerLookup> {
        let Some(entry) = self.controller_by_id.get(operation_id) else {
            return Ok(ControllerLookup::Missing);
        };
        ensure!(
            entry.request_digest == request_digest && entry.provenance == *provenance,
            "Activity operation id was reused with different input"
        );
        match &entry.phase {
            ControllerReceiptPhase::Started { incarnation_id }
                if incarnation_id == &self.incarnation_id =>
            {
                Ok(ControllerLookup::InFlight)
            }
            ControllerReceiptPhase::Started { .. } => Ok(ControllerLookup::Indeterminate),
            ControllerReceiptPhase::Retryable { retry, .. } => {
                Ok(ControllerLookup::Retryable(retry.clone()))
            }
            ControllerReceiptPhase::SettlementPending { receipt } => {
                Ok(ControllerLookup::SettlementPending(receipt.clone()))
            }
            // The persisted receipt is the Runner's terminal evidence. Replay
            // it byte-for-byte at the semantic field level: rewriting an
            // aborted/failed result as `duplicate` both loses the audit fact
            // and incorrectly turns a receipt without a Run sequence into a
            // successful one.
            ControllerReceiptPhase::Settled { receipt } => {
                Ok(ControllerLookup::Settled(receipt.clone()))
            }
        }
    }

    fn begin_controller_operation(
        &mut self,
        operation_id: String,
        request_digest: String,
        provenance: ControllerOperationProvenance,
    ) -> Result<ControllerLookup> {
        match self.controller_lookup(&operation_id, &request_digest, &provenance)? {
            ControllerLookup::Missing => {}
            known => return Ok(known),
        }
        ensure!(
            self.controller_by_id.len() < MAX_RECEIPTS,
            "Activity operation journal reached its lease bound"
        );
        let entry = ControllerReceiptEntry {
            request_digest,
            provenance,
            phase: ControllerReceiptPhase::Started {
                incarnation_id: self.incarnation_id.clone(),
            },
        };
        if let Some(journal) = &self.journal {
            journal.persist(&DurableOperationEntry::from_cache_entry(
                operation_id.clone(),
                &entry,
            ))?;
        }
        self.insertion_order.push_back(operation_id.clone());
        self.controller_by_id.insert(operation_id, entry);
        Ok(ControllerLookup::Owner)
    }

    fn settle_controller_operation(
        &mut self,
        operation_id: &str,
        request_digest: &str,
        provenance: &ControllerOperationProvenance,
        receipt: ActivityOperationReceipt,
    ) -> Result<()> {
        let known = self
            .controller_by_id
            .get(operation_id)
            .context("Activity operation was not started before settlement")?;
        ensure!(
            known.request_digest == request_digest && known.provenance == *provenance,
            "Activity operation id was reused with different input"
        );
        if matches!(known.phase, ControllerReceiptPhase::Settled { .. }) {
            return Ok(());
        }
        let entry = ControllerReceiptEntry {
            request_digest: request_digest.to_owned(),
            provenance: provenance.clone(),
            phase: ControllerReceiptPhase::SettlementPending {
                receipt: receipt.clone(),
            },
        };
        self.controller_by_id
            .insert(operation_id.to_owned(), entry.clone());
        if let Some(journal) = &self.journal {
            journal.persist(&DurableOperationEntry::from_cache_entry(
                operation_id.to_owned(),
                &entry,
            ))?;
        }
        self.controller_by_id.insert(
            operation_id.to_owned(),
            ControllerReceiptEntry {
                phase: ControllerReceiptPhase::Settled { receipt },
                ..entry
            },
        );
        Ok(())
    }

    fn claim_retryable_controller_operation(
        &mut self,
        operation_id: &str,
        request_digest: &str,
        provenance: &ControllerOperationProvenance,
    ) -> Result<bool> {
        let Some(entry) = self.controller_by_id.get_mut(operation_id) else {
            return Ok(false);
        };
        ensure!(
            entry.request_digest == request_digest && entry.provenance == *provenance,
            "Activity operation id was reused with different input"
        );
        let ControllerReceiptPhase::Retryable { .. } = entry.phase else {
            return Ok(false);
        };
        entry.phase = ControllerReceiptPhase::Started {
            incarnation_id: self.incarnation_id.clone(),
        };
        Ok(true)
    }

    fn mark_controller_operation_retryable(
        &mut self,
        operation_id: &str,
        request_digest: &str,
        provenance: &ControllerOperationProvenance,
        retry: RetryableControllerOperation,
    ) -> Result<()> {
        let durable = {
            let entry = self
                .controller_by_id
                .get_mut(operation_id)
                .context("Activity operation retry lost its durable intent")?;
            ensure!(
                entry.request_digest == request_digest && entry.provenance == *provenance,
                "Activity operation id was reused with different input"
            );
            ensure!(
                matches!(entry.phase, ControllerReceiptPhase::Started { .. }),
                "Activity operation retry is not owned"
            );
            // The event/arguments stay memory-only. Disk records whether the
            // physical boundary was crossed and whether abort raced after it,
            // so crash recovery remains fail-closed without storing page data.
            entry.phase = ControllerReceiptPhase::Retryable {
                incarnation_id: self.incarnation_id.clone(),
                retry,
            };
            DurableOperationEntry::from_cache_entry(operation_id.to_owned(), entry)
        };
        if let Some(journal) = &self.journal {
            journal.persist(&durable)?;
        }
        Ok(())
    }

    fn controller_operation_for_abort(
        &self,
        input: &HostedAbortInput,
        target_run_id: &str,
    ) -> Result<AbortOperationState> {
        let Some(entry) = self.controller_by_id.get(&input.operation_id) else {
            return Ok(AbortOperationState::Missing);
        };
        ensure!(
            entry.provenance.matches_abort(input, target_run_id),
            "invalid_operation"
        );
        Ok(match entry.phase {
            ControllerReceiptPhase::Started { .. } => AbortOperationState::Started,
            ControllerReceiptPhase::Retryable { .. } => AbortOperationState::Started,
            ControllerReceiptPhase::SettlementPending { .. }
            | ControllerReceiptPhase::Settled { .. } => AbortOperationState::Settled,
        })
    }
}

impl Default for ReceiptCache {
    fn default() -> Self {
        Self {
            legacy_by_id: BTreeMap::new(),
            controller_by_id: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            journal: None,
            incarnation_id: random_secret(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ControllerOperationProvenance {
    descriptor_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    target_run_id: String,
    surface_id: String,
    surface_epoch: u64,
    protocol_id: String,
    operation_name: String,
    client_sequence: u64,
    actor_participant_id: Option<String>,
}

impl ControllerOperationProvenance {
    fn from_input(input: &HostedOperationInput, target_run_id: String) -> Self {
        Self {
            descriptor_id: input.descriptor_id.clone(),
            actor_id: input.actor_id.clone(),
            actor_run_id: input.actor_run_id.clone(),
            controller_session_id: input.controller_session_id.clone(),
            controller_epoch: input.controller_epoch,
            target_run_id,
            surface_id: input.surface_id.clone(),
            surface_epoch: input.surface_epoch,
            protocol_id: input.protocol_id.clone(),
            operation_name: input.operation_name.clone(),
            client_sequence: input.client_sequence,
            actor_participant_id: input.actor_participant_id.clone(),
        }
    }

    fn matches_abort(&self, input: &HostedAbortInput, target_run_id: &str) -> bool {
        self.descriptor_id == input.descriptor_id
            && self.actor_id == input.actor_id
            && self.actor_run_id == input.actor_run_id
            && self.controller_session_id == input.controller_session_id
            && self.controller_epoch == input.controller_epoch
            && self.target_run_id == target_run_id
            && self.surface_id == input.surface_id
            && self.surface_epoch == input.surface_epoch
    }
}

#[derive(Debug, Clone)]
struct ControllerReceiptEntry {
    request_digest: String,
    provenance: ControllerOperationProvenance,
    phase: ControllerReceiptPhase,
}

#[derive(Debug, Clone)]
enum ControllerReceiptPhase {
    Started {
        incarnation_id: String,
    },
    Retryable {
        incarnation_id: String,
        retry: RetryableControllerOperation,
    },
    SettlementPending {
        receipt: ActivityOperationReceipt,
    },
    Settled {
        receipt: ActivityOperationReceipt,
    },
}

#[derive(Debug, Clone)]
struct RetryableControllerOperation {
    event: BrowserEvent,
    realization_generation: Option<String>,
    physically_applied: bool,
    abort_requested: bool,
}

enum ControllerLookup {
    Missing,
    Owner,
    InFlight,
    Indeterminate,
    Retryable(RetryableControllerOperation),
    SettlementPending(ActivityOperationReceipt),
    Settled(ActivityOperationReceipt),
}

enum AbortOperationState {
    Missing,
    Started,
    Settled,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableOperationEntry {
    version: u8,
    operation_id: String,
    request_digest: String,
    provenance: ControllerOperationProvenance,
    phase: DurableOperationPhase,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum DurableOperationPhase {
    Started {
        incarnation_id: String,
        stage: DurableStartedStage,
        abort_requested: bool,
    },
    Settled {
        receipt: Box<ActivityOperationReceipt>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DurableStartedStage {
    BeforeApply,
    PhysicalCommitPending,
}

impl DurableOperationEntry {
    fn from_cache_entry(operation_id: String, entry: &ControllerReceiptEntry) -> Self {
        Self {
            version: OPERATION_JOURNAL_VERSION,
            operation_id,
            request_digest: entry.request_digest.clone(),
            provenance: entry.provenance.clone(),
            phase: match &entry.phase {
                ControllerReceiptPhase::Started { incarnation_id } => {
                    DurableOperationPhase::Started {
                        incarnation_id: incarnation_id.clone(),
                        stage: DurableStartedStage::BeforeApply,
                        abort_requested: false,
                    }
                }
                ControllerReceiptPhase::Retryable {
                    incarnation_id,
                    retry,
                } => DurableOperationPhase::Started {
                    incarnation_id: incarnation_id.clone(),
                    stage: if retry.physically_applied {
                        DurableStartedStage::PhysicalCommitPending
                    } else {
                        DurableStartedStage::BeforeApply
                    },
                    abort_requested: retry.abort_requested,
                },
                ControllerReceiptPhase::SettlementPending { receipt }
                | ControllerReceiptPhase::Settled { receipt } => DurableOperationPhase::Settled {
                    receipt: Box::new(receipt.clone()),
                },
            },
        }
    }

    fn into_cache_entry(self) -> ControllerReceiptEntry {
        ControllerReceiptEntry {
            request_digest: self.request_digest,
            provenance: self.provenance,
            phase: match self.phase {
                DurableOperationPhase::Started { incarnation_id, .. } => {
                    ControllerReceiptPhase::Started { incarnation_id }
                }
                DurableOperationPhase::Settled { receipt } => {
                    ControllerReceiptPhase::Settled { receipt: *receipt }
                }
            },
        }
    }
}

struct OperationJournal {
    root: PathBuf,
}

impl OperationJournal {
    fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("create Activity operation journal {}", root.display()))?;
        ensure!(
            fs::symlink_metadata(root)?.file_type().is_dir(),
            "Activity operation journal root is not a directory"
        );
        set_owner_only_directory(root)?;
        Ok(Self {
            root: root.to_owned(),
        })
    }

    fn load(&self) -> Result<Vec<DurableOperationEntry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            ensure!(
                item.file_type()?.is_file(),
                "Activity operation journal contains a non-file entry"
            );
            let bytes = fs::read(&path)
                .with_context(|| format!("read Activity operation journal {}", path.display()))?;
            ensure!(
                bytes.len() <= MAX_BODY_BYTES,
                "Activity operation journal entry exceeds bound"
            );
            let entry: DurableOperationEntry = serde_json::from_slice(&bytes)
                .with_context(|| format!("decode Activity operation journal {}", path.display()))?;
            entry.validate(&path)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn persist(&self, entry: &DurableOperationEntry) -> Result<()> {
        let path = self
            .root
            .join(operation_journal_filename(&entry.operation_id));
        let bytes = serde_jcs::to_vec(entry)?;
        ensure!(
            bytes.len() <= MAX_BODY_BYTES,
            "Activity operation journal entry exceeds bound"
        );
        atomic_write_owner_only(&self.root, &path, &bytes)
    }
}

impl DurableOperationEntry {
    fn validate(&self, path: &Path) -> Result<()> {
        ensure!(
            self.version == OPERATION_JOURNAL_VERSION,
            "unsupported Activity operation journal version"
        );
        ensure!(
            !self.operation_id.is_empty()
                && self.operation_id.len() <= 160
                && self
                    .operation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
                && self.request_digest.len() == 64
                && self
                    .request_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "invalid Activity operation journal entry"
        );
        ensure!(
            path.file_name().and_then(|value| value.to_str())
                == Some(operation_journal_filename(&self.operation_id).as_str()),
            "Activity operation journal filename mismatch"
        );
        if let DurableOperationPhase::Settled { receipt } = &self.phase {
            ensure!(
                receipt.operation_id == self.operation_id
                    && receipt.actor_id.as_deref() == Some(self.provenance.actor_id.as_str())
                    && receipt.actor_run_id.as_deref()
                        == Some(self.provenance.actor_run_id.as_str())
                    && receipt.controller_session_id.as_deref()
                        == Some(self.provenance.controller_session_id.as_str())
                    && receipt.controller_epoch == Some(self.provenance.controller_epoch)
                    && receipt.target_run_id.as_deref()
                        == Some(self.provenance.target_run_id.as_str())
                    && receipt.surface_id.as_deref() == Some(self.provenance.surface_id.as_str())
                    && receipt.surface_epoch == Some(self.provenance.surface_epoch)
                    && receipt.client_sequence == self.provenance.client_sequence,
                "Activity operation journal receipt mismatch"
            );
        } else if let DurableOperationPhase::Started { incarnation_id, .. } = &self.phase {
            ensure!(
                !incarnation_id.is_empty(),
                "Activity operation journal incarnation is empty"
            );
        }
        Ok(())
    }
}

fn operation_journal_filename(operation_id: &str) -> String {
    format!(
        "{}.json",
        hex::encode(Sha256::digest(operation_id.as_bytes()))
    )
}

fn operation_request_digest(input: &HostedOperationInput) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_jcs::to_vec(input)?)))
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn atomic_write_owner_only(root: &Path, destination: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = root.join(format!(".{}.tmp", random_secret()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, destination)?;
        File::open(root)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn abort_matches_invocation(
    invocation: &HostedOperationInput,
    input: &HostedAbortInput,
    target_run_id: &str,
) -> bool {
    invocation.operation_id == input.operation_id
        && invocation.descriptor_id == input.descriptor_id
        && invocation.actor_id == input.actor_id
        && invocation.actor_run_id == input.actor_run_id
        && invocation.controller_session_id == input.controller_session_id
        && invocation.controller_epoch == input.controller_epoch
        && invocation.target_run_id().ok() == Some(target_run_id)
        && invocation.surface_id == input.surface_id
        && invocation.surface_epoch == input.surface_epoch
}

struct ActivityControllerContext {
    html: String,
    csp: String,
    secret: String,
    bootstrap_path: String,
    run_id: String,
    events: Sender<ActivityControllerEvent>,
    frame: Mutex<Option<Vec<u8>>>,
    receipts: Mutex<ReceiptCache>,
    receipt_changed: Condvar,
    surface: Mutex<Option<PublishedSurface>>,
    abort_requests: Mutex<BTreeSet<String>>,
    active_operations: Mutex<BTreeMap<String, HostedOperationInput>>,
    ingress: Arc<dyn BrowserControlIngress>,
}

#[derive(Debug, Clone, Serialize)]
struct PublishedSurface {
    #[serde(flatten)]
    projection: BrowserSurfaceProjectionV1,
    #[serde(skip_serializing)]
    registry_generation: u64,
    /// Opaque main-world document incarnation used only to fence physical
    /// dispatch. It never crosses the Activity Room or semantic Run payload.
    #[serde(skip_serializing)]
    document_token: String,
}

fn serve(
    listener: TcpListener,
    context: Arc<ActivityControllerContext>,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    let mut requests: Vec<JoinHandle<()>> = Vec::new();
    while !stopping.load(Ordering::Acquire) {
        let mut index = 0;
        while index < requests.len() {
            if requests[index].is_finished() {
                let request = requests.swap_remove(index);
                let _ = request.join();
            } else {
                index += 1;
            }
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                if requests.len() >= MAX_CONCURRENT_CONTROLLER_REQUESTS {
                    // Loopback is not a trust boundary: an application page
                    // can probe this port and hold slow headers. Bound native
                    // request threads and fail excess connections closed.
                    continue;
                }
                let context = Arc::clone(&context);
                requests.push(thread::spawn(move || {
                    if handle_request(&mut stream, &context).is_err() {
                        let _ = respond_json(
                            &mut stream,
                            400,
                            &serde_json::json!({"error":"invalid_request"}),
                        );
                    }
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("accept Activity controller request"),
        }
    }
    for request in requests {
        let _ = request.join();
    }
    Ok(())
}

fn handle_request(stream: &mut TcpStream, context: &ActivityControllerContext) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == context.bootstrap_path {
        return respond(
            stream,
            200,
            "text/html; charset=utf-8",
            context.html.as_bytes(),
            &[("Content-Security-Policy", &context.csp)],
        );
    }
    if request.path == "/frame" && request.method == "GET" {
        authorize_host_request(&request, context)?;
        let frame = context
            .frame
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity frame mutex poisoned"))?
            .take();
        return match frame {
            Some(frame) => respond(stream, 200, "image/jpeg", &frame, &[]),
            None => respond(stream, 204, "application/octet-stream", &[], &[]),
        };
    }
    if request.path == "/surface" && request.method == "GET" {
        authorize_host_request(&request, context)?;
        let surface = context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
            .clone();
        return match surface {
            Some(surface) => respond_json(stream, 200, &surface),
            None => respond(stream, 204, "application/octet-stream", &[], &[]),
        };
    }
    authorize_host_request(&request, context)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/input") => {
            let input: HostedRunInput =
                serde_json::from_slice(&request.body).context("decode Activity Browser input")?;
            match apply_input(context, input) {
                Ok(receipt) => respond_json(stream, 200, &receipt),
                Err(error) => {
                    let code = stable_operation_error(&error);
                    respond_json(
                        stream,
                        if matches!(
                            code,
                            "head_persistence_pending"
                                | "operation_in_flight"
                                | "operation_queued_before_apply"
                                | "physical_commit_pending"
                        ) {
                            202
                        } else {
                            409
                        },
                        &serde_json::json!({"error":code}),
                    )
                }
            }
        }
        ("POST", "/operation/invoke") => {
            let input: HostedOperationInput = serde_json::from_slice(&request.body)
                .context("decode Activity operation invocation")?;
            match apply_operation(context, input) {
                Ok(receipt) => respond_json(stream, 200, &receipt),
                Err(error) => {
                    let code = stable_operation_error(&error);
                    respond_json(
                        stream,
                        if matches!(
                            code,
                            "head_persistence_pending"
                                | "operation_queued_before_apply"
                                | "physical_commit_pending"
                        ) {
                            202
                        } else {
                            409
                        },
                        &serde_json::json!({"error":code}),
                    )
                }
            }
        }
        ("POST", "/operation/abort") => {
            let input: HostedAbortInput =
                serde_json::from_slice(&request.body).context("decode Activity operation abort")?;
            match request_abort(context, input) {
                Ok(receipt) => respond_json(stream, 200, &receipt),
                Err(error) => respond_json(
                    stream,
                    409,
                    &serde_json::json!({"error":stable_operation_error(&error)}),
                ),
            }
        }
        ("POST", "/ready") => {
            let _ = context.events.send(ActivityControllerEvent::Ready);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/end") => {
            let _ = context.events.send(ActivityControllerEvent::Ended);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/failure") => {
            let body_text = String::from_utf8_lossy(&request.body).to_string();
            eprintln!("activity controller failure report: body={body_text}");
            let _ = context.events.send(ActivityControllerEvent::Failed);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        _ => respond_json(stream, 404, &serde_json::json!({"error":"not_found"})),
    }
}

fn authorize_host_request(
    request: &HttpRequest,
    context: &ActivityControllerContext,
) -> Result<()> {
    ensure!(
        request
            .headers
            .get("x-ato-activity-host")
            .map(String::as_str)
            == Some(context.secret.as_str()),
        "Activity controller authorization failed"
    );
    Ok(())
}

fn apply_input(
    context: &ActivityControllerContext,
    input: HostedRunInput,
) -> Result<ActivityOperationReceipt> {
    ensure!(
        input.run_id == context.run_id
            && input.adapter_id == BROWSER_PROTOCOL
            && input.protocol_id == BROWSER_PROTOCOL
            && !input.source_connection_id.is_empty()
            && !input.actor_participant_id.is_empty(),
        "Activity input escaped its Run scope"
    );
    let payload = serde_json::to_vec(&input)?;
    if let Some(receipt) = context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .get(&input.operation_id, &payload)?
    {
        return Ok(receipt);
    }
    let event_bytes = serde_json::to_vec(&input.event)?;
    let event =
        ato_adapter_browser::decode_event(&event_bytes).context("decode Activity Browser event")?;
    let accepted =
        accept_with_persistence_recovery(context, input.operation_id.clone(), event, None)?;
    let record_evidence_persisted = accepted.record_error.is_none();
    let receipt = ActivityOperationReceipt {
        run_sequence: Some(accepted.run_seq),
        operation_id: input.operation_id.clone(),
        actor_participant_id: Some(input.actor_participant_id),
        actor_id: None,
        actor_run_id: None,
        controller_session_id: None,
        controller_epoch: None,
        target_run_id: Some(context.run_id.clone()),
        surface_id: None,
        surface_epoch: None,
        client_sequence: input.client_seq,
        result: "applied".to_owned(),
        error: None,
        output: serde_json::json!({"record_evidence_persisted":record_evidence_persisted}),
        applied_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .insert(input.operation_id, payload, receipt.clone());
    Ok(receipt)
}

fn apply_operation(
    context: &ActivityControllerContext,
    input: HostedOperationInput,
) -> Result<ActivityOperationReceipt> {
    let target_run_id = input.target_run_id()?.to_owned();
    ensure!(
        target_run_id == context.run_id
            && !input.operation_id.is_empty()
            && !input.descriptor_id.is_empty()
            && !input.actor_id.is_empty()
            && !input.actor_run_id.is_empty()
            && !input.controller_session_id.is_empty()
            && input.controller_epoch > 0
            && input.client_sequence > 0,
        "Activity operation escaped its Controller scope"
    );
    let request_digest = operation_request_digest(&input)?;
    let provenance = ControllerOperationProvenance::from_input(&input, target_run_id.clone());

    // Durable terminal replay precedes current-surface validation. A Room may
    // redeliver an already-applied operation after navigation advanced the
    // surface epoch; only genuinely new work must match the current surface.
    let mut retained_abort_requested = false;
    let mut retrying_physical_commit = false;
    let (event, realization_generation) = loop {
        let lookup = context
            .receipts
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
            .controller_lookup(&input.operation_id, &request_digest, &provenance)?;
        let mut retrying_before_apply = false;
        let mut retry_before_apply_abort = false;
        match lookup {
            ControllerLookup::Settled(receipt) => return Ok(receipt),
            ControllerLookup::SettlementPending(receipt) => {
                let mut receipts = context
                    .receipts
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?;
                receipts
                    .settle_controller_operation(
                        &input.operation_id,
                        &request_digest,
                        &provenance,
                        receipt.clone(),
                    )
                    .context("head_persistence_pending")?;
                drop(receipts);
                context.receipt_changed.notify_all();
                return Ok(receipt);
            }
            ControllerLookup::Indeterminate => bail!("operation_indeterminate"),
            ControllerLookup::Retryable(retry) => {
                if retry.physically_applied {
                    let claimed = context
                        .receipts
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
                        .claim_retryable_controller_operation(
                            &input.operation_id,
                            &request_digest,
                            &provenance,
                        )?;
                    if claimed {
                        retained_abort_requested |= retry.abort_requested;
                        retrying_physical_commit = true;
                        break (retry.event, retry.realization_generation);
                    }
                    continue;
                } else {
                    retrying_before_apply = true;
                    retry_before_apply_abort = retry.abort_requested;
                }
            }
            ControllerLookup::InFlight => {
                let receipts = context
                    .receipts
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?;
                if !matches!(
                    receipts.controller_lookup(
                        &input.operation_id,
                        &request_digest,
                        &provenance
                    )?,
                    ControllerLookup::InFlight
                ) {
                    continue;
                }
                let (_receipts, timeout) = context
                    .receipt_changed
                    .wait_timeout(receipts, Duration::from_secs(30))
                    .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?;
                if timeout.timed_out() {
                    bail!("operation_in_flight");
                }
                continue;
            }
            ControllerLookup::Missing => {}
            ControllerLookup::Owner => unreachable!("lookup never creates an owner"),
        }

        let published = context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
            .clone()
            .context("stale_operation")?;
        ensure!(
            published.projection.observation.surface_id == input.surface_id
                && published.projection.observation.surface_epoch == input.surface_epoch,
            "stale_operation"
        );
        let descriptor = published
            .projection
            .operations
            .iter()
            .find(|descriptor| descriptor.id == input.descriptor_id)
            .context("stale_operation")?;
        ensure!(
            descriptor.protocol_id == input.protocol_id
                && descriptor.operation_name == input.operation_name,
            "stale_operation"
        );
        let event = operation_event(descriptor, &input.arguments, published.registry_generation)?;
        // Bind every fixed and WebMCP operation to the exact main-world
        // snapshot incarnation. Registry replacement invalidates fixed input
        // too, even before the worker's next 250ms projection poll.
        let realization_generation = Some(format!(
            "{}.{}",
            published.document_token, published.registry_generation
        ));
        if retrying_before_apply {
            let claimed = context
                .receipts
                .lock()
                .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
                .claim_retryable_controller_operation(
                    &input.operation_id,
                    &request_digest,
                    &provenance,
                )?;
            if claimed {
                retained_abort_requested |= retry_before_apply_abort;
                break (event, realization_generation);
            }
            continue;
        }
        let claim = context
            .receipts
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
            .begin_controller_operation(
                input.operation_id.clone(),
                request_digest.clone(),
                provenance.clone(),
            )?;
        match claim {
            ControllerLookup::Owner => break (event, realization_generation),
            // Another request won the durable intent race. Re-enter lookup so
            // it either joins or replays that exact operation.
            ControllerLookup::InFlight
            | ControllerLookup::Retryable(_)
            | ControllerLookup::SettlementPending(_)
            | ControllerLookup::Settled(_) => continue,
            ControllerLookup::Indeterminate => bail!("operation_indeterminate"),
            ControllerLookup::Missing => unreachable!("begin always records missing work"),
        }
    };

    let queued_abort = context
        .abort_requests
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity abort mutex poisoned"))?
        .remove(&input.operation_id);
    let aborted_before_dispatch =
        !retrying_physical_commit && (queued_abort || retained_abort_requested);
    retained_abort_requested |= queued_abort && retrying_physical_commit;
    let is_webmcp = matches!(&event, BrowserEvent::Operation { .. });
    if is_webmcp && !aborted_before_dispatch {
        use std::collections::btree_map::Entry;

        let mut active = context
            .active_operations
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity active operation mutex poisoned"))?;
        match active.entry(input.operation_id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(input.clone());
            }
            Entry::Occupied(_) => bail!("operation_in_flight"),
        }
    }
    let retry_event = event.clone();
    let retry_realization_generation = realization_generation.clone();
    let accepted = if aborted_before_dispatch {
        Err(anyhow::anyhow!("operation_aborted"))
    } else {
        accept_with_persistence_recovery(
            context,
            input.operation_id.clone(),
            event,
            realization_generation,
        )
    };
    if is_webmcp && !aborted_before_dispatch {
        context
            .active_operations
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity active operation mutex poisoned"))?
            .remove(&input.operation_id);
    }
    // Keep abort state and durable terminal settlement in one critical
    // section. A late abort therefore observes either the abort marker here or
    // the settled journal entry, never a gap between the two projections.
    let mut abort_requests = context
        .abort_requests
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity abort mutex poisoned"))?;
    let abort_requested = retained_abort_requested || abort_requests.remove(&input.operation_id);
    let (run_sequence, result, error, output) = match accepted {
        Ok(accepted) => (
            Some(accepted.run_seq),
            if abort_requested {
                "applied_after_abort_requested"
            } else {
                "applied"
            },
            None,
            serde_json::json!({
                "adapter_ack":true,
                "record_evidence_persisted":accepted.record_error.is_none()
            }),
        ),
        Err(error)
            if matches!(
                stable_operation_error(&error),
                "head_persistence_pending"
                    | "operation_in_flight"
                    | "operation_queued_before_apply"
                    | "physical_commit_pending"
            ) =>
        {
            context
                .receipts
                .lock()
                .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
                .mark_controller_operation_retryable(
                    &input.operation_id,
                    &request_digest,
                    &provenance,
                    RetryableControllerOperation {
                        event: retry_event,
                        realization_generation: retry_realization_generation,
                        physically_applied: stable_operation_error(&error)
                            == "physical_commit_pending",
                        abort_requested,
                    },
                )
                .context("head_persistence_pending")?;
            context.receipt_changed.notify_all();
            return Err(error);
        }
        Err(error) => {
            let code = if aborted_before_dispatch
                || (abort_requested && error.to_string().contains("aborted"))
            {
                "operation_aborted"
            } else {
                stable_operation_error(&error)
            };
            (
                None,
                if code == "operation_aborted" {
                    "aborted"
                } else {
                    "failed"
                },
                Some(code.to_owned()),
                serde_json::json!({"error":code}),
            )
        }
    };
    let receipt = ActivityOperationReceipt {
        run_sequence,
        operation_id: input.operation_id.clone(),
        actor_participant_id: provenance.actor_participant_id.clone(),
        actor_id: Some(provenance.actor_id.clone()),
        actor_run_id: Some(provenance.actor_run_id.clone()),
        controller_session_id: Some(provenance.controller_session_id.clone()),
        controller_epoch: Some(provenance.controller_epoch),
        target_run_id: Some(provenance.target_run_id.clone()),
        surface_id: Some(provenance.surface_id.clone()),
        surface_epoch: Some(provenance.surface_epoch),
        client_sequence: provenance.client_sequence,
        result: result.to_owned(),
        error,
        // Page-provided output never becomes an Ato instruction. Callers can
        // re-observe the surface to inspect resulting state.
        output,
        applied_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .settle_controller_operation(
            &input.operation_id,
            &request_digest,
            &provenance,
            receipt.clone(),
        )
        .context("head_persistence_pending")?;
    abort_requests.remove(&input.operation_id);
    drop(abort_requests);
    context.receipt_changed.notify_all();
    Ok(receipt)
}

fn accept_with_persistence_recovery(
    context: &ActivityControllerContext,
    operation_id: String,
    event: BrowserEvent,
    realization_generation: Option<String>,
) -> Result<AcceptedOperation> {
    for attempt in 0..5 {
        match context.ingress.accept_control_operation_in_context(
            operation_id.clone(),
            event.clone(),
            realization_generation.clone(),
        ) {
            Ok(accepted) => return Ok(accepted),
            Err(EvolutionError::Persist(_) | EvolutionError::PersistencePending(_)) => {
                match context.ingress.retry_control_persistence() {
                    Ok(Some((recovered_id, accepted))) if recovered_id == operation_id => {
                        return Ok(accepted);
                    }
                    // A concurrent operation may have recovered this pending
                    // transition first, or this retry may have recovered the
                    // preceding operation. Re-enter the same operation ID so
                    // the accepted cache (or the now-unblocked head) remains
                    // the source of truth.
                    Ok(_) => {}
                    Err(EvolutionError::Persist(_) | EvolutionError::PersistencePending(_)) => {}
                    Err(EvolutionError::Apply(message))
                        if message == "no Browser operation is pending" => {}
                    Err(error) => return Err(anyhow::anyhow!(error.to_string())),
                }
                if attempt < 4 {
                    thread::sleep(Duration::from_millis(20_u64 << attempt));
                }
            }
            Err(EvolutionError::Apply(message)) if message == "operation_in_flight" => {
                return Err(retry_stage_error(context, &operation_id));
            }
            Err(error) => return Err(anyhow::anyhow!(error.to_string())),
        }
    }
    // The final retry may have settled between the preceding calls. Always
    // perform one last idempotent lookup before exposing a non-terminal retry
    // response to the Room bridge.
    match context.ingress.accept_control_operation_in_context(
        operation_id.clone(),
        event,
        realization_generation,
    ) {
        Ok(accepted) => Ok(accepted),
        Err(EvolutionError::Persist(_) | EvolutionError::PersistencePending(_)) => {
            Err(retry_stage_error(context, &operation_id))
        }
        Err(EvolutionError::Apply(message)) if message == "operation_in_flight" => {
            Err(retry_stage_error(context, &operation_id))
        }
        Err(error) => Err(anyhow::anyhow!(error.to_string())),
    }
}

fn retry_stage_error(context: &ActivityControllerContext, operation_id: &str) -> anyhow::Error {
    match context.ingress.control_operation_retry_stage(operation_id) {
        BrowserOperationRetryStage::BeforeApply => {
            anyhow::anyhow!("operation_queued_before_apply")
        }
        BrowserOperationRetryStage::PhysicallyAppliedPendingCommit => {
            anyhow::anyhow!("physical_commit_pending")
        }
    }
}

fn operation_event(
    descriptor: &SurfaceOperationDescriptorV1,
    arguments: &Value,
    registry_generation: u64,
) -> Result<BrowserEvent> {
    if descriptor.source == OperationSource::Webmcp {
        ensure!(descriptor.protocol_id == WEBMCP_PROTOCOL, "stale_operation");
        return Ok(BrowserEvent::Operation {
            operation_name: descriptor.operation_name.clone(),
            arguments: arguments.clone(),
            surface_generation: registry_generation,
        });
    }
    ensure!(
        descriptor.source == OperationSource::Browser && descriptor.protocol_id == BROWSER_PROTOCOL,
        "unsupported_operation"
    );
    let mut event = arguments
        .as_object()
        .cloned()
        .context("invalid_operation")?;
    let event_type = match descriptor.operation_name.as_str() {
        "browser_keyboard" => "keyboard",
        "browser_pointer" => "pointer",
        "browser_click" => "click",
        "browser_scroll_to" => "scroll",
        _ => bail!("unsupported_operation"),
    };
    event.insert("type".to_owned(), Value::String(event_type.to_owned()));
    let bytes = serde_jcs::to_vec(&Value::Object(event))?;
    ato_adapter_browser::decode_event(&bytes)
        .map_err(|error| anyhow::anyhow!("invalid_operation: {error}"))
}

fn request_abort(
    context: &ActivityControllerContext,
    input: HostedAbortInput,
) -> Result<ActivityAbortReceipt> {
    let target_run_id = match (input.target_run_id.as_deref(), input.run_id.as_deref()) {
        (Some(target), None) | (None, Some(target)) => target,
        (Some(target), Some(run)) if target == run => target,
        _ => bail!("invalid_operation"),
    }
    .to_owned();
    ensure!(
        target_run_id == context.run_id
            && !input.operation_id.is_empty()
            && !input.descriptor_id.is_empty()
            && input.controller_epoch > 0
            && !input.actor_id.is_empty()
            && !input.actor_run_id.is_empty()
            && !input.controller_session_id.is_empty(),
        "invalid_operation"
    );
    let journal_state = context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .controller_operation_for_abort(&input, &target_run_id)?;
    if matches!(journal_state, AbortOperationState::Settled) {
        return already_settled_abort_receipt(input, target_run_id);
    }
    let active = context
        .active_operations
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity active operation mutex poisoned"))?
        .get(&input.operation_id)
        .cloned();
    let is_active = active.is_some();
    if let Some(operation) = active.as_ref() {
        ensure!(
            abort_matches_invocation(operation, &input, &target_run_id),
            "invalid_operation"
        );
    } else if matches!(journal_state, AbortOperationState::Missing) {
        let published = context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
            .clone()
            .context("stale_operation")?;
        ensure!(
            published.projection.observation.surface_id == input.surface_id
                && published.projection.observation.surface_epoch == input.surface_epoch
                && published
                    .projection
                    .operations
                    .iter()
                    .any(|descriptor| descriptor.id == input.descriptor_id),
            "stale_operation"
        );
    }
    {
        let mut requests = context
            .abort_requests
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity abort mutex poisoned"))?;
        if matches!(
            context
                .receipts
                .lock()
                .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
                .controller_operation_for_abort(&input, &target_run_id)?,
            AbortOperationState::Settled
        ) {
            drop(requests);
            return already_settled_abort_receipt(input, target_run_id);
        }
        ensure!(
            requests.contains(&input.operation_id) || requests.len() < MAX_ABORT_REQUESTS,
            "invalid_operation"
        );
        requests.insert(input.operation_id.clone());
    }
    let signaled = if is_active {
        let (result, receiver) = mpsc::channel();
        context
            .events
            .send(ActivityControllerEvent::AbortRequested {
                operation_id: input.operation_id.clone(),
                result,
            })
            .context("Activity controller abort channel closed")?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or(false)
    } else {
        false
    };
    Ok(ActivityAbortReceipt {
        operation_id: input.operation_id,
        actor_id: input.actor_id,
        actor_run_id: input.actor_run_id,
        controller_session_id: input.controller_session_id,
        controller_epoch: input.controller_epoch,
        target_run_id,
        surface_id: input.surface_id,
        surface_epoch: input.surface_epoch,
        status: "abort_requested",
        best_effort_result: if signaled {
            "abort_signal_delivered".to_owned()
        } else if is_active {
            "settle_only_abort_unavailable".to_owned()
        } else {
            "not_in_flight_or_queued".to_owned()
        },
        requested_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    })
}

fn already_settled_abort_receipt(
    input: HostedAbortInput,
    target_run_id: String,
) -> Result<ActivityAbortReceipt> {
    Ok(ActivityAbortReceipt {
        operation_id: input.operation_id,
        actor_id: input.actor_id,
        actor_run_id: input.actor_run_id,
        controller_session_id: input.controller_session_id,
        controller_epoch: input.controller_epoch,
        target_run_id,
        surface_id: input.surface_id,
        surface_epoch: input.surface_epoch,
        status: "failed",
        best_effort_result: "already_settled".to_owned(),
        requested_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    })
}

fn stable_operation_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    for code in [
        "stale_operation",
        "unsupported_operation",
        "invalid_operation",
        "operation_aborted",
        "fenced_controller",
        "head_persistence_pending",
        "operation_in_flight",
        "operation_queued_before_apply",
        "physical_commit_pending",
        "physical_outcome_indeterminate",
        "operation_indeterminate",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    "operation_failed"
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("Activity controller request closed before headers");
        }
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES + MAX_BODY_BYTES,
            "Activity controller request exceeds bound"
        );
        if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES,
            "Activity controller headers exceed bound"
        );
    };
    let header = std::str::from_utf8(&bytes[..header_end - 4])?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines
        .next()
        .context("Activity controller request line missing")?
        .split_whitespace();
    let method = request_line
        .next()
        .context("request method missing")?
        .to_owned();
    let path = request_line
        .next()
        .context("request path missing")?
        .to_owned();
    ensure!(
        request_line.next() == Some("HTTP/1.1") && request_line.next().is_none(),
        "Activity controller requires HTTP/1.1"
    );
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').context("invalid request header")?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    ensure!(
        content_length <= MAX_BODY_BYTES,
        "Activity controller body exceeds bound"
    );
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("Activity controller request closed before body");
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn respond_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) -> Result<()> {
    respond(
        stream,
        status,
        "application/json",
        &serde_json::to_vec(value)?,
        &[],
    )
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    )?;
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn websocket_origin(value: &str) -> Result<String> {
    let parsed = url::Url::parse(value).context("parse Activity Room URL")?;
    ensure!(
        matches!(parsed.scheme(), "ws" | "wss")
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "Activity Room URL is invalid"
    );
    Ok(parsed.origin().ascii_serialization())
}

fn controller_html(
    config: &ActivityControllerPageConfig,
    secret: &str,
    nonce: &str,
) -> Result<String> {
    let mut value = serde_json::to_value(config)?;
    value["hostSecret"] = Value::String(secret.to_owned());
    let encoded = serde_json::to_string(&value)?.replace('<', "\\u003c");
    Ok(CONTROLLER_HTML
        .replace("__ATO_CONFIG__", &encoded)
        .replace("__ATO_NONCE__", nonce))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use super::*;

    fn accepted_operation(run_seq: u64) -> ato_kernel::AcceptedOperation {
        let reference =
            ato_computation::ComputationRef::parse(format!("blake3:{}", "ab".repeat(32)))
                .expect("test reference");
        ato_kernel::AcceptedOperation {
            transition: ato_kernel::Transition {
                from: reference.clone(),
                offer: ato_kernel::TransitionOffer::selected(
                    ato_kernel::ChoiceId::new("test"),
                    ato_kernel::Action::Tau,
                ),
                to: reference,
            },
            run_seq,
            record_error: None,
        }
    }

    #[derive(Default)]
    struct TestIngress {
        accepted: Mutex<Vec<(String, BrowserEvent, Option<String>)>>,
    }

    impl BrowserControlIngress for TestIngress {
        fn accept_control_operation(
            &self,
            operation_id: String,
            event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            self.accepted
                .lock()
                .expect("test ingress mutex")
                .push((operation_id, event, None));
            Ok(accepted_operation(73))
        }

        fn accept_control_operation_in_context(
            &self,
            operation_id: String,
            event: BrowserEvent,
            realization_generation: Option<String>,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            self.accepted.lock().expect("test ingress mutex").push((
                operation_id,
                event,
                realization_generation,
            ));
            Ok(accepted_operation(73))
        }
    }

    struct LastRetryIngress {
        retry_calls: AtomicUsize,
    }

    impl BrowserControlIngress for LastRetryIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            Err(EvolutionError::Persist("temporary outage".to_owned()))
        }

        fn retry_control_persistence(
            &self,
        ) -> std::result::Result<
            Option<(String, ato_kernel::AcceptedOperation)>,
            ato_kernel::EvolutionError,
        > {
            let call = self.retry_calls.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            if call == 5 {
                Ok(Some((
                    "operation-last-retry".to_owned(),
                    accepted_operation(41),
                )))
            } else {
                Err(EvolutionError::Persist("temporary outage".to_owned()))
            }
        }
    }

    struct ConcurrentRecoveryIngress {
        accept_calls: AtomicUsize,
    }

    struct RetryAfterHttpIngress {
        accept_calls: AtomicUsize,
    }

    impl BrowserControlIngress for RetryAfterHttpIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            if self.accept_calls.fetch_add(1, AtomicOrdering::SeqCst) < 6 {
                Err(EvolutionError::Persist("temporary outage".to_owned()))
            } else {
                Ok(accepted_operation(55))
            }
        }

        fn retry_control_persistence(
            &self,
        ) -> std::result::Result<
            Option<(String, ato_kernel::AcceptedOperation)>,
            ato_kernel::EvolutionError,
        > {
            Err(EvolutionError::Persist("temporary outage".to_owned()))
        }

        fn control_operation_retry_stage(&self, _operation_id: &str) -> BrowserOperationRetryStage {
            BrowserOperationRetryStage::PhysicallyAppliedPendingCommit
        }
    }

    struct BlockedBeforeApplyIngress;

    impl BrowserControlIngress for BlockedBeforeApplyIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            Err(EvolutionError::Apply("operation_in_flight".to_owned()))
        }
    }

    struct AbortDuringPersistenceIngress {
        accept_calls: AtomicUsize,
        started: Sender<()>,
    }

    impl BrowserControlIngress for AbortDuringPersistenceIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            let call = self.accept_calls.fetch_add(1, AtomicOrdering::SeqCst);
            if call == 0 {
                let _ = self.started.send(());
            }
            if call < 6 {
                Err(EvolutionError::Persist("temporary outage".to_owned()))
            } else {
                Ok(accepted_operation(56))
            }
        }

        fn retry_control_persistence(
            &self,
        ) -> std::result::Result<
            Option<(String, ato_kernel::AcceptedOperation)>,
            ato_kernel::EvolutionError,
        > {
            Err(EvolutionError::Persist("temporary outage".to_owned()))
        }

        fn control_operation_retry_stage(&self, _operation_id: &str) -> BrowserOperationRetryStage {
            BrowserOperationRetryStage::PhysicallyAppliedPendingCommit
        }
    }

    impl BrowserControlIngress for ConcurrentRecoveryIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            if self.accept_calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                Err(EvolutionError::PersistencePending(1))
            } else {
                Ok(accepted_operation(42))
            }
        }

        fn retry_control_persistence(
            &self,
        ) -> std::result::Result<
            Option<(String, ato_kernel::AcceptedOperation)>,
            ato_kernel::EvolutionError,
        > {
            Ok(Some((
                "preceding-operation".to_owned(),
                accepted_operation(41),
            )))
        }
    }

    struct BlockingTestIngress {
        started: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    struct BlockingSuccessIngress {
        calls: AtomicUsize,
        started: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl BrowserControlIngress for BlockingSuccessIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            let _ = self.started.send(());
            self.release
                .lock()
                .expect("release mutex")
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release operation");
            Ok(accepted_operation(81))
        }
    }

    impl BrowserControlIngress for BlockingTestIngress {
        fn accept_control_operation(
            &self,
            _operation_id: String,
            _event: BrowserEvent,
        ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>
        {
            let _ = self.started.send(());
            self.release
                .lock()
                .expect("release mutex")
                .recv_timeout(Duration::from_secs(2))
                .expect("test should release operation");
            Err(ato_kernel::EvolutionError::Apply("aborted".to_owned()))
        }
    }

    fn test_context(
        ingress: Arc<dyn BrowserControlIngress>,
    ) -> (ActivityControllerContext, Receiver<ActivityControllerEvent>) {
        let (events, receiver) = mpsc::channel();
        let descriptor = SurfaceOperationDescriptorV1 {
            id: "descriptor-current".to_owned(),
            protocol_id: WEBMCP_PROTOCOL.to_owned(),
            operation_name: "increment_counter".to_owned(),
            safe_description: "safe".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            source: OperationSource::Webmcp,
            origin: "https://fixture.example".to_owned(),
            read_only: false,
            discovered_at: "now".to_owned(),
        };
        (
            ActivityControllerContext {
                html: String::new(),
                csp: String::new(),
                secret: "secret".to_owned(),
                bootstrap_path: "/bootstrap/test".to_owned(),
                run_id: "run-browser".to_owned(),
                events,
                frame: Mutex::new(None),
                receipts: Mutex::new(ReceiptCache::default()),
                receipt_changed: Condvar::new(),
                surface: Mutex::new(Some(PublishedSurface {
                    projection: BrowserSurfaceProjectionV1 {
                        revision: 1,
                        observation: ato_adapter_browser::SurfaceObservationV1 {
                            surface_id: "surface-browser".to_owned(),
                            target_run_id: "run-browser".to_owned(),
                            surface_epoch: 4,
                            origin: "https://fixture.example".to_owned(),
                            producer_api:
                                ato_adapter_browser::WebMcpProducerApi::DocumentModelContext,
                            untrusted_content: serde_json::json!({"counter":0}),
                            observed_at: "now".to_owned(),
                        },
                        operations: vec![descriptor],
                    },
                    registry_generation: 9,
                    document_token: "test_document_9".to_owned(),
                })),
                abort_requests: Mutex::new(BTreeSet::new()),
                active_operations: Mutex::new(BTreeMap::new()),
                ingress,
            },
            receiver,
        )
    }

    fn test_operation_input(operation_id: &str) -> HostedOperationInput {
        HostedOperationInput {
            operation_id: operation_id.to_owned(),
            descriptor_id: "descriptor-current".to_owned(),
            actor_id: "actor-child".to_owned(),
            actor_run_id: "run-actor-child".to_owned(),
            controller_session_id: "controller-session".to_owned(),
            controller_epoch: 6,
            target_run_id: Some("run-browser".to_owned()),
            run_id: None,
            surface_id: "surface-browser".to_owned(),
            surface_epoch: 4,
            protocol_id: WEBMCP_PROTOCOL.to_owned(),
            operation_name: "increment_counter".to_owned(),
            arguments: serde_json::json!({}),
            client_sequence: 2,
            actor_participant_id: None,
        }
    }

    #[test]
    fn controller_media_sdp_matches_room_string_wire_contract() {
        let offer = CONTROLLER_HTML
            .split("sendRoom(\"run.media.offer\"")
            .nth(1)
            .and_then(|value| value.split("});").next())
            .expect("media offer source");
        assert!(
            offer.contains("sdp: offer.sdp ?? \"\""),
            "Room media offers must carry SDP as a bare string"
        );
        assert!(
            !offer.contains("sdp: {"),
            "RTCSessionDescription objects violate the Room media schema"
        );

        let answer = CONTROLLER_HTML
            .split("async function applyMediaSignal")
            .nth(1)
            .and_then(|value| value.split("async function pumpFrame").next())
            .expect("media answer source");
        assert!(
            answer.contains("{ type: \"answer\", sdp: payload.sdp }"),
            "the controller must reconstruct an answer description from Room string SDP"
        );
    }

    #[test]
    fn controller_uses_canvas_media_and_application_receipts() {
        assert!(CONTROLLER_HTML.contains("mediaCanvas.captureStream(30)"));
        assert!(CONTROLLER_HTML.contains("run.operation.receipt"));
        assert!(CONTROLLER_HTML.contains("surface.observe"));
        assert!(CONTROLLER_HTML.contains("surface.operations.replace"));
        assert!(CONTROLLER_HTML.contains("run.operation.invoke"));
        assert!(CONTROLLER_HTML.contains("run.operation.abort.receipt"));
        assert!(CONTROLLER_HTML.contains("already_settled"));
        assert!(CONTROLLER_HTML.contains("receiptOutbox"));
        assert!(CONTROLLER_HTML.contains("scheduleRoomReconnect"));
        assert!(CONTROLLER_HTML.contains("replayReceiptOutbox"));
        assert!(CONTROLLER_HTML.contains("sendRoom(\"runner.ping\", {})"));
        assert!(CONTROLLER_HTML.contains("startRoomHeartbeat(room)"));
        assert!(CONTROLLER_HTML.contains("stopRoomHeartbeat()"));
        assert!(!CONTROLLER_HTML.contains("app_view_token"));
    }

    #[test]
    fn controller_backoff_does_not_reset_on_room_connected() {
        // The storm guard: room.connected must NOT reset the attempt counter,
        // otherwise an immediate replacement restarts the storm at the floor.
        let connected = CONTROLLER_HTML
            .split("if (type === \"room.connected\")")
            .nth(1)
            .and_then(|value| value.split("announceReady();").next())
            .expect("room.connected handler source");
        assert!(
            !connected.contains("roomReconnectAttempt = 0"),
            "backoff must not reset inside the room.connected handler"
        );
        assert!(
            CONTROLLER_HTML.contains("ROOM_STABLE_CONNECTION_MS"),
            "a stability window must gate the backoff reset"
        );
        let schedule = CONTROLLER_HTML
            .split("function scheduleRoomReconnect")
            .nth(1)
            .and_then(|value| value.split("function roomReconnectDelay").next())
            .expect("scheduleRoomReconnect source");
        let reset_position = schedule
            .find("roomReconnectAttempt = 0")
            .expect("stability-gated reset");
        let stable_guard = schedule
            .find("ROOM_STABLE_CONNECTION_MS")
            .expect("stability check inside schedule");
        assert!(
            stable_guard < reset_position,
            "the reset must be gated by the stability window check"
        );
    }

    #[test]
    fn controller_reconnect_delays_grow_exponentially_with_jitter() {
        let source = CONTROLLER_HTML
            .split("function roomReconnectDelay")
            .nth(1)
            .and_then(|value| value.split("}").next())
            .expect("roomReconnectDelay source");
        // Exponential base with the 5s ceiling…
        assert!(source.contains("Math.min(5000, 100 * (2 ** (attempt - 1)))"));
        // …plus a ±20% jitter band.
        assert!(source.contains("0.8 + Math.random() * 0.4"));

        // Simulate the exact JS delay curve per attempt to lock the contract:
        // monotonic growth, ceiling at 5s, jitter never leaves [80%, 120%].
        fn js_delay(attempt: u32, jitter: f64) -> u64 {
            let base = std::cmp::min(5000u64, 100u64.saturating_mul(1 << (attempt - 1)));
            ((base as f64 * (0.8 + jitter * 0.4)).round()) as u64
        }
        for attempt in 1..=7_u32 {
            let low = js_delay(attempt, 0.0);
            let high = js_delay(attempt, 1.0);
            let expected_base = 100u64.saturating_mul(1 << (attempt - 1)).min(5000);
            assert_eq!(low, expected_base * 8 / 10);
            assert_eq!(high, expected_base * 12 / 10);
            if attempt < 6 {
                // Growth across attempts even at worst-case current vs best-case next.
                assert!(
                    js_delay(attempt + 1, 0.0) > js_delay(attempt, 1.0),
                    "delay must grow from attempt {attempt} despite jitter bounds"
                );
            }
        }
    }

    #[test]
    fn controller_close_diagnostics_capture_storm_evidence() {
        let log_source = CONTROLLER_HTML
            .split("function logRoomClose")
            .nth(1)
            .and_then(|value| value.split("function scheduleRoomReconnect").next())
            .expect("logRoomClose source");
        for required in [
            "code",
            "reason",
            "uptime_ms",
            "reconnect_attempt",
            "connection_id",
            "run_id",
        ] {
            assert!(
                log_source.contains(required),
                "close diagnostics must include {required}"
            );
        }
        // Both close and error paths route through diagnostics.
        assert!(
            CONTROLLER_HTML.contains("logRoomClose(\"close\", event.code, event.reason, socket)")
        );
        assert!(CONTROLLER_HTML.contains("logRoomClose(\"error\", null, null, socket)"));
    }

    #[test]
    fn controller_retries_lost_or_non_json_loopback_receipts_without_synthetic_failure() {
        let apply_operation = CONTROLLER_HTML
            .split("async function applyOperation")
            .nth(1)
            .and_then(|value| value.split("async function abortOperation").next())
            .expect("applyOperation source");
        assert!(apply_operation.contains("ambiguousAttempts += 1"));
        assert!(apply_operation.contains("operation receipt reconciliation"));
        assert!(apply_operation.contains("receipt = await response.json()"));

        let response_error = CONTROLLER_HTML
            .split("async function responseError")
            .nth(1)
            .and_then(|value| value.split("function cryptoToken").next())
            .expect("responseError source");
        assert!(response_error.contains("throw new Error(\"ambiguous loopback response\")"));
        assert!(
            !response_error.contains("catch"),
            "decode failure must reach the caller's same-id retry path"
        );
    }

    #[test]
    fn receipt_cache_rejects_operation_id_payload_conflicts() {
        let mut cache = ReceiptCache::default();
        cache.insert(
            "op-1".to_owned(),
            b"first".to_vec(),
            ActivityOperationReceipt {
                run_sequence: Some(1),
                operation_id: "op-1".to_owned(),
                actor_participant_id: Some("participant-1".to_owned()),
                actor_id: None,
                actor_run_id: None,
                controller_session_id: None,
                controller_epoch: None,
                target_run_id: Some("run-1".to_owned()),
                surface_id: None,
                surface_epoch: None,
                client_sequence: 1,
                result: "applied".to_owned(),
                error: None,
                output: Value::Null,
                applied_at: "2026-08-25T00:00:00Z".to_owned(),
            },
        );
        assert!(cache.get("op-1", b"different").is_err());
    }

    #[test]
    fn durable_controller_journal_redacts_arguments_and_replays_settled_receipt() {
        let temporary = tempfile::tempdir().expect("journal tempdir");
        let journal_root = temporary.path().join("receipts");
        let ingress = Arc::new(TestIngress::default());
        let (mut context, _events) = test_context(ingress.clone());
        context.receipts = Mutex::new(
            ReceiptCache::durable(&journal_root, "incarnation-one".to_owned())
                .expect("durable receipt cache"),
        );
        let mut input = test_operation_input("operation-durable-1");
        input.arguments = serde_json::json!({"value":"raw-secret-canary"});
        let digest = operation_request_digest(&input).expect("request digest");
        let provenance = ControllerOperationProvenance::from_input(
            &input,
            input.target_run_id().expect("target").to_owned(),
        );

        let receipt = apply_operation(&context, input.clone()).expect("durable operation");
        assert_eq!(receipt.run_sequence, Some(73));
        assert_eq!(ingress.accepted.lock().expect("accepted mutex").len(), 1);

        let journal_path = journal_root.join(operation_journal_filename(&input.operation_id));
        let journal_bytes = fs::read(&journal_path).expect("journal bytes");
        let journal_text = String::from_utf8(journal_bytes).expect("journal utf8");
        assert!(!journal_text.contains("raw-secret-canary"));
        assert!(!journal_text.contains("arguments"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&journal_root)
                    .expect("journal directory metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            assert_eq!(
                fs::metadata(&journal_path)
                    .expect("journal file metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }

        let restarted = ReceiptCache::durable(&journal_root, "incarnation-two".to_owned())
            .expect("reload settled journal");
        let ControllerLookup::Settled(duplicate) = restarted
            .controller_lookup(&input.operation_id, &digest, &provenance)
            .expect("settled replay lookup")
        else {
            panic!("settled operation must replay after restart");
        };
        assert_eq!(duplicate.result, "applied");
        assert_eq!(duplicate.run_sequence, receipt.run_sequence);

        input.arguments = serde_json::json!({"value":"different"});
        let mismatched_digest = operation_request_digest(&input).expect("mismatched digest");
        assert!(
            restarted
                .controller_lookup(&input.operation_id, &mismatched_digest, &provenance)
                .is_err()
        );
    }

    #[test]
    fn durable_started_intent_is_indeterminate_after_runner_restart() {
        let temporary = tempfile::tempdir().expect("journal tempdir");
        let journal_root = temporary.path().join("receipts");
        let input = test_operation_input("operation-started-1");
        let digest = operation_request_digest(&input).expect("request digest");
        let provenance = ControllerOperationProvenance::from_input(
            &input,
            input.target_run_id().expect("target").to_owned(),
        );
        let mut first = ReceiptCache::durable(&journal_root, "incarnation-one".to_owned())
            .expect("first receipt cache");
        assert!(matches!(
            first
                .begin_controller_operation(
                    input.operation_id.clone(),
                    digest.clone(),
                    provenance.clone(),
                )
                .expect("persist started intent"),
            ControllerLookup::Owner
        ));

        let restarted = ReceiptCache::durable(&journal_root, "incarnation-two".to_owned())
            .expect("reload started intent");
        assert!(matches!(
            restarted
                .controller_lookup(&input.operation_id, &digest, &provenance)
                .expect("started lookup"),
            ControllerLookup::Indeterminate
        ));
    }

    #[test]
    fn corrupt_durable_journal_fails_closed_before_any_operation_replay() {
        let temporary = tempfile::tempdir().expect("journal tempdir");
        let journal_root = temporary.path().join("receipts");
        drop(OperationJournal::open(&journal_root).expect("create journal root"));
        fs::write(
            journal_root.join(operation_journal_filename("operation-corrupt")),
            br#"{"version":1,"status":"settled""#,
        )
        .expect("write corrupt journal evidence");
        assert!(
            ReceiptCache::durable(&journal_root, "incarnation-after-crash".to_owned()).is_err(),
            "corrupt evidence must never be treated as missing/retryable work"
        );
    }

    #[test]
    fn durable_settled_replay_does_not_require_current_surface_epoch() {
        let temporary = tempfile::tempdir().expect("journal tempdir");
        let ingress = Arc::new(TestIngress::default());
        let (mut context, _events) = test_context(ingress.clone());
        context.receipts = Mutex::new(
            ReceiptCache::durable(
                &temporary.path().join("receipts"),
                "incarnation-one".to_owned(),
            )
            .expect("durable receipt cache"),
        );
        let input = test_operation_input("operation-replay-old-surface");
        let first = apply_operation(&context, input.clone()).expect("first operation");
        context
            .surface
            .lock()
            .expect("surface mutex")
            .as_mut()
            .expect("surface")
            .projection
            .observation
            .surface_epoch += 1;

        let replay = apply_operation(&context, input).expect("old surface settled replay");
        assert_eq!(replay.result, "applied");
        assert_eq!(replay.run_sequence, first.run_sequence);
        assert_eq!(replay.operation_id, first.operation_id);
        assert_eq!(replay.actor_id, first.actor_id);
        assert_eq!(replay.applied_at, first.applied_at);
        assert_eq!(ingress.accepted.lock().expect("accepted mutex").len(), 1);
    }

    #[test]
    fn concurrent_same_id_joins_owner_and_never_emits_competing_terminal_result() {
        let (started, operation_started) = mpsc::channel();
        let (release, operation_release) = mpsc::channel();
        let ingress = Arc::new(BlockingSuccessIngress {
            calls: AtomicUsize::new(0),
            started,
            release: Mutex::new(operation_release),
        });
        let (context, _events) = test_context(ingress.clone());
        let context = Arc::new(context);
        let first_context = Arc::clone(&context);
        let first_input = test_operation_input("operation-same-id");
        let first = thread::spawn(move || apply_operation(&first_context, first_input));
        operation_started
            .recv_timeout(Duration::from_secs(1))
            .expect("owner should start");

        let duplicate_context = Arc::clone(&context);
        let duplicate = thread::spawn(move || {
            apply_operation(
                &duplicate_context,
                test_operation_input("operation-same-id"),
            )
        });
        thread::sleep(Duration::from_millis(50));
        assert_eq!(ingress.calls.load(AtomicOrdering::SeqCst), 1);
        release.send(()).expect("release owner");

        let first = first.join().expect("owner thread").expect("owner receipt");
        let duplicate = duplicate
            .join()
            .expect("duplicate thread")
            .expect("duplicate receipt");
        assert_eq!(first.result, "applied");
        assert_eq!(duplicate.result, "applied");
        assert_eq!(first.run_sequence, duplicate.run_sequence);
        assert_eq!(ingress.calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn webmcp_operation_maps_to_generic_browser_ingress_without_raw_description() {
        let descriptor = SurfaceOperationDescriptorV1 {
            id: "descriptor-1".to_owned(),
            protocol_id: WEBMCP_PROTOCOL.to_owned(),
            operation_name: "increment_counter".to_owned(),
            safe_description: "safe".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            source: OperationSource::Webmcp,
            origin: "https://fixture.example".to_owned(),
            read_only: false,
            discovered_at: "now".to_owned(),
        };
        assert_eq!(
            operation_event(&descriptor, &serde_json::json!({"amount":1}), 7)
                .expect("operation should map"),
            BrowserEvent::Operation {
                operation_name: "increment_counter".to_owned(),
                arguments: serde_json::json!({"amount":1}),
                surface_generation: 7,
            }
        );
    }

    #[test]
    fn fixed_browser_operations_map_to_legacy_events() {
        let descriptor = SurfaceOperationDescriptorV1 {
            id: "descriptor-click".to_owned(),
            protocol_id: BROWSER_PROTOCOL.to_owned(),
            operation_name: "browser_click".to_owned(),
            safe_description: "safe".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            source: OperationSource::Browser,
            origin: "https://fixture.example".to_owned(),
            read_only: false,
            discovered_at: "now".to_owned(),
        };
        assert_eq!(
            operation_event(
                &descriptor,
                &serde_json::json!({
                    "x_normalized":0.25,
                    "y_normalized":0.75,
                    "button":0
                }),
                1,
            )
            .expect("click should map"),
            BrowserEvent::Click {
                x_normalized: 0.25,
                y_normalized: 0.75,
                button: 0,
            }
        );
    }

    #[test]
    fn persistence_recovery_returns_success_from_the_final_retry_attempt() {
        let ingress: Arc<dyn BrowserControlIngress> = Arc::new(LastRetryIngress {
            retry_calls: AtomicUsize::new(0),
        });
        let (context, _events) = test_context(ingress);
        let accepted = accept_with_persistence_recovery(
            &context,
            "operation-last-retry".to_owned(),
            BrowserEvent::Click {
                x_normalized: 0.5,
                y_normalized: 0.5,
                button: 0,
            },
            None,
        )
        .expect("last retry acceptance");
        assert_eq!(accepted.run_seq, 41);
    }

    #[test]
    fn public_participant_input_keeps_runner_sequence_across_head_retry() {
        let ingress: Arc<dyn BrowserControlIngress> = Arc::new(LastRetryIngress {
            retry_calls: AtomicUsize::new(0),
        });
        let (context, _events) = test_context(ingress);
        let receipt = apply_input(
            &context,
            HostedRunInput {
                run_id: "run-browser".to_owned(),
                operation_id: "operation-last-retry".to_owned(),
                client_seq: 9,
                adapter_id: BROWSER_PROTOCOL.to_owned(),
                protocol_id: BROWSER_PROTOCOL.to_owned(),
                event: serde_json::json!({
                    "type":"click",
                    "x_normalized":0.5,
                    "y_normalized":0.5,
                    "button":0
                }),
                actor_participant_id: "participant-human".to_owned(),
                source_connection_id: "connection-human".to_owned(),
            },
        )
        .expect("participant input recovery");
        assert_eq!(receipt.run_sequence, Some(41));
        assert_eq!(
            receipt.actor_participant_id.as_deref(),
            Some("participant-human")
        );
    }

    #[test]
    fn persistence_recovery_reenters_current_id_after_predecessor_is_recovered() {
        let ingress: Arc<dyn BrowserControlIngress> = Arc::new(ConcurrentRecoveryIngress {
            accept_calls: AtomicUsize::new(0),
        });
        let (context, _events) = test_context(ingress);
        let accepted = accept_with_persistence_recovery(
            &context,
            "current-operation".to_owned(),
            BrowserEvent::Click {
                x_normalized: 0.5,
                y_normalized: 0.5,
                button: 0,
            },
            None,
        )
        .expect("current operation after predecessor recovery");
        assert_eq!(accepted.run_seq, 42);
    }

    #[test]
    fn persistence_pending_http_retry_reclaims_intent_without_current_surface() {
        let ingress = Arc::new(RetryAfterHttpIngress {
            accept_calls: AtomicUsize::new(0),
        });
        let (context, _events) = test_context(ingress.clone());
        let input = test_operation_input("operation-http-retry");
        let first = apply_operation(&context, input.clone())
            .expect_err("exhausted persistence retry stays non-terminal");
        assert_eq!(stable_operation_error(&first), "physical_commit_pending");
        context
            .surface
            .lock()
            .expect("surface mutex")
            .as_mut()
            .expect("surface")
            .projection
            .observation
            .surface_epoch += 1;

        let recovered = apply_operation(&context, input)
            .expect("same-process retry uses retained event and ingress tombstone");
        assert_eq!(recovered.run_sequence, Some(55));
        assert_eq!(recovered.result, "applied");
        assert_eq!(ingress.accept_calls.load(AtomicOrdering::SeqCst), 7);
    }

    #[test]
    fn before_apply_retry_revalidates_surface_and_never_targets_new_document() {
        let ingress: Arc<dyn BrowserControlIngress> = Arc::new(BlockedBeforeApplyIngress);
        let (context, _events) = test_context(ingress);
        let input = test_operation_input("operation-before-apply");
        let first = apply_operation(&context, input.clone())
            .expect_err("blocked operation stays non-terminal");
        assert_eq!(
            stable_operation_error(&first),
            "operation_queued_before_apply"
        );
        context
            .surface
            .lock()
            .expect("surface mutex")
            .as_mut()
            .expect("surface")
            .projection
            .observation
            .surface_epoch += 1;

        let stale = apply_operation(&context, input)
            .expect_err("queued fixed/WebMCP operation must revalidate after navigation");
        assert_eq!(stable_operation_error(&stale), "stale_operation");
    }

    #[test]
    fn receipt_serialization_carries_actor_controller_and_runner_ordering() {
        let receipt = ActivityOperationReceipt {
            run_sequence: Some(91),
            operation_id: "operation-91".to_owned(),
            actor_participant_id: None,
            actor_id: Some("actor-1".to_owned()),
            actor_run_id: Some("actor-run-1".to_owned()),
            controller_session_id: Some("controller-session-1".to_owned()),
            controller_epoch: Some(4),
            target_run_id: Some("browser-run-1".to_owned()),
            surface_id: Some("surface-1".to_owned()),
            surface_epoch: Some(3),
            client_sequence: 2,
            result: "applied".to_owned(),
            error: None,
            output: serde_json::json!({"adapter_ack":true}),
            applied_at: "2026-08-26T00:00:00Z".to_owned(),
        };
        let value = serde_json::to_value(receipt).expect("receipt should serialize");
        assert_eq!(value["run_sequence"], 91);
        assert_eq!(value["actor_id"], "actor-1");
        assert_eq!(value["actor_run_id"], "actor-run-1");
        assert_eq!(value["controller_session_id"], "controller-session-1");
        assert_eq!(value["controller_epoch"], 4);
        assert_eq!(value["surface_epoch"], 3);
    }

    #[test]
    fn failed_receipt_omits_null_sequence_and_terminal_replay_keeps_audit_result() {
        let input = test_operation_input("operation-stale-replay");
        let digest = operation_request_digest(&input).expect("request digest");
        let provenance = ControllerOperationProvenance::from_input(
            &input,
            input.target_run_id().expect("target").to_owned(),
        );
        let receipt = ActivityOperationReceipt {
            run_sequence: Some(92),
            operation_id: input.operation_id.clone(),
            actor_participant_id: None,
            actor_id: Some(input.actor_id.clone()),
            actor_run_id: Some(input.actor_run_id.clone()),
            controller_session_id: Some(input.controller_session_id.clone()),
            controller_epoch: Some(input.controller_epoch),
            target_run_id: input.target_run_id.clone(),
            surface_id: Some(input.surface_id.clone()),
            surface_epoch: Some(input.surface_epoch),
            client_sequence: input.client_sequence,
            result: "applied_after_abort_requested".to_owned(),
            error: None,
            output: Value::Null,
            applied_at: "2026-08-26T00:00:00Z".to_owned(),
        };
        let serialized = serde_json::to_value(ActivityOperationReceipt {
            run_sequence: None,
            result: "failed".to_owned(),
            error: Some("stale_operation".to_owned()),
            ..receipt.clone()
        })
        .expect("failed receipt serialization");
        assert!(serialized.get("run_sequence").is_none());
        assert_eq!(serialized["error"], "stale_operation");

        let mut cache = ReceiptCache::default();
        assert!(matches!(
            cache
                .begin_controller_operation(
                    input.operation_id.clone(),
                    digest.clone(),
                    provenance.clone(),
                )
                .expect("begin operation"),
            ControllerLookup::Owner
        ));
        cache
            .settle_controller_operation(&input.operation_id, &digest, &provenance, receipt)
            .expect("settle operation");
        let ControllerLookup::Settled(replayed) = cache
            .controller_lookup(&input.operation_id, &digest, &provenance)
            .expect("terminal replay")
        else {
            panic!("settled receipt must replay");
        };
        assert_eq!(replayed.result, "applied_after_abort_requested");
        assert_eq!(replayed.run_sequence, Some(92));
    }

    #[test]
    fn generic_controller_operation_uses_runner_sequence_and_full_provenance() {
        let ingress = Arc::new(TestIngress::default());
        let (context, _events) = test_context(ingress.clone());
        let receipt = apply_operation(&context, test_operation_input("invocation-1"))
            .expect("operation should reach ingress");
        assert_eq!(receipt.run_sequence, Some(73));
        assert_eq!(receipt.actor_id.as_deref(), Some("actor-child"));
        assert_eq!(receipt.actor_run_id.as_deref(), Some("run-actor-child"));
        assert_eq!(
            receipt.controller_session_id.as_deref(),
            Some("controller-session")
        );
        assert_eq!(receipt.controller_epoch, Some(6));
        assert_eq!(receipt.surface_epoch, Some(4));
        let accepted = ingress.accepted.lock().expect("test ingress mutex");
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].0, "invocation-1");
        assert_eq!(
            accepted[0].1,
            BrowserEvent::Operation {
                operation_name: "increment_counter".to_owned(),
                arguments: serde_json::json!({}),
                surface_generation: 9,
            }
        );
        assert_eq!(accepted[0].2.as_deref(), Some("test_document_9.9"));
        drop(accepted);
        let late_abort = request_abort(
            &context,
            HostedAbortInput {
                operation_id: "invocation-1".to_owned(),
                descriptor_id: "descriptor-current".to_owned(),
                actor_id: "actor-child".to_owned(),
                actor_run_id: "run-actor-child".to_owned(),
                controller_session_id: "controller-session".to_owned(),
                controller_epoch: 6,
                target_run_id: Some("run-browser".to_owned()),
                run_id: None,
                surface_id: "surface-browser".to_owned(),
                surface_epoch: 4,
            },
        )
        .expect("late abort should produce idempotent audit evidence");
        assert_eq!(late_abort.status, "failed");
        assert_eq!(late_abort.best_effort_result, "already_settled");
    }

    #[test]
    fn stale_descriptor_is_rejected_without_same_name_reresolution() {
        let ingress = Arc::new(TestIngress::default());
        let (context, _events) = test_context(ingress.clone());
        let mut input = test_operation_input("invocation-stale");
        input.descriptor_id = "descriptor-from-prior-epoch".to_owned();
        let error = apply_operation(&context, input).expect_err("stale descriptor must fail");
        assert_eq!(stable_operation_error(&error), "stale_operation");
        assert!(
            ingress
                .accepted
                .lock()
                .expect("test ingress mutex")
                .is_empty()
        );
    }

    #[test]
    fn active_webmcp_abort_is_signaled_and_settles_as_aborted() {
        let (started, operation_started) = mpsc::channel();
        let (release, operation_release) = mpsc::channel();
        let ingress: Arc<dyn BrowserControlIngress> = Arc::new(BlockingTestIngress {
            started,
            release: Mutex::new(operation_release),
        });
        let (context, events) = test_context(ingress);
        let context = Arc::new(context);
        let apply_context = Arc::clone(&context);
        let operation = thread::spawn(move || {
            apply_operation(&apply_context, test_operation_input("invocation-abort"))
        });
        operation_started
            .recv_timeout(Duration::from_secs(1))
            .expect("operation should reach the Runner ingress");
        context
            .surface
            .lock()
            .expect("surface mutex")
            .as_mut()
            .expect("published surface")
            .projection
            .observation
            .surface_epoch = 5;

        let abort_context = Arc::clone(&context);
        let abort = thread::spawn(move || {
            request_abort(
                &abort_context,
                HostedAbortInput {
                    operation_id: "invocation-abort".to_owned(),
                    descriptor_id: "descriptor-current".to_owned(),
                    actor_id: "actor-child".to_owned(),
                    actor_run_id: "run-actor-child".to_owned(),
                    controller_session_id: "controller-session".to_owned(),
                    controller_epoch: 6,
                    target_run_id: Some("run-browser".to_owned()),
                    run_id: None,
                    surface_id: "surface-browser".to_owned(),
                    surface_epoch: 4,
                },
            )
        });
        let ActivityControllerEvent::AbortRequested {
            operation_id,
            result,
        } = events
            .recv_timeout(Duration::from_secs(1))
            .expect("active operation should request the Browser abort")
        else {
            panic!("unexpected controller event");
        };
        assert_eq!(operation_id, "invocation-abort");
        result.send(true).expect("abort result channel");
        let abort_receipt = abort
            .join()
            .expect("abort request thread")
            .expect("abort receipt");
        assert_eq!(abort_receipt.status, "abort_requested");
        assert_eq!(abort_receipt.best_effort_result, "abort_signal_delivered");

        release.send(()).expect("release operation");
        let receipt = operation
            .join()
            .expect("operation thread")
            .expect("aborted ingress must issue terminal Runner evidence");
        assert_eq!(receipt.result, "aborted");
        assert_eq!(receipt.run_sequence, None);
        assert_eq!(receipt.output["error"], "operation_aborted");
        assert!(
            context
                .abort_requests
                .lock()
                .expect("abort request mutex")
                .is_empty(),
            "settled abort evidence must not leak into later invocations"
        );
    }

    #[test]
    fn abort_after_physical_ack_survives_persistence_retry_and_clears_queue() {
        let (started, operation_started) = mpsc::channel();
        let ingress = Arc::new(AbortDuringPersistenceIngress {
            accept_calls: AtomicUsize::new(0),
            started,
        });
        let (context, events) = test_context(ingress.clone());
        let context = Arc::new(context);
        let apply_context = Arc::clone(&context);
        let operation = thread::spawn(move || {
            apply_operation(
                &apply_context,
                test_operation_input("operation-persist-abort"),
            )
        });
        operation_started
            .recv_timeout(Duration::from_secs(1))
            .expect("physical operation should start");

        let abort_context = Arc::clone(&context);
        let abort = thread::spawn(move || {
            request_abort(
                &abort_context,
                HostedAbortInput {
                    operation_id: "operation-persist-abort".to_owned(),
                    descriptor_id: "descriptor-current".to_owned(),
                    actor_id: "actor-child".to_owned(),
                    actor_run_id: "run-actor-child".to_owned(),
                    controller_session_id: "controller-session".to_owned(),
                    controller_epoch: 6,
                    target_run_id: Some("run-browser".to_owned()),
                    run_id: None,
                    surface_id: "surface-browser".to_owned(),
                    surface_epoch: 4,
                },
            )
        });
        let ActivityControllerEvent::AbortRequested { result, .. } = events
            .recv_timeout(Duration::from_secs(1))
            .expect("abort signal event")
        else {
            panic!("unexpected event");
        };
        result.send(false).expect("abort result");
        abort
            .join()
            .expect("abort thread")
            .expect("abort audit receipt");
        let pending = operation
            .join()
            .expect("operation thread")
            .expect_err("physical commit remains non-terminal");
        assert_eq!(stable_operation_error(&pending), "physical_commit_pending");

        let recovered = apply_operation(&context, test_operation_input("operation-persist-abort"))
            .expect("same operation repairs committed head");
        assert_eq!(recovered.run_sequence, Some(56));
        assert_eq!(recovered.result, "applied_after_abort_requested");
        assert!(
            context
                .abort_requests
                .lock()
                .expect("abort mutex")
                .is_empty()
        );
        assert_eq!(ingress.accept_calls.load(AtomicOrdering::SeqCst), 7);
    }
}
