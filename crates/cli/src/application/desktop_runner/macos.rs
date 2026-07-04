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
    SUBSTRATE_APPLE_CONTAINERIZATION, SubstrateCapability,
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
/// Advertises the Apple Containerization cold-OCI backend **only** when all
/// three hold: Apple silicon, macOS ≥ 26, and `container` installed. Otherwise
/// the substrate is reported `available: false` with an actionable diagnostic
/// and **no** backend is produced — a missing substrate must never silently
/// degrade into a usable capability.
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
    let available = inputs.is_apple_silicon && macos_supported && container_present;

    let mut diagnostics = Vec::new();
    let mut blockers = Vec::new();
    if !available {
        // One coarse, actionable line per missing precondition. Surfaced only
        // when the user selects local Desktop Runner execution (see mod.rs).
        // Each missing precondition is also recorded as a structured
        // `LocalBackendBlocker` so the placement decision / CLI error can name
        // every reason individually instead of a generic "no local backend".
        if !inputs.is_apple_silicon {
            blockers.push(LocalBackendBlocker::NotAppleSilicon);
            diagnostics.push(
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
            blockers.push(LocalBackendBlocker::MacOsTooOld {
                found: inputs.product_version.clone(),
                required: MIN_MACOS_MAJOR_FOR_CONTAINER,
            });
            diagnostics.push(format!(
                "Apple Containerization requires macOS {MIN_MACOS_MAJOR_FOR_CONTAINER}+ \
                 (found macOS {have}). Upgrade macOS or use a managed runner."
            ));
        }
        if !container_present {
            blockers.push(LocalBackendBlocker::AppleContainerMissing);
            diagnostics.push(
                "Apple `container` is not installed. Install it from \
                 https://github.com/apple/container, or use a managed runner."
                    .to_string(),
            );
        }
    }

    let substrate = SubstrateCapability {
        substrate: SUBSTRATE_APPLE_CONTAINERIZATION.into(),
        available,
        tool: container_present.then(|| "container".to_string()),
        tool_path: inputs.container_path.clone(),
        tool_version: inputs.container_version.clone(),
        system_service_running: inputs.container_service_running,
        accelerator: available.then(|| ACCELERATOR_APPLE_VZ.to_string()),
        maturity: Maturity::Experimental,
    };

    let backends = if available {
        vec![BackendCapability {
            provider: PROVIDER_KIND_DESKTOP.into(),
            substrate: SUBSTRATE_APPLE_CONTAINERIZATION.into(),
            host_os: "macos".into(),
            host_arch: inputs.host_arch.clone(),
            guest_os: "linux".into(),
            // No cross-arch / Rosetta in M0: guest matches host exactly.
            guest_arch: inputs.host_arch.clone(),
            isolation_boundary: IsolationBoundary::VmWrappedContainer,
            ready_state_kind: ReadyStateKind::ColdOci,
            accelerator: Some(ACCELERATOR_APPLE_VZ.into()),
            // M0: cold OCI only. Every richer mechanism stays off until built.
            supports_bindings: false,
            supports_ready_state_restore: false,
            supports_criu_checkpoint: false,
            supports_readonly_shared_rootfs: false,
            maturity: Maturity::Experimental,
        }]
    } else {
        Vec::new()
    };

    DesktopRunnerFacts {
        provider_kind: PROVIDER_KIND_DESKTOP.into(),
        host_os: "macos".into(),
        host_arch: inputs.host_arch.clone(),
        host_platform_version: inputs.product_version.clone(),
        desktop_runtime_version: runtime_version.to_string(),
        // VM-backed substrates need Apple VZ, which requires Apple silicon here.
        virtualization_available: inputs.is_apple_silicon,
        substrates: vec![substrate],
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

    MacosProbeInputs {
        host_arch,
        product_version: run_capture("sw_vers", &["-productVersion"]),
        is_apple_silicon,
        container_path,
        container_version,
        container_service_running,
    }
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
}
