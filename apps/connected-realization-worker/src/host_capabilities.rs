//! Why this host does or does not advertise a capability.
//!
//! The heartbeat advertises capabilities and the control plane routes work by
//! them, so a capability that is missing is not a cosmetic gap: the operator
//! sees "no runner available" and has nothing to act on. This module keeps that
//! answer explicit — every capability this host does not advertise carries a
//! stable reason code plus a human-readable explanation — and derives it from
//! the SAME decision the heartbeat uses, so doctor cannot drift from what the
//! Runner really reports.
//!
//! Two layers, deliberately separated:
//!
//! * the pure decision (capability_report / lease_admission) over observed
//!   HostFacts;
//! * the thin I/O probe (HostFacts::probe) that reads this host once.
//!
//! Nothing here writes to the host, launches a workload, or uses the network.
//! A fact that was not observed is None and is never treated as available.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::runtime_launch::lease::RUNTIME_LAUNCH_LEASE_KIND;
use crate::runtime_launch::volume_maintenance::STATE_VOLUME_MAINTENANCE_LEASE_KIND;
use crate::{
    ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND, PORTABLE_CAPSULE_LEASE_KIND,
    explicit_runtime_placement_kinds,
};

/// Every lease kind this module reasons about, in report order.
const ALL_LEASE_KINDS: [&str; 4] = [
    PORTABLE_CAPSULE_LEASE_KIND,
    RUNTIME_LAUNCH_LEASE_KIND,
    STATE_VOLUME_MAINTENANCE_LEASE_KIND,
    ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND,
];

/// The execution ABI that requires Docker access.
pub const OCI_EXECUTION_CAPABILITY: &str = "execution_abi:oci";

// The reason vocabulary and capability statuses are owned by `ato-host-facts`
// so the CLI, the adapters and this worker cannot drift apart. The worker's
// lease-kind advertisement (below) stays here: which lease kinds this Runner
// offers to the control plane is a Runner concern, not a host fact.
pub use ato_host_facts::{CapabilityState, CapabilityStatus, Reason, ReasonCode};

/// Everything this worker measured about its host, plus the operator
/// configuration that changes what would be advertised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFacts {
    pub os: &'static str,
    pub platform_linux: bool,
    /// The authoritative containment answer from the launch path's own probe
    /// (runtime_launch::sandbox::containment_available), not a re-derivation.
    pub containment_available: bool,
    /// Diagnostic detail behind a negative containment_available.
    pub bwrap_binary_present: Option<bool>,
    pub bwrap_probe_stderr: Option<String>,
    /// /proc/sys/kernel/apparmor_restrict_unprivileged_userns == 1.
    pub apparmor_restricts_unprivileged_userns: Option<bool>,
    /// /proc/sys/kernel/unprivileged_userns_clone == 0 (Debian patch).
    pub unprivileged_userns_clone_disabled: Option<bool>,
    /// CapEff of this process. Non-zero means it holds effective capabilities,
    /// which is not the unprivileged state bubblewrap expects.
    pub effective_capabilities: Option<u64>,
    /// Whether the slot finished reconciling the previous incarnation.
    pub slot_recovered: bool,
    /// A writable Runner-local volume store was proven by the worker itself.
    pub persistent_volumes: bool,
    pub state_volume_root: Option<PathBuf>,
    /// Read-only access check of the configured root. Doctor never creates it,
    /// so it can only report "not proven" and never "opened successfully".
    pub state_volume_root_writable: Option<bool>,
    pub explicit_placement: bool,
    pub browser_executor_configured: bool,
    /// Whether the caller actually knows the browser executor configuration.
    /// Doctor does not read the Run-control key, so it reports "not evaluated"
    /// rather than claiming the executor is unconfigured.
    pub browser_executor_evaluated: bool,
    /// The authoritative Docker answer
    /// (ato_adapter_oci::docker_runtime_available).
    pub docker_available: bool,
    pub docker_binary_present: Option<bool>,
    pub docker_probe_stderr: Option<String>,
}

/// The lease kinds this host advertises, and one status per kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseAdmission {
    pub kinds: Vec<&'static str>,
    pub statuses: Vec<CapabilityStatus>,
}

/// What the heartbeat advertises plus why everything else is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityReport {
    pub advertised_lease_kinds: Vec<&'static str>,
    /// One entry per capability, in ALL_LEASE_KINDS order followed by the
    /// execution ABI. A capability that is not advertised always carries a
    /// reason.
    pub capabilities: Vec<CapabilityStatus>,
}

/// The capability name the control plane sees for one lease kind.
fn lease_capability(kind: &str) -> String {
    format!("lease_kind:{kind}")
}

fn platform_name(facts: &HostFacts) -> &str {
    if facts.os.is_empty() {
        "an unidentified platform"
    } else {
        facts.os
    }
}

fn slot_reason() -> Reason {
    Reason::new(
        ReasonCode::SlotNotRecovered,
        "this slot has not finished reconciling the previous Run (recovery is still running or \
         failed), so it takes no new runtime work until the old workload is confirmed stopped; \
         the worker log line runtime-launch-recovery names the cause",
    )
}

/// Why containment is not available, from observations the probe actually made.
///
/// An unreadable sysctl never becomes a reason: only a value that was read, and
/// that restricts the namespaces bwrap needs, is reported as a restriction. A
/// probe that failed without such an explanation is still reported as a failure
/// rather than as silence.
fn containment_reasons(facts: &HostFacts) -> Vec<Reason> {
    if !facts.platform_linux {
        return vec![Reason::new(
            ReasonCode::PlatformUnsupported,
            format!(
                "bubblewrap containment only exists on Linux and this host is {}; \
                 runtime_launch stays unadvertised here",
                platform_name(facts)
            ),
        )];
    }
    if facts.bwrap_binary_present == Some(false) {
        return vec![Reason::new(
            ReasonCode::BwrapBinaryMissing,
            "bubblewrap (bwrap) is not installed on PATH, so a process workload cannot be \
             contained; install the bubblewrap package for the worker user and re-run doctor",
        )];
    }
    let mut reasons = Vec::new();
    if facts.apparmor_restricts_unprivileged_userns == Some(true) {
        reasons.push(Reason::new(
            ReasonCode::ApparmorRestrictsUnprivilegedUserns,
            "AppArmor restricts unprivileged user namespaces \
             (kernel.apparmor_restrict_unprivileged_userns=1 for confined programs and \
             kernel.apparmor_restrict_unprivileged_unconfined=1 for unconfined ones), so bwrap \
             cannot create the namespaces runtime_launch needs; give this binary a profile with \
             the userns permission, set the applicable sysctl to 0, or use --isolation \
             host-boundary on a dedicated machine",
        ));
    }
    if facts.unprivileged_userns_clone_disabled == Some(true) {
        reasons.push(Reason::new(
            ReasonCode::UnprivilegedUsernsCloneDisabled,
            "the kernel disables unprivileged user namespaces \
             (kernel.unprivileged_userns_clone=0), so bwrap cannot create the namespaces \
             runtime_launch needs; enable unprivileged user namespaces or use --isolation \
             host-boundary on a dedicated machine",
        ));
    }
    if let Some(mask) = facts.effective_capabilities.filter(|mask| *mask != 0) {
        reasons.push(Reason::new(
            ReasonCode::ProcessEffectiveCapabilitiesPresent,
            format!(
                "this worker process holds effective capabilities (CapEff=0x{mask:016x}); the \
                 unprivileged bubblewrap path needs a capability-free process, so the containment \
                 probe failed. Start the worker as an unprivileged user and re-run doctor"
            ),
        ));
    }
    if reasons.is_empty() {
        let detail = facts
            .bwrap_probe_stderr
            .as_deref()
            .map(|stderr| format!(" ({stderr})"))
            .unwrap_or_default();
        reasons.push(Reason::new(
            ReasonCode::BwrapProbeFailed,
            format!(
                "bwrap is installed but the containment probe could not create its namespaces\
                 {detail}; run bwrap --unshare-all --ro-bind / / -- true as the worker user to \
                 read the kernel's refusal"
            ),
        ));
    }
    reasons
}

fn runtime_launch_status(facts: &HostFacts) -> CapabilityStatus {
    let capability = lease_capability(RUNTIME_LAUNCH_LEASE_KIND);
    if facts.containment_available && facts.slot_recovered {
        return CapabilityStatus {
            capability,
            state: CapabilityState::Advertised,
            reasons: Vec::new(),
        };
    }
    let mut reasons = Vec::new();
    if !facts.slot_recovered {
        reasons.push(slot_reason());
    }
    if !facts.containment_available {
        reasons.extend(containment_reasons(facts));
    }
    CapabilityStatus {
        capability,
        state: CapabilityState::Unavailable,
        reasons,
    }
}

fn state_volume_maintenance_status(facts: &HostFacts) -> CapabilityStatus {
    let capability = lease_capability(STATE_VOLUME_MAINTENANCE_LEASE_KIND);
    let mut state = CapabilityState::Advertised;
    let mut reasons = Vec::new();
    // Volume maintenance runs through the same slot as runtime_launch, so a
    // slot that cannot take runtime_launch cannot take volume maintenance.
    if !(facts.containment_available && facts.slot_recovered) {
        state = CapabilityState::Unavailable;
        reasons.push(Reason::new(
            ReasonCode::RuntimeLaunchUnavailable,
            "state_volume_maintenance is served by the same runtime slot as runtime_launch, which \
             this host does not advertise (see its reasons); checkpoint, restore and delete would \
             have nowhere to run",
        ));
    }
    if !facts.persistent_volumes {
        match (
            facts.state_volume_root.as_deref(),
            facts.state_volume_root_writable,
        ) {
            (None, _) => {
                state = CapabilityState::Unavailable;
                reasons.push(Reason::new(
                    ReasonCode::StateVolumeRootUnconfigured,
                    "no Runner-local volume root is configured (--state-volume-root or \
                     ATO_RUNNER_STATE_VOLUME_ROOT), so state_volume_maintenance is not advertised \
                     and volume-backed Runs have no durable store here",
                ));
            }
            (Some(root), Some(false)) => {
                state = CapabilityState::Unavailable;
                reasons.push(Reason::new(
                    ReasonCode::StateVolumeStoreNotWritable,
                    format!(
                        "the configured volume root {} is not writable by this user; the worker \
                         opens the store at start-up and a store it cannot write is never \
                         advertised",
                        root.display()
                    ),
                ));
            }
            (Some(root), _) => {
                // A writable directory is not proof that the worker opened the
                // store, and doctor must not create it to find out.
                if state == CapabilityState::Advertised {
                    state = CapabilityState::Unverified;
                }
                reasons.push(Reason::new(
                    ReasonCode::StateVolumeStoreNotVerified,
                    format!(
                        "the worker only advertises volumes after it opens the store at start-up, \
                         and doctor cannot open {} without creating it; the worker's first \
                         heartbeat is the answer",
                        root.display()
                    ),
                ));
            }
        }
    }
    CapabilityStatus {
        capability,
        state,
        reasons,
    }
}

fn portable_capsule_status() -> CapabilityStatus {
    CapabilityStatus {
        capability: lease_capability(PORTABLE_CAPSULE_LEASE_KIND),
        state: CapabilityState::Advertised,
        reasons: Vec::new(),
    }
}

fn browser_executor_status(facts: &HostFacts) -> CapabilityStatus {
    let capability = lease_capability(ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND);
    if facts.browser_executor_configured {
        return CapabilityStatus {
            capability,
            state: CapabilityState::Advertised,
            reasons: Vec::new(),
        };
    }
    if !facts.browser_executor_evaluated {
        return CapabilityStatus {
            capability,
            state: CapabilityState::Unverified,
            reasons: vec![Reason::new(
                ReasonCode::BrowserExecutorNotEvaluated,
                "doctor does not read the browser executor configuration (--browser-chrome plus a \
                 Run-control verification key), so it cannot say whether \
                 activity_browser_executor_v0 is advertised; the worker heartbeat is the answer",
            )],
        };
    }
    CapabilityStatus {
        capability,
        state: CapabilityState::Unavailable,
        reasons: vec![Reason::new(
            ReasonCode::BrowserExecutorUnconfigured,
            "no browser executor is configured (a Chrome path and a Run-control verification key \
             are both required), so activity_browser_executor_v0 is not advertised; ordinary Runs \
             are unaffected",
        )],
    }
}

fn docker_reasons(facts: &HostFacts) -> Vec<Reason> {
    if !facts.platform_linux {
        return vec![Reason::new(
            ReasonCode::PlatformUnsupported,
            format!(
                "the OCI Adapter enables container execution on Linux only and this host is {}; \
                 execution_abi=oci stays unadvertised here",
                platform_name(facts)
            ),
        )];
    }
    if facts.docker_binary_present == Some(false) {
        return vec![Reason::new(
            ReasonCode::DockerBinaryMissing,
            "the docker CLI is not on PATH, so this Runner cannot launch a container workload; \
             install Docker Engine for the worker user and re-run doctor",
        )];
    }
    let stderr = facts.docker_probe_stderr.as_deref().unwrap_or("");
    let lower = stderr.to_lowercase();
    let detail = if stderr.trim().is_empty() {
        "the probe returned no detail".to_owned()
    } else {
        stderr.to_owned()
    };
    if lower.contains("permission denied") || lower.contains("access denied") {
        return vec![Reason::new(
            ReasonCode::DockerSocketAccessDenied,
            format!(
                "the docker CLI is installed but cannot reach the daemon socket: {detail}; add the \
                 worker user to the docker group (or grant equivalent socket access), otherwise \
                 container workloads stay unavailable"
            ),
        )];
    }
    if lower.contains("cannot connect")
        || lower.contains("connection refused")
        || lower.contains("is the docker daemon running")
        || lower.contains("no such file")
    {
        return vec![Reason::new(
            ReasonCode::DockerDaemonUnreachable,
            format!(
                "the docker client is installed but no daemon answered: {detail}; start Docker \
                 Engine for the worker user and re-run doctor"
            ),
        )];
    }
    vec![Reason::new(
        ReasonCode::DockerUnavailable,
        format!("docker did not answer its version probe: {detail}"),
    )]
}

fn execution_status(facts: &HostFacts) -> CapabilityStatus {
    let capability = OCI_EXECUTION_CAPABILITY.to_owned();
    if facts.docker_available {
        return CapabilityStatus {
            capability,
            state: CapabilityState::Advertised,
            reasons: Vec::new(),
        };
    }
    CapabilityStatus {
        capability,
        state: CapabilityState::Unavailable,
        reasons: docker_reasons(facts),
    }
}

/// The one decision the heartbeat and the doctor both use.
///
/// Every kind that is not returned in kinds appears in statuses with a state
/// other than Advertised and at least one reason, so a lease kind can never go
/// missing without an explanation.
pub fn lease_admission(facts: &HostFacts) -> LeaseAdmission {
    let mut statuses = vec![
        portable_capsule_status(),
        runtime_launch_status(facts),
        state_volume_maintenance_status(facts),
        browser_executor_status(facts),
    ];
    if facts.explicit_placement {
        let runtime_launch = lease_capability(RUNTIME_LAUNCH_LEASE_KIND);
        let restriction = Reason::new(
            ReasonCode::ExplicitPlacementRestrictsLeaseKinds,
            "this Runner was started in explicit placement mode \
             (--require-explicit-runtime-placement), so only runtime_launch leases are advertised; \
             this lease kind is reported as removed instead of being dropped silently",
        );
        for status in &mut statuses {
            if status.capability != runtime_launch {
                status.state = CapabilityState::Unavailable;
                status.reasons.push(restriction.clone());
            }
        }
    }
    let advertised: Vec<&'static str> = ALL_LEASE_KINDS
        .iter()
        .zip(statuses.iter())
        .filter(|(_, status)| status.state == CapabilityState::Advertised)
        .map(|(kind, _)| *kind)
        .collect();
    // The same restriction the heartbeat applies, not a copy of it.
    LeaseAdmission {
        kinds: explicit_runtime_placement_kinds(advertised, facts.explicit_placement),
        statuses,
    }
}

/// What the heartbeat advertises, plus the reason every other capability is
/// missing.
pub fn capability_report(facts: &HostFacts) -> CapabilityReport {
    let admission = lease_admission(facts);
    let mut capabilities = admission.statuses;
    capabilities.push(execution_status(facts));
    CapabilityReport {
        advertised_lease_kinds: admission.kinds,
        capabilities,
    }
}

/// "CapEff: 00000000a80425fb" from /proc/self/status.
pub fn parse_cap_eff(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("CapEff:"))?;
    parse_hex_mask(line.strip_prefix("CapEff:")?)
}

/// A hexadecimal capability mask. Anything that is not plain hex is unknown,
/// and unknown is not "no capabilities".
pub fn parse_hex_mask(value: &str) -> Option<u64> {
    let value = value.trim();
    (value.len() <= 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) && !value.is_empty())
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
}

// ---------------------------------------------------------------------------
// Thin I/O probe
// ---------------------------------------------------------------------------

impl HostFacts {
    /// Read this host once. Offline and non-destructive: no workload is
    /// launched, nothing is created, and no request leaves the machine.
    ///
    /// The verdicts come from the probes a launch itself uses
    /// (containment_available, docker_available); the other observations exist
    /// only to explain a negative verdict.
    ///
    /// Read-only properties are reported honestly: this function creates no
    /// directory and reads no worker state. A fresh worker starts with its
    /// slot recovered, so slot recovery is assumed here and the doctor report
    /// says that it was not measured.
    pub fn probe(state_volume_root: Option<&Path>, explicit_placement: bool) -> Self {
        let platform_linux = cfg!(target_os = "linux");
        let bwrap = ato_sandbox::probe_bubblewrap();
        let docker = if platform_linux {
            ato_adapter_oci::docker_access()
        } else {
            ato_adapter_oci::DockerAccess::default()
        };
        Self {
            os: std::env::consts::OS,
            platform_linux,
            containment_available: crate::runtime_launch::sandbox::containment_available(),
            bwrap_binary_present: platform_linux.then_some(bwrap.binary.is_some()),
            bwrap_probe_stderr: bwrap.stderr.clone(),
            apparmor_restricts_unprivileged_userns: apparmor_restricts_unprivileged_userns(),
            unprivileged_userns_clone_disabled: unprivileged_userns_clone_disabled(),
            effective_capabilities: effective_capabilities(),
            slot_recovered: true,
            // Opening the volume store creates directories. Doctor must not.
            persistent_volumes: false,
            state_volume_root: state_volume_root.map(Path::to_path_buf),
            state_volume_root_writable: state_volume_root.and_then(directory_writable),
            explicit_placement,
            browser_executor_configured: false,
            browser_executor_evaluated: false,
            docker_available: docker.daemon_reachable,
            docker_binary_present: docker.binary.as_deref().map(Path::is_file),
            docker_probe_stderr: docker.stderr.clone(),
        }
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

/// Whether AppArmor currently refuses the unprivileged user namespace this
/// Worker needs.
///
/// Ubuntu 24.04 has two independent switches: one for confined programs and one
/// for unconfined ones. Only the switch matching this process's own confinement
/// applies, so a restriction aimed at confined programs is not reported for an
/// unconfined Worker (or the reverse). An unreadable or unrecognised sysctl is
/// unknown, never "allowed".
fn apparmor_restricts_unprivileged_userns() -> Option<bool> {
    let confined = process_is_apparmor_confined()?;
    let sysctl = if confined {
        "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
    } else {
        "/proc/sys/kernel/apparmor_restrict_unprivileged_unconfined"
    };
    match read_trimmed(Path::new(sysctl))?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Reads /proc/self/attr/current, which AppArmor sets to a profile name and
/// leaves as "unconfined" otherwise. Absent on hosts without AppArmor.
fn process_is_apparmor_confined() -> Option<bool> {
    let current = read_trimmed(Path::new("/proc/self/attr/current"))?;
    Some(!current.starts_with("unconfined"))
}

fn unprivileged_userns_clone_disabled() -> Option<bool> {
    match read_trimmed(Path::new("/proc/sys/kernel/unprivileged_userns_clone"))?.as_str() {
        "0" => Some(true),
        "1" => Some(false),
        _ => None,
    }
}

fn effective_capabilities() -> Option<u64> {
    parse_cap_eff(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// Whether this user could create files under path, or under its nearest
/// existing ancestor (what create_dir_all would use), WITHOUT creating
/// anything: access(2) only asks the kernel.
fn directory_writable(path: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use rustix::fs::{Access, access};
        let mut candidate = path.to_path_buf();
        loop {
            if candidate.exists() {
                return Some(
                    access(
                        &candidate,
                        Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK,
                    )
                    .is_ok(),
                );
            }
            if !candidate.pop() {
                return None;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_facts() -> HostFacts {
        HostFacts {
            os: "linux",
            platform_linux: true,
            containment_available: true,
            bwrap_binary_present: Some(true),
            bwrap_probe_stderr: None,
            apparmor_restricts_unprivileged_userns: Some(false),
            unprivileged_userns_clone_disabled: Some(false),
            effective_capabilities: Some(0),
            slot_recovered: true,
            persistent_volumes: true,
            state_volume_root: Some(PathBuf::from("/var/lib/ato/runner-volumes")),
            state_volume_root_writable: Some(true),
            explicit_placement: false,
            browser_executor_configured: false,
            browser_executor_evaluated: true,
            docker_available: true,
            docker_binary_present: Some(true),
            docker_probe_stderr: None,
        }
    }

    fn status<'a>(report: &'a CapabilityReport, capability: &str) -> &'a CapabilityStatus {
        report
            .capabilities
            .iter()
            .find(|entry| entry.capability == capability)
            .unwrap_or_else(|| panic!("{capability} is missing from the capability report"))
    }

    fn codes(status: &CapabilityStatus) -> Vec<&'static str> {
        status
            .reasons
            .iter()
            .map(|reason| reason.code.as_str())
            .collect()
    }

    #[test]
    fn q52_missing_bwrap_binary_is_the_reported_reason_for_runtime_launch() {
        let facts = HostFacts {
            containment_available: false,
            bwrap_binary_present: Some(false),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(runtime_launch.state, CapabilityState::Unavailable);
        assert_eq!(codes(runtime_launch), vec!["bwrap_binary_missing"]);
        assert!(runtime_launch.reasons[0].message.contains("bubblewrap"));
        assert!(
            !report
                .advertised_lease_kinds
                .contains(&RUNTIME_LAUNCH_LEASE_KIND)
        );
        // The reason must be actionable, not only a code.
        assert!(runtime_launch.reasons[0].message.len() > 40);
    }

    #[test]
    fn q52_apparmor_unprivileged_userns_restriction_is_named_as_the_reason() {
        let facts = HostFacts {
            containment_available: false,
            bwrap_binary_present: Some(true),
            apparmor_restricts_unprivileged_userns: Some(true),
            bwrap_probe_stderr: Some("bwrap: setting up uid map: Permission denied".into()),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(runtime_launch.state, CapabilityState::Unavailable);
        assert_eq!(
            codes(runtime_launch),
            vec!["apparmor_restricts_unprivileged_userns"]
        );
        let message = &runtime_launch.reasons[0].message;
        assert!(message.contains("AppArmor"), "{message}");
        assert!(
            message.contains("kernel.apparmor_restrict_unprivileged_userns=1"),
            "{message}"
        );
    }

    #[test]
    fn q52_disabled_unprivileged_userns_clone_is_named_as_the_reason() {
        let facts = HostFacts {
            containment_available: false,
            bwrap_binary_present: Some(true),
            unprivileged_userns_clone_disabled: Some(true),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(
            codes(runtime_launch),
            vec!["unprivileged_userns_clone_disabled"]
        );
        assert!(
            runtime_launch.reasons[0]
                .message
                .contains("unprivileged_userns_clone")
        );
    }

    #[test]
    fn q52_extra_process_capabilities_are_named_as_the_reason() {
        let facts = HostFacts {
            containment_available: false,
            bwrap_binary_present: Some(true),
            effective_capabilities: Some(0x0000_0000_a804_25fb),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(runtime_launch.state, CapabilityState::Unavailable);
        assert_eq!(
            codes(runtime_launch),
            vec!["process_effective_capabilities_present"]
        );
        let message = &runtime_launch.reasons[0].message;
        assert!(message.contains("CapEff"), "{message}");
        assert!(message.contains("a80425fb"), "{message}");
    }

    #[test]
    fn q52_state_volume_maintenance_names_an_unconfigured_volume_root() {
        let facts = HostFacts {
            persistent_volumes: false,
            state_volume_root: None,
            state_volume_root_writable: None,
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let volumes = status(&report, "lease_kind:state_volume_maintenance");
        assert_eq!(volumes.state, CapabilityState::Unavailable);
        assert_eq!(codes(volumes), vec!["state_volume_root_unconfigured"]);
        assert!(volumes.reasons[0].message.contains("--state-volume-root"));
        assert!(
            !report
                .advertised_lease_kinds
                .contains(&STATE_VOLUME_MAINTENANCE_LEASE_KIND)
        );
        // runtime_launch itself is unaffected by the missing volume store.
        assert_eq!(
            status(&report, "lease_kind:runtime_launch").state,
            CapabilityState::Advertised
        );
    }

    #[test]
    fn q52_state_volume_maintenance_names_a_non_writable_volume_root() {
        let facts = HostFacts {
            persistent_volumes: false,
            state_volume_root: Some(PathBuf::from("/run/ato-volumes")),
            state_volume_root_writable: Some(false),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let volumes = status(&report, "lease_kind:state_volume_maintenance");
        assert_eq!(volumes.state, CapabilityState::Unavailable);
        assert_eq!(codes(volumes), vec!["state_volume_store_not_writable"]);
        assert!(volumes.reasons[0].message.contains("/run/ato-volumes"));
    }

    #[test]
    fn q52_a_writable_but_unopened_volume_root_is_reported_as_unverified() {
        // Doctor must not create the store, so a writable directory is not
        // proof that the worker opened it. Reporting "advertised" here would
        // claim a capability nobody verified.
        let facts = HostFacts {
            persistent_volumes: false,
            state_volume_root: Some(PathBuf::from("/var/lib/ato/volumes")),
            state_volume_root_writable: Some(true),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let volumes = status(&report, "lease_kind:state_volume_maintenance");
        assert_eq!(volumes.state, CapabilityState::Unverified);
        assert_eq!(codes(volumes), vec!["state_volume_store_not_verified"]);
        assert!(
            !report
                .advertised_lease_kinds
                .contains(&STATE_VOLUME_MAINTENANCE_LEASE_KIND)
        );
    }

    #[test]
    fn q52_explicit_placement_names_every_lease_kind_it_removes() {
        let facts = HostFacts {
            explicit_placement: true,
            ..linux_facts()
        };
        let report = capability_report(&facts);
        assert_eq!(
            report.advertised_lease_kinds,
            vec![RUNTIME_LAUNCH_LEASE_KIND]
        );
        for kind in [
            PORTABLE_CAPSULE_LEASE_KIND,
            STATE_VOLUME_MAINTENANCE_LEASE_KIND,
            ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND,
        ] {
            let entry = status(&report, &format!("lease_kind:{kind}"));
            assert_eq!(entry.state, CapabilityState::Unavailable);
            assert!(
                codes(entry).contains(&"explicit_placement_restricts_lease_kinds"),
                "{kind} must name explicit placement, got {:?}",
                codes(entry)
            );
            assert!(
                entry
                    .reasons
                    .iter()
                    .any(|reason| reason.message.contains("explicit placement"))
            );
        }
    }

    #[test]
    fn q52_missing_docker_access_is_named_for_oci_execution() {
        let denied = HostFacts {
            docker_available: false,
            docker_binary_present: Some(true),
            docker_probe_stderr: Some(
                "permission denied while trying to connect to the Docker daemon socket".into(),
            ),
            ..linux_facts()
        };
        let report = capability_report(&denied);
        let oci = status(&report, "execution_abi:oci");
        assert_eq!(oci.state, CapabilityState::Unavailable);
        assert_eq!(codes(oci), vec!["docker_socket_access_denied"]);
        assert!(oci.reasons[0].message.contains("docker group"));

        let absent = HostFacts {
            docker_available: false,
            docker_binary_present: Some(false),
            ..linux_facts()
        };
        assert_eq!(
            codes(status(&capability_report(&absent), "execution_abi:oci")),
            vec!["docker_binary_missing"]
        );

        let unreachable = HostFacts {
            docker_available: false,
            docker_binary_present: Some(true),
            docker_probe_stderr: Some(
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. \
                 Is the docker daemon running?"
                    .into(),
            ),
            ..linux_facts()
        };
        assert_eq!(
            codes(status(
                &capability_report(&unreachable),
                "execution_abi:oci"
            )),
            vec!["docker_daemon_unreachable"]
        );

        let macos = HostFacts {
            os: "macos",
            platform_linux: false,
            docker_available: false,
            ..linux_facts()
        };
        let report = capability_report(&macos);
        let oci = status(&report, "execution_abi:oci");
        assert_eq!(codes(oci), vec!["platform_unsupported"]);
        assert!(oci.reasons[0].message.contains("macos"));
    }

    #[test]
    fn q92_an_unexplained_containment_failure_is_never_advertised() {
        let facts = HostFacts {
            containment_available: false,
            bwrap_binary_present: None,
            bwrap_probe_stderr: None,
            apparmor_restricts_unprivileged_userns: None,
            unprivileged_userns_clone_disabled: None,
            effective_capabilities: None,
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(runtime_launch.state, CapabilityState::Unavailable);
        assert_eq!(codes(runtime_launch), vec!["bwrap_probe_failed"]);
        assert!(
            !report
                .advertised_lease_kinds
                .contains(&RUNTIME_LAUNCH_LEASE_KIND)
        );
    }

    #[test]
    fn q92_a_containment_verdict_that_already_passed_raises_no_bwrap_alarm() {
        // Host-boundary isolation proves containment without bubblewrap. The
        // reasons must follow the verdict, not the raw bwrap observation.
        let facts = HostFacts {
            containment_available: true,
            bwrap_binary_present: Some(false),
            ..linux_facts()
        };
        let report = capability_report(&facts);
        let runtime_launch = status(&report, "lease_kind:runtime_launch");
        assert_eq!(runtime_launch.state, CapabilityState::Advertised);
        assert!(runtime_launch.reasons.is_empty());
    }

    #[test]
    fn q52_a_healthy_host_advertises_both_runtime_lease_kinds() {
        let report = capability_report(&linux_facts());
        assert_eq!(
            report.advertised_lease_kinds,
            vec![
                PORTABLE_CAPSULE_LEASE_KIND,
                RUNTIME_LAUNCH_LEASE_KIND,
                STATE_VOLUME_MAINTENANCE_LEASE_KIND
            ]
        );
        assert_eq!(
            status(&report, "lease_kind:runtime_launch").state,
            CapabilityState::Advertised
        );
        assert_eq!(
            status(&report, "lease_kind:state_volume_maintenance").state,
            CapabilityState::Advertised
        );
        assert_eq!(
            status(&report, "execution_abi:oci").state,
            CapabilityState::Advertised
        );
        // The browser executor is a separate opt-in and is named, not dropped.
        assert!(
            codes(status(&report, "lease_kind:activity_browser_executor_v0"))
                .contains(&"browser_executor_unconfigured")
        );
    }

    #[test]
    fn q92_every_capability_that_is_not_advertised_carries_a_reason() {
        let matrix = [
            linux_facts(),
            HostFacts {
                containment_available: false,
                bwrap_binary_present: Some(false),
                ..linux_facts()
            },
            HostFacts {
                containment_available: false,
                apparmor_restricts_unprivileged_userns: Some(true),
                ..linux_facts()
            },
            HostFacts {
                containment_available: false,
                effective_capabilities: Some(0x0000_0000_a804_25fb),
                ..linux_facts()
            },
            HostFacts {
                containment_available: false,
                ..linux_facts()
            },
            HostFacts {
                slot_recovered: false,
                ..linux_facts()
            },
            HostFacts {
                persistent_volumes: false,
                state_volume_root: None,
                ..linux_facts()
            },
            HostFacts {
                persistent_volumes: false,
                state_volume_root_writable: Some(false),
                ..linux_facts()
            },
            HostFacts {
                explicit_placement: true,
                ..linux_facts()
            },
            HostFacts {
                docker_available: false,
                docker_binary_present: Some(false),
                ..linux_facts()
            },
            HostFacts {
                os: "macos",
                platform_linux: false,
                containment_available: false,
                docker_available: false,
                ..linux_facts()
            },
        ];
        for facts in matrix {
            let report = capability_report(&facts);
            assert_eq!(
                report.capabilities.len(),
                ALL_LEASE_KINDS.len() + 1,
                "one entry per capability: {facts:?}"
            );
            for entry in &report.capabilities {
                match entry.state {
                    CapabilityState::Advertised => assert!(
                        entry.reasons.is_empty(),
                        "{} is advertised but carries reasons",
                        entry.capability
                    ),
                    CapabilityState::Unavailable | CapabilityState::Unverified => {
                        assert!(
                            !entry.reasons.is_empty(),
                            "{} is {:?} without a reason",
                            entry.capability,
                            entry.state
                        );
                        for reason in &entry.reasons {
                            assert!(!reason.code.as_str().is_empty());
                            assert!(!reason.message.trim().is_empty());
                        }
                    }
                }
            }
            for kind in &report.advertised_lease_kinds {
                let entry = status(&report, &format!("lease_kind:{kind}"));
                assert_eq!(
                    entry.state,
                    CapabilityState::Advertised,
                    "{kind} is advertised in the list but not in the statuses"
                );
            }
        }
    }

    #[test]
    fn q52_effective_capabilities_are_parsed_from_proc_status() {
        let status = concat!(
            "Name:\tato-connected-realization-worker\n",
            "CapInh:\t0000000000000000\n",
            "CapPrm:\t00000000a80425fb\n",
            "CapEff:\t00000000a80425fb\n",
            "CapBnd:\t000001ffffffffff\n",
        );
        assert_eq!(parse_cap_eff(status), Some(0x0000_0000_a804_25fb));
        assert_eq!(parse_cap_eff("CapEff:\t0000000000000000\n"), Some(0));
        assert_eq!(parse_cap_eff("CapEff: 1f\n"), Some(0x1f));
        // Missing, empty and non-hex masks are unknown, not "no capabilities".
        assert_eq!(parse_cap_eff("Name:\tworker\n"), None);
        assert_eq!(parse_cap_eff("CapEff:\n"), None);
        assert_eq!(parse_cap_eff("CapEff:\tnot-a-mask\n"), None);
        assert_eq!(parse_cap_eff("CapEff:\t0x1f\n"), None);
        assert_eq!(parse_hex_mask("0x1f"), None);
        assert_eq!(
            parse_hex_mask("00000000a80425fb"),
            Some(0x0000_0000_a804_25fb)
        );
        assert_eq!(parse_hex_mask("ffffffffffffffff"), Some(u64::MAX));
        assert_eq!(parse_hex_mask("fffffffffffffffff"), None);
    }
}
