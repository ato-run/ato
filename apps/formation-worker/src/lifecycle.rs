//! The worker's local update lifecycle.
//!
//! The control plane is the authority on *whether* this worker should stop
//! taking work and which binary it should be running. This module is the
//! host-side evidence of where that update got to, so a restart can answer
//! "am I still draining, did the swap land, or must I roll back?" without
//! guessing from process state that no longer exists.
//!
//! ```text
//! active ──▶ draining ──▶ quiescent ──▶ updating ──▶ verifying ──▶ active
//!                                          │             │
//!                                          └──▶ rolled_back ◀──┘
//!                              any phase ──▶ quarantined
//! ```
//!
//! Repeating the current phase is a no-op; every other unlisted edge is
//! refused. `rolled_back` and `quarantined` are terminal: clearing either is an
//! operator action on the journal file, not a transition this code invents
//! (an absent journal reads as `active`, which is how the runbook resumes a
//! host after a rollback or a maintenance drain).
//!
//! Writes are atomic and durable (temp file in the same directory, `fsync`,
//! rename, then sync the directory where the platform supports it). A journal
//! that cannot be parsed is never read back as "absent" or as "active": it is
//! quarantined and the worker refuses to claim, because a record this process
//! cannot understand is not evidence that no update was in progress.
//!
//! Nothing here does I/O inside a signal handler; this module has no signal
//! handling at all. It decides phases and persists them; the binary decides
//! when a phase applies.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The journal schema. A different value is an unreadable journal, not a
/// newer one this build should silently interpret.
pub const UPDATE_JOURNAL_SCHEMA: &str = "ato.formation-worker-update/1";

/// The journal lives directly under the worker's work root.
pub const UPDATE_JOURNAL_FILE: &str = "worker-update.json";

/// Exited without claiming because the running binary did not match the
/// update target: the operator must roll back (see the runbook).
pub const EXIT_ROLLBACK_REQUIRED: i32 = 74;

/// Exited without claiming because the worker is drained / quiescent. The
/// supervisor may start it again after a binary swap or a maintenance window.
pub const EXIT_DRAINED: i32 = 75;

/// Where an update of this host has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    /// Taking work; the normal state.
    Active,
    /// New claims have stopped; an in-flight job may still be running.
    Draining,
    /// No job is in flight. The process exits and the operator may swap the
    /// binary.
    Quiescent,
    /// The operator recorded that the binary is being replaced.
    Updating,
    /// A new binary is running and is being matched against the target
    /// version/digest before it is allowed to claim.
    Verifying,
    /// The running binary does not match the target; it must not claim.
    RolledBack,
    /// The journal could not be trusted or the lifecycle could not be
    /// resumed. Nothing may claim until an operator acts.
    Quarantined,
}

impl UpdatePhase {
    /// The wire spelling, identical to the serde representation and to the
    /// contract's `lifecycle_state` vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Quiescent => "quiescent",
            Self::Updating => "updating",
            Self::Verifying => "verifying",
            Self::RolledBack => "rolled_back",
            Self::Quarantined => "quarantined",
        }
    }

    /// Only `active` may claim work.
    pub fn may_claim(self) -> bool {
        matches!(self, Self::Active)
    }

    /// Terminal phases are not left by a transition; an operator clears the
    /// journal instead.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::RolledBack | Self::Quarantined)
    }
}

impl std::fmt::Display for UpdatePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The durable record of this host's update lifecycle.
///
/// Field names and the nullable target mirror the shared control contract;
/// `current_*` says which binary wrote the record, which is what crash
/// recovery compares a restart against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJournal {
    pub schema: String,
    pub phase: UpdatePhase,
    /// Present when the control plane asked for a specific version.
    #[serde(default)]
    pub target_version: Option<String>,
    /// Optional byte-level target; when set, a version match is not enough.
    #[serde(default)]
    pub target_digest: Option<String>,
    pub current_version: String,
    pub current_sha256: String,
    /// Where the previous binary was kept, when an operator recorded it.
    #[serde(default)]
    pub previous_binary: Option<String>,
    /// One sentence about the last transition, for an operator reading the
    /// file after the fact.
    #[serde(default)]
    pub detail: Option<String>,
    /// RFC 3339 UTC, e.g. `2026-10-24T07:30:00Z`.
    pub updated_at: String,
}

impl UpdateJournal {
    /// An `active` record for the binary that is running now.
    pub fn fresh(identity: &BinaryIdentity) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_owned(),
            phase: UpdatePhase::Active,
            target_version: None,
            target_digest: None,
            current_version: identity.version.clone(),
            current_sha256: identity.sha256.clone(),
            previous_binary: None,
            detail: None,
            updated_at: now_rfc3339(),
        }
    }

    /// A record for a host that must not claim until an operator intervenes.
    pub fn quarantined(identity: &BinaryIdentity, detail: impl Into<String>) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_owned(),
            phase: UpdatePhase::Quarantined,
            target_version: None,
            target_digest: None,
            current_version: identity.version.clone(),
            current_sha256: identity.sha256.clone(),
            previous_binary: None,
            detail: Some(detail.into()),
            updated_at: now_rfc3339(),
        }
    }

    /// Record which version/digest the control plane asked this host to reach.
    ///
    /// A malformed digest from the control plane is dropped rather than
    /// persisted; version-only matching is then used, and the journal stays
    /// readable. `--expect-digest` is validated separately and fails closed.
    pub fn with_target(mut self, version: impl Into<String>, digest: Option<String>) -> Self {
        self.target_version = Some(version.into());
        self.target_digest = digest.and_then(|digest| normalize_sha256(&digest).ok());
        self
    }

    /// Whether this record's target names the binary that is running now.
    ///
    /// A target with no digest is matched on version alone; a target with a
    /// digest must match both. A record with no target never matches — there
    /// is nothing to have arrived at.
    pub fn matches_target(&self, identity: &BinaryIdentity) -> bool {
        let Some(target_version) = self.target_version.as_deref() else {
            return false;
        };
        if target_version != identity.version {
            return false;
        }
        match self.target_digest.as_deref() {
            Some(digest) => normalize_sha256(digest).is_ok_and(|digest| digest == identity.sha256),
            None => true,
        }
    }

    /// Move to `next`, if the contract allows it.
    ///
    /// Repeating the current phase is an accepted no-op: a retry after a
    /// partially reported transition must not look like an illegal edge, and
    /// it must not churn `updated_at` either.
    pub fn transition(
        &self,
        next: UpdatePhase,
        detail: Option<String>,
    ) -> std::result::Result<Self, TransitionRefused> {
        if !allowed_transition(self.phase, next) {
            return Err(TransitionRefused {
                from: self.phase,
                to: next,
            });
        }
        if next == self.phase {
            return Ok(self.clone());
        }
        let mut next_record = self.clone();
        next_record.phase = next;
        next_record.detail = detail;
        next_record.updated_at = now_rfc3339();
        Ok(next_record)
    }

    /// Persist the record atomically and durably.
    pub fn store(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("the update journal has no directory")?;
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        let temporary = temporary_path(path);
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)
                .with_context(|| format!("cannot write {}", temporary.display()))?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all().context("cannot sync the update journal")?;
        }
        ato_runtime_attempt::durability::rename(&temporary, path)
    }

    /// Read the journal, or say precisely why it cannot be trusted.
    pub fn load(path: &Path) -> LoadOutcome {
        match std::fs::read(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => LoadOutcome::Missing,
            Err(error) => LoadOutcome::Corrupt {
                reason: format!("cannot read {}: {error}", path.display()),
            },
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(journal) => match journal.validate() {
                    Ok(()) => LoadOutcome::Present(Box::new(journal)),
                    Err(error) => LoadOutcome::Corrupt {
                        reason: format!(
                            "{} is not a valid update journal: {error:#}",
                            path.display()
                        ),
                    },
                },
                Err(error) => LoadOutcome::Corrupt {
                    reason: format!("{} is not an update journal: {error}", path.display()),
                },
            },
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == UPDATE_JOURNAL_SCHEMA,
            "unsupported update journal schema {:?}",
            self.schema
        );
        ensure!(!self.current_version.is_empty(), "current_version is empty");
        normalize_sha256(&self.current_sha256).context("current_sha256 is not a sha256 digest")?;
        if let Some(digest) = self.target_digest.as_deref() {
            normalize_sha256(digest).context("target_digest is not a sha256 digest")?;
        }
        if let Some(version) = self.target_version.as_deref() {
            ensure!(!version.is_empty(), "target_version is empty");
        }
        ensure!(!self.updated_at.is_empty(), "updated_at is empty");
        Ok(())
    }
}

/// The non-transition skips in the graph: every edge the contract lists.
fn allowed_transition(from: UpdatePhase, to: UpdatePhase) -> bool {
    use UpdatePhase::{Active, Draining, Quarantined, Quiescent, RolledBack, Updating, Verifying};
    if from == to {
        return true;
    }
    if to == Quarantined {
        return true;
    }
    matches!(
        (from, to),
        (Active, Draining)
            | (Draining, Quiescent)
            | (Quiescent, Updating)
            | (Updating, Verifying)
            | (Verifying, Active)
            | (Updating, RolledBack)
            | (Verifying, RolledBack)
    )
}

/// Why a transition was refused. Carries both phases so the operator line can
/// say what the worker thought it was doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRefused {
    pub from: UpdatePhase,
    pub to: UpdatePhase,
}

impl std::fmt::Display for TransitionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the update lifecycle cannot move from {} to {}",
            self.from, self.to
        )
    }
}

impl std::error::Error for TransitionRefused {}

/// What `UpdateJournal::load` found on disk.
#[derive(Debug)]
pub enum LoadOutcome {
    /// No journal: the worker has never been asked to update.
    Missing,
    /// A readable, valid record.
    Present(Box<UpdateJournal>),
    /// Bytes exist but cannot be trusted. Never treated as missing.
    Corrupt { reason: String },
}

/// What startup should do after looking at the journal and the running binary.
#[derive(Debug)]
pub enum Recovery {
    /// Serve normally.
    Active,
    /// The swap landed: report `verifying`, then move to `active` once the
    /// control plane has the report.
    Verifying { journal: Box<UpdateJournal> },
    /// A drain was in progress: stay non-claiming, report, exit 75.
    Drained { journal: Box<UpdateJournal> },
    /// The running binary does not match the recorded target: do not claim,
    /// exit 74.
    RolledBack { journal: Box<UpdateJournal> },
    /// The record cannot be trusted or the lifecycle cannot be resumed: fail
    /// closed. `replaced_unreadable` says the original bytes should be moved
    /// aside before the quarantined record is written over them.
    Quarantined {
        journal: Box<UpdateJournal>,
        replaced_unreadable: bool,
    },
}

/// Decide what a restart may do, before it claims anything.
///
/// This is pure: the caller persists the returned journal and decides exit
/// codes. `identity` is the binary that is running *now*, not the one the
/// journal was written by.
pub fn recover(loaded: LoadOutcome, identity: &BinaryIdentity) -> Recovery {
    match loaded {
        LoadOutcome::Missing => Recovery::Active,
        LoadOutcome::Corrupt { reason } => Recovery::Quarantined {
            journal: Box::new(UpdateJournal::quarantined(
                identity,
                format!("the update journal cannot be read: {reason}"),
            )),
            replaced_unreadable: true,
        },
        LoadOutcome::Present(journal) => {
            let journal = *journal;
            match journal.phase {
                UpdatePhase::Active => Recovery::Active,
                UpdatePhase::Draining | UpdatePhase::Quiescent => Recovery::Drained {
                    journal: Box::new(journal),
                },
                UpdatePhase::Updating | UpdatePhase::Verifying => {
                    if journal.matches_target(identity) {
                        match journal.transition(
                            UpdatePhase::Verifying,
                            Some("restarted binary matches the update target".to_owned()),
                        ) {
                            Ok(verifying) => Recovery::Verifying {
                                journal: Box::new(verifying),
                            },
                            Err(error) => quarantine_from(journal, identity, error),
                        }
                    } else {
                        let detail = format!(
                            "running binary {} ({}) does not match the update target {}",
                            identity.version,
                            identity.sha256,
                            journal
                                .target_version
                                .as_deref()
                                .unwrap_or("(no target version recorded)"),
                        );
                        match journal.transition(UpdatePhase::RolledBack, Some(detail)) {
                            Ok(rolled_back) => Recovery::RolledBack {
                                journal: Box::new(rolled_back),
                            },
                            Err(error) => quarantine_from(journal, identity, error),
                        }
                    }
                }
                // Terminal by design. Exiting non-claiming is what makes
                // "operator clears the journal" an explicit step.
                UpdatePhase::RolledBack => Recovery::RolledBack {
                    journal: Box::new(journal),
                },
                UpdatePhase::Quarantined => Recovery::Quarantined {
                    journal: Box::new(journal),
                    replaced_unreadable: false,
                },
            }
        }
    }
}

fn quarantine_from(
    journal: UpdateJournal,
    identity: &BinaryIdentity,
    error: TransitionRefused,
) -> Recovery {
    let detail = format!(
        "could not resume {} with the running binary: {error}",
        journal.phase
    );
    Recovery::Quarantined {
        journal: Box::new(UpdateJournal::quarantined(identity, detail)),
        replaced_unreadable: false,
    }
}

/// Move an unreadable journal aside so the quarantined record does not erase
/// the bytes that explain what happened.
pub fn preserve_unreadable_journal(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let preserved = path.with_extension("json.corrupt");
    std::fs::rename(path, &preserved).with_context(|| {
        format!(
            "cannot preserve the unreadable journal as {}",
            preserved.display()
        )
    })?;
    Ok(Some(preserved))
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("worker-update");
    path.with_file_name(format!(".{name}.tmp"))
}

/// Which bytes and which stamped identity are running.
///
/// `sha256` is of this binary's bytes (`std::env::current_exe()`), so the
/// digest the control plane holds identifies the file on the host rather than
/// a claim the file makes about itself. `commit`/`dirty`/`build_id`/`rustc`
/// come from the compile-time build stamps and are reported, not trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryIdentity {
    pub version: String,
    pub commit: String,
    pub dirty: String,
    pub build_id: String,
    pub rustc: String,
    /// 64 lowercase hex characters.
    pub sha256: String,
}

impl BinaryIdentity {
    /// Hash this process's own executable and pair it with the build stamps.
    pub fn of_current_exe(
        version: String,
        commit: String,
        dirty: String,
        build_id: String,
        rustc: String,
    ) -> Result<Self> {
        let path = std::env::current_exe().context("cannot locate this worker's own binary")?;
        let sha256 = Self::sha256_file(&path)?;
        Ok(Self {
            version,
            commit,
            dirty,
            build_id,
            rustc,
            sha256,
        })
    }

    /// The SHA-256 of a file's bytes, streamed so a binary-sized file is not
    /// held in memory.
    pub fn sha256_file(path: &Path) -> Result<String> {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("cannot open {} for hashing", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("cannot read {} for hashing", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Fail closed unless this process is the binary an operator expects.
    ///
    /// `--expect-version` / `--expect-digest` are the operator's statement of
    /// what systemd should have started. A mismatch is never "close enough":
    /// it means the swap landed wrong or the unit points at the wrong file,
    /// and claiming work under the wrong identity is how a stale binary keeps
    /// serving.
    pub fn verify_expected(
        &self,
        expect_version: Option<&str>,
        expect_digest: Option<&str>,
    ) -> std::result::Result<(), IdentityMismatch> {
        if let Some(expected) = expect_version {
            let expected = expected.trim();
            if expected != self.version {
                return Err(IdentityMismatch {
                    field: "version",
                    expected: expected.to_owned(),
                    actual: self.version.clone(),
                });
            }
        }
        if let Some(expected) = expect_digest {
            let expected = normalize_sha256(expected).map_err(|error| IdentityMismatch {
                field: "sha256",
                expected: format!("{expected:?} ({error:#})"),
                actual: self.sha256.clone(),
            })?;
            if expected != self.sha256 {
                return Err(IdentityMismatch {
                    field: "sha256",
                    expected,
                    actual: self.sha256.clone(),
                });
            }
        }
        Ok(())
    }
}

/// A binary is not what the operator expected. The message is written for an
/// operator line, so it names both values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityMismatch {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
}

impl std::fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "binary {} is {}, but the operator expected {}",
            self.field, self.actual, self.expected
        )
    }
}

impl std::error::Error for IdentityMismatch {}

/// A sha256 as 64 lowercase hex characters, accepting an optional `sha256:`
/// prefix so an operator can paste either form.
pub fn normalize_sha256(value: &str) -> Result<String> {
    let value = value.trim();
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    ensure!(
        value.len() == 64,
        "a sha256 digest is 64 hex characters, not {}",
        value.len()
    );
    ensure!(
        value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a sha256 digest is hex"
    );
    Ok(value.to_ascii_lowercase())
}

/// The current time in the journal's format.
pub fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_rfc3339(seconds)
}

/// RFC 3339 UTC with no fractional seconds, from Unix seconds.
pub fn format_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let second_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's civil-from-days, so no date crate is needed for one
/// timestamp.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(fill: char) -> String {
        fill.to_string().repeat(64)
    }

    fn identity(version: &str, sha_fill: char) -> BinaryIdentity {
        BinaryIdentity {
            version: version.to_owned(),
            commit: "c0ffee".repeat(6),
            dirty: "clean".to_owned(),
            build_id: "ci-run-1".to_owned(),
            rustc: "rustc 1.96.0 (ac68faa20 2026-05-25)".to_owned(),
            sha256: digest(sha_fill),
        }
    }

    fn journal(
        phase: UpdatePhase,
        target_version: Option<&str>,
        target_digest: Option<&str>,
    ) -> UpdateJournal {
        UpdateJournal {
            schema: UPDATE_JOURNAL_SCHEMA.to_owned(),
            phase,
            target_version: target_version.map(ToOwned::to_owned),
            target_digest: target_digest.map(ToOwned::to_owned),
            current_version: "0.1.0".to_owned(),
            current_sha256: digest('a'),
            previous_binary: None,
            detail: None,
            updated_at: "2026-10-24T00:00:00Z".to_owned(),
        }
    }

    fn scratch() -> tempfile::TempDir {
        std::fs::create_dir_all(".tmp").unwrap();
        tempfile::tempdir_in(".tmp").unwrap()
    }

    #[test]
    fn the_journal_round_trips_with_the_contract_schema() {
        let record = journal(UpdatePhase::Active, None, None);
        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded["schema"], UPDATE_JOURNAL_SCHEMA);
        assert_eq!(encoded["phase"], "active");
        assert_eq!(encoded["target_version"], serde_json::Value::Null);
        assert_eq!(encoded["target_digest"], serde_json::Value::Null);
        assert_eq!(encoded["previous_binary"], serde_json::Value::Null);
        assert_eq!(encoded["detail"], serde_json::Value::Null);
        let decoded: UpdateJournal = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn the_phase_vocabulary_matches_the_wire_contract() {
        for (phase, wire) in [
            (UpdatePhase::Active, "active"),
            (UpdatePhase::Draining, "draining"),
            (UpdatePhase::Quiescent, "quiescent"),
            (UpdatePhase::Updating, "updating"),
            (UpdatePhase::Verifying, "verifying"),
            (UpdatePhase::RolledBack, "rolled_back"),
            (UpdatePhase::Quarantined, "quarantined"),
        ] {
            assert_eq!(phase.as_str(), wire);
            assert_eq!(
                serde_json::to_value(phase).unwrap(),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<UpdatePhase>(serde_json::json!(wire)).unwrap(),
                phase
            );
        }
    }

    #[test]
    fn repeating_a_phase_is_an_idempotent_no_op() {
        let record = journal(UpdatePhase::Draining, None, None);
        let repeated = record
            .transition(
                UpdatePhase::Draining,
                Some("should not be applied".to_owned()),
            )
            .unwrap();
        assert_eq!(
            repeated, record,
            "a repeated phase must not rewrite the record"
        );
        assert_eq!(repeated.updated_at, record.updated_at);
        assert_eq!(repeated.detail, record.detail);
    }

    #[test]
    fn the_full_forward_chain_is_allowed() {
        let phases = [
            UpdatePhase::Active,
            UpdatePhase::Draining,
            UpdatePhase::Quiescent,
            UpdatePhase::Updating,
            UpdatePhase::Verifying,
            UpdatePhase::Active,
        ];
        let mut record = journal(phases[0], None, None);
        for next in &phases[1..] {
            record = record.transition(*next, None).unwrap();
            assert_eq!(record.phase, *next);
            assert_ne!(record.updated_at, "");
        }
    }

    #[test]
    fn backward_and_skipped_transitions_are_refused() {
        let cases = [
            (UpdatePhase::Draining, UpdatePhase::Active),
            (UpdatePhase::Quiescent, UpdatePhase::Draining),
            (UpdatePhase::Updating, UpdatePhase::Quiescent),
            (UpdatePhase::Verifying, UpdatePhase::Updating),
            (UpdatePhase::Active, UpdatePhase::Quiescent),
            (UpdatePhase::Updating, UpdatePhase::Active),
            (UpdatePhase::RolledBack, UpdatePhase::Active),
            (UpdatePhase::Quarantined, UpdatePhase::Active),
        ];
        for (from, to) in cases {
            let record = journal(from, None, None);
            let refused = record.transition(to, None).unwrap_err();
            assert_eq!(refused.from, from);
            assert_eq!(refused.to, to);
            assert!(refused.to_string().contains(from.as_str()));
        }
    }

    #[test]
    fn rollback_is_reachable_from_updating_and_verifying_only() {
        for from in [UpdatePhase::Updating, UpdatePhase::Verifying] {
            let rolled = journal(from, None, None)
                .transition(UpdatePhase::RolledBack, Some("mismatch".to_owned()))
                .unwrap();
            assert_eq!(rolled.phase, UpdatePhase::RolledBack);
            assert_eq!(rolled.detail.as_deref(), Some("mismatch"));
        }
        for from in [
            UpdatePhase::Active,
            UpdatePhase::Draining,
            UpdatePhase::Quiescent,
        ] {
            assert!(
                journal(from, None, None)
                    .transition(UpdatePhase::RolledBack, None)
                    .is_err(),
                "{from} must not roll back directly"
            );
        }
    }

    #[test]
    fn quarantine_is_reachable_from_every_phase() {
        for from in [
            UpdatePhase::Active,
            UpdatePhase::Draining,
            UpdatePhase::Quiescent,
            UpdatePhase::Updating,
            UpdatePhase::Verifying,
            UpdatePhase::RolledBack,
            UpdatePhase::Quarantined,
        ] {
            let quarantined = journal(from, None, None)
                .transition(UpdatePhase::Quarantined, Some("why".to_owned()))
                .unwrap();
            assert_eq!(quarantined.phase, UpdatePhase::Quarantined);
        }
    }

    #[test]
    fn terminal_phases_never_leave_by_transition() {
        for from in [UpdatePhase::RolledBack, UpdatePhase::Quarantined] {
            for to in [
                UpdatePhase::Active,
                UpdatePhase::Draining,
                UpdatePhase::Quiescent,
                UpdatePhase::Updating,
                UpdatePhase::Verifying,
            ] {
                assert!(
                    journal(from, None, None).transition(to, None).is_err(),
                    "{from} -> {to} must be refused"
                );
            }
        }
    }

    #[test]
    fn a_missing_journal_starts_active() {
        match recover(LoadOutcome::Missing, &identity("0.1.0", 'a')) {
            Recovery::Active => {}
            other => panic!("missing journal must start active, got {other:?}"),
        }
    }

    #[test]
    fn a_corrupt_journal_quarantines_and_keeps_the_evidence() {
        let dir = scratch();
        let path = dir.path().join(UPDATE_JOURNAL_FILE);
        std::fs::write(&path, b"{ not json").unwrap();
        let loaded = UpdateJournal::load(&path);
        assert!(matches!(loaded, LoadOutcome::Corrupt { .. }));

        let recovery = recover(loaded, &identity("0.1.0", 'a'));
        match recovery {
            Recovery::Quarantined {
                journal,
                replaced_unreadable,
            } => {
                assert!(replaced_unreadable);
                assert_eq!(journal.phase, UpdatePhase::Quarantined);
                assert!(journal.detail.unwrap().contains("cannot be read"));
            }
            other => panic!("corrupt journal must quarantine, got {other:?}"),
        }

        let preserved = preserve_unreadable_journal(&path).unwrap().unwrap();
        assert_eq!(std::fs::read(&preserved).unwrap(), b"{ not json");
        assert!(!path.exists());
    }

    #[test]
    fn a_journal_with_another_schema_is_not_a_journal() {
        let dir = scratch();
        let path = dir.path().join(UPDATE_JOURNAL_FILE);
        let mut record = journal(UpdatePhase::Active, None, None);
        record.schema = "ato.formation-worker-update/2".to_owned();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(matches!(
            UpdateJournal::load(&path),
            LoadOutcome::Corrupt { .. }
        ));
    }

    #[test]
    fn a_crash_during_update_with_the_matching_binary_resumes_verifying() {
        let record = journal(UpdatePhase::Updating, Some("0.1.1"), Some(&digest('b')));
        let running = identity("0.1.1", 'b');
        match recover(LoadOutcome::Present(Box::new(record)), &running) {
            Recovery::Verifying { journal } => {
                assert_eq!(journal.phase, UpdatePhase::Verifying);
                let active = journal.transition(UpdatePhase::Active, None).unwrap();
                assert_eq!(active.phase, UpdatePhase::Active);
            }
            other => panic!("matching binary must resume verifying, got {other:?}"),
        }
    }

    #[test]
    fn a_crash_during_update_with_a_mismatching_binary_rolls_back() {
        let record = journal(UpdatePhase::Updating, Some("0.1.1"), Some(&digest('b')));
        for (version, fill) in [("0.1.0", 'a'), ("0.1.1", 'c')] {
            match recover(
                LoadOutcome::Present(Box::new(record.clone())),
                &identity(version, fill),
            ) {
                Recovery::RolledBack { journal } => {
                    assert_eq!(journal.phase, UpdatePhase::RolledBack);
                    assert!(
                        journal
                            .detail
                            .as_deref()
                            .unwrap()
                            .contains("does not match"),
                        "the rollback must say why"
                    );
                }
                other => panic!("{version}/{fill} must roll back, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_target_without_a_digest_is_matched_on_version_alone() {
        let version_only = journal(UpdatePhase::Updating, Some("0.1.1"), None);
        assert!(version_only.matches_target(&identity("0.1.1", 'c')));
        assert!(!version_only.matches_target(&identity("0.1.0", 'a')));
        assert!(
            !journal(UpdatePhase::Updating, None, None).matches_target(&identity("0.1.1", 'b'))
        );
    }

    #[test]
    fn a_drained_journal_stays_non_claiming() {
        for phase in [UpdatePhase::Draining, UpdatePhase::Quiescent] {
            match recover(
                LoadOutcome::Present(Box::new(journal(phase, None, None))),
                &identity("0.1.0", 'a'),
            ) {
                Recovery::Drained { journal } => assert_eq!(journal.phase, phase),
                other => panic!("{phase} must stay drained, got {other:?}"),
            }
            assert!(!phase.may_claim());
        }
    }

    #[test]
    fn a_quarantined_journal_stays_quarantined_without_replacing_evidence() {
        let record = journal(UpdatePhase::Quarantined, None, None);
        match recover(
            LoadOutcome::Present(Box::new(record)),
            &identity("0.1.0", 'a'),
        ) {
            Recovery::Quarantined {
                journal,
                replaced_unreadable,
            } => {
                assert!(!replaced_unreadable);
                assert_eq!(journal.phase, UpdatePhase::Quarantined);
            }
            other => panic!("quarantined must stay quarantined, got {other:?}"),
        }
    }

    #[test]
    fn a_rolled_back_journal_does_not_resume_by_restart() {
        match recover(
            LoadOutcome::Present(Box::new(journal(UpdatePhase::RolledBack, None, None))),
            &identity("0.1.0", 'a'),
        ) {
            Recovery::RolledBack { journal } => assert_eq!(journal.phase, UpdatePhase::RolledBack),
            other => panic!("rolled_back must stay non-claiming, got {other:?}"),
        }
    }

    #[test]
    fn the_write_is_atomic_and_leaves_no_temporary_behind() {
        let dir = scratch();
        let path = dir.path().join(UPDATE_JOURNAL_FILE);
        let record = journal(UpdatePhase::Quiescent, None, None);
        record.store(&path).unwrap();
        let loaded = UpdateJournal::load(&path);
        match loaded {
            LoadOutcome::Present(persisted) => assert_eq!(*persisted, record),
            other => panic!("stored journal must load back, got {other:?}"),
        }
        let entries = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![UPDATE_JOURNAL_FILE.to_owned()]);
    }

    #[test]
    fn a_journal_that_cannot_validate_is_not_persisted() {
        let dir = scratch();
        let path = dir.path().join(UPDATE_JOURNAL_FILE);
        let mut record = journal(UpdatePhase::Active, None, None);
        record.current_sha256 = "not-a-digest".to_owned();
        assert!(record.store(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn the_sha256_of_a_file_is_the_digest_of_its_bytes() {
        let dir = scratch();
        let path = dir.path().join("binary");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            BinaryIdentity::sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn expected_identity_mismatches_fail_closed() {
        let running = identity("0.1.1", 'b');
        assert!(
            running
                .verify_expected(Some("0.1.1"), Some(&format!("sha256:{}", digest('b'))))
                .is_ok()
        );

        let version = running.verify_expected(Some("0.1.0"), None).unwrap_err();
        assert_eq!(version.field, "version");
        assert!(version.to_string().contains("0.1.0"));

        let digest_mismatch = running
            .verify_expected(None, Some(&digest('a')))
            .unwrap_err();
        assert_eq!(digest_mismatch.field, "sha256");

        let malformed = running.verify_expected(None, Some("abc")).unwrap_err();
        assert_eq!(malformed.field, "sha256");
        assert!(malformed.to_string().contains("operator expected"));
    }

    #[test]
    fn a_digest_is_normalized_before_comparison() {
        assert_eq!(normalize_sha256(&digest('A')).unwrap(), digest('a'));
        assert_eq!(
            normalize_sha256(&format!("sha256:{}", digest('a'))).unwrap(),
            digest('a')
        );
        assert!(normalize_sha256("xyz").is_err());
    }

    #[test]
    fn rfc3339_timestamps_are_formatted_from_the_epoch() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_rfc3339(1_735_689_600), "2025-01-01T00:00:00Z");
        assert!(now_rfc3339().ends_with('Z'));
        assert_eq!(now_rfc3339().len(), 20);
    }
}
