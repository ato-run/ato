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
        // cgroup v2 exposes cgroup.controllers at the root of the (single)
        // hierarchy; its presence distinguishes v2 from v1.
        let controllers_path = self.root.join("cgroup.controllers");
        let controllers = std::fs::read_to_string(&controllers_path).map_err(|e| {
            CgroupError::new(
                "preflight",
                None,
                format!("read {}: {e}", controllers_path.display()),
            )
        })?;
        let cpu_controller = controllers.split_whitespace().any(|c| c == "cpu");
        if !cpu_controller {
            return Err(CgroupError::new(
                "preflight",
                None,
                "cpu controller absent from delegated cgroup.controllers",
            ));
        }
        // Ensure the cpu controller is delegated to our subtree, and that we can
        // create the slots parent (proves write access).
        let subtree = self.root.join("cgroup.subtree_control");
        // Best-effort enable; ignore EBUSY-style errors, the ensure below is the
        // real writability check.
        let _ = std::fs::write(&subtree, "+cpu");
        std::fs::create_dir_all(self.slots_parent()).map_err(|e| {
            CgroupError::new(
                "preflight",
                None,
                format!("create slots parent {}: {e}", self.slots_parent().display()),
            )
        })?;
        Ok(CgroupCapabilities {
            delegated_root: self.root.clone(),
            cpu_controller,
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
            Ok(text) => Ok(text
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect()),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    RemoveSlot,
}

#[derive(Default)]
struct FakeState {
    /// slot_index → quota millis (absent = slot not created).
    quotas: BTreeMap<usize, u32>,
    /// slot_index → attached pids.
    pids: BTreeMap<usize, Vec<u32>>,
}

/// In-memory backend that records every mutation and can fail any single
/// operation on demand. `Send + Sync` via an internal mutex so it can back the
/// manager actor in tests.
pub struct FakeCgroupBackend {
    state: Mutex<FakeState>,
    fail: Mutex<Option<FakeFailPoint>>,
    cpu_controller: bool,
}

impl Default for FakeCgroupBackend {
    fn default() -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            fail: Mutex::new(None),
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

    /// Arm a single failure point; cleared with `clear_fail`.
    pub fn fail_at(&self, point: FakeFailPoint) {
        *self.fail.lock().unwrap() = Some(point);
    }

    pub fn clear_fail(&self) {
        *self.fail.lock().unwrap() = None;
    }

    fn armed(&self, point: FakeFailPoint) -> bool {
        *self.fail.lock().unwrap() == Some(point)
    }

    /// Pre-seed a slot with pids (stale-cgroup startup tests).
    pub fn seed_slot_pids(&self, slot_index: usize, pids: Vec<u32>) {
        let mut st = self.state.lock().unwrap();
        st.quotas.entry(slot_index).or_insert(1000);
        st.pids.insert(slot_index, pids);
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
}
