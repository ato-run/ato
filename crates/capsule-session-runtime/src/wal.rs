use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use capsule_protocol::{ConnectorId, ContentRef, Direction, IoRecord, Payload, RecordKindId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BoundaryOperationId, EffectIntent, EffectState};

const FRAME_MAGIC: &[u8; 8] = b"ATOWAL1\0";
const COMMIT_MAGIC: &[u8; 8] = b"COMMIT1\0";
const CHECKSUM_BYTES: usize = 32;
const MAX_WAL_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalDirection {
    Ingress,
    Egress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum WalPayload {
    Inline(Vec<u8>),
    Object(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalRecord {
    pub seq: u64,
    pub offset_ns: Option<u64>,
    pub observed_at_unix_ns: Option<i64>,
    pub connector: String,
    pub direction: WalDirection,
    pub kind: String,
    pub payload: WalPayload,
}

impl From<&IoRecord> for WalRecord {
    fn from(record: &IoRecord) -> Self {
        Self {
            seq: record.seq,
            offset_ns: record.offset_ns,
            observed_at_unix_ns: record.observed_at_unix_ns,
            connector: record.connector.to_string(),
            direction: match record.direction {
                Direction::Ingress => WalDirection::Ingress,
                Direction::Egress => WalDirection::Egress,
            },
            kind: record.kind.to_string(),
            payload: match &record.payload {
                Payload::Inline(bytes) => WalPayload::Inline(bytes.clone()),
                Payload::Object(reference) => WalPayload::Object(reference.to_string()),
            },
        }
    }
}

impl TryFrom<WalRecord> for IoRecord {
    type Error = WalError;

    fn try_from(record: WalRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            seq: record.seq,
            offset_ns: record.offset_ns,
            observed_at_unix_ns: record.observed_at_unix_ns,
            connector: ConnectorId::parse(record.connector)
                .map_err(|error| WalError::InvalidRecord(error.to_string()))?,
            direction: match record.direction {
                WalDirection::Ingress => Direction::Ingress,
                WalDirection::Egress => Direction::Egress,
            },
            kind: RecordKindId::parse(record.kind)
                .map_err(|error| WalError::InvalidRecord(error.to_string()))?,
            payload: match record.payload {
                WalPayload::Inline(bytes) => Payload::Inline(bytes),
                WalPayload::Object(reference) => Payload::Object(
                    ContentRef::parse(reference)
                        .map_err(|error| WalError::InvalidRecord(error.to_string()))?,
                ),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "entry")]
pub enum WalEntry {
    RecordCandidate {
        operation_id: BoundaryOperationId,
        record: WalRecord,
        effect: Option<EffectIntent>,
    },
    DeliveryReleased {
        operation_id: BoundaryOperationId,
    },
    DeliveryAcknowledged {
        operation_id: BoundaryOperationId,
    },
    EffectTransition {
        operation_id: BoundaryOperationId,
        state: EffectState,
    },
    HighWaterMark {
        seq: u64,
    },
}

impl WalEntry {
    fn seq(&self) -> Option<u64> {
        match self {
            Self::RecordCandidate { record, .. } => Some(record.seq),
            Self::HighWaterMark { seq } => Some(*seq),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct SessionWal {
    path: PathBuf,
    file: File,
}

impl SessionWal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path, file })
    }

    /// Appends a group and makes the entire group durable with one sync.
    /// Callers release delivery/dispatch only after this method returns.
    pub fn append_batch(&mut self, entries: &[WalEntry]) -> Result<(), WalError> {
        for entry in entries {
            let body = serde_json::to_vec(entry)?;
            if body.is_empty() || body.len() > MAX_WAL_FRAME_BYTES {
                return Err(WalError::FrameTooLarge(body.len()));
            }
            let length =
                u32::try_from(body.len()).map_err(|_| WalError::FrameTooLarge(body.len()))?;
            self.file.write_all(FRAME_MAGIC)?;
            self.file.write_all(&length.to_be_bytes())?;
            self.file.write_all(&body)?;
            self.file.write_all(blake3::hash(&body).as_bytes())?;
            self.file.write_all(COMMIT_MAGIC)?;
        }
        self.file.sync_data()?;
        Ok(())
    }

    pub fn recover(&self) -> Result<RecoveredJournal, WalError> {
        recover_path(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredJournal {
    pub entries: Vec<WalEntry>,
    pub discarded_tail: bool,
    pub durable_high_water_mark: Option<u64>,
}

fn recover_path(path: &Path) -> Result<RecoveredJournal, WalError> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(0))?;
    let mut entries = Vec::new();
    let mut discarded_tail = false;

    loop {
        let mut magic = [0_u8; 8];
        let count = file.read(&mut magic[..1])?;
        if count == 0 {
            break;
        }
        if file.read_exact(&mut magic[1..]).is_err() || &magic != FRAME_MAGIC {
            discarded_tail = true;
            break;
        }
        let mut length_bytes = [0_u8; 4];
        if file.read_exact(&mut length_bytes).is_err() {
            discarded_tail = true;
            break;
        }
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > MAX_WAL_FRAME_BYTES {
            discarded_tail = true;
            break;
        }
        let mut body = vec![0_u8; length];
        let mut checksum = [0_u8; CHECKSUM_BYTES];
        let mut commit = [0_u8; 8];
        if file.read_exact(&mut body).is_err()
            || file.read_exact(&mut checksum).is_err()
            || file.read_exact(&mut commit).is_err()
        {
            discarded_tail = true;
            break;
        }
        if checksum != *blake3::hash(&body).as_bytes() || &commit != COMMIT_MAGIC {
            discarded_tail = true;
            break;
        }
        match serde_json::from_slice(&body) {
            Ok(entry) => entries.push(entry),
            Err(_) => {
                discarded_tail = true;
                break;
            }
        }
    }

    let durable_high_water_mark = entries.iter().filter_map(WalEntry::seq).max();
    Ok(RecoveredJournal {
        entries,
        discarded_tail,
        durable_high_water_mark,
    })
}

#[derive(Debug, Error)]
pub enum WalError {
    #[error("session WAL I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("session WAL JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session WAL frame has invalid size {0}")]
    FrameTooLarge(usize),
    #[error("session WAL record is invalid: {0}")]
    InvalidRecord(String),
}

#[cfg(test)]
mod tests {
    use std::io::Seek;

    use capsule_protocol::{Payload, RecordKindId};

    use super::*;

    fn record(seq: u64) -> IoRecord {
        IoRecord {
            seq,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").expect("connector"),
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").expect("kind"),
            payload: Payload::Inline(format!("input-{seq}").into_bytes()),
        }
    }

    fn candidate(seq: u64) -> WalEntry {
        WalEntry::RecordCandidate {
            operation_id: BoundaryOperationId::parse(format!("op-{seq}")).expect("operation"),
            record: WalRecord::from(&record(seq)),
            effect: None,
        }
    }

    #[test]
    fn group_commit_recovers_all_entries_and_high_water_mark() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut wal =
            SessionWal::open(directory.path().join("journal/wal-000001")).expect("open WAL");
        wal.append_batch(&[candidate(100), candidate(101), candidate(102)])
            .expect("append batch");

        let recovered = wal.recover().expect("recover");
        assert_eq!(recovered.entries.len(), 3);
        assert_eq!(recovered.durable_high_water_mark, Some(102));
        assert!(!recovered.discarded_tail);
    }

    #[test]
    fn recovery_discards_only_incomplete_tail() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("journal/wal-000001");
        let mut wal = SessionWal::open(&path).expect("open WAL");
        wal.append_batch(&[candidate(1), candidate(2)])
            .expect("append batch");
        wal.file.write_all(FRAME_MAGIC).expect("partial frame");
        wal.file.write_all(&100_u32.to_be_bytes()).expect("length");
        wal.file.write_all(b"partial").expect("partial body");
        wal.file.flush().expect("flush partial tail");
        wal.file.rewind().expect("rewind");

        let recovered = wal.recover().expect("recover");
        assert_eq!(recovered.entries, vec![candidate(1), candidate(2)]);
        assert!(recovered.discarded_tail);
        assert_eq!(recovered.durable_high_water_mark, Some(2));
    }

    #[test]
    fn wal_record_round_trips_semantic_record_without_serde_on_domain() {
        let expected = record(7);
        let actual = IoRecord::try_from(WalRecord::from(&expected)).expect("decode WAL record");
        assert_eq!(actual, expected);
    }
}
