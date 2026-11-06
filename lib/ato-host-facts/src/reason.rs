//! Why a capability is or is not available here.
//!
//! A capability that is missing without a reason is a dead end for whoever has
//! to act on it: "no runner available" is not something an operator can fix.
//! Every answer here therefore carries a stable, machine-readable code plus one
//! sentence with the action to take.
//!
//! A code is never reused for a different cause and is never renamed silently:
//! scripts match on them.

use serde::Serialize;

/// A stable, machine-readable cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    // ── platform / runtime requirements ────────────────────────────────
    /// The requested platform cannot run on this host at all.
    PlatformUnsupported,
    /// The requested platform runs through emulation: it works, and it is
    /// slower. Carried as a warning, not as a refusal.
    PlatformEmulated,
    /// The route declares filesystem state, which needs a Linux sandbox that
    /// this host does not provide.
    StateNeedsLinuxSandbox,
    // ── Docker ────────────────────────────────────────────────────────
    /// The `docker` CLI is not on `PATH`.
    DockerBinaryMissing,
    /// The CLI exists but the daemon socket refuses this user.
    DockerSocketAccessDenied,
    /// The CLI exists and no daemon answered.
    DockerDaemonUnreachable,
    /// The daemon answered but the facts needed to decide were not available.
    DockerUnavailable,
    /// The daemon runs in a VM (Docker Desktop and equivalents). Its container
    /// bridge is inside that VM: the container IPs are not reachable from the
    /// CLI's kernel.
    DockerBridgeUnreachableVm,
    /// The daemon is rootless. Its container network is in a user namespace the
    /// CLI cannot enter, so container IPs are not reachable either.
    DockerBridgeUnreachableRootless,
    /// The daemon is on another machine (tcp:// or ssh://). This host's
    /// workspace tree cannot be delivered to it, so a Run cannot be replayed
    /// there.
    RemoteDaemonWorkspaceUnsupported,
    /// The daemon is local but its file sharing does not include the path this
    /// Run needs to bind.
    DockerWorkspaceNotShared,
    // ── containment (process workloads) ───────────────────────────────
    BwrapBinaryMissing,
    BwrapProbeFailed,
    ApparmorRestrictsUnprivilegedUserns,
    UnprivilegedUsernsCloneDisabled,
    ProcessEffectiveCapabilitiesPresent,
    // ── Runner slot / local configuration ─────────────────────────────
    SlotNotRecovered,
    RuntimeLaunchUnavailable,
    StateVolumeRootUnconfigured,
    StateVolumeStoreNotWritable,
    StateVolumeStoreNotVerified,
    ExplicitPlacementRestrictsLeaseKinds,
    BrowserExecutorUnconfigured,
    BrowserExecutorNotEvaluated,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlatformUnsupported => "platform_unsupported",
            Self::PlatformEmulated => "platform_emulated",
            Self::StateNeedsLinuxSandbox => "state_needs_linux_sandbox",
            Self::DockerBinaryMissing => "docker_binary_missing",
            Self::DockerSocketAccessDenied => "docker_socket_access_denied",
            Self::DockerDaemonUnreachable => "docker_daemon_unreachable",
            Self::DockerUnavailable => "docker_unavailable",
            Self::DockerBridgeUnreachableVm => "docker_bridge_unreachable_vm",
            Self::DockerBridgeUnreachableRootless => "docker_bridge_unreachable_rootless",
            Self::RemoteDaemonWorkspaceUnsupported => "remote_daemon_workspace_unsupported",
            Self::DockerWorkspaceNotShared => "docker_workspace_not_shared",
            Self::BwrapBinaryMissing => "bwrap_binary_missing",
            Self::BwrapProbeFailed => "bwrap_probe_failed",
            Self::ApparmorRestrictsUnprivilegedUserns => "apparmor_restricts_unprivileged_userns",
            Self::UnprivilegedUsernsCloneDisabled => "unprivileged_userns_clone_disabled",
            Self::ProcessEffectiveCapabilitiesPresent => "process_effective_capabilities_present",
            Self::SlotNotRecovered => "slot_not_recovered",
            Self::RuntimeLaunchUnavailable => "runtime_launch_unavailable",
            Self::StateVolumeRootUnconfigured => "state_volume_root_unconfigured",
            Self::StateVolumeStoreNotWritable => "state_volume_store_not_writable",
            Self::StateVolumeStoreNotVerified => "state_volume_store_not_verified",
            Self::ExplicitPlacementRestrictsLeaseKinds => {
                "explicit_placement_restricts_lease_kinds"
            }
            Self::BrowserExecutorUnconfigured => "browser_executor_unconfigured",
            Self::BrowserExecutorNotEvaluated => "browser_executor_not_evaluated",
        }
    }

    /// Whether this code describes a working capability with a caveat.
    ///
    /// Only emulation qualifies: the workload runs, and the receipt says how.
    /// Everything else either works plainly or does not work.
    pub fn is_warning(self) -> bool {
        matches!(self, Self::PlatformEmulated)
    }
}

impl Serialize for ReasonCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One cause, with the sentence whoever reads it needs to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub code: ReasonCode,
    pub message: String,
}

impl Reason {
    pub fn new(code: ReasonCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Whether a capability reaches the control plane (or the Run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    /// Proven here and advertised by the heartbeat.
    Advertised,
    /// Proven impossible with this host/configuration.
    Unavailable,
    /// Not proven either way. Not advertised, and the reason says what is
    /// missing: unknown is never reported as available.
    Unverified,
}

/// One capability, its state, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityStatus {
    /// The capability as it is named at the boundary, e.g.
    /// "execution_abi:oci" or "lease_kind:runtime_launch".
    pub capability: String,
    pub state: CapabilityState,
    /// Never empty unless state is Advertised.
    pub reasons: Vec<Reason>,
}

impl CapabilityStatus {
    pub fn advertised(capability: impl Into<String>) -> Self {
        Self {
            capability: capability.into(),
            state: CapabilityState::Advertised,
            reasons: Vec::new(),
        }
    }

    pub fn unavailable(capability: impl Into<String>, reasons: Vec<Reason>) -> Self {
        Self {
            capability: capability.into(),
            state: CapabilityState::Unavailable,
            reasons,
        }
    }

    pub fn unverified(capability: impl Into<String>, reasons: Vec<Reason>) -> Self {
        Self {
            capability: capability.into(),
            state: CapabilityState::Unverified,
            reasons,
        }
    }

    pub fn is_advertised(&self) -> bool {
        self.state == CapabilityState::Advertised
    }

    /// The warnings this capability carries while still being available.
    pub fn warnings(&self) -> impl Iterator<Item = &Reason> {
        self.reasons
            .iter()
            .filter(|reason| reason.code.is_warning())
    }

    /// One human-readable line, used by `ato pack` and the Run's refusal.
    pub fn describe(&self) -> String {
        if self.reasons.is_empty() {
            return format!("{}: {:?}", self.capability, self.state);
        }
        format!(
            "{}: {:?} ({})",
            self.capability,
            self.state,
            self.reasons
                .iter()
                .map(|reason| format!("{}: {}", reason.code.as_str(), reason.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    }
}
