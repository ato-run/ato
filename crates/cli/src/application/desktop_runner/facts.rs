//! Desktop Runner capability facts — what a *local* Ato Runner provider can
//! honestly execute on this host (#838, M0).
//!
//! A Desktop Runner is a local provider (the macOS/desktop shell acting as a
//! Connected Runner) backed by an isolation substrate. These types describe the
//! substrate and per-backend capabilities the host advertises — they are the
//! Desktop equivalent of the runner-agent heartbeat's
//! [`collect_capabilities`](crate::application::runner_agent::collect_capabilities),
//! but structured rather than flat strings so a placement decision can reason
//! about guest OS/arch, isolation boundary, and Ready-State maturity.
//!
//! Honesty is the contract: every `supports_*` flag starts `false` and only a
//! real, validated mechanism flips it to `true`. M0 advertises a cold-OCI
//! substrate only — no Ready-State restore, no CRIU, no bindings (see
//! `docs/ready-state/desktop-runner.md`). The restore-compatibility class a
//! Ready-State artifact pins lives in
//! [`RunnerClassFacts`](capsule::foundation::install_lifecycle::RunnerClassFacts);
//! these facts describe the *provider*, not the restore class.

use serde::{Deserialize, Serialize};

/// `provider_kind` for every Desktop Runner backend/fact.
pub(crate) const PROVIDER_KIND_DESKTOP: &str = "desktop";

/// Apple Containerization substrate identifier (`container` / Apple VZ).
pub(crate) const SUBSTRATE_APPLE_CONTAINERIZATION: &str = "apple_containerization";

/// Podman substrate identifier (`podman` / the shared `ato-podman` machine).
pub(crate) const SUBSTRATE_PODMAN: &str = "podman";

/// Apple Virtualization.framework accelerator label.
pub(crate) const ACCELERATOR_APPLE_VZ: &str = "apple_vz";

/// The isolation boundary a backend executes a session inside.
///
/// Serialized in snake_case so a receipt reads `"vm_wrapped_container"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IsolationBoundary {
    /// An OCI container running inside a per-session lightweight Linux VM
    /// (Apple Containerization / Apple VZ).
    VmWrappedContainer,
    /// A bare microVM (e.g. Firecracker) — the existing snapshot path's domain,
    /// reported here only for matrix completeness, never produced by M0.
    MicroVm,
}

/// The VM scope of a `vm_wrapped_container` substrate. Both Apple
/// Containerization and Podman are `vm_wrapped_container` at the boundary
/// level, but the VM scope differs — and that difference matters for cleanup,
/// state reuse, and what a session receipt honestly claims.
///
/// - `per_session_vm`: a fresh VM is started per container and torn down on
///   exit (Apple Containerization).
/// - `shared_machine`: containers run inside a shared, persistent Podman
///   machine; the container outlives the wrapper process and must be
///   explicitly removed (`podman rm -f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubstrateScope {
    PerSessionVm,
    SharedMachine,
}

impl SubstrateScope {
    /// The snake_case wire label (matches the serde representation).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PerSessionVm => "per_session_vm",
            Self::SharedMachine => "shared_machine",
        }
    }
}

/// The Ready-State mechanism a backend can restore from.
///
/// M0 is `ColdOci` only; `CriuCheckpoint`/`VmSnapshot` are future inner
/// mechanisms tracked separately (CRIU is #839, a Linux-gated spike first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadyStateKind {
    /// Cold start of an OCI image (no warm restore). The only M0 mode.
    ColdOci,
    /// In-VM CRIU checkpoint/restore (future, #839).
    CriuCheckpoint,
    /// Full VM-memory snapshot/restore (future; Firecracker's mode).
    VmSnapshot,
}

impl IsolationBoundary {
    /// The snake_case wire label (matches the serde representation).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::VmWrappedContainer => "vm_wrapped_container",
            Self::MicroVm => "micro_vm",
        }
    }
}

impl ReadyStateKind {
    /// The snake_case wire label (matches the serde representation).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ColdOci => "cold_oci",
            Self::CriuCheckpoint => "criu_checkpoint",
            Self::VmSnapshot => "vm_snapshot",
        }
    }
}

/// How mature/trustworthy a backend is. M0 backends are `Experimental`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Maturity {
    Experimental,
    Beta,
    Stable,
}

impl Maturity {
    /// The snake_case wire label (matches the serde representation).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Experimental => "experimental",
            Self::Beta => "beta",
            Self::Stable => "stable",
        }
    }
}

/// A structured reason the host cannot serve a local cold-OCI Desktop Runner
/// backend. The machine-readable counterpart to the free-text `diagnostics`
/// strings: each blocker names *one* missing precondition so a placement
/// failure can tell the user exactly what to fix (upgrade macOS / install
/// `container` / start the `ato-podman` machine / use a managed runner) instead
/// of a single generic "no local backend" message. Populated by the host probes
/// ([`super::macos`] / [`super::build_other_facts`]); empty when a local backend
/// is available.
///
/// **Rendering policy:** blockers are emitted only for substrates that are
/// unavailable. If any substrate is available, the unavailable substrates'
/// blockers do NOT appear in the placement failure path (a backend IS
/// available, so placement succeeds with no blockers). They may still appear
/// in `ato doctor desktop-runner` substrate details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LocalBackendBlocker {
    // ── Apple Containerization substrate ────────────────────────────────────
    /// Host is not Apple silicon (Apple Containerization requires it).
    NotAppleSilicon,
    /// macOS is older than the minimum required for Apple Containerization.
    MacOsTooOld {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        found: Option<String>,
        required: u32,
    },
    /// The Apple `container` tool is not installed / not on PATH.
    AppleContainerMissing,
    // ── Podman substrate ────────────────────────────────────────────────────
    /// The `podman` binary is not installed / not resolvable.
    PodmanBinaryMissing,
    /// The `ato-podman` machine is configured but not running.
    PodmanMachineStopped,
    /// No `ato-podman` machine is configured (`podman machine list` returned
    /// no entries, or none named `ato-podman`).
    PodmanMachineNotConfigured,
    /// `podman machine list` could not run, returned unparseable output, or hit
    /// a permission error. `reason` is a short, safe summary (never raw stderr).
    PodmanMachineStatusUnavailable { reason: String },
    // ── non-macOS ───────────────────────────────────────────────────────────
    /// Host OS has no Desktop Runner cold-OCI substrate (Linux/Windows/etc.).
    NonMacOsHost { host_os: String },
}

impl LocalBackendBlocker {
    /// Stable snake_case tag (matches the serde `kind` label).
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::NotAppleSilicon => "not_apple_silicon",
            Self::MacOsTooOld { .. } => "macos_too_old",
            Self::AppleContainerMissing => "apple_container_missing",
            Self::PodmanBinaryMissing => "podman_binary_missing",
            Self::PodmanMachineStopped => "podman_machine_stopped",
            Self::PodmanMachineNotConfigured => "podman_machine_not_configured",
            Self::PodmanMachineStatusUnavailable { .. } => "podman_machine_status_unavailable",
            Self::NonMacOsHost { .. } => "non_macos_host",
        }
    }

    /// One-line actionable next step for the user.
    pub(crate) fn next_action(&self) -> &'static str {
        match self {
            Self::NotAppleSilicon => {
                "use a managed runner (Apple Containerization requires Apple silicon)"
            }
            Self::MacOsTooOld { .. } => "upgrade macOS to 26+ or use a managed runner",
            Self::AppleContainerMissing => {
                "install Apple `container` from https://github.com/apple/container, or use a \
                 managed runner"
            }
            Self::PodmanBinaryMissing => {
                "install Podman (or start it with `ato runtime setup`), or use a managed runner"
            }
            Self::PodmanMachineStopped => {
                "start the `ato-podman` machine (`podman machine start ato-podman`), or use a \
                 managed runner"
            }
            Self::PodmanMachineNotConfigured => {
                "create the `ato-podman` machine (`ato runtime setup`), or use a managed runner"
            }
            Self::PodmanMachineStatusUnavailable { .. } => {
                "check Podman installation (`podman machine list`), or use a managed runner"
            }
            Self::NonMacOsHost { .. } => "use a managed runner (local cold-OCI is macOS-only)",
        }
    }
}

/// Availability of one isolation substrate on this host (e.g. Apple
/// Containerization, Podman). Reported even when unavailable so a diagnostic
/// can name what is missing — but an unavailable substrate yields no
/// [`BackendCapability`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SubstrateCapability {
    /// e.g. [`SUBSTRATE_APPLE_CONTAINERIZATION`] or [`SUBSTRATE_PODMAN`].
    pub(crate) substrate: String,
    /// Whether this substrate can actually be used on this host right now.
    pub(crate) available: bool,
    /// The CLI/tool backing the substrate (e.g. `"container"`, `"podman"`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool: Option<String>,
    /// Resolved path to the tool, if found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_path: Option<String>,
    /// Tool version string, if probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_version: Option<String>,
    /// Whether the substrate's system service is already running. **Detected,
    /// never auto-started** — M0 must not invoke `container system start` or
    /// `podman machine start`.
    pub(crate) system_service_running: bool,
    /// Hardware accelerator the substrate uses, e.g. [`ACCELERATOR_APPLE_VZ`].
    /// `None` for Podman (it uses its own virtualization; no Ato-managed
    /// accelerator label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) accelerator: Option<String>,
    /// VM scope of this substrate — distinguishes per-session VMs (Apple
    /// Containerization) from shared-machine substrates (Podman) within the
    /// same `vm_wrapped_container` isolation boundary.
    pub(crate) substrate_scope: SubstrateScope,
    pub(crate) maturity: Maturity,
}

/// One concrete entry in the Desktop Runner backend matrix: a (substrate ×
/// host × guest × ready-state) capability the host can honestly serve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackendCapability {
    /// Always [`PROVIDER_KIND_DESKTOP`] for Desktop Runner backends.
    pub(crate) provider: String,
    pub(crate) substrate: String,
    pub(crate) host_os: String,
    pub(crate) host_arch: String,
    pub(crate) guest_os: String,
    pub(crate) guest_arch: String,
    pub(crate) isolation_boundary: IsolationBoundary,
    /// VM scope — `per_session_vm` (Apple Containerization) or
    /// `shared_machine` (Podman). Drives cleanup semantics: a shared-machine
    /// container outlives the wrapper process and must be explicitly removed.
    pub(crate) substrate_scope: SubstrateScope,
    pub(crate) ready_state_kind: ReadyStateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) accelerator: Option<String>,
    /// Runtime binding injection after restore/start. `false` in M0.
    pub(crate) supports_bindings: bool,
    /// Warm Ready-State restore on this backend. `false` in M0 (cold only).
    pub(crate) supports_ready_state_restore: bool,
    /// In-VM CRIU checkpoint. `false` in M0 (#839 track).
    pub(crate) supports_criu_checkpoint: bool,
    /// Read-only shared rootfs reuse. `false` in M0.
    pub(crate) supports_readonly_shared_rootfs: bool,
    pub(crate) maturity: Maturity,
}

/// The Desktop Runner provider facts for this host — the structured capability
/// report (and receipt) a local Desktop Runner advertises.
///
/// `RunnerProviderFacts` in the design; named `DesktopRunnerFacts` because every
/// instance is `provider_kind == "desktop"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DesktopRunnerFacts {
    /// Always [`PROVIDER_KIND_DESKTOP`].
    pub(crate) provider_kind: String,
    /// `"macos"` | `"linux"` | `"windows"` (from `std::env::consts::OS`).
    pub(crate) host_os: String,
    /// `"aarch64"` | `"x86_64"`.
    pub(crate) host_arch: String,
    /// Host platform version, e.g. macOS `sw_vers -productVersion` → `"26.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) host_platform_version: Option<String>,
    /// Version of the Desktop Runner runtime (the `ato` CLI driving it).
    pub(crate) desktop_runtime_version: String,
    /// Whether hardware virtualization is available for VM-backed substrates.
    pub(crate) virtualization_available: bool,
    pub(crate) substrates: Vec<SubstrateCapability>,
    pub(crate) backends: Vec<BackendCapability>,
    /// Actionable diagnostics (e.g. "container not installed"). Carried but
    /// surfaced **only** when the user selects local Desktop Runner execution —
    /// never raised during normal Desktop startup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<String>,
    /// Structured reasons the host cannot serve a local cold-OCI backend (empty
    /// when a backend is available). The machine-readable counterpart to
    /// `diagnostics`; surfaced in placement decisions so the CLI can tell users
    /// exactly what to fix instead of a generic "no local backend" message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) local_backend_blockers: Vec<LocalBackendBlocker>,
}

impl DesktopRunnerFacts {
    /// True when this host advertises at least one backend on the named substrate.
    ///
    /// Test-only today (assertions across simulated hosts); the M2 placement
    /// path will use it from non-test code and drop the `cfg`.
    #[cfg(test)]
    pub(crate) fn has_substrate_backend(&self, substrate: &str) -> bool {
        self.backends.iter().any(|b| b.substrate == substrate)
    }

    /// The first backend whose host facets match this host (the local backend a
    /// placement would target), if any. Unopinionated about substrate preference
    /// — returns whichever backend was inserted first.
    pub(crate) fn local_backend(&self) -> Option<&BackendCapability> {
        self.backends
            .iter()
            .find(|b| b.host_os == self.host_os && b.host_arch == self.host_arch)
    }

    /// The preferred local cold-OCI backend, honoring the substrate preference
    /// order: Apple Containerization (lighter-weight, per-session VM) first,
    /// then Podman (broader host coverage, shared machine). Returns `None` when
    /// no cold-OCI backend is available on this host. Used by
    /// `matching::cold_or_managed` so the placement gate picks the best
    /// substrate, not just the first-inserted one.
    pub(crate) fn preferred_local_cold_backend(&self) -> Option<&BackendCapability> {
        let candidates: Vec<_> = self
            .backends
            .iter()
            .filter(|b| {
                b.host_os == self.host_os
                    && b.host_arch == self.host_arch
                    && b.ready_state_kind == ReadyStateKind::ColdOci
            })
            .collect();
        // Preference order: apple_containerization before podman.
        candidates
            .iter()
            .copied()
            .find(|b| b.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
            .or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|b| b.substrate == SUBSTRATE_PODMAN)
            })
            .or_else(|| candidates.first().copied())
    }

    /// Render the facts as a stable, pretty JSON receipt.
    ///
    /// Test-only today: the live `ato doctor desktop-runner --json` emits the
    /// richer `{facts, placement}` report, and the smoke's skip path dumps facts.
    /// The M3 run path will log facts from non-test code and drop the `cfg`.
    #[cfg(test)]
    pub(crate) fn to_receipt_json(&self) -> String {
        // DesktopRunnerFacts is always serializable (no NaN floats / non-string
        // keys), so this never fails; fall back to a debug string if it ever does.
        serde_json::to_string_pretty(self).unwrap_or_else(|_| format!("{self:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&IsolationBoundary::VmWrappedContainer).unwrap(),
            "\"vm_wrapped_container\""
        );
        assert_eq!(
            serde_json::to_string(&ReadyStateKind::ColdOci).unwrap(),
            "\"cold_oci\""
        );
        assert_eq!(
            serde_json::to_string(&Maturity::Experimental).unwrap(),
            "\"experimental\""
        );
    }

    #[test]
    fn receipt_roundtrips() {
        let facts = DesktopRunnerFacts {
            provider_kind: PROVIDER_KIND_DESKTOP.into(),
            host_os: "macos".into(),
            host_arch: "aarch64".into(),
            host_platform_version: Some("26.0".into()),
            desktop_runtime_version: "0.7.0".into(),
            virtualization_available: true,
            substrates: vec![],
            backends: vec![],
            diagnostics: vec![],
            local_backend_blockers: vec![],
        };
        let json = facts.to_receipt_json();
        let back: DesktopRunnerFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(facts, back);
    }
}
