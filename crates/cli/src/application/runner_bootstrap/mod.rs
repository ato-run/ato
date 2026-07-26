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

/// Transactional activation of a rendered ingress generation (slice 2): the
/// three-state model that keeps a swapped-but-never-reloaded generation from
/// being mistaken for a no-op.
/// The production [`ingress_activation::CaddyControl`]: argv-only, deadline
/// bounded invocations of the Caddy CLI.
pub(crate) mod caddy_control;
pub(crate) mod checks;
pub(crate) mod doctor;
pub(crate) mod ingress_activation;
/// The durable [`ingress_activation::GenerationStore`]: generation directories,
/// atomic markers and the `current` symlink, over a real directory tree.
pub(crate) mod ingress_store;
pub(crate) mod official_preview;
pub(crate) mod setup;
pub(crate) mod smoke;

// ── Host layout constants (what `setup --fix` installs and where) ──

pub(crate) const DEFAULT_ARTIFACT_ROOT: &str = "/var/lib/ato/snapshots";
pub(crate) const ENV_FILE: &str = "/etc/ato/runner.env";
pub(crate) const FC_INSTALL_PATH: &str = "/usr/local/bin/firecracker";
/// Where the systemd units expect the Ato binaries; setup installs them here so the
/// ExecStart paths it writes are ones it can actually make true.
pub(crate) const ATO_CLI_INSTALL_PATH: &str = "/usr/local/bin/ato";
pub(crate) const BUILDER_INSTALL_PATH: &str = "/usr/local/bin/ato-snapshot-builder";
/// v0 is x86_64-only: the pinned Firecracker release + guest kernel are x86_64. A host
/// on any other arch is Blocked (setup must not install the x86_64 stack there).
pub(crate) const SUPPORTED_ARCH: &str = "x86_64";
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
        Self {
            id,
            label,
            status: CheckStatus::Ok,
            detail: detail.into(),
            fix: None,
        }
    }
    pub(crate) fn missing(
        id: &'static str,
        label: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id,
            label,
            status: CheckStatus::Missing,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    pub(crate) fn warn(
        id: &'static str,
        label: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id,
            label,
            status: CheckStatus::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    pub(crate) fn blocked(
        id: &'static str,
        label: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id,
            label,
            status: CheckStatus::Blocked,
            detail: detail.into(),
            fix: None,
        }
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
    // The microVM substrate both paths need (arch gates the whole x86_64 stack).
    let vm_blockers: Vec<String> = [
        "arch",
        "cpu_virt",
        "kvm_device",
        "firecracker",
        "guest_kernel",
        "tun_tap",
    ]
    .iter()
    .filter(|id| failed(id))
    .map(|id| id.to_string())
    .collect();
    // Restoring/serving additionally needs the ato binary the runner unit runs.
    let mut restore_blockers = vm_blockers.clone();
    if failed("ato_cli_binary") {
        restore_blockers.push("ato_cli_binary".to_string());
    }
    // Building additionally needs Docker (rootfs assembly) + the snapshot-builder binary.
    let mut build_blockers = vm_blockers.clone();
    if failed("docker") {
        build_blockers.push("docker".to_string());
    }
    if failed("snapshot_builder_binary") {
        build_blockers.push("snapshot_builder_binary".to_string());
    }
    let verdict = |blockers: Vec<String>| {
        if blockers.is_empty() {
            ReadinessVerdict::Ok
        } else {
            ReadinessVerdict::Blocked(blockers)
        }
    };
    ReadyStateSummary {
        build_ready_state: verdict(build_blockers),
        restore_snapshot: verdict(restore_blockers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &'static str, status: CheckStatus) -> Check {
        Check {
            id,
            label: "x",
            status,
            detail: String::new(),
            fix: None,
        }
    }

    fn all_green() -> Vec<Check> {
        [
            "arch",
            "cpu_virt",
            "kvm_device",
            "firecracker",
            "guest_kernel",
            "tun_tap",
            "docker",
            "ato_cli_binary",
            "snapshot_builder_binary",
        ]
        .iter()
        .map(|id| c(id, CheckStatus::Ok))
        .collect()
    }
    fn set(checks: &mut [Check], id: &str, status: CheckStatus) {
        checks.iter_mut().find(|c| c.id == id).unwrap().status = status;
    }

    #[test]
    fn ready_state_summary_blocks_on_the_right_checks() {
        // All green ⇒ both paths Ok.
        let s = ready_state_summary(&all_green());
        assert!(matches!(s.build_ready_state, ReadinessVerdict::Ok));
        assert!(matches!(s.restore_snapshot, ReadinessVerdict::Ok));

        // Docker + snapshot-builder block BUILD only — a restore-only host is still viable.
        let mut b = all_green();
        set(&mut b, "docker", CheckStatus::Missing);
        let s = ready_state_summary(&b);
        assert!(matches!(&s.build_ready_state, ReadinessVerdict::Blocked(x) if x == &["docker"]));
        assert!(matches!(s.restore_snapshot, ReadinessVerdict::Ok));
        let mut b = all_green();
        set(&mut b, "snapshot_builder_binary", CheckStatus::Missing);
        let s = ready_state_summary(&b);
        assert!(
            matches!(&s.build_ready_state, ReadinessVerdict::Blocked(x) if x == &["snapshot_builder_binary"])
        );
        assert!(matches!(s.restore_snapshot, ReadinessVerdict::Ok));

        // The ato binary blocks RESTORE only (the runner unit runs it).
        let mut b = all_green();
        set(&mut b, "ato_cli_binary", CheckStatus::Missing);
        let s = ready_state_summary(&b);
        assert!(
            matches!(&s.restore_snapshot, ReadinessVerdict::Blocked(x) if x == &["ato_cli_binary"])
        );
        assert!(matches!(s.build_ready_state, ReadinessVerdict::Ok));

        // Arch blocks BOTH (the whole x86_64 stack); a Warn does not block.
        let mut b = all_green();
        set(&mut b, "arch", CheckStatus::Blocked);
        set(&mut b, "tun_tap", CheckStatus::Warn);
        let s = ready_state_summary(&b);
        assert!(matches!(&s.restore_snapshot, ReadinessVerdict::Blocked(x) if x == &["arch"]));
        assert!(matches!(&s.build_ready_state, ReadinessVerdict::Blocked(x) if x == &["arch"]));
    }
}
