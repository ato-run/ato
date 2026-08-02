//! cgroup v2 `cpu.max` backend for runtime CPU entitlement (ADR-016 PR2).
//!
//! The [`CpuCgroupBackend`] trait is the only surface the entitlement manager
//! touches; it never writes `/sys/fs/cgroup` directly. Two implementations:
//! [`LinuxCgroupV2Backend`] for a real delegated cgroup tree, and
//! [`FakeCgroupBackend`] for exhaustive failure-injection tests (create / write
//! / attach / read / remove can each be made to fail on demand).
//!
//! Quota model: a slot's `cpu.max` is `<quota_micros> <period_micros>` with a
//! fixed 100 ms period, so `1000m -> "100000 100000"`, `1500m -> "150000
//! 100000"`, `2000m -> "200000 100000"`. Millicores are the manager's currency;
//! this module converts at the boundary.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// cgroup v2 CPU period, microseconds. `cpu.max` quota is expressed against it.
pub const CPU_PERIOD_MICROS: u32 = 100_000;

/// Render a millicore entitlement as a cgroup v2 `cpu.max` value.
///
/// `1000m` is one full CPU = `period` microseconds of quota per period.
pub fn cpu_max_line(quota_millis: u32) -> String {
    // quota_micros = millis/1000 * period = millis * period / 1000, exact for
    // the 100_000 period and any millicore value (period is divisible by 1000).
    let quota_micros = (u64::from(quota_millis) * u64::from(CPU_PERIOD_MICROS)) / 1000;
    format!("{quota_micros} {CPU_PERIOD_MICROS}")
}

/// What a preflight found about the host's cgroup CPU support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupCapabilities {
    /// The delegated cgroup root the runner may write under.
    pub delegated_root: PathBuf,
    /// The `cpu` controller is present and enabled in `cgroup.subtree_control`.
    pub cpu_controller: bool,
}

/// A failure from any cgroup operation. Carries the offending slot where one
/// applies so the manager can report precisely without leaking host paths to a
/// client (the string is for logs only).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cgroup {op} failed{}: {reason}", .slot_index.map(|s| format!(" for slot {s}")).unwrap_or_default())]
pub struct CgroupError {
    pub op: &'static str,
    pub slot_index: Option<usize>,
    pub reason: String,
}

impl CgroupError {
    pub fn new(op: &'static str, slot_index: Option<usize>, reason: impl Into<String>) -> Self {
        Self {
            op,
            slot_index,
            reason: reason.into(),
        }
    }
}

/// Host CPU control for per-slot cgroups. All millicore in, all fail-closed.
pub trait CpuCgroupBackend: Send + Sync {
    /// Confirm cgroup v2 + cpu controller + a writable delegated root. Called
    /// once at runner start under `enforce`; failure means the runner runs but
    /// never advertises the entitlement capability nor claims leases.
    fn preflight(&self) -> Result<CgroupCapabilities, CgroupError>;
    /// Create the slot's cgroup (idempotent).
    fn ensure_slot(&self, slot_index: usize) -> Result<(), CgroupError>;
    /// Read the slot's current quota in millicores (`None` = `max`, unlimited).
    fn read_quota_millis(&self, slot_index: usize) -> Result<Option<u32>, CgroupError>;
    /// Write the slot's quota in millicores.
    fn write_quota_millis(&self, slot_index: usize, quota_millis: u32) -> Result<(), CgroupError>;
    /// Attach a host pid to the slot cgroup (writes `cgroup.procs`).
    fn attach_pid(&self, slot_index: usize, pid: u32) -> Result<(), CgroupError>;
    /// List pids currently in the slot cgroup (stale-detection at startup).
    fn slot_pids(&self, slot_index: usize) -> Result<Vec<u32>, CgroupError>;
    /// Remove the slot cgroup (only when empty).
    fn remove_slot(&self, slot_index: usize) -> Result<(), CgroupError>;
}

// ─── Linux cgroup v2 ────────────────────────────────────────────────────────

/// Real backend over a delegated cgroup v2 subtree (e.g. the systemd
/// `Delegate=yes` scope). Slots live at `<root>/ato-slots/ato-slot-<i>/`.
pub struct LinuxCgroupV2Backend {
    root: PathBuf,
}

impl LinuxCgroupV2Backend {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn slots_parent(&self) -> PathBuf {
        self.root.join("ato-slots")
    }

    fn slot_dir(&self, slot_index: usize) -> PathBuf {
        self.slots_parent().join(format!("ato-slot-{slot_index}"))
    }
}

impl CpuCgroupBackend for LinuxCgroupV2Backend {
    fn preflight(&self) -> Result<CgroupCapabilities, CgroupError> {
        let fail = |reason: String| CgroupError::new("preflight", None, reason);

        // 1. cgroup v2: the unified hierarchy exposes cgroup.controllers at the
        //    delegated root; its presence distinguishes v2 from v1.
        let controllers_path = self.root.join("cgroup.controllers");
        let controllers = std::fs::read_to_string(&controllers_path).map_err(|e| {
            fail(format!(
                "not cgroup v2 (read {}: {e})",
                controllers_path.display()
            ))
        })?;
        if !controllers.split_whitespace().any(|c| c == "cpu") {
            return Err(fail(
                "cpu controller absent from delegated cgroup.controllers".to_string(),
            ));
        }

        // 2. Delegate the cpu controller into our subtree. In cgroup v2 a cgroup
        //    that holds processes cannot enable a controller for its children
        //    (the "no internal process" constraint), so `+cpu` failing here is a
        //    real, fail-closed error — NOT something to ignore. The runner main
        //    process is expected to sit in a leaf subgroup (systemd
        //    DelegateSubgroup=, or an explicit move), leaving this root
        //    process-free and able to delegate.
        let subtree = self.root.join("cgroup.subtree_control");
        std::fs::write(&subtree, "+cpu").map_err(|e| {
            fail(format!(
                "enable cpu in {} (is the delegated root process-free?): {e}",
                subtree.display()
            ))
        })?;
        // Read back: the write can succeed yet not stick on some setups.
        let subtree_now = std::fs::read_to_string(&subtree)
            .map_err(|e| fail(format!("read back {}: {e}", subtree.display())))?;
        if !subtree_now.split_whitespace().any(|c| c == "cpu") {
            return Err(fail(
                "cpu did not appear in cgroup.subtree_control after +cpu".to_string(),
            ));
        }

        // 3. Create the slots parent and delegate cpu one level further, so slot
        //    children can carry cpu.max.
        std::fs::create_dir_all(self.slots_parent())
            .map_err(|e| fail(format!("create {}: {e}", self.slots_parent().display())))?;
        let slots_subtree = self.slots_parent().join("cgroup.subtree_control");
        std::fs::write(&slots_subtree, "+cpu")
            .map_err(|e| fail(format!("enable cpu in {}: {e}", slots_subtree.display())))?;

        // 4. Prove a slot child can actually carry a cpu.max: create a probe,
        //    round-trip a quota, and remove it. This is the difference between
        //    "the directory exists" and "the cpu controller really works here".
        let probe = self.slots_parent().join("ato-preflight-probe");
        std::fs::create_dir_all(&probe)
            .map_err(|e| fail(format!("create probe {}: {e}", probe.display())))?;
        let probe_result = (|| {
            let cpu_max = probe.join("cpu.max");
            std::fs::write(&cpu_max, cpu_max_line(1000))
                .map_err(|e| fail(format!("probe cpu.max write: {e}")))?;
            let read = std::fs::read_to_string(&cpu_max)
                .map_err(|e| fail(format!("probe cpu.max read: {e}")))?;
            // Validate BOTH fields — quota AND period — round-tripped.
            let mut tokens = read.split_whitespace();
            let (quota, period) = (tokens.next(), tokens.next());
            if quota != Some("100000") || period != Some("100000") {
                return Err(fail(format!("probe cpu.max round-trip mismatch: {read:?}")));
            }
            Ok(())
        })();
        // Removing the probe cgroup is part of the contract: a probe that cannot
        // be cleaned up is a delegated-tree we don't fully control, so a remove
        // failure fails preflight (surfaced alongside any probe error).
        let remove_result = std::fs::remove_dir(&probe)
            .map_err(|e| fail(format!("remove probe {}: {e}", probe.display())));
        probe_result.and(remove_result)?;

        Ok(CgroupCapabilities {
            delegated_root: self.root.clone(),
            cpu_controller: true,
        })
    }

    fn ensure_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
        std::fs::create_dir_all(self.slot_dir(slot_index))
            .map_err(|e| CgroupError::new("ensure_slot", Some(slot_index), e.to_string()))
    }

    fn read_quota_millis(&self, slot_index: usize) -> Result<Option<u32>, CgroupError> {
        let path = self.slot_dir(slot_index).join("cpu.max");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CgroupError::new("read_quota", Some(slot_index), e.to_string()))?;
        let quota_token = raw.split_whitespace().next().unwrap_or("max");
        if quota_token == "max" {
            return Ok(None);
        }
        let quota_micros: u64 = quota_token.parse().map_err(|e| {
            CgroupError::new(
                "read_quota",
                Some(slot_index),
                format!("parse cpu.max: {e}"),
            )
        })?;
        Ok(Some(
            (quota_micros * 1000 / u64::from(CPU_PERIOD_MICROS)) as u32,
        ))
    }

    fn write_quota_millis(&self, slot_index: usize, quota_millis: u32) -> Result<(), CgroupError> {
        let path = self.slot_dir(slot_index).join("cpu.max");
        std::fs::write(&path, cpu_max_line(quota_millis))
            .map_err(|e| CgroupError::new("write_quota", Some(slot_index), e.to_string()))
    }

    fn attach_pid(&self, slot_index: usize, pid: u32) -> Result<(), CgroupError> {
        let path = self.slot_dir(slot_index).join("cgroup.procs");
        std::fs::write(&path, pid.to_string())
            .map_err(|e| CgroupError::new("attach_pid", Some(slot_index), e.to_string()))
    }

    fn slot_pids(&self, slot_index: usize) -> Result<Vec<u32>, CgroupError> {
        let path = self.slot_dir(slot_index).join("cgroup.procs");
        match std::fs::read_to_string(&path) {
            // STRICT parse: this read is load-bearing for release/admit safety,
            // so a token we cannot parse is an error, not a silently-dropped pid
            // that could make an occupied cgroup look empty (fail-closed).
            Ok(text) => text
                .split_whitespace()
                .map(|token| {
                    token.parse::<u32>().map_err(|e| {
                        CgroupError::new(
                            "slot_pids",
                            Some(slot_index),
                            format!("invalid pid token {token:?}: {e}"),
                        )
                    })
                })
                .collect(),
            // A not-yet-created slot has no procs file; treat as empty.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(CgroupError::new(
                "slot_pids",
                Some(slot_index),
                e.to_string(),
            )),
        }
    }

    fn remove_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
        match std::fs::remove_dir(self.slot_dir(slot_index)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CgroupError::new(
                "remove_slot",
                Some(slot_index),
                e.to_string(),
            )),
        }
    }
}

// ─── Fake backend for tests ─────────────────────────────────────────────────

/// Which operation a [`FakeCgroupBackend`] should fail, for injection tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FakeFailPoint {
    Preflight,
    EnsureSlot,
    ReadQuota,
    /// Fail a quota write, but only when lowering (`new < current`) — models the
    /// dangerous case where the decrease pass fails mid-rebalance.
    WriteQuotaDecrease,
    /// Fail a quota write, but only when raising (`new > current`).
    WriteQuotaIncrease,
    /// Fail EVERY quota write regardless of direction — models a host where
    /// both the apply and its rollback cannot write, forcing the unhealthy path.
    WriteQuotaAny,
    AttachPid,
    SlotPidsRead,
    RemoveSlot,
}

#[derive(Default)]
struct FakeState {
    /// slot_index → quota millis (absent = slot not created).
    quotas: BTreeMap<usize, u32>,
    /// slot_index → attached pids.
    pids: BTreeMap<usize, Vec<u32>>,
}

/// In-memory backend that records every mutation and can fail any subset of
/// operations on demand. `Send + Sync` via an internal mutex so it can back the
/// manager actor in tests.
pub struct FakeCgroupBackend {
    state: Mutex<FakeState>,
    fail: Mutex<std::collections::BTreeSet<FakeFailPoint>>,
    cpu_controller: bool,
}

// FakeFailPoint needs Ord for the armed-set.
impl Default for FakeCgroupBackend {
    fn default() -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            fail: Mutex::new(std::collections::BTreeSet::new()),
            cpu_controller: true,
        }
    }
}

impl FakeCgroupBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preflight should report no cpu controller (host lacks it).
    pub fn without_cpu_controller() -> Self {
        Self {
            cpu_controller: false,
            ..Self::default()
        }
    }

    /// Arm a failure point. Multiple may be armed at once (e.g. attach + remove
    /// to model a rollback whose cleanup also fails).
    pub fn fail_at(&self, point: FakeFailPoint) {
        self.fail.lock().unwrap().insert(point);
    }

    pub fn clear_fail(&self) {
        self.fail.lock().unwrap().clear();
    }

    fn armed(&self, point: FakeFailPoint) -> bool {
        self.fail.lock().unwrap().contains(&point)
    }

    /// Pre-seed a slot with pids (stale-cgroup startup tests).
    pub fn seed_slot_pids(&self, slot_index: usize, pids: Vec<u32>) {
        let mut st = self.state.lock().unwrap();
        st.quotas.entry(slot_index).or_insert(1000);
        st.pids.insert(slot_index, pids);
    }

    /// Model a VMM process exiting: it leaves the slot cgroup's `cgroup.procs`,
    /// as the kernel reflects once the process is reaped. Tests call this just
    /// before `release_after_teardown` to reproduce the real post-teardown
    /// state (an empty slot cgroup).
    pub fn simulate_process_exit(&self, slot_index: usize, pid: u32) {
        if let Some(pids) = self.state.lock().unwrap().pids.get_mut(&slot_index) {
            pids.retain(|&p| p != pid);
        }
    }

    /// Current recorded quota for assertions.
    pub fn quota_of(&self, slot_index: usize) -> Option<u32> {
        self.state.lock().unwrap().quotas.get(&slot_index).copied()
    }

    /// Current recorded pids for assertions.
    pub fn pids_of(&self, slot_index: usize) -> Vec<u32> {
        self.state
            .lock()
            .unwrap()
            .pids
            .get(&slot_index)
            .cloned()
            .unwrap_or_default()
    }
}

impl CpuCgroupBackend for FakeCgroupBackend {
    fn preflight(&self) -> Result<CgroupCapabilities, CgroupError> {
        if self.armed(FakeFailPoint::Preflight) {
            return Err(CgroupError::new("preflight", None, "injected"));
        }
        if !self.cpu_controller {
            return Err(CgroupError::new("preflight", None, "cpu controller absent"));
        }
        Ok(CgroupCapabilities {
            delegated_root: PathBuf::from("/fake/cgroup"),
            cpu_controller: true,
        })
    }

    fn ensure_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
        if self.armed(FakeFailPoint::EnsureSlot) {
            return Err(CgroupError::new(
                "ensure_slot",
                Some(slot_index),
                "injected",
            ));
        }
        let mut st = self.state.lock().unwrap();
        st.quotas.entry(slot_index).or_insert(0);
        st.pids.entry(slot_index).or_default();
        Ok(())
    }

    fn read_quota_millis(&self, slot_index: usize) -> Result<Option<u32>, CgroupError> {
        if self.armed(FakeFailPoint::ReadQuota) {
            return Err(CgroupError::new("read_quota", Some(slot_index), "injected"));
        }
        Ok(self.state.lock().unwrap().quotas.get(&slot_index).copied())
    }

    fn write_quota_millis(&self, slot_index: usize, quota_millis: u32) -> Result<(), CgroupError> {
        let current = self.state.lock().unwrap().quotas.get(&slot_index).copied();
        let lowering = current.is_some_and(|c| quota_millis < c);
        let raising = current.is_some_and(|c| quota_millis > c);
        if self.armed(FakeFailPoint::WriteQuotaAny)
            || (lowering && self.armed(FakeFailPoint::WriteQuotaDecrease))
            || (raising && self.armed(FakeFailPoint::WriteQuotaIncrease))
        {
            return Err(CgroupError::new(
                "write_quota",
                Some(slot_index),
                "injected",
            ));
        }
        self.state
            .lock()
            .unwrap()
            .quotas
            .insert(slot_index, quota_millis);
        Ok(())
    }

    fn attach_pid(&self, slot_index: usize, pid: u32) -> Result<(), CgroupError> {
        if self.armed(FakeFailPoint::AttachPid) {
            return Err(CgroupError::new("attach_pid", Some(slot_index), "injected"));
        }
        self.state
            .lock()
            .unwrap()
            .pids
            .entry(slot_index)
            .or_default()
            .push(pid);
        Ok(())
    }

    fn slot_pids(&self, slot_index: usize) -> Result<Vec<u32>, CgroupError> {
        if self.armed(FakeFailPoint::SlotPidsRead) {
            return Err(CgroupError::new("slot_pids", Some(slot_index), "injected"));
        }
        Ok(self.pids_of(slot_index))
    }

    fn remove_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
        if self.armed(FakeFailPoint::RemoveSlot) {
            return Err(CgroupError::new(
                "remove_slot",
                Some(slot_index),
                "injected",
            ));
        }
        let mut st = self.state.lock().unwrap();
        st.quotas.remove(&slot_index);
        st.pids.remove(&slot_index);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_max_line_renders_period_100ms() {
        assert_eq!(cpu_max_line(1000), "100000 100000");
        assert_eq!(cpu_max_line(1500), "150000 100000");
        assert_eq!(cpu_max_line(2000), "200000 100000");
        assert_eq!(cpu_max_line(1667), "166700 100000");
    }

    #[test]
    fn fake_records_quota_and_pids() {
        let be = FakeCgroupBackend::new();
        be.ensure_slot(0).unwrap();
        be.write_quota_millis(0, 1500).unwrap();
        be.attach_pid(0, 4242).unwrap();
        assert_eq!(be.quota_of(0), Some(1500));
        assert_eq!(be.pids_of(0), vec![4242]);
        assert_eq!(be.slot_pids(0).unwrap(), vec![4242]);
    }

    #[test]
    fn fake_injects_directional_write_failures() {
        let be = FakeCgroupBackend::new();
        be.ensure_slot(0).unwrap();
        be.write_quota_millis(0, 2000).unwrap();
        be.fail_at(FakeFailPoint::WriteQuotaDecrease);
        assert!(be.write_quota_millis(0, 1000).is_err(), "decrease fails");
        assert!(be.write_quota_millis(0, 2000).is_ok(), "increase still ok");
        be.clear_fail();
        be.fail_at(FakeFailPoint::WriteQuotaIncrease);
        assert!(be.write_quota_millis(0, 3000).is_err(), "increase fails");
    }

    #[test]
    fn preflight_without_cpu_controller_fails() {
        assert!(
            FakeCgroupBackend::without_cpu_controller()
                .preflight()
                .is_err()
        );
        assert!(FakeCgroupBackend::new().preflight().is_ok());
    }

    #[test]
    fn seeded_stale_pids_are_reported() {
        let be = FakeCgroupBackend::new();
        be.seed_slot_pids(1, vec![9001, 9002]);
        assert_eq!(be.slot_pids(1).unwrap(), vec![9001, 9002]);
    }

    // The Linux backend's preflight NEGATIVE paths are robust over a tempdir
    // (they short-circuit before any kernel-specific write semantics). The
    // POSITIVE path depends on the kernel reporting an enabled controller in
    // cgroup.subtree_control after a `+cpu` write — a plain file can't simulate
    // that — so it is covered by the KVM acceptance test in PR 2b.
    #[test]
    fn linux_preflight_rejects_non_v2() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No cgroup.controllers file → not cgroup v2.
        let be = LinuxCgroupV2Backend::new(dir.path().to_path_buf());
        assert!(be.preflight().is_err());
    }

    #[test]
    fn linux_preflight_rejects_missing_cpu_controller() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("cgroup.controllers"), "cpuset io memory").unwrap();
        let be = LinuxCgroupV2Backend::new(dir.path().to_path_buf());
        assert!(be.preflight().is_err());
    }

    #[test]
    fn linux_slot_pids_valid_tokens_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let be = LinuxCgroupV2Backend::new(dir.path().to_path_buf());
        let slot = be.slot_dir(0);
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("cgroup.procs"), "101\n202\n303\n").unwrap();
        assert_eq!(be.slot_pids(0).unwrap(), vec![101, 202, 303]);
    }

    #[test]
    fn linux_slot_pids_malformed_is_error_not_empty() {
        // A garbage cgroup.procs must NOT read as an empty (releasable) slot.
        let dir = tempfile::tempdir().expect("tempdir");
        let be = LinuxCgroupV2Backend::new(dir.path().to_path_buf());
        let slot = be.slot_dir(0);
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("cgroup.procs"), "101\nnot-a-pid\n303").unwrap();
        assert!(be.slot_pids(0).is_err(), "malformed token must be an error");
    }

    #[test]
    fn linux_slot_pids_absent_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let be = LinuxCgroupV2Backend::new(dir.path().to_path_buf());
        // Slot dir never created → no procs file → empty, not error.
        assert_eq!(be.slot_pids(5).unwrap(), Vec::<u32>::new());
    }
}
