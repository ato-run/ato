//! Durable local computation DAG storage under `.capsule/`.

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
    pub branch: String,
    pub head: ComputationRef,
    pub pid: u32,
    pub process_start_time: String,
    pub process_group: u32,
    pub boot_session: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveRunWire {
    branch: String,
    head: String,
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

    pub fn append_record(
        &self,
        mut record: RecordEnvelope,
    ) -> Result<RecordEnvelope, RepositoryError> {
        validate_name("stream", &record.id.stream)?;
        validate_name("adapter id", &record.adapter_id)?;
        let _transaction = self.lock_transaction()?;
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
        let bytes = serde_jcs::to_vec(&RecordWire::from(&record))?;
        atomic_create(&self.record_path(&record.id), &bytes)?;
        Ok(record)
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
        wire.try_into().map(Some)
    }

    pub fn set_active_run(&self, run: &ActiveRun) -> Result<(), RepositoryError> {
        validate_name("branch", &run.branch)?;
        let bytes = serde_jcs::to_vec(&ActiveRunWire::from(run))?;
        atomic_write(&self.root.join("runs/active.json"), &bytes)
    }

    pub fn clear_active_run(&self) -> Result<(), RepositoryError> {
        let path = self.root.join("runs/active.json");
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
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
            branch: value.branch.clone(),
            head: value.head.to_string(),
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
            branch: value.branch,
            head: parse_computation(&value.head)?,
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
}
