//! macOS Desktop Runner probe (#838, M0): detect Apple Containerization /
//! Apple `container` and report a cold-OCI [`BackendCapability`] when — and only
//! when — the host can honestly serve it (Apple silicon + macOS 26+ + `container`
//! installed). Probing is read-only: it never starts the `container` system
//! service and never runs a workload.
//!
//! The probe is split into [`MacosProbeInputs`] (raw host facts gathered by
//! running `sw_vers`/`uname`/`sysctl`/`container`) and [`build_macos_facts`]
//! (a pure function of those inputs), so the capability-gating logic is unit
//! tested across simulated hosts without a real macOS 26 machine.

use super::facts::{
    ACCELERATOR_APPLE_VZ, BackendCapability, DesktopRunnerFacts, IsolationBoundary,
    LocalBackendBlocker, Maturity, PROVIDER_KIND_DESKTOP, ReadyStateKind,
    SUBSTRATE_APPLE_CONTAINERIZATION, SUBSTRATE_PODMAN, SubstrateCapability, SubstrateScope,
};

/// Apple `container` requires macOS 26 or newer. Below this the substrate is
/// reported unavailable with a diagnostic, never advertised as a backend.
const MIN_MACOS_MAJOR_FOR_CONTAINER: u32 = 26;

/// Raw, host-gathered inputs for the macOS probe. Separated from
/// [`build_macos_facts`] so the gating logic is testable without a Mac.
#[derive(Debug, Clone, Default)]
pub(crate) struct MacosProbeInputs {
    /// Normalized host arch (`"aarch64"` | `"x86_64"`).
    pub(crate) host_arch: String,
    /// `sw_vers -productVersion`, e.g. `"26.0"`.
    pub(crate) product_version: Option<String>,
    /// `sysctl -n hw.optional.arm64 == "1"` — true silicon, Rosetta-proof.
    pub(crate) is_apple_silicon: bool,
    /// Resolved path to the `container` tool, if installed.
    pub(crate) container_path: Option<String>,
    /// `container --version` string, if probed.
    pub(crate) container_version: Option<String>,
    /// Whether `container system status` reports the service already running.
    /// **Detected, never started.**
    pub(crate) container_service_running: bool,
    // ── Podman substrate inputs ────────────────────────────────────────────
    /// Whether the `podman` binary was resolved (via
    /// `capsule::foundation::podman::resolve_podman`). `false` when podman is
    /// not installed / not on PATH / not in a known location.
    pub(crate) podman_binary_present: bool,
    /// Resolved podman version string (`podman --version` first line), if probed.
    pub(crate) podman_version: Option<String>,
    /// The `ato-podman` machine state, derived from `podman machine list`.
    pub(crate) podman_machine: PodmanMachineProbe,
}

/// The `ato-podman` machine state as seen by the Desktop Runner probe. Mirrors
/// the subset of [`crate::adapters::runtime::podman_machine::PodmanMachineStatus`]
/// the Desktop Runner cares about, kept as a separate type so the Desktop Runner
/// module stays decoupled from the OCI provider's readiness state machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum PodmanMachineProbe {
    /// `podman machine list` has not been run / not available (default).
    #[default]
    NotProbed,
    /// The `ato-podman` machine is present and running.
    AtoPodmanRunning,
    /// One or more machines are configured but `ato-podman` is not running.
    AtoPodmanStopped,
    /// No `ato-podman` machine is configured.
    NotConfigured,
    /// `podman machine list` could not run / output unparseable / permission
    /// error. `reason` is a short, safe summary.
    Unavailable { reason: String },
}

/// Parse the leading major version from a `sw_vers -productVersion` string:
/// `"26.0"` → `Some(26)`, `"15.5"` → `Some(15)`. Non-numeric → `None`.
fn parse_macos_major(version: &str) -> Option<u32> {
    version
        .split('.')
        .next()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
}

/// Build the Desktop Runner facts from gathered macOS inputs (pure).
///
/// Advertises **two** cold-OCI substrates:
/// - Apple Containerization: available when Apple silicon + macOS ≥ 26 +
///   `container` installed.
/// - Podman: available when the `podman` binary is resolved AND the
///   `ato-podman` machine is running.
///
/// Each unavailable substrate contributes structured `LocalBackendBlocker`s.
/// **Rendering policy:** if any substrate is available, the unavailable
/// substrates' blockers are suppressed (a backend IS available, so placement
/// succeeds with no blockers). Blockers appear only when no backend is
/// available.
pub(crate) fn build_macos_facts(
    inputs: &MacosProbeInputs,
    runtime_version: &str,
) -> DesktopRunnerFacts {
    let major = inputs
        .product_version
        .as_deref()
        .and_then(parse_macos_major);
    let macos_supported = major.is_some_and(|m| m >= MIN_MACOS_MAJOR_FOR_CONTAINER);
    let container_present = inputs.container_path.is_some();
    let apple_available = inputs.is_apple_silicon && macos_supported && container_present;

    let podman_available = inputs.podman_binary_present
        && matches!(inputs.podman_machine, PodmanMachineProbe::AtoPodmanRunning);

    // ── Apple Containerization blockers (only if Apple substrate unavailable) ──
    let mut apple_blockers = Vec::new();
    let mut apple_diagnostics = Vec::new();
    if !apple_available {
        if !inputs.is_apple_silicon {
            apple_blockers.push(LocalBackendBlocker::NotAppleSilicon);
            apple_diagnostics.push(
                "Apple Containerization requires Apple silicon; this Mac is Intel. \
                 Use a managed runner for local-equivalent execution."
                    .to_string(),
            );
        }
        if !macos_supported {
            let have = inputs
                .product_version
                .clone()
                .unwrap_or_else(|| "unknown".into());
            apple_blockers.push(LocalBackendBlocker::MacOsTooOld {
                found: inputs.product_version.clone(),
                required: MIN_MACOS_MAJOR_FOR_CONTAINER,
            });
            apple_diagnostics.push(format!(
                "Apple Containerization requires macOS {MIN_MACOS_MAJOR_FOR_CONTAINER}+ \
                 (found macOS {have}). Upgrade macOS or use a managed runner."
            ));
        }
        if !container_present {
            apple_blockers.push(LocalBackendBlocker::AppleContainerMissing);
            apple_diagnostics.push(
                "Apple `container` is not installed. Install it from \
                 https://github.com/apple/container, or use a managed runner."
                    .to_string(),
            );
        }
    }

    // ── Podman blockers (only if Podman substrate unavailable) ───────────────
    let mut podman_blockers = Vec::new();
    let mut podman_diagnostics = Vec::new();
    if !podman_available {
        if !inputs.podman_binary_present {
            podman_blockers.push(LocalBackendBlocker::PodmanBinaryMissing);
            podman_diagnostics.push(
                "Podman is not installed. Install Podman (or run `ato runtime setup`), or use \
                 a managed runner."
                    .to_string(),
            );
        } else {
            match &inputs.podman_machine {
                PodmanMachineProbe::AtoPodmanRunning => { /* available — no blocker */ }
                PodmanMachineProbe::AtoPodmanStopped => {
                    podman_blockers.push(LocalBackendBlocker::PodmanMachineStopped);
                    podman_diagnostics.push(
                        "The `ato-podman` machine is stopped. Start it with `podman machine \
                         start ato-podman`, or use a managed runner."
                            .to_string(),
                    );
                }
                PodmanMachineProbe::NotConfigured => {
                    podman_blockers.push(LocalBackendBlocker::PodmanMachineNotConfigured);
                    podman_diagnostics.push(
                        "No `ato-podman` machine is configured. Create one with `ato runtime \
                         setup`, or use a managed runner."
                            .to_string(),
                    );
                }
                PodmanMachineProbe::Unavailable { reason } => {
                    podman_blockers.push(LocalBackendBlocker::PodmanMachineStatusUnavailable {
                        reason: reason.clone(),
                    });
                    podman_diagnostics.push(format!(
                        "Could not query the Podman machine state ({reason}). Check your \
                         Podman installation, or use a managed runner."
                    ));
                }
                PodmanMachineProbe::NotProbed => {
                    // The probe did not run (e.g. test inputs); no blocker —
                    // the binary-missing blocker above covers the common case.
                }
            }
        }
    }

    // ── Rendering policy: suppress blockers AND diagnostics when any backend
    //    is available. A host with one available substrate shows no blockers
    //    in the placement failure path (placement succeeds). Doctor may still
    //    show per-substrate details, but the facts-level diagnostics stay clean
    //    so a successful host reads as clean. ─────────────────────────────────
    let any_available = apple_available || podman_available;
    let (blockers, diagnostics) = if any_available {
        (Vec::new(), Vec::new())
    } else {
        let mut all_blockers = apple_blockers;
        all_blockers.extend(podman_blockers);
        let mut all_diagnostics = apple_diagnostics;
        all_diagnostics.extend(podman_diagnostics);
        (all_blockers, all_diagnostics)
    };

    // ── Substrate capabilities ───────────────────────────────────────────────
    let apple_substrate = SubstrateCapability {
        substrate: SUBSTRATE_APPLE_CONTAINERIZATION.into(),
        available: apple_available,
        tool: container_present.then(|| "container".to_string()),
        tool_path: inputs.container_path.clone(),
        tool_version: inputs.container_version.clone(),
        system_service_running: inputs.container_service_running,
        accelerator: apple_available.then(|| ACCELERATOR_APPLE_VZ.to_string()),
        substrate_scope: SubstrateScope::PerSessionVm,
        maturity: Maturity::Experimental,
    };
    let podman_substrate = SubstrateCapability {
        substrate: SUBSTRATE_PODMAN.into(),
        available: podman_available,
        tool: inputs.podman_binary_present.then(|| "podman".to_string()),
        tool_path: None, // podman binary path is not surfaced here to stay decoupled
        tool_version: inputs.podman_version.clone(),
        system_service_running: podman_available,
        accelerator: None,
        substrate_scope: SubstrateScope::SharedMachine,
        maturity: Maturity::Experimental,
    };

    // ── Backends ─────────────────────────────────────────────────────────────
    let mut backends = Vec::new();
    if apple_available {
        backends.push(BackendCapability {
            provider: PROVIDER_KIND_DESKTOP.into(),
            substrate: SUBSTRATE_APPLE_CONTAINERIZATION.into(),
            host_os: "macos".into(),
            host_arch: inputs.host_arch.clone(),
            guest_os: "linux".into(),
            guest_arch: inputs.host_arch.clone(),
            isolation_boundary: IsolationBoundary::VmWrappedContainer,
            substrate_scope: SubstrateScope::PerSessionVm,
            ready_state_kind: ReadyStateKind::ColdOci,
            accelerator: Some(ACCELERATOR_APPLE_VZ.into()),
            supports_bindings: false,
            supports_ready_state_restore: false,
            supports_criu_checkpoint: false,
            supports_readonly_shared_rootfs: false,
            maturity: Maturity::Experimental,
        });
    }
    if podman_available {
        backends.push(BackendCapability {
            provider: PROVIDER_KIND_DESKTOP.into(),
            substrate: SUBSTRATE_PODMAN.into(),
            host_os: "macos".into(),
            host_arch: inputs.host_arch.clone(),
            guest_os: "linux".into(),
            // No cross-arch / Rosetta in M0: guest matches host exactly.
            guest_arch: inputs.host_arch.clone(),
            isolation_boundary: IsolationBoundary::VmWrappedContainer,
            substrate_scope: SubstrateScope::SharedMachine,
            ready_state_kind: ReadyStateKind::ColdOci,
            accelerator: None,
            supports_bindings: false,
            supports_ready_state_restore: false,
            supports_criu_checkpoint: false,
            supports_readonly_shared_rootfs: false,
            maturity: Maturity::Experimental,
        });
    }

    DesktopRunnerFacts {
        provider_kind: PROVIDER_KIND_DESKTOP.into(),
        host_os: "macos".into(),
        host_arch: inputs.host_arch.clone(),
        host_platform_version: inputs.product_version.clone(),
        desktop_runtime_version: runtime_version.to_string(),
        // VM-backed substrates need Apple VZ, which requires Apple silicon here.
        // Podman's VM is its own; virtualization_available tracks Apple VZ only.
        virtualization_available: inputs.is_apple_silicon,
        substrates: vec![apple_substrate, podman_substrate],
        backends,
        diagnostics,
        local_backend_blockers: blockers,
    }
}

/// Probe the live macOS host and build its Desktop Runner facts.
pub(crate) fn probe(runtime_version: &str) -> DesktopRunnerFacts {
    build_macos_facts(&gather_inputs(), runtime_version)
}

/// Run the read-only host probes. Best-effort and total: every probe falls back
/// to a conservative default (absent/false), so this never fails and never has
/// side effects (no `container system start`).
fn gather_inputs() -> MacosProbeInputs {
    let is_apple_silicon = sysctl_flag("hw.optional.arm64");
    let raw_uname = run_capture("uname", &["-m"]);
    let host_arch = normalize_arch(raw_uname.as_deref(), is_apple_silicon);

    let container_path = locate_container();
    let (container_version, container_service_running) = if container_path.is_some() {
        (
            run_capture("container", &["--version"]),
            // `container system status` is read-only; success ⇒ already running.
            // We never call `container system start`.
            command_succeeds("container", &["system", "status"]),
        )
    } else {
        (None, false)
    };

    // ── Podman probe (read-only, side-effect free) ───────────────────────────
    // Reuses the shared binary resolver (capsule::foundation::podman) and the
    // shared machine-list parser (adapters::runtime::podman_machine) so the
    // Desktop Runner never diverges from the OCI provider's detection. The
    // probe NEVER starts the `ato-podman` machine.
    let (podman_binary_present, podman_version, podman_machine) = probe_podman();

    MacosProbeInputs {
        host_arch,
        product_version: run_capture("sw_vers", &["-productVersion"]),
        is_apple_silicon,
        container_path,
        container_version,
        container_service_running,
        podman_binary_present,
        podman_version,
        podman_machine,
    }
}

/// Read-only Podman probe: resolve the binary, read its version, and query the
/// `ato-podman` machine state via `podman machine list --format json`. Never
/// starts the machine. Returns `(binary_present, version, machine_state)`.
fn probe_podman() -> (bool, Option<String>, PodmanMachineProbe) {
    use crate::adapters::runtime::podman_machine::{
        PodmanMachineStatus, parse_podman_machine_list,
    };
    use capsule::foundation::podman::{ATO_PODMAN_MACHINE_NAME, resolve_podman};

    // Binary resolution — reuses the same resolver as the OCI provider.
    let resolved = match resolve_podman() {
        Ok(mut r) => {
            let version = r.query_version().map(str::to_string);
            (true, version)
        }
        Err(_) => (false, None),
    };
    let (binary_present, version) = resolved;

    if !binary_present {
        return (false, None, PodmanMachineProbe::NotProbed);
    }

    // `podman machine list --format json` — read-only, never starts anything.
    let machine_state = match run_capture("podman", &["machine", "list", "--format", "json"]) {
        Some(stdout) => match parse_podman_machine_list(&stdout) {
            PodmanMachineStatus::Running {
                running_names,
                all_names,
            } => {
                if running_names.iter().any(|n| n == ATO_PODMAN_MACHINE_NAME) {
                    PodmanMachineProbe::AtoPodmanRunning
                } else if all_names.iter().any(|n| n == ATO_PODMAN_MACHINE_NAME) {
                    // ato-podman exists but is not running
                    PodmanMachineProbe::AtoPodmanStopped
                } else {
                    // Other machines running but not ato-podman
                    PodmanMachineProbe::NotConfigured
                }
            }
            PodmanMachineStatus::Stopped { names } => {
                if names.iter().any(|n| n == ATO_PODMAN_MACHINE_NAME) {
                    PodmanMachineProbe::AtoPodmanStopped
                } else {
                    PodmanMachineProbe::NotConfigured
                }
            }
            PodmanMachineStatus::NotConfigured => PodmanMachineProbe::NotConfigured,
            PodmanMachineStatus::Unavailable { reason } => {
                PodmanMachineProbe::Unavailable { reason }
            }
            PodmanMachineStatus::Unknown { reason } => PodmanMachineProbe::Unavailable { reason },
        },
        None => PodmanMachineProbe::Unavailable {
            reason: "podman machine list did not produce output".to_string(),
        },
    };

    (true, version, machine_state)
}

/// Normalize `uname -m` to the runner-class arch vocabulary. On Apple silicon
/// the host is `aarch64` even if the probing process runs under Rosetta.
fn normalize_arch(uname_m: Option<&str>, is_apple_silicon: bool) -> String {
    if is_apple_silicon {
        return "aarch64".to_string();
    }
    match uname_m.map(str::trim) {
        Some("arm64") | Some("aarch64") => "aarch64".to_string(),
        Some("x86_64") => "x86_64".to_string(),
        Some(other) if !other.is_empty() => other.to_string(),
        // Fall back to the build target arch when uname is unavailable.
        _ => std::env::consts::ARCH.to_string(),
    }
}

/// Locate the Apple `container` tool: PATH first, then its default install path.
fn locate_container() -> Option<String> {
    if crate::application::runner_agent::binary_on_path("container") {
        return Some("container".to_string());
    }
    let default = "/usr/local/bin/container";
    std::path::Path::new(default)
        .is_file()
        .then(|| default.to_string())
}

/// `sysctl -n <name>` parsed as a `"1"` boolean flag. Any error ⇒ false.
fn sysctl_flag(name: &str) -> bool {
    run_capture("sysctl", &["-n", name])
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// Run a command and capture trimmed stdout, or `None` if it cannot run / fails.
fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// True only if the command runs and exits 0. Used for read-only status checks.
fn command_succeeds(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_inputs() -> MacosProbeInputs {
        MacosProbeInputs {
            host_arch: "aarch64".into(),
            product_version: Some("26.0".into()),
            is_apple_silicon: true,
            container_path: Some("/usr/local/bin/container".into()),
            container_version: Some("container 0.1.0".into()),
            container_service_running: false,
            podman_binary_present: false,
            podman_version: None,
            podman_machine: PodmanMachineProbe::default(),
        }
    }

    /// Helper: supported inputs with Podman also running (ato-podman machine up).
    fn supported_inputs_with_podman_running() -> MacosProbeInputs {
        MacosProbeInputs {
            podman_binary_present: true,
            podman_version: Some("podman 5.2.3".into()),
            podman_machine: PodmanMachineProbe::AtoPodmanRunning,
            ..supported_inputs()
        }
    }

    #[test]
    fn parse_macos_major_extracts_major() {
        assert_eq!(parse_macos_major("26.0"), Some(26));
        assert_eq!(parse_macos_major("15.5"), Some(15));
        assert_eq!(parse_macos_major("26"), Some(26));
        assert_eq!(parse_macos_major(""), None);
        assert_eq!(parse_macos_major("beta"), None);
    }

    #[test]
    fn normalize_arch_is_aarch64_on_apple_silicon_even_under_rosetta() {
        // Rosetta makes uname report x86_64, but the host is still aarch64.
        assert_eq!(normalize_arch(Some("x86_64"), true), "aarch64");
        assert_eq!(normalize_arch(Some("arm64"), false), "aarch64");
        assert_eq!(normalize_arch(Some("x86_64"), false), "x86_64");
    }

    #[test]
    fn apple_silicon_macos26_with_container_yields_cold_oci_capability() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        assert!(facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.virtualization_available);
        let b = facts.local_backend().expect("a local backend");
        assert_eq!(b.substrate, SUBSTRATE_APPLE_CONTAINERIZATION);
        assert_eq!(b.host_arch, "aarch64");
        assert_eq!(b.guest_os, "linux");
        assert_eq!(b.guest_arch, "aarch64");
        assert_eq!(b.isolation_boundary, IsolationBoundary::VmWrappedContainer);
        assert_eq!(b.ready_state_kind, ReadyStateKind::ColdOci);
        assert_eq!(b.accelerator.as_deref(), Some(ACCELERATOR_APPLE_VZ));
        // M0 honesty: nothing richer than cold OCI is advertised.
        assert!(!b.supports_bindings);
        assert!(!b.supports_ready_state_restore);
        assert!(!b.supports_criu_checkpoint);
        assert!(!b.supports_readonly_shared_rootfs);
        assert!(facts.diagnostics.is_empty());
    }

    #[test]
    fn container_missing_yields_no_capability_but_a_diagnostic() {
        let mut inputs = supported_inputs();
        inputs.container_path = None;
        inputs.container_version = None;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.backends.is_empty());
        assert!(!facts.substrates[0].available);
        assert!(
            facts.diagnostics.iter().any(|d| d.contains("container")),
            "diagnostic should name the missing tool: {:?}",
            facts.diagnostics
        );
    }

    #[test]
    fn intel_mac_has_no_apple_silicon_container_capability() {
        let mut inputs = supported_inputs();
        inputs.is_apple_silicon = false;
        inputs.host_arch = "x86_64".into();
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(!facts.virtualization_available);
        assert!(
            facts
                .diagnostics
                .iter()
                .any(|d| d.contains("Apple silicon")),
            "{:?}",
            facts.diagnostics
        );
    }

    #[test]
    fn old_macos_yields_no_capability_with_version_diagnostic() {
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(
            facts
                .diagnostics
                .iter()
                .any(|d| d.contains("macOS 26") && d.contains("15.5")),
            "{:?}",
            facts.diagnostics
        );
    }

    #[test]
    fn service_running_flag_is_carried_without_starting_it() {
        let mut inputs = supported_inputs();
        inputs.container_service_running = true;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(facts.substrates[0].system_service_running);
    }

    // ── Podman substrate tests ───────────────────────────────────────────────

    #[test]
    fn podman_running_with_apple_available_advertises_both_and_prefers_apple() {
        let facts = build_macos_facts(&supported_inputs_with_podman_running(), "0.7.0");
        assert!(facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.has_substrate_backend(SUBSTRATE_PODMAN));
        // Preferred backend is Apple Containerization.
        let b = facts
            .preferred_local_cold_backend()
            .expect("a preferred backend");
        assert_eq!(b.substrate, SUBSTRATE_APPLE_CONTAINERIZATION);
        // Both available → no blockers, no diagnostics.
        assert!(
            facts.local_backend_blockers.is_empty(),
            "{:?}",
            facts.local_backend_blockers
        );
        assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
    }

    #[test]
    fn podman_running_macos15_no_container_advertises_podman_backend_no_blockers() {
        // The win: macOS 15 + no Apple container + Podman running → Podman backend,
        // no blockers (a backend IS available).
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        inputs.container_path = None;
        inputs.container_version = None;
        inputs.container_service_running = false;
        inputs.podman_binary_present = true;
        inputs.podman_version = Some("podman 5.2.3".into());
        inputs.podman_machine = PodmanMachineProbe::AtoPodmanRunning;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.has_substrate_backend(SUBSTRATE_PODMAN));
        let b = facts
            .preferred_local_cold_backend()
            .expect("Podman backend");
        assert_eq!(b.substrate, SUBSTRATE_PODMAN);
        assert_eq!(b.substrate_scope, SubstrateScope::SharedMachine);
        assert!(
            facts.local_backend_blockers.is_empty(),
            "available backend → no blockers"
        );
        assert!(
            facts.diagnostics.is_empty(),
            "available backend → no diagnostics"
        );
    }

    #[test]
    fn podman_stopped_macos15_no_container_has_both_substrate_blockers() {
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        inputs.container_path = None;
        inputs.container_version = None;
        inputs.container_service_running = false;
        inputs.podman_binary_present = true;
        inputs.podman_version = Some("podman 5.2.3".into());
        inputs.podman_machine = PodmanMachineProbe::AtoPodmanStopped;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(facts.backends.is_empty());
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "macos_too_old"),
            "should have Apple blocker: {:?}",
            facts.local_backend_blockers
        );
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "apple_container_missing"),
            "should have Apple blocker: {:?}",
            facts.local_backend_blockers
        );
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "podman_machine_stopped"),
            "should have Podman blocker: {:?}",
            facts.local_backend_blockers
        );
    }

    #[test]
    fn podman_binary_missing_no_container_has_both_blockers() {
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        inputs.container_path = None;
        inputs.container_version = None;
        inputs.container_service_running = false;
        // podman_binary_present defaults to false, podman_machine defaults to NotProbed
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(facts.backends.is_empty());
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "podman_binary_missing"),
            "{:?}",
            facts.local_backend_blockers
        );
    }

    #[test]
    fn intel_mac_with_podman_running_advertises_podman_backend() {
        let mut inputs = supported_inputs();
        inputs.is_apple_silicon = false;
        inputs.host_arch = "x86_64".into();
        inputs.podman_binary_present = true;
        inputs.podman_version = Some("podman 5.2.3".into());
        inputs.podman_machine = PodmanMachineProbe::AtoPodmanRunning;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.has_substrate_backend(SUBSTRATE_PODMAN));
        let b = facts
            .preferred_local_cold_backend()
            .expect("Podman backend");
        assert_eq!(b.substrate, SUBSTRATE_PODMAN);
        assert_eq!(b.host_arch, "x86_64");
        assert!(facts.local_backend_blockers.is_empty());
    }

    #[test]
    fn podman_machine_unavailable_blocker_covers_parse_failure() {
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        inputs.container_path = None;
        inputs.container_version = None;
        inputs.podman_binary_present = true;
        inputs.podman_machine = PodmanMachineProbe::Unavailable {
            reason: "permission denied".into(),
        };
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(facts.backends.is_empty());
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "podman_machine_status_unavailable"),
            "{:?}",
            facts.local_backend_blockers
        );
    }

    #[test]
    fn podman_not_configured_blocker_when_no_ato_podman_machine() {
        let mut inputs = supported_inputs();
        inputs.product_version = Some("15.5".into());
        inputs.container_path = None;
        inputs.container_version = None;
        inputs.podman_binary_present = true;
        inputs.podman_machine = PodmanMachineProbe::NotConfigured;
        let facts = build_macos_facts(&inputs, "0.7.0");
        assert!(facts.backends.is_empty());
        assert!(
            facts
                .local_backend_blockers
                .iter()
                .any(|b| b.as_str() == "podman_machine_not_configured"),
            "{:?}",
            facts.local_backend_blockers
        );
    }

    #[test]
    fn substrate_scope_distinguishes_per_session_vm_and_shared_machine() {
        let facts = build_macos_facts(&supported_inputs_with_podman_running(), "0.7.0");
        let apple = facts
            .substrates
            .iter()
            .find(|s| s.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
            .unwrap();
        let podman = facts
            .substrates
            .iter()
            .find(|s| s.substrate == SUBSTRATE_PODMAN)
            .unwrap();
        assert_eq!(apple.substrate_scope, SubstrateScope::PerSessionVm);
        assert_eq!(podman.substrate_scope, SubstrateScope::SharedMachine);
    }
}
