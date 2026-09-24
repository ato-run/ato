//! The start record an attempt writes before anything of it can have an
//! effect, and the rule that record enforces.
//!
//! ```text
//! admission ──▶ begin (durable: written, synced, renamed)  ──▶ execute ...
//!                  │                                              │
//!                  └── refuses: an earlier attempt of this        ▼
//!                      request never finished (UNKNOWN), or   finish
//!                      this attempt already started
//! ```
//!
//! A record that was begun and never finished is UNKNOWN: the process may
//! have stopped before the first step or after the last effect, and nothing
//! here can tell which. It is never read back as "did not run". While a
//! request has an UNKNOWN attempt, no further attempt of that request starts
//! — not a retry, not another Derivation, not a redelivery.
//!
//! Local and small on purpose: one directory per request, one file per
//! attempt, an exclusive lock around the check-and-write. Hosted jobs keep
//! their own durable attempt rows and fences; this is the Runtime's record.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What an attempt was allowed to be, recorded before it ran. Identifiers
/// only; never a secret, never a Binding value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartIdentity {
    pub contract_ref: String,
    pub derivation_ref: String,
    pub runtime_id: String,
    /// The effect class the Runtime admitted.
    pub effects: String,
    /// The network the build was allowed.
    pub network: String,
    /// Who authorized the Derivation's effects: `unattended` or
    /// `user_invoked`. Absent from records written before it was kept.
    #[serde(default)]
    pub authorization: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Started,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub schema: String,
    pub request_id: String,
    pub attempt_id: String,
    pub state: AttemptState,
    pub identity: StartIdentity,
    /// How it ended, once it did: `verified`, `failed`, `cleanup_failed`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

pub const ATTEMPT_RECORD_SCHEMA: &str = "ato.formation.attempt-record/1";

/// What the Runtime's durable record says about one attempt — the fact a
/// coordinator decides UNKNOWN from, rather than from how the attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptRecordState {
    /// No start record: the attempt was refused before it began (admission,
    /// an UNKNOWN sibling, a record that could not be written). Nothing of
    /// the candidate ran.
    NotStarted,
    /// The start and the finish are both durable: the attempt ran and its
    /// result is the one reported.
    Finished,
    /// The start is durable and the finish is not: the attempt ran, and what
    /// it did is not known from this Runtime's record.
    StartedUnfinished,
}

impl AttemptRecordState {
    /// Whether anything of the candidate may have run.
    pub fn execution_started(self) -> bool {
        self != Self::NotStarted
    }
}

/// Where an attempt's start and finish are made durable. [`AttemptJournal`]
/// is the Runtime's; a test substitutes one whose writes fail.
pub trait AttemptLedger {
    /// Durably record that the attempt is starting, or refuse.
    fn start(
        &self,
        request_id: &str,
        attempt_id: &str,
        identity: StartIdentity,
    ) -> std::result::Result<Box<dyn StartedRecord>, BeginRefusal>;
}

/// A durable start, waiting for its finish.
pub trait StartedRecord {
    /// Record how the attempt ended.
    fn finish(self: Box<Self>, outcome: &str) -> Result<()>;
}

/// Why an attempt was not begun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginRefusal {
    /// An earlier attempt of this request started and never finished.
    Unknown { attempt_id: String },
    /// This attempt was already begun once. A redelivery never re-executes.
    AlreadyStarted { state: AttemptState },
    /// The record could not be made durable, so nothing may start.
    NotDurable { reason: String },
}

impl BeginRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "request_effect_unknown",
            Self::AlreadyStarted { .. } => "attempt_already_started",
            Self::NotDurable { .. } => "start_record_unavailable",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unknown { attempt_id } => format!(
                "attempt {attempt_id} of this request started and never finished; whether it had \
                 an effect is unknown, so no further attempt of this request is started"
            ),
            Self::AlreadyStarted { state } => format!(
                "this attempt was already started ({}); it is never executed twice",
                match state {
                    AttemptState::Started => "unfinished",
                    AttemptState::Finished => "finished",
                }
            ),
            Self::NotDurable { reason } => {
                format!("the attempt could not be recorded before it started: {reason}")
            }
        }
    }
}

/// The Runtime's attempt records, rooted at one directory it owns.
#[derive(Debug, Clone)]
pub struct AttemptJournal {
    root: PathBuf,
}

/// A begun attempt. Dropping it without [`StartedAttempt::finish`] leaves the
/// record UNKNOWN, which is the point: a panic or a kill is not a clean end.
#[derive(Debug)]
pub struct StartedAttempt {
    path: PathBuf,
    record: AttemptRecord,
}

impl AttemptJournal {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn request_dir(&self, request_id: &str) -> PathBuf {
        self.root.join(name_for(request_id))
    }

    /// Durably record that `attempt_id` of `request_id` is starting, unless
    /// the request has an UNKNOWN attempt or this attempt already started.
    pub fn begin(
        &self,
        request_id: &str,
        attempt_id: &str,
        identity: StartIdentity,
    ) -> std::result::Result<StartedAttempt, BeginRefusal> {
        let not_durable = |error: anyhow::Error| BeginRefusal::NotDurable {
            reason: format!("{error:#}"),
        };
        let dir = self.request_dir(request_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))
            .map_err(not_durable)?;
        let _lock = lock(&dir.join(".lock")).map_err(not_durable)?;

        let path = dir.join(format!("{}.json", name_for(attempt_id)));
        for existing in read_records(&dir).map_err(not_durable)? {
            if existing.attempt_id == attempt_id {
                return Err(BeginRefusal::AlreadyStarted {
                    state: existing.state,
                });
            }
            if existing.state == AttemptState::Started {
                return Err(BeginRefusal::Unknown {
                    attempt_id: existing.attempt_id,
                });
            }
        }
        let record = AttemptRecord {
            schema: ATTEMPT_RECORD_SCHEMA.to_owned(),
            request_id: request_id.to_owned(),
            attempt_id: attempt_id.to_owned(),
            state: AttemptState::Started,
            identity,
            outcome: None,
        };
        write_durably(&path, &record).map_err(not_durable)?;
        Ok(StartedAttempt { path, record })
    }

    /// Every record of a request, as written.
    pub fn records(&self, request_id: &str) -> Result<Vec<AttemptRecord>> {
        let dir = self.request_dir(request_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        read_records(&dir)
    }
}

impl AttemptLedger for AttemptJournal {
    fn start(
        &self,
        request_id: &str,
        attempt_id: &str,
        identity: StartIdentity,
    ) -> std::result::Result<Box<dyn StartedRecord>, BeginRefusal> {
        let started = self.begin(request_id, attempt_id, identity)?;
        Ok(Box::new(started))
    }
}

impl StartedRecord for StartedAttempt {
    fn finish(self: Box<Self>, outcome: &str) -> Result<()> {
        StartedAttempt::finish(*self, outcome)
    }
}

impl StartedAttempt {
    pub fn record(&self) -> &AttemptRecord {
        &self.record
    }

    /// Record how the attempt ended. Only a finished record lets the request
    /// start another attempt.
    pub fn finish(mut self, outcome: &str) -> Result<()> {
        self.record.state = AttemptState::Finished;
        self.record.outcome = Some(outcome.to_owned());
        write_durably(&self.path, &self.record)
    }
}

/// A file name that cannot escape the directory, whatever the id is.
fn name_for(id: &str) -> String {
    let readable: String = id
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let digest = format!("{:x}", Sha256::digest(id.as_bytes()));
    format!("{readable}-{}", &digest[..16])
}

fn read_records(dir: &Path) -> Result<Vec<AttemptRecord>> {
    let mut records = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes =
            std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        // An unreadable record is not an absent one: refuse rather than
        // treat it as "nothing started".
        let record: AttemptRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not an attempt record", path.display()))?;
        records.push(record);
    }
    Ok(records)
}

/// Write, sync, rename, sync the directory: after this returns the record is
/// what a restarted process reads.
fn write_durably(path: &Path, record: &AttemptRecord) -> Result<()> {
    let dir = path.parent().context("record has no directory")?;
    let temporary = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("record")
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        file.write_all(&serde_json::to_vec_pretty(record)?)?;
        file.sync_all().context("cannot sync the attempt record")?;
    }
    std::fs::rename(&temporary, path)
        .with_context(|| format!("cannot move the attempt record into {}", path.display()))?;
    File::open(dir)
        .and_then(|directory| directory.sync_all())
        .context("cannot sync the attempt record directory")?;
    Ok(())
}

/// An exclusive advisory lock, released when the file is dropped.
fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot lock {}", path.display()));
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> StartIdentity {
        StartIdentity {
            contract_ref: "sha256:k".to_owned(),
            derivation_ref: "sha256:d".to_owned(),
            runtime_id: "local".to_owned(),
            effects: "pure".to_owned(),
            network: "denied".to_owned(),
            authorization: "unattended".to_owned(),
        }
    }

    #[test]
    fn an_unfinished_attempt_stops_every_later_attempt_of_its_request() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AttemptJournal::new(dir.path());
        let first = journal.begin("req", "a1", identity()).unwrap();
        // The process "stops" here: the record is never finished.
        drop(first);

        // A restarted process reads the same directory.
        let restarted = AttemptJournal::new(dir.path());
        assert_eq!(
            restarted.begin("req", "a2", identity()).unwrap_err(),
            BeginRefusal::Unknown {
                attempt_id: "a1".to_owned()
            }
        );
        // Redelivering the unfinished attempt does not run it again either.
        assert!(matches!(
            restarted.begin("req", "a1", identity()).unwrap_err(),
            BeginRefusal::AlreadyStarted {
                state: AttemptState::Started
            }
        ));
        // A different request is not held by it.
        assert!(restarted.begin("other", "b1", identity()).is_ok());
    }

    #[test]
    fn a_finished_attempt_lets_the_request_continue_and_is_never_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AttemptJournal::new(dir.path());
        journal
            .begin("req", "a1", identity())
            .unwrap()
            .finish("failed")
            .unwrap();
        assert!(matches!(
            journal.begin("req", "a1", identity()).unwrap_err(),
            BeginRefusal::AlreadyStarted {
                state: AttemptState::Finished
            }
        ));
        let second = journal.begin("req", "a2", identity()).unwrap();
        assert_eq!(second.record().state, AttemptState::Started);
        let records = journal.records("req").unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn an_unwritable_journal_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, "not a directory").unwrap();
        let journal = AttemptJournal::new(&blocked);
        assert!(matches!(
            journal.begin("req", "a1", identity()).unwrap_err(),
            BeginRefusal::NotDurable { .. }
        ));
    }

    #[test]
    fn an_unreadable_record_is_not_read_as_nothing_started() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AttemptJournal::new(dir.path());
        journal
            .begin("req", "a1", identity())
            .unwrap()
            .finish("ok")
            .unwrap();
        let request = journal.request_dir("req");
        std::fs::write(request.join("garbage.json"), "{").unwrap();
        assert!(matches!(
            journal.begin("req", "a2", identity()).unwrap_err(),
            BeginRefusal::NotDurable { .. }
        ));
    }
}
