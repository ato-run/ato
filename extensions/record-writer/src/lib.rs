//! Bounded asynchronous Record pipeline. Stylus submission performs only
//! validation and a non-blocking queue operation; persistence is owned by the
//! background Record Writer and synchronized only by a Capture Barrier.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, RwLock};

use ato_adapter_api::{AdapterError, OperationRequirement, Stylus, SupportedOperation};
use ato_computation::{ContentRef, OperationId, ProtocolId};
use ato_objects::{
    ObjectStore, RecordBodyV2, RecordCandidate, RecordEnvelopeV2, RecordIdV2, decode_record_v2,
    encode_record_v2,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SEGMENT_MAGIC: &[u8] = b"ATO-RECORD-SEGMENT-V1\n";
const FRONTIER_VERSION: u32 = 1;
const SEGMENT_METADATA_FIELDS: usize = 5;

type Validator = dyn Fn(&[u8]) -> Result<(), String> + Send + Sync;

struct SchemaEntry {
    operation: SupportedOperation,
    validator: Arc<Validator>,
}

/// Extension-owned payload schemas used by the Record Writer before CAS
/// insertion. Registration is per Protocol operation and payload version.
#[derive(Default)]
pub struct RecordSchemaRegistry {
    entries: BTreeMap<SchemaKey, SchemaEntry>,
}

impl RecordSchemaRegistry {
    pub fn register(
        &mut self,
        operation: SupportedOperation,
        validator: impl Fn(&[u8]) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<(), RecordWriterError> {
        let key = SchemaKey::from(&operation);
        if self
            .entries
            .insert(
                key.clone(),
                SchemaEntry {
                    operation,
                    validator: Arc::new(validator),
                },
            )
            .is_some()
        {
            return Err(RecordWriterError::DuplicateSchema(key.to_string()));
        }
        Ok(())
    }

    fn validate(&self, candidate: &RecordCandidate) -> Result<(), RecordWriterError> {
        let key = SchemaKey {
            protocol_id: candidate.protocol_id.clone(),
            operation_id: candidate.operation_id.clone(),
            payload_version: candidate.payload_version,
        };
        let entry = self
            .entries
            .get(&key)
            .ok_or_else(|| RecordWriterError::UnsupportedOperation(key.to_string()))?;
        let requirement = OperationRequirement {
            protocol_id: candidate.protocol_id.clone(),
            operation_id: candidate.operation_id.clone(),
            payload_version: candidate.payload_version,
            required_features: candidate.required_features.clone(),
        };
        if !entry.operation.supports(&requirement) {
            return Err(RecordWriterError::UnsupportedFeatures(key.to_string()));
        }
        (entry.validator)(&candidate.payload).map_err(|reason| RecordWriterError::InvalidPayload {
            operation: key.to_string(),
            reason,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaKey {
    protocol_id: ProtocolId,
    operation_id: OperationId,
    payload_version: u32,
}

impl From<&SupportedOperation> for SchemaKey {
    fn from(value: &SupportedOperation) -> Self {
        Self {
            protocol_id: value.protocol_id.clone(),
            operation_id: value.operation_id.clone(),
            payload_version: value.payload_version,
        }
    }
}

impl std::fmt::Display for SchemaKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}/{} payload v{}",
            self.protocol_id, self.operation_id, self.payload_version
        )
    }
}

#[derive(Debug, Clone)]
pub struct RecordWriterConfig {
    pub records_root: PathBuf,
    pub run_id: String,
    pub queue_capacity: usize,
    pub max_candidate_bytes: usize,
    pub max_segment_records: usize,
    pub max_segment_bytes: usize,
}

impl RecordWriterConfig {
    pub fn at(records_root: impl Into<PathBuf>, run_id: impl Into<String>) -> Self {
        Self {
            records_root: records_root.into(),
            run_id: run_id.into(),
            queue_capacity: 1024,
            max_candidate_bytes: 16 * 1024 * 1024,
            max_segment_records: 1024,
            max_segment_bytes: 16 * 1024 * 1024,
        }
    }

    fn validate(&self) -> Result<(), RecordWriterError> {
        validate_component("run id", &self.run_id)?;
        if self.queue_capacity == 0
            || self.max_candidate_bytes == 0
            || self.max_segment_records == 0
            || self.max_segment_bytes == 0
        {
            return Err(RecordWriterError::InvalidConfig(
                "queue and size limits must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Handles for the independently running writer and its only durability
/// synchronization point.
pub struct RecordPipeline {
    pub stylus: Arc<AsyncRecordStylus>,
    pub barrier: CaptureBarrier,
    pub published: PublishedWatermark,
}

impl RecordPipeline {
    pub fn start(
        config: RecordWriterConfig,
        objects: Arc<dyn ObjectStore>,
        schemas: RecordSchemaRegistry,
    ) -> Result<Self, RecordWriterError> {
        config.validate()?;
        let (sender, receiver) = sync_channel(config.queue_capacity);
        let failure = Arc::new(Mutex::new(None));
        let accepted = Arc::new(RwLock::new(BTreeMap::new()));
        let submission_gate = Arc::new(Mutex::new(()));
        let paused = Arc::new(AtomicBool::new(false));
        let published = PublishedWatermark(Arc::new(RwLock::new(BTreeMap::new())));
        let stylus = Arc::new(AsyncRecordStylus {
            sender: sender.clone(),
            accepted: Arc::clone(&accepted),
            submission_gate: Arc::clone(&submission_gate),
            paused: Arc::clone(&paused),
            failure: Arc::clone(&failure),
            max_candidate_bytes: config.max_candidate_bytes,
        });
        let barrier = CaptureBarrier {
            sender,
            accepted,
            submission_gate,
            paused,
            failure: Arc::clone(&failure),
        };
        let state = WriterState::open(config, objects, schemas, published.clone())?;
        std::thread::Builder::new()
            .name("ato-record-writer".to_owned())
            .spawn(move || writer_loop(receiver, state, failure))
            .map_err(RecordWriterError::Io)?;
        Ok(Self {
            stylus,
            barrier,
            published,
        })
    }
}

pub struct AsyncRecordStylus {
    sender: SyncSender<WriterCommand>,
    accepted: Arc<RwLock<BTreeMap<String, u64>>>,
    submission_gate: Arc<Mutex<()>>,
    paused: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    max_candidate_bytes: usize,
}

impl AsyncRecordStylus {
    pub fn observed_through(&self) -> BTreeMap<String, u64> {
        self.accepted
            .read()
            .expect("watermark lock poisoned")
            .clone()
    }

    pub fn health(&self) -> Result<(), RecordWriterError> {
        check_failure(&self.failure)
    }
}

impl Stylus for AsyncRecordStylus {
    fn record(&self, candidate: RecordCandidate) -> Result<(), AdapterError> {
        let _submission = self.submission_gate.lock().map_err(|_| {
            AdapterError::Operation("Record submission gate is poisoned".to_owned())
        })?;
        if self.paused.load(Ordering::Acquire) {
            return Err(AdapterError::Operation(
                RecordWriterError::CapturePaused.to_string(),
            ));
        }
        self.health()
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        candidate
            .validate()
            .map_err(|error| AdapterError::InvalidPayload(error.to_string()))?;
        if candidate.payload.len() > self.max_candidate_bytes {
            return Err(AdapterError::InvalidPayload(format!(
                "Record candidate is {} bytes; maximum is {}",
                candidate.payload.len(),
                self.max_candidate_bytes
            )));
        }
        let stream = candidate.stream.clone();
        let local_seq = candidate.local_seq;
        match self.sender.try_send(WriterCommand::Record(candidate)) {
            Ok(()) => {
                let mut accepted = self.accepted.write().map_err(|_| {
                    AdapterError::Operation("Record watermark lock poisoned".to_owned())
                })?;
                accepted
                    .entry(stream)
                    .and_modify(|value| *value = (*value).max(local_seq))
                    .or_insert(local_seq);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                set_failure(&self.failure, RecordWriterError::QueueFull.to_string());
                Err(AdapterError::Operation(
                    RecordWriterError::QueueFull.to_string(),
                ))
            }
            Err(TrySendError::Disconnected(_)) => Err(AdapterError::Operation(
                RecordWriterError::Disconnected.to_string(),
            )),
        }
    }
}

#[derive(Clone)]
pub struct PublishedWatermark(Arc<RwLock<BTreeMap<String, u64>>>);

impl PublishedWatermark {
    pub fn observed_through(&self) -> BTreeMap<String, u64> {
        self.0
            .read()
            .expect("published watermark lock poisoned")
            .clone()
    }
}

#[derive(Clone)]
pub struct CaptureBarrier {
    sender: SyncSender<WriterCommand>,
    accepted: Arc<RwLock<BTreeMap<String, u64>>>,
    submission_gate: Arc<Mutex<()>>,
    paused: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl CaptureBarrier {
    /// Waits for persistence only at an explicit capture/seal boundary.
    pub fn seal(&self) -> Result<RecordFrontier, RecordWriterError> {
        let capture = self.pause_and_seal()?;
        let frontier = capture.frontier.clone();
        drop(capture);
        Ok(frontier)
    }

    /// Establishes a causal cut and keeps Stylus submission paused until the
    /// returned lease is dropped. VM capture uses this to quiesce physical
    /// resources after the Record cut without admitting later operations.
    pub fn pause_and_seal(&self) -> Result<PausedCapture, RecordWriterError> {
        check_failure(&self.failure)?;
        let _submission = self
            .submission_gate
            .lock()
            .map_err(|_| RecordWriterError::Poisoned("submission gate"))?;
        self.paused.store(true, Ordering::Release);
        let observed_through = self
            .accepted
            .read()
            .map_err(|_| RecordWriterError::Poisoned("accepted watermark"))?
            .clone();
        let (reply, receive_reply) = sync_channel(1);
        if self
            .sender
            .send(WriterCommand::Barrier {
                observed_through,
                reply,
            })
            .is_err()
        {
            self.paused.store(false, Ordering::Release);
            return Err(RecordWriterError::Disconnected);
        }
        let result = match receive_reply.recv() {
            Ok(result) => result,
            Err(_) => {
                self.paused.store(false, Ordering::Release);
                return Err(RecordWriterError::Disconnected);
            }
        };
        match result {
            Ok(frontier) => Ok(PausedCapture {
                frontier,
                paused: Arc::clone(&self.paused),
            }),
            Err(error) => {
                self.paused.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    pub fn health(&self) -> Result<(), RecordWriterError> {
        check_failure(&self.failure)
    }
}

pub struct PausedCapture {
    pub frontier: RecordFrontier,
    paused: Arc<AtomicBool>,
}

impl Drop for PausedCapture {
    fn drop(&mut self) {
        self.paused.store(false, Ordering::Release);
    }
}

enum WriterCommand {
    Record(RecordCandidate),
    Barrier {
        observed_through: BTreeMap<String, u64>,
        reply: SyncSender<Result<RecordFrontier, RecordWriterError>>,
    },
}

fn writer_loop(
    receiver: Receiver<WriterCommand>,
    mut state: WriterState,
    failure: Arc<Mutex<Option<String>>>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            WriterCommand::Record(candidate) => {
                if check_failure(&failure).is_ok()
                    && let Err(error) = state.append(candidate)
                {
                    set_failure(&failure, error.to_string());
                }
            }
            WriterCommand::Barrier {
                observed_through,
                reply,
            } => {
                let result =
                    check_failure(&failure).and_then(|_| state.seal_frontier(&observed_through));
                if let Err(error) = &result {
                    set_failure(&failure, error.to_string());
                }
                let _ = reply.send(result);
            }
        }
    }
}

struct WriterState {
    config: RecordWriterConfig,
    objects: Arc<dyn ObjectStore>,
    schemas: RecordSchemaRegistry,
    active_path: PathBuf,
    active: File,
    active_records: Vec<RecordEnvelopeV2>,
    active_bytes: usize,
    sealed_segments: Vec<SealedSegment>,
    next_order: u64,
    seen: BTreeSet<RecordIdV2>,
    referenced: BTreeSet<RecordIdV2>,
    processed: BTreeMap<String, u64>,
    published: PublishedWatermark,
}

impl WriterState {
    fn open(
        config: RecordWriterConfig,
        objects: Arc<dyn ObjectStore>,
        schemas: RecordSchemaRegistry,
        published: PublishedWatermark,
    ) -> Result<Self, RecordWriterError> {
        let run_root = config.records_root.join("runs").join(&config.run_id);
        let segments_root = run_root.join("segments");
        let frontiers_root = run_root.join("frontiers");
        fs::create_dir_all(&segments_root)?;
        fs::create_dir_all(&frontiers_root)?;
        let active_path = run_root.join("active.open");
        let mut sealed_segments = Vec::new();
        let mut recovered = Vec::new();
        for entry in fs::read_dir(&segments_root)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("seg") {
                return Err(RecordWriterError::UnexpectedRecordFile(path));
            }
            let bytes = fs::read(&path)?;
            let (summary, records) = decode_segment(&bytes, objects.as_ref())?;
            let expected = segment_path(&segments_root, &summary.digest)?;
            if path != expected {
                return Err(RecordWriterError::SegmentPathMismatch(path));
            }
            sealed_segments.push(summary);
            recovered.extend(records);
        }
        sealed_segments.sort_by_key(|segment| segment.first_writer_order);
        recovered.sort_by_key(|record| record.writer_order);

        let active_records = if active_path.exists() {
            decode_active(&fs::read(&active_path)?)?
        } else {
            Vec::new()
        };
        recovered.extend(active_records.iter().cloned());
        recovered.sort_by_key(|record| record.writer_order);
        verify_contiguous_orders(&recovered)?;
        let mut seen = BTreeSet::new();
        let mut referenced = BTreeSet::new();
        let mut processed: BTreeMap<String, u64> = BTreeMap::new();
        for record in &recovered {
            if !seen.insert(record.id.clone()) {
                return Err(RecordWriterError::DuplicateRecord(record.id.to_string()));
            }
            referenced.extend(record.caused_by.iter().cloned());
            processed
                .entry(record.stream.clone())
                .and_modify(|value| *value = (*value).max(record.local_seq))
                .or_insert(record.local_seq);
        }
        if !referenced.is_subset(&seen) {
            return Err(RecordWriterError::MissingCausalRecord);
        }
        *published
            .0
            .write()
            .map_err(|_| RecordWriterError::Poisoned("published watermark"))? = processed.clone();
        let active_bytes = fs::metadata(&active_path).map_or(0, |metadata| metadata.len() as usize);
        let active = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&active_path)?;
        Ok(Self {
            config,
            objects,
            schemas,
            active_path,
            active,
            active_records,
            active_bytes,
            sealed_segments,
            next_order: recovered.last().map_or(1, |record| record.writer_order + 1),
            seen,
            referenced,
            processed,
            published,
        })
    }

    fn append(&mut self, candidate: RecordCandidate) -> Result<(), RecordWriterError> {
        candidate.validate()?;
        self.schemas.validate(&candidate)?;
        for cause in &candidate.caused_by {
            if !self.seen.contains(cause) {
                return Err(RecordWriterError::UnknownCause(cause.to_string()));
            }
        }
        let payload_ref = self.objects.put(&candidate.payload)?;
        let record = RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: candidate.protocol_id,
            operation_id: candidate.operation_id,
            port_id: candidate.port_id,
            payload_ref,
            payload_version: candidate.payload_version,
            required_features: candidate.required_features,
            recorded_by: candidate.recorded_by,
            stream: candidate.stream,
            local_seq: candidate.local_seq,
            writer_order: self.next_order,
            caused_by: candidate.caused_by,
            observed_at: candidate.observed_at,
        })?;
        let bytes = encode_record_v2(&record)?;
        write_frame(&mut self.active, &bytes)?;
        self.active_bytes = self.active_bytes.saturating_add(8 + bytes.len());
        self.next_order += 1;
        self.referenced.extend(record.caused_by.iter().cloned());
        self.seen.insert(record.id.clone());
        self.processed
            .entry(record.stream.clone())
            .and_modify(|value| *value = (*value).max(record.local_seq))
            .or_insert(record.local_seq);
        *self
            .published
            .0
            .write()
            .map_err(|_| RecordWriterError::Poisoned("published watermark"))? =
            self.processed.clone();
        self.active_records.push(record);
        if self.active_records.len() >= self.config.max_segment_records
            || self.active_bytes >= self.config.max_segment_bytes
        {
            self.seal_active()?;
        }
        Ok(())
    }

    fn seal_active(&mut self) -> Result<(), RecordWriterError> {
        if self.active_records.is_empty() {
            self.active.sync_all()?;
            return Ok(());
        }
        self.active.sync_all()?;
        let bytes = encode_segment(&self.active_records)?;
        let (summary, records) = decode_segment(&bytes, self.objects.as_ref())?;
        if records != self.active_records {
            return Err(RecordWriterError::SegmentRoundtrip);
        }
        self.objects.insert(&summary.digest, &bytes)?;
        let segments_root = self
            .active_path
            .parent()
            .expect("active.open has a run directory")
            .join("segments");
        let path = segment_path(&segments_root, &summary.digest)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&path)? != bytes {
                    return Err(RecordWriterError::ImmutableSegmentMismatch(path));
                }
            }
            Err(error) => return Err(error.into()),
        }
        sync_directory(&segments_root)?;
        self.active.set_len(0)?;
        self.active.seek(SeekFrom::Start(0))?;
        self.active.sync_all()?;
        self.active_records.clear();
        self.active_bytes = 0;
        self.sealed_segments.push(summary);
        self.sealed_segments
            .sort_by_key(|segment| segment.first_writer_order);
        Ok(())
    }

    fn seal_frontier(
        &mut self,
        requested: &BTreeMap<String, u64>,
    ) -> Result<RecordFrontier, RecordWriterError> {
        self.seal_active()?;
        for (stream, local_seq) in requested {
            if self.processed.get(stream).copied().unwrap_or(0) < *local_seq {
                return Err(RecordWriterError::BarrierIncomplete {
                    stream: stream.clone(),
                    requested: *local_seq,
                    processed: self.processed.get(stream).copied().unwrap_or(0),
                });
            }
        }
        let causal_cut = self.seen.difference(&self.referenced).cloned().collect();
        let frontier = RecordFrontier::seal(RecordFrontierBody {
            run_id: self.config.run_id.clone(),
            sealed_segments: self.sealed_segments.clone(),
            last_writer_order: self.next_order.saturating_sub(1),
            observed_through: self.processed.clone(),
            causal_cut,
        })?;
        let bytes = frontier.encode()?;
        self.objects
            .insert(&frontier.frontier_digest, &frontier.identity_bytes()?)?;
        let frontiers_root = self
            .active_path
            .parent()
            .expect("active.open has a run directory")
            .join("frontiers");
        let path = frontier_path(&frontiers_root, &frontier.frontier_digest)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&path)? != bytes {
                    return Err(RecordWriterError::ImmutableFrontierMismatch(path));
                }
            }
            Err(error) => return Err(error.into()),
        }
        sync_directory(&frontiers_root)?;
        Ok(frontier)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSegment {
    pub digest: ContentRef,
    pub record_count: u64,
    pub first_writer_order: u64,
    pub last_writer_order: u64,
    pub byte_length: u64,
    pub payload_closure: Vec<ContentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFrontier {
    pub version: u32,
    pub run_id: String,
    pub sealed_segments: Vec<SealedSegment>,
    pub last_writer_order: u64,
    pub observed_through: BTreeMap<String, u64>,
    pub causal_cut: Vec<RecordIdV2>,
    pub frontier_digest: ContentRef,
}

struct RecordFrontierBody {
    run_id: String,
    sealed_segments: Vec<SealedSegment>,
    last_writer_order: u64,
    observed_through: BTreeMap<String, u64>,
    causal_cut: Vec<RecordIdV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontierWire {
    version: u32,
    run_id: String,
    sealed_segments: Vec<SegmentWire>,
    last_writer_order: u64,
    observed_through: BTreeMap<String, u64>,
    causal_cut: Vec<String>,
    frontier_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontierIdentityWire {
    version: u32,
    run_id: String,
    sealed_segments: Vec<SegmentWire>,
    last_writer_order: u64,
    observed_through: BTreeMap<String, u64>,
    causal_cut: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentWire {
    digest: String,
    record_count: u64,
    first_writer_order: u64,
    last_writer_order: u64,
    byte_length: u64,
    payload_closure: Vec<String>,
}

impl RecordFrontier {
    fn seal(body: RecordFrontierBody) -> Result<Self, RecordWriterError> {
        let identity = FrontierIdentityWire::from(&body);
        let frontier_digest = ato_objects::blake3_reference(&serde_jcs::to_vec(&identity)?);
        Ok(Self {
            version: FRONTIER_VERSION,
            run_id: body.run_id,
            sealed_segments: body.sealed_segments,
            last_writer_order: body.last_writer_order,
            observed_through: body.observed_through,
            causal_cut: body.causal_cut,
            frontier_digest,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, RecordWriterError> {
        let body = RecordFrontierBody {
            run_id: self.run_id.clone(),
            sealed_segments: self.sealed_segments.clone(),
            last_writer_order: self.last_writer_order,
            observed_through: self.observed_through.clone(),
            causal_cut: self.causal_cut.clone(),
        };
        let expected = RecordFrontier::seal(body)?;
        if expected.frontier_digest != self.frontier_digest {
            return Err(RecordWriterError::FrontierIdentityMismatch);
        }
        Ok(serde_jcs::to_vec(&FrontierWire::from(self))?)
    }

    fn identity_bytes(&self) -> Result<Vec<u8>, RecordWriterError> {
        let body = RecordFrontierBody {
            run_id: self.run_id.clone(),
            sealed_segments: self.sealed_segments.clone(),
            last_writer_order: self.last_writer_order,
            observed_through: self.observed_through.clone(),
            causal_cut: self.causal_cut.clone(),
        };
        Ok(serde_jcs::to_vec(&FrontierIdentityWire::from(&body))?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, RecordWriterError> {
        let wire: FrontierWire = serde_json::from_slice(bytes)?;
        if wire.version != FRONTIER_VERSION || serde_jcs::to_vec(&wire)? != bytes {
            return Err(RecordWriterError::NonCanonicalFrontier);
        }
        let actual_digest = ContentRef::parse(&wire.frontier_digest)
            .map_err(|error| RecordWriterError::InvalidReference(error.to_string()))?;
        let body = RecordFrontierBody::try_from(wire)?;
        let frontier = Self::seal(body)?;
        if frontier.frontier_digest != actual_digest {
            return Err(RecordWriterError::FrontierIdentityMismatch);
        }
        Ok(frontier)
    }
}

/// Loads one sealed frontier by identity from the run-scoped frontier store.
pub fn load_frontier(
    records_root: &Path,
    run_id: &str,
    reference: &ContentRef,
) -> Result<RecordFrontier, RecordWriterError> {
    validate_component("run id", run_id)?;
    let root = records_root.join("runs").join(run_id).join("frontiers");
    let path = frontier_path(&root, reference)?;
    let frontier = RecordFrontier::decode(&fs::read(path)?)?;
    if &frontier.frontier_digest != reference || frontier.run_id != run_id {
        return Err(RecordWriterError::FrontierIdentityMismatch);
    }
    Ok(frontier)
}

/// Resolves and revalidates the immutable Record closure selected by a sealed
/// frontier. Descriptor summaries never replace this traversal.
pub fn records_for_frontier(
    records_root: &Path,
    frontier: &RecordFrontier,
    objects: &dyn ObjectStore,
) -> Result<Vec<RecordEnvelopeV2>, RecordWriterError> {
    let segments_root = records_root
        .join("runs")
        .join(&frontier.run_id)
        .join("segments");
    let mut records = Vec::new();
    for expected in &frontier.sealed_segments {
        let path = segment_path(&segments_root, &expected.digest)?;
        let (actual, segment_records) = decode_segment(&fs::read(path)?, objects)?;
        if &actual != expected {
            return Err(RecordWriterError::FrontierSegmentMismatch(
                expected.digest.to_string(),
            ));
        }
        records.extend(segment_records);
    }
    verify_contiguous_orders(&records)?;
    if records.last().map_or(0, |record| record.writer_order) != frontier.last_writer_order {
        return Err(RecordWriterError::FrontierWriterOrderMismatch);
    }
    let seen: BTreeSet<_> = records.iter().map(|record| record.id.clone()).collect();
    let referenced: BTreeSet<_> = records
        .iter()
        .flat_map(|record| record.caused_by.iter().cloned())
        .collect();
    let causal_cut: Vec<_> = seen.difference(&referenced).cloned().collect();
    if causal_cut != frontier.causal_cut {
        return Err(RecordWriterError::FrontierCausalCutMismatch);
    }
    let mut observed: BTreeMap<String, u64> = BTreeMap::new();
    for record in &records {
        observed
            .entry(record.stream.clone())
            .and_modify(|value| *value = (*value).max(record.local_seq))
            .or_insert(record.local_seq);
    }
    if observed != frontier.observed_through {
        return Err(RecordWriterError::FrontierWatermarkMismatch);
    }
    Ok(records)
}

impl From<&RecordFrontierBody> for FrontierIdentityWire {
    fn from(value: &RecordFrontierBody) -> Self {
        Self {
            version: FRONTIER_VERSION,
            run_id: value.run_id.clone(),
            sealed_segments: value
                .sealed_segments
                .iter()
                .map(SegmentWire::from)
                .collect(),
            last_writer_order: value.last_writer_order,
            observed_through: value.observed_through.clone(),
            causal_cut: value.causal_cut.iter().map(ToString::to_string).collect(),
        }
    }
}

impl From<&RecordFrontier> for FrontierWire {
    fn from(value: &RecordFrontier) -> Self {
        let identity = FrontierIdentityWire {
            version: value.version,
            run_id: value.run_id.clone(),
            sealed_segments: value
                .sealed_segments
                .iter()
                .map(SegmentWire::from)
                .collect(),
            last_writer_order: value.last_writer_order,
            observed_through: value.observed_through.clone(),
            causal_cut: value.causal_cut.iter().map(ToString::to_string).collect(),
        };
        Self {
            version: identity.version,
            run_id: identity.run_id,
            sealed_segments: identity.sealed_segments,
            last_writer_order: identity.last_writer_order,
            observed_through: identity.observed_through,
            causal_cut: identity.causal_cut,
            frontier_digest: value.frontier_digest.to_string(),
        }
    }
}

impl TryFrom<FrontierWire> for RecordFrontierBody {
    type Error = RecordWriterError;

    fn try_from(value: FrontierWire) -> Result<Self, Self::Error> {
        validate_component("run id", &value.run_id)?;
        Ok(Self {
            run_id: value.run_id,
            sealed_segments: value
                .sealed_segments
                .into_iter()
                .map(SealedSegment::try_from)
                .collect::<Result<_, _>>()?,
            last_writer_order: value.last_writer_order,
            observed_through: value.observed_through,
            causal_cut: value
                .causal_cut
                .into_iter()
                .map(RecordIdV2::parse)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl From<&SealedSegment> for SegmentWire {
    fn from(value: &SealedSegment) -> Self {
        Self {
            digest: value.digest.to_string(),
            record_count: value.record_count,
            first_writer_order: value.first_writer_order,
            last_writer_order: value.last_writer_order,
            byte_length: value.byte_length,
            payload_closure: value
                .payload_closure
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

impl TryFrom<SegmentWire> for SealedSegment {
    type Error = RecordWriterError;

    fn try_from(value: SegmentWire) -> Result<Self, Self::Error> {
        Ok(Self {
            digest: ContentRef::parse(value.digest)
                .map_err(|error| RecordWriterError::InvalidReference(error.to_string()))?,
            record_count: value.record_count,
            first_writer_order: value.first_writer_order,
            last_writer_order: value.last_writer_order,
            byte_length: value.byte_length,
            payload_closure: value
                .payload_closure
                .into_iter()
                .map(|reference| {
                    ContentRef::parse(reference)
                        .map_err(|error| RecordWriterError::InvalidReference(error.to_string()))
                })
                .collect::<Result<_, _>>()?,
        })
    }
}

fn encode_segment(records: &[RecordEnvelopeV2]) -> Result<Vec<u8>, RecordWriterError> {
    if records.is_empty() {
        return Err(RecordWriterError::EmptySegment);
    }
    verify_contiguous_orders(records)?;
    let payload_closure: BTreeSet<_> = records
        .iter()
        .map(|record| record.payload_ref.to_string())
        .collect();
    let mut record_bytes = Vec::new();
    for record in records {
        write_frame(&mut record_bytes, &encode_record_v2(record)?)?;
    }
    let closure_bytes: usize = payload_closure.iter().map(|value| 4 + value.len()).sum();
    let byte_length =
        SEGMENT_MAGIC.len() + SEGMENT_METADATA_FIELDS * 8 + closure_bytes + record_bytes.len();
    let mut bytes = Vec::with_capacity(byte_length);
    bytes.extend_from_slice(SEGMENT_MAGIC);
    write_u64(&mut bytes, records.len() as u64)?;
    write_u64(&mut bytes, records.first().unwrap().writer_order)?;
    write_u64(&mut bytes, records.last().unwrap().writer_order)?;
    write_u64(&mut bytes, byte_length as u64)?;
    write_u64(&mut bytes, payload_closure.len() as u64)?;
    for reference in payload_closure {
        let length =
            u32::try_from(reference.len()).map_err(|_| RecordWriterError::SegmentLengthOverflow)?;
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(reference.as_bytes());
    }
    bytes.extend_from_slice(&record_bytes);
    Ok(bytes)
}

fn decode_segment(
    bytes: &[u8],
    objects: &dyn ObjectStore,
) -> Result<(SealedSegment, Vec<RecordEnvelopeV2>), RecordWriterError> {
    if !bytes.starts_with(SEGMENT_MAGIC) {
        return Err(RecordWriterError::InvalidSegment("bad magic".to_owned()));
    }
    let digest = ato_objects::blake3_reference(bytes);
    let mut cursor = std::io::Cursor::new(&bytes[SEGMENT_MAGIC.len()..]);
    let record_count = read_u64(&mut cursor)?;
    let first_writer_order = read_u64(&mut cursor)?;
    let last_writer_order = read_u64(&mut cursor)?;
    let byte_length = read_u64(&mut cursor)?;
    let payload_count = read_u64(&mut cursor)?;
    if byte_length != bytes.len() as u64 {
        return Err(RecordWriterError::SegmentByteLength {
            expected: byte_length,
            actual: bytes.len() as u64,
        });
    }
    let mut payload_closure = Vec::new();
    for _ in 0..payload_count {
        let mut length = [0_u8; 4];
        cursor.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        let mut reference = vec![0_u8; length];
        cursor.read_exact(&mut reference)?;
        let reference = String::from_utf8(reference)
            .map_err(|error| RecordWriterError::InvalidSegment(error.to_string()))?;
        payload_closure.push(
            ContentRef::parse(reference)
                .map_err(|error| RecordWriterError::InvalidReference(error.to_string()))?,
        );
    }
    if !payload_closure.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(RecordWriterError::InvalidSegment(
            "payload closure is not unique and sorted".to_owned(),
        ));
    }
    let records_offset = SEGMENT_MAGIC.len() + cursor.position() as usize;
    let records = decode_active(&bytes[records_offset..])?;
    if records.len() as u64 != record_count {
        return Err(RecordWriterError::SegmentRecordCount {
            expected: record_count,
            actual: records.len() as u64,
        });
    }
    verify_contiguous_orders(&records)?;
    if records.first().map(|record| record.writer_order) != Some(first_writer_order)
        || records.last().map(|record| record.writer_order) != Some(last_writer_order)
    {
        return Err(RecordWriterError::SegmentOrderBounds);
    }
    let actual_closure: Vec<_> = records
        .iter()
        .map(|record| record.payload_ref.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if actual_closure != payload_closure {
        return Err(RecordWriterError::SegmentPayloadClosure);
    }
    for reference in &payload_closure {
        objects.metadata(reference)?;
    }
    Ok((
        SealedSegment {
            digest,
            record_count,
            first_writer_order,
            last_writer_order,
            byte_length,
            payload_closure,
        },
        records,
    ))
}

fn decode_active(bytes: &[u8]) -> Result<Vec<RecordEnvelopeV2>, RecordWriterError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut records = Vec::new();
    while cursor.position() < bytes.len() as u64 {
        let length = read_u64(&mut cursor)?;
        let length =
            usize::try_from(length).map_err(|_| RecordWriterError::SegmentLengthOverflow)?;
        let mut record = vec![0_u8; length];
        cursor.read_exact(&mut record)?;
        records.push(decode_record_v2(&record)?);
    }
    Ok(records)
}

fn verify_contiguous_orders(records: &[RecordEnvelopeV2]) -> Result<(), RecordWriterError> {
    for pair in records.windows(2) {
        if pair[0].writer_order.checked_add(1) != Some(pair[1].writer_order) {
            return Err(RecordWriterError::NonContiguousWriterOrder {
                previous: pair[0].writer_order,
                next: pair[1].writer_order,
            });
        }
    }
    Ok(())
}

fn write_frame(mut writer: impl Write, bytes: &[u8]) -> Result<(), RecordWriterError> {
    write_u64(&mut writer, bytes.len() as u64)?;
    writer.write_all(bytes)?;
    Ok(())
}

fn write_u64(mut writer: impl Write, value: u64) -> Result<(), RecordWriterError> {
    writer.write_all(&value.to_be_bytes())?;
    Ok(())
}

fn read_u64(mut reader: impl Read) -> Result<u64, RecordWriterError> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn segment_path(root: &Path, digest: &ContentRef) -> Result<PathBuf, RecordWriterError> {
    if digest.algorithm() != "blake3" {
        return Err(RecordWriterError::InvalidReference(
            "segment digest must use blake3".to_owned(),
        ));
    }
    Ok(root.join(format!("{}.seg", digest.digest())))
}

fn frontier_path(root: &Path, digest: &ContentRef) -> Result<PathBuf, RecordWriterError> {
    if digest.algorithm() != "blake3" {
        return Err(RecordWriterError::InvalidReference(
            "frontier digest must use blake3".to_owned(),
        ));
    }
    Ok(root.join(format!("{}.json", digest.digest())))
}

fn sync_directory(path: &Path) -> Result<(), RecordWriterError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn validate_component(kind: &'static str, value: &str) -> Result<(), RecordWriterError> {
    if value.is_empty()
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
    {
        return Err(RecordWriterError::InvalidComponent {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn set_failure(failure: &Mutex<Option<String>>, message: String) {
    if let Ok(mut slot) = failure.lock()
        && slot.is_none()
    {
        *slot = Some(message);
    }
}

fn check_failure(failure: &Mutex<Option<String>>) -> Result<(), RecordWriterError> {
    let slot = failure
        .lock()
        .map_err(|_| RecordWriterError::Poisoned("writer failure"))?;
    match slot.as_ref() {
        Some(message) => Err(RecordWriterError::Failed(message.clone())),
        None => Ok(()),
    }
}

#[derive(Debug, Error)]
pub enum RecordWriterError {
    #[error("invalid Record Writer configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid {kind} `{value}`")]
    InvalidComponent { kind: &'static str, value: String },
    #[error("Record schema is already registered for {0}")]
    DuplicateSchema(String),
    #[error("no Record schema is registered for {0}")]
    UnsupportedOperation(String),
    #[error("required features are unsupported for {0}")]
    UnsupportedFeatures(String),
    #[error("invalid payload for {operation}: {reason}")]
    InvalidPayload { operation: String, reason: String },
    #[error("bounded Record queue is full; the Run must fail rather than lose a Record")]
    QueueFull,
    #[error("Record Writer is disconnected")]
    Disconnected,
    #[error("Record submission is paused at a Capture Barrier")]
    CapturePaused,
    #[error("Record Writer failed: {0}")]
    Failed(String),
    #[error("Record Writer {0} lock is poisoned")]
    Poisoned(&'static str),
    #[error("unknown causal Record {0}")]
    UnknownCause(String),
    #[error("recovered Record closure contains a missing causal Record")]
    MissingCausalRecord,
    #[error("duplicate Record {0}")]
    DuplicateRecord(String),
    #[error("writer order is non-contiguous: {previous} then {next}")]
    NonContiguousWriterOrder { previous: u64, next: u64 },
    #[error("cannot seal an empty segment")]
    EmptySegment,
    #[error("segment length exceeds the platform limit")]
    SegmentLengthOverflow,
    #[error("invalid segment: {0}")]
    InvalidSegment(String),
    #[error("segment declared {expected} bytes but contains {actual}")]
    SegmentByteLength { expected: u64, actual: u64 },
    #[error("segment declared {expected} Records but contains {actual}")]
    SegmentRecordCount { expected: u64, actual: u64 },
    #[error("segment first/last writer order does not match its Records")]
    SegmentOrderBounds,
    #[error("segment payload closure does not match its Records")]
    SegmentPayloadClosure,
    #[error("segment failed its canonical roundtrip")]
    SegmentRoundtrip,
    #[error("immutable segment differs at {0}")]
    ImmutableSegmentMismatch(PathBuf),
    #[error("immutable frontier differs at {0}")]
    ImmutableFrontierMismatch(PathBuf),
    #[error("segment file is not stored at its digest path: {0}")]
    SegmentPathMismatch(PathBuf),
    #[error("unexpected file in Record segment directory: {0}")]
    UnexpectedRecordFile(PathBuf),
    #[error("Capture Barrier for {stream} requested {requested}, Writer observed {processed}")]
    BarrierIncomplete {
        stream: String,
        requested: u64,
        processed: u64,
    },
    #[error("RecordFrontier is not canonical")]
    NonCanonicalFrontier,
    #[error("RecordFrontier identity does not match its canonical body")]
    FrontierIdentityMismatch,
    #[error("RecordFrontier segment {0} does not match the immutable segment")]
    FrontierSegmentMismatch(String),
    #[error("RecordFrontier last_writer_order does not match the Record closure")]
    FrontierWriterOrderMismatch,
    #[error("RecordFrontier causal cut does not match the Record closure")]
    FrontierCausalCutMismatch,
    #[error("RecordFrontier observed-through watermark does not match the Record closure")]
    FrontierWatermarkMismatch,
    #[error("invalid content reference: {0}")]
    InvalidReference(String),
    #[error(transparent)]
    Repository(#[from] ato_objects::RepositoryError),
    #[error(transparent)]
    Object(#[from] ato_objects::ObjectError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};

    use ato_objects::{MemoryObjectStore, ObjectResolver};

    use super::*;

    fn operation() -> SupportedOperation {
        SupportedOperation::new("ato.pty@1", "input", 1, BTreeSet::new()).unwrap()
    }

    fn schemas(
        validator: impl Fn(&[u8]) -> Result<(), String> + Send + Sync + 'static,
    ) -> RecordSchemaRegistry {
        let mut schemas = RecordSchemaRegistry::default();
        schemas.register(operation(), validator).unwrap();
        schemas
    }

    fn candidate(stream: &str, local_seq: u64, payload: &[u8]) -> RecordCandidate {
        RecordCandidate {
            protocol_id: ProtocolId::parse("ato.pty@1").unwrap(),
            operation_id: OperationId::parse("input").unwrap(),
            port_id: ato_computation::PortId::parse("terminal.main").unwrap(),
            payload: payload.to_vec(),
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: Some("ato.pty.local@1".to_owned()),
            stream: stream.to_owned(),
            local_seq,
            caused_by: Vec::new(),
            observed_at: format!("{local_seq}"),
        }
    }

    fn config(directory: &Path) -> RecordWriterConfig {
        let mut config = RecordWriterConfig::at(directory.join("records"), "run-1");
        config.queue_capacity = 8;
        config.max_segment_records = 8;
        config.max_segment_bytes = 1024 * 1024;
        config
    }

    #[test]
    fn stylus_submission_does_not_wait_for_schema_or_persistence() {
        let directory = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let validator_gate = Arc::clone(&gate);
        let validator_entered = Arc::clone(&entered);
        let pipeline = RecordPipeline::start(
            config(directory.path()),
            Arc::new(MemoryObjectStore::default()),
            schemas(move |_| {
                let (lock, notify) = &*validator_entered;
                *lock.lock().unwrap() = true;
                notify.notify_one();
                let (lock, notify) = &*validator_gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = notify.wait(released).unwrap();
                }
                Ok(())
            }),
        )
        .unwrap();

        pipeline
            .stylus
            .record(candidate("pty.main", 1, br#"{"kind":"input"}"#))
            .unwrap();
        let (lock, notify) = &*entered;
        let mut is_entered = lock.lock().unwrap();
        while !*is_entered {
            is_entered = notify.wait(is_entered).unwrap();
        }
        let (lock, notify) = &*gate;
        *lock.lock().unwrap() = true;
        notify.notify_one();

        let frontier = pipeline.barrier.seal().unwrap();
        assert_eq!(frontier.last_writer_order, 1);
    }

    #[test]
    fn queue_overflow_fails_the_run_instead_of_silently_dropping() {
        let directory = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let validator_gate = Arc::clone(&gate);
        let validator_entered = Arc::clone(&entered);
        let mut writer_config = config(directory.path());
        writer_config.queue_capacity = 1;
        let pipeline = RecordPipeline::start(
            writer_config,
            Arc::new(MemoryObjectStore::default()),
            schemas(move |_| {
                let (lock, notify) = &*validator_entered;
                *lock.lock().unwrap() = true;
                notify.notify_one();
                let (lock, notify) = &*validator_gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = notify.wait(released).unwrap();
                }
                Ok(())
            }),
        )
        .unwrap();

        pipeline
            .stylus
            .record(candidate("pty.main", 1, b"one"))
            .unwrap();
        let (lock, notify) = &*entered;
        let mut is_entered = lock.lock().unwrap();
        while !*is_entered {
            is_entered = notify.wait(is_entered).unwrap();
        }
        pipeline
            .stylus
            .record(candidate("pty.main", 2, b"two"))
            .unwrap();
        let error = pipeline
            .stylus
            .record(candidate("pty.main", 3, b"three"))
            .unwrap_err();
        assert!(error.to_string().contains("queue is full"));
        assert!(pipeline.stylus.health().is_err());

        let (lock, notify) = &*gate;
        *lock.lock().unwrap() = true;
        notify.notify_one();
    }

    #[test]
    fn capture_barrier_seals_one_canonical_segment_and_frontier() {
        let directory = tempfile::tempdir().unwrap();
        let objects = Arc::new(MemoryObjectStore::default());
        let pipeline = RecordPipeline::start(
            config(directory.path()),
            objects.clone(),
            schemas(|_| Ok(())),
        )
        .unwrap();
        pipeline
            .stylus
            .record(candidate("pty.main", 1, b"one"))
            .unwrap();
        pipeline
            .stylus
            .record(candidate("pty.main", 2, b"two"))
            .unwrap();

        let frontier = pipeline.barrier.seal().unwrap();

        assert_eq!(frontier.last_writer_order, 2);
        assert_eq!(frontier.observed_through["pty.main"], 2);
        assert_eq!(frontier.sealed_segments.len(), 1);
        let segment = &frontier.sealed_segments[0];
        assert_eq!(segment.record_count, 2);
        assert_eq!(segment.first_writer_order, 1);
        assert_eq!(segment.last_writer_order, 2);
        assert_eq!(segment.payload_closure.len(), 2);
        for payload in &segment.payload_closure {
            objects.metadata(payload).unwrap();
        }
        let segments_root = directory.path().join("records/runs/run-1/segments");
        let segment_bytes =
            fs::read(segment_path(&segments_root, &segment.digest).unwrap()).unwrap();
        assert_eq!(segment_bytes.len() as u64, segment.byte_length);
        let (_, records) = decode_segment(&segment_bytes, objects.as_ref()).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.writer_order)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            fs::metadata(directory.path().join("records/runs/run-1/active.open"))
                .unwrap()
                .len(),
            0
        );

        let frontier_bytes = frontier.encode().unwrap();
        assert_eq!(RecordFrontier::decode(&frontier_bytes).unwrap(), frontier);
        let loaded = load_frontier(
            &directory.path().join("records"),
            "run-1",
            &frontier.frontier_digest,
        )
        .unwrap();
        assert_eq!(loaded, frontier);
        assert_eq!(
            records_for_frontier(&directory.path().join("records"), &loaded, objects.as_ref())
                .unwrap()
                .len(),
            2
        );
        let json = String::from_utf8(frontier_bytes).unwrap();
        assert!(!json.contains("computation"));
        assert!(!json.contains("snapshot"));
    }

    #[test]
    fn payload_schema_failure_is_reported_at_the_capture_barrier() {
        let directory = tempfile::tempdir().unwrap();
        let pipeline = RecordPipeline::start(
            config(directory.path()),
            Arc::new(MemoryObjectStore::default()),
            schemas(|_| Err("schema mismatch".to_owned())),
        )
        .unwrap();
        pipeline
            .stylus
            .record(candidate("pty.main", 1, b"invalid"))
            .unwrap();

        let error = pipeline.barrier.seal().unwrap_err();

        assert!(error.to_string().contains("schema mismatch"));
        assert!(
            !directory
                .path()
                .join("records/runs/run-1/frontiers")
                .read_dir()
                .unwrap()
                .any(|entry| entry.is_ok())
        );
    }

    #[test]
    fn paused_capture_rejects_later_operations_until_quiesce_lease_is_released() {
        let directory = tempfile::tempdir().unwrap();
        let pipeline = RecordPipeline::start(
            config(directory.path()),
            Arc::new(MemoryObjectStore::default()),
            schemas(|_| Ok(())),
        )
        .unwrap();
        pipeline
            .stylus
            .record(candidate("pty.main", 1, b"one"))
            .unwrap();

        let capture = pipeline.barrier.pause_and_seal().unwrap();
        assert_eq!(capture.frontier.last_writer_order, 1);
        assert!(
            pipeline
                .stylus
                .record(candidate("pty.main", 2, b"two"))
                .unwrap_err()
                .to_string()
                .contains("Capture Barrier")
        );
        drop(capture);
        pipeline
            .stylus
            .record(candidate("pty.main", 2, b"two"))
            .unwrap();
        assert_eq!(pipeline.barrier.seal().unwrap().last_writer_order, 2);
    }

    #[test]
    fn segment_validation_rejects_a_declared_byte_length_mismatch() {
        let objects = MemoryObjectStore::default();
        let payload_ref = objects.put(b"payload").unwrap();
        let record = RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse("ato.pty@1").unwrap(),
            operation_id: OperationId::parse("input").unwrap(),
            port_id: ato_computation::PortId::parse("terminal.main").unwrap(),
            payload_ref,
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: None,
            stream: "pty.main".to_owned(),
            local_seq: 1,
            writer_order: 1,
            caused_by: Vec::new(),
            observed_at: "1".to_owned(),
        })
        .unwrap();
        let mut bytes = encode_segment(&[record]).unwrap();
        let byte_length_offset = SEGMENT_MAGIC.len() + 3 * 8;
        bytes[byte_length_offset + 7] ^= 1;

        assert!(matches!(
            decode_segment(&bytes, &objects),
            Err(RecordWriterError::SegmentByteLength { .. })
        ));
    }
}
