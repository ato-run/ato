//! Durable local computation DAG storage under `.capsule/`.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{FsObjectStore, ObjectError};

const REPOSITORY_DIRECTORY: &str = ".capsule";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleSelector {
    pub capsule: String,
    pub branch: String,
    pub record: Option<u64>,
}

impl FromStr for CapsuleSelector {
    type Err = RepositoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (without_record, record) = match value.rsplit_once('#') {
            Some((selector, number)) => {
                if selector.is_empty() || number.is_empty() {
                    return Err(RepositoryError::InvalidSelector(value.to_owned()));
                }
                let record = number
                    .parse::<u64>()
                    .map_err(|_| RepositoryError::InvalidSelector(value.to_owned()))?;
                (selector, Some(record))
            }
            None => (value, None),
        };
        let (capsule, branch) = match without_record.rsplit_once('@') {
            Some((capsule, branch)) => (capsule, branch),
            None => (without_record, "main"),
        };
        if capsule.is_empty() {
            return Err(RepositoryError::InvalidSelector(value.to_owned()));
        }
        validate_name("branch", branch)?;
        Ok(Self {
            capsule: capsule.to_owned(),
            branch: branch.to_owned(),
            record,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordId {
    pub stream: String,
    pub seq: u64,
}

impl RecordId {
    pub fn new(stream: impl Into<String>, seq: u64) -> Self {
        Self {
            stream: stream.into(),
            seq,
        }
    }
}

/// Protocol-neutral evidence for one adapter-selected interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEnvelope {
    pub id: RecordId,
    pub adapter_id: String,
    pub protocol_id: ProtocolId,
    pub port_id: PortId,
    pub direction: Direction,
    pub payload_ref: ContentRef,
    pub head_before: ComputationRef,
    pub head_after: ComputationRef,
    pub caused_by: Vec<RecordId>,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchOrigin {
    pub computation: ComputationRef,
    pub parent_record: Option<RecordId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchOriginWire {
    computation: String,
    parent_record: Option<RecordId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWire {
    id: RecordId,
    adapter_id: String,
    protocol_id: String,
    port_id: String,
    direction: Direction,
    payload_ref: String,
    head_before: String,
    head_after: String,
    caused_by: Vec<RecordId>,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRun {
    pub token: String,
    pub branch: String,
    pub branch_base: ComputationRef,
    pub head: ComputationRef,
    pub record_seq: u64,
    pub pid: u32,
    pub process_start_time: String,
    pub process_group: u32,
    pub boot_session: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveRunWire {
    token: String,
    branch: String,
    branch_base: String,
    head: String,
    record_seq: u64,
    pid: u32,
    process_start_time: String,
    process_group: u32,
    boot_session: String,
    status: String,
}

#[derive(Debug, Clone)]
pub struct LocalCapsuleRepository {
    project: PathBuf,
    root: PathBuf,
    objects: FsObjectStore,
}

impl LocalCapsuleRepository {
    pub fn open(project: impl Into<PathBuf>) -> Result<Self, RepositoryError> {
        let project = project.into();
        let root = project.join(REPOSITORY_DIRECTORY);
        for directory in [
            root.join("refs/heads"),
            root.join("refs/origins"),
            root.join("records"),
            root.join("runs"),
            root.join("protocols"),
            root.join("contracts"),
            root.join("bindings"),
            root.join("provenance"),
        ] {
            fs::create_dir_all(directory)?;
        }
        let objects = FsObjectStore::open(root.join("objects"))?;
        Ok(Self {
            project,
            root,
            objects,
        })
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn objects(&self) -> &FsObjectStore {
        &self.objects
    }

    pub fn head(&self, branch: &str) -> Result<Option<ComputationRef>, RepositoryError> {
        validate_name("branch", branch)?;
        let path = self.root.join("refs/heads").join(branch);
        match fs::read_to_string(path) {
            Ok(value) => Ok(Some(parse_computation(value.trim())?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically advances a branch while rejecting a stale expected head.
    pub fn update_head(
        &self,
        branch: &str,
        expected: Option<&ComputationRef>,
        next: &ComputationRef,
    ) -> Result<(), RepositoryError> {
        validate_name("branch", branch)?;
        let _transaction = self.lock_transaction()?;
        let actual = self.head(branch)?;
        if actual.as_ref() != expected {
            return Err(RepositoryError::RefConflict {
                branch: branch.to_owned(),
                expected: expected.map(ToString::to_string),
                actual: actual.map(|value| value.to_string()),
            });
        }
        atomic_write(
            &self.root.join("refs/heads").join(branch),
            format!("{next}\n").as_bytes(),
        )
    }

    pub fn create_branch(
        &self,
        branch: &str,
        head: &ComputationRef,
        origin: Option<&BranchOrigin>,
    ) -> Result<(), RepositoryError> {
        validate_name("branch", branch)?;
        let _transaction = self.lock_transaction()?;
        if let Some(actual) = self.head(branch)? {
            return Err(RepositoryError::RefConflict {
                branch: branch.to_owned(),
                expected: None,
                actual: Some(actual.to_string()),
            });
        }
        if let Some(origin) = origin {
            if &origin.computation != head {
                return Err(RepositoryError::InvalidBranchOrigin(branch.to_owned()));
            }
            if let Some(parent) = &origin.parent_record {
                let record = self.record(parent)?;
                if record.head_after != origin.computation {
                    return Err(RepositoryError::InvalidBranchOrigin(branch.to_owned()));
                }
            }
            let bytes = serde_jcs::to_vec(&BranchOriginWire::from(origin))?;
            atomic_create(
                &self
                    .root
                    .join("refs/origins")
                    .join(format!("{branch}.json")),
                &bytes,
            )?;
        }
        atomic_create(
            &self.root.join("refs/heads").join(branch),
            format!("{head}\n").as_bytes(),
        )
    }

    pub fn branch_origin(&self, branch: &str) -> Result<Option<BranchOrigin>, RepositoryError> {
        validate_name("branch", branch)?;
        let path = self
            .root
            .join("refs/origins")
            .join(format!("{branch}.json"));
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let wire: BranchOriginWire = serde_json::from_slice(&bytes)?;
        if serde_jcs::to_vec(&wire)? != bytes {
            return Err(RepositoryError::InvalidBranchOrigin(branch.to_owned()));
        }
        wire.try_into().map(Some)
    }

    pub fn append_record(
        &self,
        mut record: RecordEnvelope,
    ) -> Result<RecordEnvelope, RepositoryError> {
        validate_name("stream", &record.id.stream)?;
        validate_name("adapter id", &record.adapter_id)?;
        let _transaction = self.lock_transaction()?;
        self.append_record_locked(&mut record)
    }

    fn append_record_locked(
        &self,
        record: &mut RecordEnvelope,
    ) -> Result<RecordEnvelope, RepositoryError> {
        let next = self.next_sequence(&record.id.stream)?;
        if record.id.seq != 0 && record.id.seq != next {
            return Err(RepositoryError::Sequence {
                stream: record.id.stream.clone(),
                expected: next,
                actual: record.id.seq,
            });
        }
        record.id.seq = next;
        for cause in &record.caused_by {
            if (cause.stream == record.id.stream && cause.seq >= next)
                || !self.record_path(cause).is_file()
            {
                return Err(RepositoryError::InvalidCause(cause.clone()));
            }
        }
        let bytes = serde_jcs::to_vec(&RecordWire::from(&*record))?;
        atomic_create(&self.record_path(&record.id), &bytes)?;
        Ok(record.clone())
    }

    /// Commits evidence and the live Run cursor under one repository
    /// transaction. A cursor that was not flushed before a process crash is
    /// recovered from the append-only record chain by `active_run`.
    pub fn commit_observation(
        &self,
        token: &str,
        expected_head: &ComputationRef,
        mut record: RecordEnvelope,
    ) -> Result<RecordEnvelope, RepositoryError> {
        validate_name("run token", token)?;
        validate_name("stream", &record.id.stream)?;
        validate_name("adapter id", &record.adapter_id)?;
        let _transaction = self.lock_transaction()?;
        let mut run = self
            .active_run()?
            .ok_or_else(|| RepositoryError::ActiveRunConflict {
                token: token.to_owned(),
                status: "missing".to_owned(),
            })?;
        if run.token != token || &run.head != expected_head || run.branch != record.id.stream {
            return Err(RepositoryError::ActiveRunConflict {
                token: run.token,
                status: run.status,
            });
        }
        if record.caused_by.is_empty() {
            let previous = self
                .records_for_stream(&record.id.stream, None)?
                .last()
                .map(|previous| previous.id.clone());
            let origin = if previous.is_none() {
                self.branch_origin(&record.id.stream)?
                    .and_then(|origin| origin.parent_record)
            } else {
                None
            };
            record.caused_by = previous
                .or(origin)
                .into_iter()
                .collect();
        }
        let committed = self.append_record_locked(&mut record)?;
        run.head = committed.head_after.clone();
        run.record_seq = committed.id.seq;
        let bytes = serde_jcs::to_vec(&ActiveRunWire::from(&run))?;
        atomic_write(&self.root.join("runs/active.json"), &bytes)?;
        Ok(committed)
    }

    pub fn record(&self, id: &RecordId) -> Result<RecordEnvelope, RepositoryError> {
        validate_name("stream", &id.stream)?;
        let bytes = fs::read(self.record_path(id)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RepositoryError::RecordNotFound(id.clone())
            } else {
                RepositoryError::Io(error)
            }
        })?;
        let wire: RecordWire = serde_json::from_slice(&bytes)?;
        if serde_jcs::to_vec(&wire)? != bytes {
            return Err(RepositoryError::NonCanonicalRecord(id.clone()));
        }
        let record: RecordEnvelope = wire.try_into()?;
        if &record.id != id {
            return Err(RepositoryError::RecordIdentityMismatch {
                expected: id.clone(),
                actual: record.id,
            });
        }
        Ok(record)
    }

    pub fn records_for_stream(
        &self,
        stream: &str,
        through: Option<u64>,
    ) -> Result<Vec<RecordEnvelope>, RepositoryError> {
        validate_name("stream", stream)?;
        let mut records = Vec::new();
        let entries = match fs::read_dir(self.root.join("records").join(stream)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(seq) = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse::<u64>().ok())
            else {
                continue;
            };
            if through.is_some_and(|maximum| seq > maximum) {
                continue;
            }
            records.push(self.record(&RecordId::new(stream, seq))?);
        }
        records.sort_by_key(|record| record.id.seq);
        Ok(records)
    }

    pub fn records_for_causal_branch(
        &self,
        branch: &str,
        through: Option<u64>,
    ) -> Result<Vec<RecordEnvelope>, RepositoryError> {
        self.causal_records(branch, through, &mut BTreeSet::new())
    }

    fn causal_records(
        &self,
        branch: &str,
        through: Option<u64>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<Vec<RecordEnvelope>, RepositoryError> {
        if !visiting.insert(branch.to_owned()) {
            return Err(RepositoryError::BranchOriginCycle(branch.to_owned()));
        }
        let origin = self.branch_origin(branch)?;
        let mut records = if let Some(parent) = origin
            .as_ref()
            .and_then(|origin| origin.parent_record.as_ref())
        {
            self.causal_records(&parent.stream, Some(parent.seq), visiting)?
        } else {
            Vec::new()
        };
        visiting.remove(branch);
        let own = self.records_for_stream(branch, through)?;
        let mut current = records
            .last()
            .map(|record| record.head_after.clone())
            .or_else(|| origin.as_ref().map(|origin| origin.computation.clone()));
        for record in &own {
            if current
                .as_ref()
                .is_some_and(|head| head != &record.head_before)
            {
                return Err(RepositoryError::CausalHeadMismatch(record.id.clone()));
            }
            current = Some(record.head_after.clone());
        }
        records.extend(own);
        Ok(records)
    }

    pub fn resolve(&self, selector: &CapsuleSelector) -> Result<ComputationRef, RepositoryError> {
        if let Some(seq) = selector.record {
            let record = self.record(&RecordId::new(&selector.branch, seq))?;
            return Ok(record.head_after);
        }
        self.head(&selector.branch)?
            .ok_or_else(|| RepositoryError::UnknownBranch(selector.branch.clone()))
    }

    pub fn active_run(&self) -> Result<Option<ActiveRun>, RepositoryError> {
        let path = self.root.join("runs/active.json");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let wire: ActiveRunWire = serde_json::from_slice(&bytes)?;
        let mut run: ActiveRun = wire.try_into()?;
        for record in self.records_for_stream(&run.branch, None)? {
            if record.id.seq <= run.record_seq {
                continue;
            }
            let expected_seq = run
                .record_seq
                .checked_add(1)
                .ok_or(RepositoryError::SequenceOverflow)?;
            if record.id.seq != expected_seq {
                return Err(RepositoryError::Sequence {
                    stream: run.branch.clone(),
                    expected: expected_seq,
                    actual: record.id.seq,
                });
            }
            if record.head_before != run.head {
                return Err(RepositoryError::CausalHeadMismatch(record.id));
            }
            run.head = record.head_after;
            run.record_seq = record.id.seq;
        }
        Ok(Some(run))
    }

    pub fn claim_active_run(&self, run: &ActiveRun) -> Result<(), RepositoryError> {
        validate_name("run token", &run.token)?;
        validate_name("branch", &run.branch)?;
        let _transaction = self.lock_transaction()?;
        if let Some(active) = self.active_run()? {
            return Err(RepositoryError::ActiveRunConflict {
                token: active.token,
                status: active.status,
            });
        }
        if run.status != "starting" {
            return Err(RepositoryError::InvalidRunStatus(run.status.clone()));
        }
        let bytes = serde_jcs::to_vec(&ActiveRunWire::from(run))?;
        atomic_create(&self.root.join("runs/active.json"), &bytes)
    }

    pub fn activate_run(&self, token: &str, run: &ActiveRun) -> Result<(), RepositoryError> {
        let _transaction = self.lock_transaction()?;
        let claimed = self
            .active_run()?
            .ok_or_else(|| RepositoryError::ActiveRunConflict {
                token: token.to_owned(),
                status: "missing".to_owned(),
            })?;
        if claimed.token != token || claimed.status != "starting" {
            return Err(RepositoryError::ActiveRunConflict {
                token: claimed.token,
                status: claimed.status,
            });
        }
        if run.token != token
            || run.status != "active"
            || run.branch_base != claimed.branch_base
            || run.head != claimed.head
            || run.record_seq != claimed.record_seq
        {
            return Err(RepositoryError::InvalidRunStatus(run.status.clone()));
        }
        let bytes = serde_jcs::to_vec(&ActiveRunWire::from(run))?;
        atomic_write(&self.root.join("runs/active.json"), &bytes)
    }

    pub fn release_active_run(&self, token: &str) -> Result<(), RepositoryError> {
        let _transaction = self.lock_transaction()?;
        let Some(active) = self.active_run()? else {
            return Ok(());
        };
        if active.token != token {
            return Err(RepositoryError::ActiveRunConflict {
                token: active.token,
                status: active.status,
            });
        }
        fs::remove_file(self.root.join("runs/active.json"))?;
        Ok(())
    }

    fn next_sequence(&self, stream: &str) -> Result<u64, RepositoryError> {
        let mut maximum = 0;
        let directory = self.root.join("records").join(stream);
        fs::create_dir_all(&directory)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(stem) = entry.path().file_stem().and_then(|value| value.to_str())
                && let Ok(seq) = stem.parse::<u64>()
            {
                maximum = maximum.max(seq);
            }
        }
        maximum
            .checked_add(1)
            .ok_or(RepositoryError::SequenceOverflow)
    }

    fn record_path(&self, id: &RecordId) -> PathBuf {
        self.root
            .join("records")
            .join(&id.stream)
            .join(format!("{}.json", id.seq))
    }

    fn lock_transaction(&self) -> Result<TransactionGuard, RepositoryError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("transaction.lock"))?;
        file.lock_exclusive()?;
        Ok(TransactionGuard(file))
    }
}

struct TransactionGuard(fs::File);

impl Drop for TransactionGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl From<&RecordEnvelope> for RecordWire {
    fn from(value: &RecordEnvelope) -> Self {
        Self {
            id: value.id.clone(),
            adapter_id: value.adapter_id.clone(),
            protocol_id: value.protocol_id.to_string(),
            port_id: value.port_id.to_string(),
            direction: value.direction,
            payload_ref: value.payload_ref.to_string(),
            head_before: value.head_before.to_string(),
            head_after: value.head_after.to_string(),
            caused_by: value.caused_by.clone(),
            observed_at: value.observed_at.clone(),
        }
    }
}

impl From<&BranchOrigin> for BranchOriginWire {
    fn from(value: &BranchOrigin) -> Self {
        Self {
            computation: value.computation.to_string(),
            parent_record: value.parent_record.clone(),
        }
    }
}

impl TryFrom<BranchOriginWire> for BranchOrigin {
    type Error = RepositoryError;

    fn try_from(value: BranchOriginWire) -> Result<Self, Self::Error> {
        Ok(Self {
            computation: parse_computation(&value.computation)?,
            parent_record: value.parent_record,
        })
    }
}

impl TryFrom<RecordWire> for RecordEnvelope {
    type Error = RepositoryError;

    fn try_from(value: RecordWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            adapter_id: value.adapter_id,
            protocol_id: ProtocolId::parse(value.protocol_id)
                .map_err(|error| RepositoryError::InvalidReference(error.to_string()))?,
            port_id: PortId::parse(value.port_id)
                .map_err(|error| RepositoryError::InvalidReference(error.to_string()))?,
            direction: value.direction,
            payload_ref: ContentRef::parse(value.payload_ref)
                .map_err(|error| RepositoryError::InvalidReference(error.to_string()))?,
            head_before: parse_computation(&value.head_before)?,
            head_after: parse_computation(&value.head_after)?,
            caused_by: value.caused_by,
            observed_at: value.observed_at,
        })
    }
}

impl From<&ActiveRun> for ActiveRunWire {
    fn from(value: &ActiveRun) -> Self {
        Self {
            token: value.token.clone(),
            branch: value.branch.clone(),
            branch_base: value.branch_base.to_string(),
            head: value.head.to_string(),
            record_seq: value.record_seq,
            pid: value.pid,
            process_start_time: value.process_start_time.clone(),
            process_group: value.process_group,
            boot_session: value.boot_session.clone(),
            status: value.status.clone(),
        }
    }
}

impl TryFrom<ActiveRunWire> for ActiveRun {
    type Error = RepositoryError;

    fn try_from(value: ActiveRunWire) -> Result<Self, Self::Error> {
        Ok(Self {
            token: value.token,
            branch: value.branch,
            branch_base: parse_computation(&value.branch_base)?,
            head: parse_computation(&value.head)?,
            record_seq: value.record_seq,
            pid: value.pid,
            process_start_time: value.process_start_time,
            process_group: value.process_group,
            boot_session: value.boot_session,
            status: value.status,
        })
    }
}

fn parse_computation(value: &str) -> Result<ComputationRef, RepositoryError> {
    ComputationRef::parse(value)
        .map_err(|error| RepositoryError::InvalidReference(error.to_string()))
}

fn validate_name(kind: &'static str, value: &str) -> Result<(), RepositoryError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
        || value.contains("..")
    {
        return Err(RepositoryError::InvalidName {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RepositoryError> {
    let parent = path.parent().ok_or(RepositoryError::MissingParent)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.new",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ref"),
        std::process::id()
    ));
    let mut file = fs::File::create(&temporary)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), RepositoryError> {
    let parent = path.parent().ok_or(RepositoryError::MissingParent)?;
    fs::create_dir_all(parent)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("local capsule repository I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Objects(#[from] ObjectError),
    #[error("local capsule repository JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid capsule selector `{0}`; expected <capsule>[@branch][#record]")]
    InvalidSelector(String),
    #[error("invalid {kind} `{value}`")]
    InvalidName { kind: &'static str, value: String },
    #[error("invalid stored reference: {0}")]
    InvalidReference(String),
    #[error("unknown branch `{0}`")]
    UnknownBranch(String),
    #[error("branch `{branch}` changed concurrently (expected {expected:?}, actual {actual:?})")]
    RefConflict {
        branch: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("active Run lease is already held by `{token}` in `{status}` state")]
    ActiveRunConflict { token: String, status: String },
    #[error("invalid active Run status `{0}`")]
    InvalidRunStatus(String),
    #[error("invalid branch origin for `{0}`")]
    InvalidBranchOrigin(String),
    #[error("branch origin cycle includes `{0}`")]
    BranchOriginCycle(String),
    #[error("causal record closure has a head mismatch at {0:?}")]
    CausalHeadMismatch(RecordId),
    #[error("record sequence for `{stream}` must be {expected}, got {actual}")]
    Sequence {
        stream: String,
        expected: u64,
        actual: u64,
    },
    #[error("record sequence overflow")]
    SequenceOverflow,
    #[error("record {0:?} does not exist")]
    RecordNotFound(RecordId),
    #[error("record {0:?} is not canonical JCS")]
    NonCanonicalRecord(RecordId),
    #[error("causal parent record {0:?} does not exist before this record")]
    InvalidCause(RecordId),
    #[error("record identity mismatch (expected {expected:?}, actual {actual:?})")]
    RecordIdentityMismatch {
        expected: RecordId,
        actual: RecordId,
    },
    #[error("repository path has no parent")]
    MissingParent,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn reference(byte: &str) -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", byte.repeat(64))).unwrap()
    }

    fn evidence(
        stream: &str,
        head_before: &ComputationRef,
        head_after: &ComputationRef,
    ) -> RecordEnvelope {
        RecordEnvelope {
            id: RecordId::new(stream, 0),
            adapter_id: "ato.workspace@1".to_owned(),
            protocol_id: ProtocolId::parse("ato.workspace@1").unwrap(),
            port_id: PortId::parse("workspace.main").unwrap(),
            direction: Direction::Inbound,
            payload_ref: ContentRef::parse(format!("blake3:{}", "d".repeat(64))).unwrap(),
            head_before: head_before.clone(),
            head_after: head_after.clone(),
            caused_by: Vec::new(),
            observed_at: "2030-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn selectors_are_independent_from_cli_parsing() {
        assert_eq!(
            "hoge".parse::<CapsuleSelector>().unwrap(),
            CapsuleSelector {
                capsule: "hoge".to_owned(),
                branch: "main".to_owned(),
                record: None,
            }
        );
        assert_eq!(
            "hoge@experiment#42".parse::<CapsuleSelector>().unwrap(),
            CapsuleSelector {
                capsule: "hoge".to_owned(),
                branch: "experiment".to_owned(),
                record: Some(42),
            }
        );
        assert!("hoge@../main".parse::<CapsuleSelector>().is_err());
    }

    #[test]
    fn branch_refs_and_record_frontiers_preserve_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let repository = LocalCapsuleRepository::open(directory.path()).unwrap();
        let c0 = reference("a");
        let c1 = reference("b");
        let c2 = reference("c");
        repository.update_head("main", None, &c0).unwrap();
        repository.update_head("experiment", None, &c0).unwrap();
        let record = repository
            .append_record(evidence("main", &c0, &c1))
            .unwrap();
        assert_eq!(record.id, RecordId::new("main", 1));
        repository.update_head("main", Some(&c0), &c2).unwrap();
        assert_eq!(repository.head("experiment").unwrap(), Some(c0));
        assert_eq!(
            repository.resolve(&"demo@main#1".parse().unwrap()).unwrap(),
            c1
        );
    }

    #[test]
    fn concurrent_ref_compare_and_swap_has_exactly_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let repository = LocalCapsuleRepository::open(directory.path()).unwrap();
        let c0 = reference("a");
        repository.update_head("main", None, &c0).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [reference("b"), reference("c")]
            .into_iter()
            .map(|next| {
                let repository = repository.clone();
                let expected = c0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    repository.update_head("main", Some(&expected), &next)
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(RepositoryError::RefConflict { .. })))
                .count(),
            1
        );
    }

    #[test]
    fn concurrent_records_are_append_only_and_stream_local() {
        let directory = tempfile::tempdir().unwrap();
        let repository = LocalCapsuleRepository::open(directory.path()).unwrap();
        let c0 = reference("a");
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = ["main", "main"]
            .into_iter()
            .map(|stream| {
                let repository = repository.clone();
                let head = c0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..25 {
                        repository
                            .append_record(evidence(stream, &head, &head))
                            .unwrap();
                    }
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        let main = repository.records_for_stream("main", None).unwrap();
        assert_eq!(main.len(), 50);
        assert_eq!(
            main.iter().map(|record| record.id.seq).collect::<Vec<_>>(),
            (1..=50).collect::<Vec<_>>()
        );
        let experiment = repository
            .append_record(evidence("experiment", &c0, &c0))
            .unwrap();
        assert_eq!(experiment.id, RecordId::new("experiment", 1));
    }

    #[test]
    fn branch_record_closure_includes_parent_evolution_without_affecting_identity() {
        let directory = tempfile::tempdir().unwrap();
        let repository = LocalCapsuleRepository::open(directory.path()).unwrap();
        let c0 = reference("a");
        let c1 = reference("b");
        let c2 = reference("c");
        repository.create_branch("main", &c0, None).unwrap();
        let parent = repository
            .append_record(evidence("main", &c0, &c1))
            .unwrap();
        repository.update_head("main", Some(&c0), &c1).unwrap();
        repository
            .create_branch(
                "experiment",
                &c1,
                Some(&BranchOrigin {
                    computation: c1.clone(),
                    parent_record: Some(parent.id.clone()),
                }),
            )
            .unwrap();
        let mut child = evidence("experiment", &c1, &c2);
        child.caused_by = vec![parent.id.clone()];
        repository.append_record(child).unwrap();

        let closure = repository
            .records_for_causal_branch("experiment", None)
            .unwrap();
        assert_eq!(
            closure
                .iter()
                .map(|record| record.id.clone())
                .collect::<Vec<_>>(),
            vec![parent.id, RecordId::new("experiment", 1)]
        );
        assert_eq!(repository.head("experiment").unwrap(), Some(c1));
    }

    #[test]
    fn active_cursor_recovers_an_appended_observation_after_writer_crash() {
        let directory = tempfile::tempdir().unwrap();
        let repository = LocalCapsuleRepository::open(directory.path()).unwrap();
        let c0 = reference("a");
        let c1 = reference("b");
        repository.create_branch("main", &c0, None).unwrap();
        repository
            .claim_active_run(&ActiveRun {
                token: "test-token".to_owned(),
                branch: "main".to_owned(),
                branch_base: c0.clone(),
                head: c0.clone(),
                record_seq: 0,
                pid: 0,
                process_start_time: String::new(),
                process_group: 0,
                boot_session: String::new(),
                status: "starting".to_owned(),
            })
            .unwrap();
        repository
            .append_record(evidence("main", &c0, &c1))
            .unwrap();

        let recovered = repository.active_run().unwrap().unwrap();
        assert_eq!(recovered.head, c1);
        assert_eq!(recovered.record_seq, 1);
    }
}
