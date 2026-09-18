//! What a Runner slot left running, and getting it confirmed stopped.
//!
//! A Runner process dying is not evidence that its workloads stopped writing:
//! a detached container keeps running, and a new Run given the same state
//! would become a second writer. So a Runner records every Run it starts in a
//! journal on disk BEFORE it creates anything, labels everything it creates,
//! and on start-up stops whatever its slot still owns — reporting each Run as
//! confirmed stopped or not before it takes new runtime work.
//!
//! Nothing here adopts an old Run. A Run that should continue starts again
//! from a new lease through the ordinary wake path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use ato_adapter_oci::{OciOwner, OwnedResourceScanner, StopBudget, StopOutcome};
use ato_ipc::runtime_launch::LifecycleV1;
use serde::{Deserialize, Serialize};

pub const RUN_JOURNAL_SCHEMA: &str = "ato.runner-run-journal/1";

static SLOT_RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True once this process confirmed every previous Run of its slot stopped.
/// Until then the slot neither advertises nor accepts runtime launches.
pub fn slot_recovered() -> bool {
    SLOT_RECOVERED.load(std::sync::atomic::Ordering::Acquire)
}

pub fn mark_slot_recovered(recovered: bool) {
    SLOT_RECOVERED.store(recovered, std::sync::atomic::Ordering::Release);
}

/// This Runner process's incarnation: labels and journal entries written by
/// a previous process of the same slot carry a different one.
pub fn incarnation() -> &'static str {
    static INCARNATION: OnceLock<String> = OnceLock::new();
    INCARNATION.get_or_init(|| {
        use rand::Rng;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        format!("inc-{millis:x}-{:016x}", rand::thread_rng().r#gen::<u64>())
    })
}

/// The stop budget a Run's lifecycle grants.
pub fn stop_budget(lifecycle: &LifecycleV1) -> StopBudget {
    StopBudget {
        graceful: Duration::from_millis(lifecycle.graceful_shutdown_ms.max(1)),
        force: Duration::from_millis(
            lifecycle
                .force_kill_after_ms
                .saturating_sub(lifecycle.graceful_shutdown_ms)
                .max(1_000),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Recorded before any writer, network or container exists.
    Preparing,
    Launching,
    Active,
    Stopping,
    /// A stop was attempted and could not be confirmed.
    StopUnconfirmed,
}

/// The host identity of a process workload. `start_time` (clock ticks since
/// boot, from `/proc/<pid>/stat`) is what makes a live pid ours rather than a
/// reused one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

/// One Run this slot started. Non-secret: ids, fences and names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunJournalEntry {
    pub schema: String,
    pub runner_id: String,
    pub slot_id: String,
    pub lease_id: String,
    pub run_id: String,
    pub incarnation: String,
    pub phase: RunPhase,
    /// `state_key -> writer_fence` held by this Run.
    #[serde(default)]
    pub writer_fences: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessIdentity>,
}

impl RunJournalEntry {
    pub fn new(owner: &OciOwner) -> Self {
        Self {
            schema: RUN_JOURNAL_SCHEMA.to_owned(),
            runner_id: owner.runner_id.clone(),
            slot_id: owner.slot_id.clone(),
            lease_id: owner.lease_id.clone(),
            run_id: owner.run_id.clone(),
            incarnation: owner.incarnation.clone(),
            phase: RunPhase::Preparing,
            writer_fences: BTreeMap::new(),
            process: None,
        }
    }
}

/// `<work_root>/runtime-launch/journal/<runner_id>/<slot_id>/<lease_id>.json`.
///
/// Scoped by Runner and slot because several slots can share one work root
/// (the hosted Runner's six slots do): a slot must never read — let alone
/// report as stopped — a Run another slot is still serving.
pub struct RunJournal {
    dir: PathBuf,
    runner_id: String,
    slot_id: String,
}

fn valid_lease_id(value: &str) -> bool {
    ato_adapter_oci::is_label_value(value) && !value.contains(['.', ':'])
}

impl RunJournal {
    pub fn new(work_root: &Path, runner_id: &str, slot_id: &str) -> Result<Self> {
        anyhow::ensure!(
            valid_lease_id(runner_id) && valid_lease_id(slot_id),
            "journal Runner and slot ids must be plain identifiers"
        );
        Ok(Self {
            dir: work_root
                .join("runtime-launch")
                .join("journal")
                .join(runner_id)
                .join(slot_id),
            runner_id: runner_id.to_owned(),
            slot_id: slot_id.to_owned(),
        })
    }

    fn path(&self, lease_id: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            valid_lease_id(lease_id),
            "journal lease id is not an identifier"
        );
        Ok(self.dir.join(format!("{lease_id}.json")))
    }

    /// Durably replace the entry: write, fsync, rename, fsync the directory.
    pub fn record(&self, entry: &RunJournalEntry) -> Result<()> {
        anyhow::ensure!(
            entry.runner_id == self.runner_id && entry.slot_id == self.slot_id,
            "a journal entry must belong to this Runner slot"
        );
        fs::create_dir_all(&self.dir).context("create the run journal directory")?;
        let path = self.path(&entry.lease_id)?;
        let temporary = self.dir.join(format!(".{}.tmp", entry.lease_id));
        {
            let mut file = fs::File::create(&temporary).context("create a journal entry")?;
            file.write_all(&serde_json::to_vec(entry)?)?;
            file.sync_all().context("sync a journal entry")?;
        }
        fs::rename(&temporary, &path).context("publish a journal entry")?;
        fs::File::open(&self.dir)
            .and_then(|directory| directory.sync_all())
            .context("sync the run journal directory")?;
        Ok(())
    }

    pub fn remove(&self, lease_id: &str) -> Result<()> {
        match fs::remove_file(self.path(lease_id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove a journal entry"),
        }
    }

    /// Every entry this slot recorded. An unreadable entry is an error, not
    /// something to skip: skipping it would forget a Run that may be running.
    pub fn load(&self) -> Result<Vec<RunJournalEntry>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("read the run journal"),
        };
        let mut loaded = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)
                .with_context(|| format!("read journal entry {}", path.display()))?;
            let parsed: RunJournalEntry = serde_json::from_slice(&bytes)
                .with_context(|| format!("journal entry {} is malformed", path.display()))?;
            anyhow::ensure!(
                parsed.schema == RUN_JOURNAL_SCHEMA,
                "journal entry {} has an unknown schema",
                path.display()
            );
            // Another slot's entry here is corruption, not something to act
            // on or to skip: stop recovery until someone looks.
            anyhow::ensure!(
                parsed.runner_id == self.runner_id && parsed.slot_id == self.slot_id,
                "journal entry {} belongs to another Runner slot",
                path.display()
            );
            loaded.push(parsed);
        }
        Ok(loaded)
    }
}

/// Stop every container labelled with this lease, then remove its networks
/// if all stops are confirmed. Used whenever a start fails part-way: the
/// labels find what the failed path created even if it lost the handles.
pub fn settle_lease(owner: &OciOwner, budget: StopBudget) -> StopOutcome {
    let scanner = match OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id) {
        Ok(scanner) => scanner,
        Err(error) => {
            return StopOutcome::Unconfirmed {
                reason: format!("{error:#}"),
            };
        }
    };
    settle_with(&scanner, &owner.lease_id, budget)
}

fn settle_with(scanner: &OwnedResourceScanner, lease_id: &str, budget: StopBudget) -> StopOutcome {
    let owned = match scanner.scan() {
        Ok(owned) => owned,
        Err(error) => {
            return StopOutcome::Unconfirmed {
                reason: format!("{error:#}"),
            };
        }
    };
    let outcomes = owned
        .containers
        .iter()
        .filter(|container| container.lease_id() == Some(lease_id))
        .map(|container| scanner.stop_container(container, budget))
        .collect::<Vec<_>>();
    let settled =
        StopOutcome::worst(&outcomes).unwrap_or(StopOutcome::AlreadyExited { exit_code: 0 });
    if settled.is_confirmed() {
        for network in owned
            .networks
            .iter()
            .filter(|network| network.lease_id() == Some(lease_id))
        {
            let _ = scanner.remove_network(network);
        }
    }
    settled
}

/// Reads `/proc/<pid>/stat` field 22 (starttime).
#[cfg(target_os = "linux")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces; fields resume after its ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
pub fn process_start_time(_pid: u32) -> Option<u64> {
    None
}

/// A recorded process workload is confirmed gone when no process with that
/// pid AND start time exists. A live pid with a different start time is a
/// reused pid, not ours, and is never signalled.
fn settle_process(identity: &ProcessIdentity) -> StopOutcome {
    match process_start_time(identity.pid) {
        None => StopOutcome::AlreadyExited { exit_code: -1 },
        Some(start) if start != identity.start_time => StopOutcome::AlreadyExited { exit_code: -1 },
        Some(_) => StopOutcome::Unconfirmed {
            reason: format!(
                "process workload {} from a previous Runner incarnation is still alive",
                identity.pid
            ),
        },
    }
}

/// What recovery found for one lease, as reported to the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseRecoveryReport {
    pub lease_id: String,
    pub run_id: Option<String>,
    pub incarnation: String,
    /// `stopped` only when every workload of the lease is confirmed stopped.
    pub outcome: &'static str,
    pub stop: StopOutcome,
    pub writer_fences: BTreeMap<String, u64>,
}

/// The control plane side of recovery.
pub trait RecoveryReporter {
    fn report_recovery(&self, report: &LeaseRecoveryReport) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryResult {
    /// True when nothing of this slot is left unconfirmed and every report
    /// reached the control plane: runtime work may be accepted.
    pub clean: bool,
    pub reports: Vec<LeaseRecoveryReport>,
}

/// Find, stop and report everything this slot owns. Journal entries are
/// removed only for leases confirmed stopped AND acknowledged.
pub fn recover_slot(
    journal: &RunJournal,
    scanner: Option<&OwnedResourceScanner>,
    reporter: &dyn RecoveryReporter,
    budget: StopBudget,
) -> Result<RecoveryResult> {
    let entries = journal.load()?;
    let owned = match scanner {
        Some(scanner) => Some(scanner.scan()?),
        None => None,
    };

    let mut leases = BTreeSet::new();
    leases.extend(entries.iter().map(|entry| entry.lease_id.clone()));
    if let Some(owned) = &owned {
        leases.extend(
            owned
                .containers
                .iter()
                .chain(&owned.networks)
                .filter_map(|resource| resource.lease_id().map(str::to_owned)),
        );
    }

    let mut clean = true;
    let mut reports = Vec::new();
    for lease_id in leases {
        let entry = entries.iter().find(|entry| entry.lease_id == lease_id);
        let mut outcomes = Vec::new();
        if let Some(identity) = entry.and_then(|entry| entry.process.as_ref()) {
            outcomes.push(settle_process(identity));
        }
        match (scanner, &owned) {
            (Some(scanner), Some(_)) => outcomes.push(settle_with(scanner, &lease_id, budget)),
            // Containers may exist that nothing can see: without Docker a
            // non-process Run cannot be confirmed stopped.
            _ if entry.is_none_or(|entry| entry.process.is_none()) => {
                outcomes.push(StopOutcome::Unconfirmed {
                    reason: "Docker is unavailable to confirm the Run's containers".to_owned(),
                });
            }
            _ => {}
        }
        let stop =
            StopOutcome::worst(&outcomes).unwrap_or(StopOutcome::AlreadyExited { exit_code: 0 });
        let run_id = entry.map(|entry| entry.run_id.clone()).or_else(|| {
            owned.as_ref().and_then(|owned| {
                owned
                    .containers
                    .iter()
                    .find(|container| container.lease_id() == Some(lease_id.as_str()))
                    .and_then(|container| container.run_id().map(str::to_owned))
            })
        });
        let report = LeaseRecoveryReport {
            lease_id: lease_id.clone(),
            run_id,
            incarnation: incarnation().to_owned(),
            outcome: if stop.is_confirmed() {
                "stopped"
            } else {
                "unconfirmed"
            },
            stop: stop.clone(),
            writer_fences: entry
                .map(|entry| entry.writer_fences.clone())
                .unwrap_or_default(),
        };
        let acknowledged = reporter.report_recovery(&report).is_ok();
        if stop.is_confirmed() && acknowledged {
            journal.remove(&lease_id)?;
        } else {
            clean = false;
            if let Some(entry) = entry
                && !stop.is_confirmed()
            {
                let mut entry = entry.clone();
                entry.phase = RunPhase::StopUnconfirmed;
                journal.record(&entry)?;
            }
        }
        reports.push(report);
    }
    Ok(RecoveryResult { clean, reports })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn owner(lease: &str) -> OciOwner {
        OciOwner {
            runner_id: "runner1".to_owned(),
            slot_id: "slot1".to_owned(),
            lease_id: lease.to_owned(),
            run_id: format!("run_{lease}"),
            incarnation: "inc-old".to_owned(),
        }
    }

    #[derive(Default)]
    struct Reporter {
        reports: RefCell<Vec<LeaseRecoveryReport>>,
        fail: bool,
    }
    impl RecoveryReporter for Reporter {
        fn report_recovery(&self, report: &LeaseRecoveryReport) -> Result<()> {
            self.reports.borrow_mut().push(report.clone());
            anyhow::ensure!(!self.fail, "control plane unavailable");
            Ok(())
        }
    }

    #[cfg(unix)]
    fn fake_docker(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("docker");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn journal_entries_round_trip_and_refuse_path_like_ids() {
        let root = tempfile::tempdir().unwrap();
        let journal = RunJournal::new(root.path(), "runner1", "slot1").unwrap();
        let mut entry = RunJournalEntry::new(&owner("L1"));
        entry.writer_fences.insert("data".to_owned(), 7);
        journal.record(&entry).unwrap();
        assert_eq!(journal.load().unwrap(), vec![entry.clone()]);
        journal.remove("L1").unwrap();
        assert!(journal.load().unwrap().is_empty());
        entry.lease_id = "../escape".to_owned();
        assert!(journal.record(&entry).is_err());
    }

    #[test]
    fn slots_sharing_a_work_root_never_see_each_others_runs() {
        let root = tempfile::tempdir().unwrap();
        let slot1 = RunJournal::new(root.path(), "runner1", "slot1").unwrap();
        let slot2 = RunJournal::new(root.path(), "runner1", "slot2").unwrap();
        slot1.record(&RunJournalEntry::new(&owner("L1"))).unwrap();
        assert!(slot2.load().unwrap().is_empty());
        let mut foreign = RunJournalEntry::new(&owner("L9"));
        foreign.slot_id = "slot2".to_owned();
        assert!(slot1.record(&foreign).is_err());
        // A misplaced foreign entry blocks recovery instead of being settled.
        fs::write(
            slot1.dir.join("L9.json"),
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        assert!(slot1.load().is_err());
    }

    #[test]
    fn a_malformed_entry_blocks_recovery_rather_than_being_forgotten() {
        let root = tempfile::tempdir().unwrap();
        let journal = RunJournal::new(root.path(), "runner1", "slot1").unwrap();
        fs::create_dir_all(&journal.dir).unwrap();
        fs::write(journal.dir.join("L1.json"), b"{not json").unwrap();
        assert!(recover_slot(&journal, None, &Reporter::default(), StopBudget::DEFAULT).is_err());
    }

    #[test]
    fn without_docker_a_container_run_stays_unconfirmed_and_journaled() {
        let root = tempfile::tempdir().unwrap();
        let journal = RunJournal::new(root.path(), "runner1", "slot1").unwrap();
        journal.record(&RunJournalEntry::new(&owner("L1"))).unwrap();
        let reporter = Reporter::default();
        let result = recover_slot(&journal, None, &reporter, StopBudget::DEFAULT).unwrap();
        assert!(!result.clean);
        assert_eq!(result.reports[0].outcome, "unconfirmed");
        assert_eq!(journal.load().unwrap()[0].phase, RunPhase::StopUnconfirmed);
    }

    #[cfg(unix)]
    #[test]
    fn labelled_leftovers_are_stopped_reported_and_forgotten_only_when_acknowledged() {
        let root = tempfile::tempdir().unwrap();
        // `ps` lists one owned exited container of lease L2; everything else
        // succeeds. The container was never journaled (Runner died first).
        let docker = fake_docker(
            root.path(),
            "#!/bin/sh\n\
             case \"$1 $2\" in\n\
             'ps --all') echo 'c1|n1|run.ato.dev/managed=true,run.ato.dev/runner-id=runner1,run.ato.dev/slot-id=slot1,run.ato.dev/lease-id=L2,run.ato.dev/run-id=run_L2';;\n\
             'network ls') ;;\n\
             inspect*) echo 'false|0';;\n\
             *) exit 0;;\n\
             esac\n",
        );
        let scanner = OwnedResourceScanner::with_docker(docker, "runner1", "slot1");
        let journal = RunJournal::new(root.path(), "runner1", "slot1").unwrap();

        let failing = Reporter {
            fail: true,
            ..Reporter::default()
        };
        let result = recover_slot(&journal, Some(&scanner), &failing, StopBudget::DEFAULT).unwrap();
        assert!(
            !result.clean,
            "an unacknowledged report is not a clean slot"
        );

        let reporter = Reporter::default();
        let result =
            recover_slot(&journal, Some(&scanner), &reporter, StopBudget::DEFAULT).unwrap();
        assert!(result.clean);
        let report = &reporter.reports.borrow()[0];
        assert_eq!(
            (
                report.lease_id.as_str(),
                report.run_id.as_deref(),
                report.outcome
            ),
            ("L2", Some("run_L2"), "stopped")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_runner_that_died_after_creating_only_the_network_is_recovered() {
        let root = tempfile::tempdir().unwrap();
        // No container yet; one owned network of lease L3; `network rm` works.
        let docker = fake_docker(
            root.path(),
            "#!/bin/sh\n\
             case \"$1 $2\" in\n\
             'ps --all') ;;\n\
             'network ls') echo 'n1|ato-net|run.ato.dev/managed=true,run.ato.dev/runner-id=runner1,run.ato.dev/slot-id=slot1,run.ato.dev/lease-id=L3';;\n\
             *) exit 0;;\n\
             esac\n",
        );
        let scanner = OwnedResourceScanner::with_docker(docker, "runner1", "slot1");
        let journal = RunJournal::new(root.path(), "runner1", "slot1").unwrap();
        journal.record(&RunJournalEntry::new(&owner("L3"))).unwrap();
        let reporter = Reporter::default();
        let result =
            recover_slot(&journal, Some(&scanner), &reporter, StopBudget::DEFAULT).unwrap();
        assert!(result.clean);
        assert_eq!(reporter.reports.borrow()[0].outcome, "stopped");
        assert!(journal.load().unwrap().is_empty());
    }

    #[test]
    fn a_reused_pid_is_not_mistaken_for_our_workload() {
        let identity = ProcessIdentity {
            pid: std::process::id(),
            start_time: u64::MAX,
        };
        assert!(settle_process(&identity).is_confirmed());
    }

    #[test]
    fn stop_budget_splits_the_lifecycle() {
        let budget = stop_budget(&LifecycleV1 {
            graceful_shutdown_ms: 10_000,
            force_kill_after_ms: 15_000,
        });
        assert_eq!(budget.graceful, Duration::from_secs(10));
        assert_eq!(budget.force, Duration::from_secs(5));
    }
}
