//! Runner Bootstrap v0: make a fresh Ubuntu host easy to prepare as an all-in-one
//! Ato snapshot builder + capsule runner.
//!
//! Three entry points, deliberately **not** a capsule:
//! - `ato doctor runner` — read-only host diagnostics ([`doctor`])
//! - `ato runner setup [--fix]` — host mutation behind an explicit confirmation ([`setup`])
//! - `ato runner smoke` — minimal local build→restore→proxy→teardown E2E ([`smoke`])
//!
//! A capsule's value is that it never touches the host; preparing a runner host is the
//! opposite by design (Docker, Firecracker, KVM group grants, systemd units, an artifact
//! root). Keeping this flow under `ato runner` / `ato doctor` keeps that host-mutation
//! boundary out of the capsule safety model instead of blending the two.
//!
//! Scope v0: Linux/Ubuntu only, KVM + Firecracker only, Docker required, all-in-one
//! builder+runner on one host, local artifact root only. Explicitly out of scope:
//! Mac/Windows hosts, binding-required capsules, UFFD as a default, remote (R2/S3)
//! artifact stores.

use serde::Serialize;

pub(crate) mod checks;
pub(crate) mod doctor;
pub(crate) mod setup;
pub(crate) mod smoke;

// ── Host layout constants (what `setup --fix` installs and where) ──

pub(crate) const DEFAULT_ARTIFACT_ROOT: &str = "/var/lib/ato/snapshots";
pub(crate) const ENV_FILE: &str = "/etc/ato/runner.env";
pub(crate) const FC_INSTALL_PATH: &str = "/usr/local/bin/firecracker";
pub(crate) const GUEST_KERNEL_INSTALL_PATH: &str = "/var/lib/ato/kernel/vmlinux-5.10.223";
pub(crate) const BUILDER_UNIT: &str = "ato-snapshot-builder.service";
pub(crate) const RUNNER_UNIT: &str = "ato-runner-agent.service";
pub(crate) const SYSTEMD_DIR: &str = "/etc/systemd/system";

/// Pinned Firecracker release (x86_64). The sha256 is of the release tarball; the
/// download is refused on mismatch — never install an unverified VMM.
pub(crate) const FC_VERSION: &str = "v1.16.0";
pub(crate) const FC_TGZ_URL: &str = "https://github.com/firecracker-microvm/firecracker/releases/download/v1.16.0/firecracker-v1.16.0-x86_64.tgz";
pub(crate) const FC_TGZ_SHA256: &str =
    "bd04e26952d4e158085778c6230a0b383d2619c319182e27eaa9d61a212e92d6";

/// Pinned guest kernel (the stack every KVM validation ran on: 5.10.223).
pub(crate) const GUEST_KERNEL_URL: &str =
    "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64/vmlinux-5.10.223";
pub(crate) const GUEST_KERNEL_SHA256: &str =
    "22847375721aceea63d934c28f2dfce4670b6f52ec904fae19f5145a970c1e65";

// ── Check model (shared by doctor / setup) ──

/// Honest status only — a check that was not actually probed must not claim Ok.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    /// Verified present / working.
    Ok,
    /// Works without it, but degraded or needs operator follow-up.
    Warn,
    /// Absent, and `ato runner setup --fix` can install/configure it.
    Missing,
    /// Absent and NOT fixable from software (BIOS virtualization, wrong OS…);
    /// `detail` carries the exact manual step.
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub status: CheckStatus,
    /// What was actually observed (version, path, error) — never a guess.
    pub detail: String,
    /// The fix `setup --fix` would apply (or the manual step), when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    pub(crate) fn ok(id: &'static str, label: &'static str, detail: impl Into<String>) -> Self {
        Self { id, label, status: CheckStatus::Ok, detail: detail.into(), fix: None }
    }
    pub(crate) fn missing(
        id: &'static str,
        label: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self { id, label, status: CheckStatus::Missing, detail: detail.into(), fix: Some(fix.into()) }
    }
    pub(crate) fn warn(
        id: &'static str,
        label: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self { id, label, status: CheckStatus::Warn, detail: detail.into(), fix: Some(fix.into()) }
    }
    pub(crate) fn blocked(id: &'static str, label: &'static str, detail: impl Into<String>) -> Self {
        Self { id, label, status: CheckStatus::Blocked, detail: detail.into(), fix: None }
    }
}

/// Derived Ready-State readiness: can this host run the two KVM paths today?
/// Both require KVM + Firecracker + the guest kernel; building additionally
/// requires Docker (rootfs assembly).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadyStateSummary {
    pub build_ready_state: ReadinessVerdict,
    pub restore_snapshot: ReadinessVerdict,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "verdict", content = "blocked_on")]
pub(crate) enum ReadinessVerdict {
    Ok,
    Blocked(Vec<String>),
}

pub(crate) fn ready_state_summary(checks: &[Check]) -> ReadyStateSummary {
    let failed = |id: &str| {
        checks
            .iter()
            .any(|c| c.id == id && matches!(c.status, CheckStatus::Missing | CheckStatus::Blocked))
    };
    let vm_blockers: Vec<String> = ["cpu_virt", "kvm_device", "firecracker", "guest_kernel", "tun_tap"]
        .iter()
        .filter(|id| failed(id))
        .map(|id| id.to_string())
        .collect();
    let mut build_blockers = vm_blockers.clone();
    if failed("docker") {
        build_blockers.push("docker".to_string());
    }
    let verdict = |blockers: Vec<String>| {
        if blockers.is_empty() { ReadinessVerdict::Ok } else { ReadinessVerdict::Blocked(blockers) }
    };
    ReadyStateSummary {
        build_ready_state: verdict(build_blockers),
        restore_snapshot: verdict(vm_blockers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &'static str, status: CheckStatus) -> Check {
        Check { id, label: "x", status, detail: String::new(), fix: None }
    }

    #[test]
    fn ready_state_summary_blocks_on_the_right_checks() {
        // All green ⇒ both paths Ok.
        let all_ok: Vec<Check> =
            ["cpu_virt", "kvm_device", "firecracker", "guest_kernel", "tun_tap", "docker"]
                .iter()
                .map(|id| c(id, CheckStatus::Ok))
                .collect();
        let s = ready_state_summary(&all_ok);
        assert!(matches!(s.build_ready_state, ReadinessVerdict::Ok));
        assert!(matches!(s.restore_snapshot, ReadinessVerdict::Ok));

        // Docker missing blocks BUILD only — a restore-only host is still viable.
        let mut docker_down = all_ok.clone();
        docker_down[5] = c("docker", CheckStatus::Missing);
        let s = ready_state_summary(&docker_down);
        assert!(matches!(&s.build_ready_state, ReadinessVerdict::Blocked(b) if b == &["docker"]));
        assert!(matches!(s.restore_snapshot, ReadinessVerdict::Ok));

        // Firecracker missing blocks BOTH; a Warn does not block.
        let mut fc_down = all_ok.clone();
        fc_down[2] = c("firecracker", CheckStatus::Missing);
        fc_down[4] = c("tun_tap", CheckStatus::Warn);
        let s = ready_state_summary(&fc_down);
        assert!(matches!(&s.restore_snapshot, ReadinessVerdict::Blocked(b) if b == &["firecracker"]));
        assert!(matches!(&s.build_ready_state, ReadinessVerdict::Blocked(b) if b == &["firecracker"]));
    }
}
