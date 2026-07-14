//! Host runtime-setup detection & managed install (issue #420 revision).
//!
//! This replaces the earlier "host device detection" / GPU-scan path. Nothing
//! here scans CPU/GPU/hardware capabilities — it reports whether the *runtime
//! tools* a recipe needs are installed and usable, and (for the Ato-managed
//! language runtimes only) installs them into the Ato toolchain cache.
//!
//! Two surfaces back this module:
//! - `ato internal runtime setup-status --json` → [`collect_setup_status`]
//! - `ato internal runtime install --tools … --json` → [`install_tools`]
//!
//! Policy (locked by the issue #420 revision decision):
//! - Node / uv / Python → Ato-managed install preferred. Host PATH copies are
//!   reported for context but readiness is keyed on the managed toolchain cache.
//! - Podman / Docker Desktop → detection only. We never auto-install a
//!   container engine; missing/unsupported surfaces install *instructions*.
//! - `ato_helper` / `nacelle` → bundled with the desktop; missing is a bundle
//!   integrity error, never a download.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use capsule::packers::runtime_fetcher::{RuntimeFetcher, locate_runtime_binary};
use capsule::reporter::CapsuleReporter;
use capsule::runtime_setup::{
    InstallPhase, InstallProgress, RecommendedAction, RuntimeSetupStatus, SUPPORTED_NODE_VERSION,
    SUPPORTED_PYTHON_VERSION, SUPPORTED_UV_VERSION, ToolKind, ToolSource, ToolStatus,
    VirtualizationStatus, WindowsSubstrateAction, WindowsSubstrateActionKind,
    WindowsSubstrateStatus, WslStatus,
};

use capsule::podman::ATO_PODMAN_MACHINE_NAME;

use crate::adapters::runtime::podman_machine::{PodmanMachineStatus, parse_podman_machine_list};

/// Collect the full host runtime-setup status by probing each tool. Probes are
/// independent and best-effort: a failure to read one tool degrades that tool's
/// row, never the whole report.
pub(crate) fn collect_setup_status() -> RuntimeSetupStatus {
    RuntimeSetupStatus {
        tools: vec![
            detect_podman(),
            detect_docker_desktop(),
            detect_managed_language_tool(ToolKind::Node, &["node", "node.exe"]),
            detect_managed_language_tool(ToolKind::Uv, &["uv", "uv.exe"]),
            detect_managed_language_tool(ToolKind::Python, &["python3", "python"]),
            detect_ato_helper(),
            detect_nacelle(),
        ],
        windows_substrate: detect_windows_substrate(),
    }
}

/// Probe Windows WSL / virtualization substrate state for the local OCI engine
/// (#460). Returns `None` on non-Windows hosts. Read-only: runs `wsl.exe
/// --status` / `wsl.exe --list --verbose` and never mutates host state.
fn detect_windows_substrate() -> Option<WindowsSubstrateStatus> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    // `wsl.exe` emits UTF-16LE; capture raw bytes and decode lossily after
    // stripping interior NULs so the pure classifier sees plain text.
    let probe = |args: &[&str]| -> Option<String> {
        let out = Command::new("wsl.exe").args(args).output().ok()?;
        let mut bytes = out.stdout;
        bytes.extend_from_slice(&out.stderr);
        Some(decode_wsl_output(&bytes))
    };
    let invocable = probe(&["--status"]).is_some();
    let status_out = probe(&["--status"]);
    let list_out = probe(&["--list", "--verbose"]);
    Some(classify_windows_substrate(
        invocable,
        status_out.as_deref(),
        list_out.as_deref(),
    ))
}

/// Decode `wsl.exe` output, which is UTF-16LE on Windows. Drops NUL bytes so a
/// naive UTF-8 lossy decode of UTF-16LE ASCII text reads cleanly.
pub(crate) fn decode_wsl_output(bytes: &[u8]) -> String {
    if bytes.iter().filter(|b| **b == 0).count() * 2 >= bytes.len() && !bytes.is_empty() {
        // Looks like UTF-16LE (roughly half the bytes are NUL): decode as such.
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Pure classifier for the Windows substrate from `wsl.exe` probe output.
///
/// * `invocable` — whether `wsl.exe --status` could be executed at all.
/// * `status_out` — `wsl --status` text (default version / distribution).
/// * `list_out` — `wsl --list --verbose` text (per-distro VERSION column).
fn classify_windows_substrate(
    invocable: bool,
    status_out: Option<&str>,
    list_out: Option<&str>,
) -> WindowsSubstrateStatus {
    let combined = format!(
        "{}\n{}",
        status_out.unwrap_or_default(),
        list_out.unwrap_or_default()
    )
    .to_ascii_lowercase();

    let reboot_required = combined.contains("restart") || combined.contains("reboot");

    let virtualization = if (combined.contains("virtual machine platform")
        || combined.contains("virtualization")
        || combined.contains("hyper-v"))
        && (combined.contains("disable")
            || combined.contains("not enabled")
            || combined.contains("enable")
            || combined.contains("bios")
            || combined.contains("firmware"))
    {
        VirtualizationStatus::UnavailableOrUnknown
    } else {
        VirtualizationStatus::Unknown
    };

    // "no installed distributions" means WSL *is* installed but has no distro —
    // that is Wsl2Unavailable, not Missing — so it is intentionally excluded here.
    let not_installed = combined.contains("not installed")
        || combined.contains("is not installed")
        || combined.contains("wsl --install")
        || combined.contains("/install");

    // A distro running on WSL2: the verbose list has a VERSION column whose
    // value is 2, or `--status` reports default version 2 with a distro present.
    let has_v2_distro = list_out.is_some_and(list_reports_version_2);
    let default_version_2 = combined.contains("default version: 2");

    let wsl = if !invocable {
        WslStatus::Missing
    } else if reboot_required {
        WslStatus::RebootRequired
    } else if has_v2_distro
        || (default_version_2 && !combined.contains("no installed distributions"))
    {
        WslStatus::Ready
    } else if not_installed {
        WslStatus::Missing
    } else if combined.contains("no installed distributions")
        || combined.contains("default version: 1")
        || list_reports_only_version_1(list_out)
    {
        WslStatus::Wsl2Unavailable
    } else {
        WslStatus::Unknown
    };

    let message = match wsl {
        WslStatus::Missing => {
            "WSL is not installed. Ato can guide installing it to run local containers.".to_string()
        }
        WslStatus::Wsl2Unavailable => {
            "WSL is present but no WSL2 distribution is available; WSL2 is required.".to_string()
        }
        WslStatus::RebootRequired => {
            "WSL setup needs a restart to finish before containers can run.".to_string()
        }
        WslStatus::Ready => "WSL2 is available.".to_string(),
        WslStatus::Unknown => "WSL state could not be determined.".to_string(),
        WslStatus::NotApplicable => "Not applicable on this host.".to_string(),
    };

    let action = WindowsSubstrateAction::for_kind(substrate_action_kind(wsl, virtualization));

    WindowsSubstrateStatus {
        wsl,
        virtualization,
        reboot_required,
        message,
        action,
    }
}

/// The single substrate remediation to offer for a (`wsl`, `virtualization`)
/// pair (#460). Podman *machine* health-error is intentionally **not** handled
/// here — that stays on the Podman [`ToolStatus`] action; this owns only the
/// WSL / virtualization / reboot substrate beneath Podman.
fn substrate_action_kind(
    wsl: WslStatus,
    virtualization: VirtualizationStatus,
) -> WindowsSubstrateActionKind {
    use WindowsSubstrateActionKind as K;
    match wsl {
        // Finish a pending reboot before anything else.
        WslStatus::RebootRequired => K::RebootRequired,
        // A disabled VM platform blocks WSL2 itself — surface it before WSL steps.
        _ if virtualization == VirtualizationStatus::UnavailableOrUnknown
            && wsl != WslStatus::Ready =>
        {
            K::OpenVirtualizationInstructions
        }
        WslStatus::Missing => K::InstallWsl,
        WslStatus::Wsl2Unavailable => K::EnableWsl2,
        WslStatus::Ready | WslStatus::Unknown | WslStatus::NotApplicable => K::None,
    }
}

/// True when `wsl --list --verbose` output has at least one distribution whose
/// VERSION column is `2`.
fn list_reports_version_2(list_out: &str) -> bool {
    wsl_list_versions(list_out).any(|v| v == 2)
}

/// True when the list has distributions and every one is version 1.
fn list_reports_only_version_1(list_out: Option<&str>) -> bool {
    let Some(list_out) = list_out else {
        return false;
    };
    let versions: Vec<u32> = wsl_list_versions(list_out).collect();
    !versions.is_empty() && versions.iter().all(|v| *v == 1)
}

/// Extract the trailing VERSION integer from each distribution row of
/// `wsl --list --verbose`. Skips the header row and the `*` default marker.
fn wsl_list_versions(list_out: &str) -> impl Iterator<Item = u32> + '_ {
    list_out.lines().filter_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("name") && lower.contains("version") {
            return None; // header
        }
        line.split_whitespace()
            .last()
            .and_then(|tok| tok.parse::<u32>().ok())
    })
}

/// List managed-language directories (`<tool>-<version>`) in the toolchain
/// cache. Returns the detected version strings (the `<version>` suffix).
fn managed_versions(tool: ToolKind) -> Vec<String> {
    let prefix = match tool {
        ToolKind::Node => "node-",
        ToolKind::Uv => "uv-",
        ToolKind::Python => "python-",
        _ => return Vec::new(),
    };
    let Ok(cache_dir) = capsule::common::paths::toolchain_cache_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&cache_dir) else {
        return Vec::new();
    };
    let mut versions = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && let Some(version) = name.strip_prefix(prefix)
            && !version.is_empty()
        {
            versions.push(version.to_string());
        }
    }
    versions.sort();
    versions
}

/// Executable filenames a managed install of `tool` may expose, matching the
/// layout `RuntimeFetcher::ensure_*` produces.
fn managed_binary_candidates(tool: ToolKind) -> &'static [&'static str] {
    match tool {
        ToolKind::Node => &["node", "node.exe"],
        ToolKind::Uv => &["uv", "uv.exe"],
        ToolKind::Python => &["python3", "python", "python3.exe", "python.exe"],
        _ => &[],
    }
}

/// The Ato-supported version policy string for a managed tool.
fn supported_version(tool: ToolKind) -> &'static str {
    match tool {
        ToolKind::Node => SUPPORTED_NODE_VERSION,
        ToolKind::Uv => SUPPORTED_UV_VERSION,
        ToolKind::Python => SUPPORTED_PYTHON_VERSION,
        _ => "",
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Extract a dotted numeric version (`22.11.0`) from a `--version` line such as
/// `v22.11.0`, `Python 3.12.7`, or `uv 0.4.19`.
fn parse_numeric_version(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_digit() {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    let token = line[start..i].trim_matches('.');
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Whether a detected numeric version satisfies the Ato-supported policy:
/// - Node: major must match (supported `22`).
/// - Python: major.minor must match (supported `3.12`).
/// - uv: exact full-version match (supported `0.4.19`).
fn version_satisfies(tool: ToolKind, detected: &str, supported: &str) -> bool {
    let d: Vec<&str> = detected.split('.').collect();
    match tool {
        ToolKind::Node => d.first().copied() == Some(supported.trim_start_matches('v')),
        ToolKind::Python => {
            let s: Vec<&str> = supported.split('.').collect();
            d.len() >= 2 && s.len() >= 2 && d[0] == s[0] && d[1] == s[1]
        }
        ToolKind::Uv => detected == supported,
        _ => false,
    }
}

/// A freshly-written binary executed before the writer's handle settles fails
/// with a transient "busy" error: ETXTBSY ("Text file busy", errno 26) on Unix,
/// sharing-violation (errno 32) on Windows. Both clear on retry. This is the
/// same race `podman_install` guards (#708); kept local here so each module's
/// fix is self-contained.
fn is_transient_exec_busy(err: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        err.raw_os_error() == Some(32)
    }
    #[cfg(not(windows))]
    {
        err.raw_os_error() == Some(26)
    }
}

/// Run `exec`, retrying briefly (bounded, 20ms·attempt backoff) only on the
/// transient busy code so a just-written binary's version probe does not flake
/// under parallel load. Any other error returns immediately.
fn exec_retrying_busy<T>(mut exec: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match exec() {
            Err(err) if is_transient_exec_busy(&err) && attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
            }
            other => return other,
        }
    }
}

/// Read a resolved binary's `--version`, trimmed to a single line. Running the
/// binary doubles as an executability probe.
fn tool_version_at(path: &Path) -> Option<String> {
    let output = exec_retrying_busy(|| Command::new(path).arg("--version").output()).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// A validated Ato-managed install: the version directory exists, contains a
/// runnable executable, and reported a parseable version.
struct ManagedProbe {
    detected: String,
    supported: bool,
}

/// Probe one `<tool>-<version_dir>` cache entry. Returns `None` when the cache
/// is incomplete or corrupt (no executable, not runnable, unreadable version),
/// so a stale/broken directory never counts as "ready".
fn probe_managed_version(tool: ToolKind, version_dir: &str) -> Option<ManagedProbe> {
    let cache = capsule::common::paths::toolchain_cache_dir().ok()?;
    let runtime_dir = cache.join(format!("{}-{}", tool.as_str(), version_dir));
    let bin = locate_runtime_binary(&runtime_dir, managed_binary_candidates(tool))?;
    if !is_executable(&bin) {
        return None;
    }
    let detected = parse_numeric_version(&tool_version_at(&bin)?)?;
    let supported = version_satisfies(tool, &detected, supported_version(tool));
    Some(ManagedProbe {
        detected,
        supported,
    })
}

/// Detect a managed language runtime (Node/uv/Python). Managed-first, but a
/// version directory only counts when it actually contains a runnable binary
/// whose version is in range:
/// - a supported managed copy → Ready;
/// - a runnable but out-of-range managed copy → `UpgradeManaged`;
/// - a missing or corrupt cache → `InstallManaged` (reinstall);
/// - otherwise note any host PATH copy but still recommend a managed install.
fn detect_managed_language_tool(tool: ToolKind, path_bins: &[&str]) -> ToolStatus {
    let label = tool.as_str();
    let versions = managed_versions(tool);
    // Probe newest first (managed_versions is sorted ascending).
    let probes: Vec<ManagedProbe> = versions
        .iter()
        .rev()
        .filter_map(|v| probe_managed_version(tool, v))
        .collect();

    if let Some(p) = probes.iter().find(|p| p.supported) {
        return ToolStatus::ready(
            tool,
            ToolSource::ManagedByAto,
            Some(p.detected.clone()),
            format!("Ato-managed {label} {} is ready", p.detected),
        );
    }

    if let Some(p) = probes.first() {
        // A runnable managed copy exists but is out of the supported range.
        return ToolStatus {
            kind: tool,
            installed: true,
            version: Some(p.detected.clone()),
            supported: false,
            ready: false,
            source: ToolSource::ManagedByAto,
            action: RecommendedAction::UpgradeManaged,
            message: format!(
                "Ato-managed {label} {} is installed, but Ato supports {}. Reinstall to upgrade.",
                p.detected,
                supported_version(tool)
            ),
        };
    }

    // No usable managed copy. Distinguish a corrupt cache (dirs exist but no
    // runnable binary) from a clean "not installed" state.
    let corrupt_cache = !versions.is_empty();
    let host = path_bins.iter().find_map(|bin| which::which(bin).ok());
    let message = if corrupt_cache {
        format!("Ato-managed {label} cache is incomplete or corrupt; Ato will reinstall it")
    } else if host.is_some() {
        format!(
            "A system {label} was found, but Ato installs its own managed copy for reproducible launches"
        )
    } else {
        format!("{label} is not installed; Ato can install a managed copy")
    };
    ToolStatus {
        kind: tool,
        // A corrupt managed cache is not a usable install.
        installed: host.is_some(),
        version: None,
        supported: false,
        ready: false,
        source: if host.is_some() {
            ToolSource::SystemPath
        } else {
            ToolSource::Missing
        },
        action: RecommendedAction::InstallManaged,
        message,
    }
}

/// Read a tool's `--version` output, trimmed to a single line.
fn tool_version(bin: &str) -> Option<String> {
    let output = exec_retrying_busy(|| Command::new(bin).arg("--version").output()).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Detect Podman. Detection-only and **read-only**: this never installs Podman
/// or inits/starts a machine — that is `ato internal runtime prepare`. It may
/// run `podman --version`, `podman machine list`, and `podman info` (all reads).
///
/// Resolution goes through [`capsule::podman`] rather than a bare
/// `which("podman")` so a GUI-launched probe with a minimal PATH still finds
/// Homebrew/known-location Podman instead of reporting a false "missing".
fn detect_podman() -> ToolStatus {
    let mut resolved = match capsule::podman::resolve_podman() {
        Ok(resolved) => resolved,
        Err(capsule::podman::PodmanResolveError::InvalidEnvOverride { path }) => {
            return ToolStatus::missing(
                ToolKind::Podman,
                RecommendedAction::OpenInstructions,
                format!(
                    "ATO_PODMAN_BIN is set to '{}' but that path is not a usable executable. \
                     Unset ATO_PODMAN_BIN or point it at a valid podman binary.",
                    path.display()
                ),
            );
        }
        Err(capsule::podman::PodmanResolveError::NotFound { .. }) => {
            return ToolStatus::missing(
                ToolKind::Podman,
                RecommendedAction::PrepareHostRuntime,
                "Podman is not installed. Prepare Podman to install and set it up.",
            );
        }
    };
    let version = resolved.query_version().map(str::to_string);
    // Build every probe from the *same* resolved binary (+ PATH override) so
    // version and readiness target one binary and work under a minimal GUI PATH.
    let invocation = resolved.invocation();
    let run = |args: &[&str]| -> Option<std::process::Output> {
        let mut cmd = Command::new(&invocation.program);
        if let Some(path_env) = &invocation.path_env {
            cmd.env("PATH", path_env);
        }
        cmd.args(args).output().ok()
    };
    // `podman info`, optionally pinned to a connection. Pinning matters because
    // the host's *default* connection may point at a different (e.g. stopped)
    // machine than the one that is actually running — so a plain `info` can fail
    // even though a machine (e.g. ato-podman) is up. Connection name == machine
    // name for machine-created connections.
    let info_ok = |connection: Option<&str>| -> bool {
        let mut args: Vec<&str> = Vec::new();
        if let Some(connection) = connection {
            args.push("--connection");
            args.push(connection);
        }
        args.extend_from_slice(&["info", "--format", "{{.Host.Arch}}"]);
        run(&args).map(|o| o.status.success()).unwrap_or(false)
    };

    // Native Linux Podman has no machine; readiness is just `podman info`.
    if cfg!(target_os = "linux") {
        return if info_ok(None) {
            ToolStatus::ready(
                ToolKind::Podman,
                ToolSource::External,
                version,
                "Podman is installed and running",
            )
        } else {
            podman_not_ready(
                version,
                RecommendedAction::RepairHostRuntime,
                "Podman is installed but `podman info` failed. Re-prepare Podman.",
            )
        };
    }

    // macOS/Windows: a machine must exist and run. Read-only probe.
    let machine = match run(&["machine", "list", "--format", "json"]) {
        Some(out) if out.status.success() => {
            parse_podman_machine_list(&String::from_utf8_lossy(&out.stdout))
        }
        Some(out) => PodmanMachineStatus::Unknown {
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
        None => PodmanMachineStatus::Unavailable {
            reason: "podman machine list could not be run".to_string(),
        },
    };
    // Confirm readiness against a *running* machine explicitly (prefer the Ato
    // machine), not the global default — only meaningful when one is running.
    let info_running_ok = match &machine {
        PodmanMachineStatus::Running { running_names, .. } => {
            info_ok(preferred_running_connection(running_names))
        }
        _ => false,
    };
    let (ready, action, message) = classify_podman_machine(&machine, info_running_ok);
    if ready {
        ToolStatus::ready(ToolKind::Podman, ToolSource::External, version, message)
    } else {
        podman_not_ready(version, action, message)
    }
}

/// Pick the connection to probe for readiness from the running machines: prefer
/// the Ato-managed machine, else the first running one. Returns `None` only when
/// nothing is running (caller treats that as not-ready). Connection name equals
/// the machine name for machine-created connections.
fn preferred_running_connection(running_names: &[String]) -> Option<&str> {
    if running_names.iter().any(|n| n == ATO_PODMAN_MACHINE_NAME) {
        Some(ATO_PODMAN_MACHINE_NAME)
    } else {
        running_names.first().map(String::as_str)
    }
}

/// Build a not-ready, installed Podman status (External source).
fn podman_not_ready(
    version: Option<String>,
    action: RecommendedAction,
    message: impl Into<String>,
) -> ToolStatus {
    ToolStatus {
        kind: ToolKind::Podman,
        installed: true,
        version,
        supported: true,
        ready: false,
        source: ToolSource::External,
        action,
        message: message.into(),
    }
}

/// Map a Podman machine status (+ whether `podman info` succeeded for a running
/// machine) to the readiness verdict and recommended action. Pure.
fn classify_podman_machine(
    machine: &PodmanMachineStatus,
    info_running_ok: bool,
) -> (bool, RecommendedAction, String) {
    match machine {
        PodmanMachineStatus::Running { .. } => {
            if info_running_ok {
                (
                    true,
                    RecommendedAction::None,
                    "Podman is installed and a machine is running".to_string(),
                )
            } else {
                (
                    false,
                    RecommendedAction::RepairHostRuntime,
                    "A Podman machine is running but `podman info` failed; it may be broken. \
                     Re-prepare Podman."
                        .to_string(),
                )
            }
        }
        PodmanMachineStatus::NotConfigured => (
            false,
            RecommendedAction::PrepareHostRuntime,
            "Podman is installed but has no machine. Prepare Podman to create and start one."
                .to_string(),
        ),
        PodmanMachineStatus::Stopped { names } if names.len() == 1 => (
            false,
            RecommendedAction::StartService,
            format!(
                "Podman is installed but its machine ({}) is stopped. Start it or prepare Podman.",
                names.join(", ")
            ),
        ),
        PodmanMachineStatus::Stopped { names } => (
            false,
            RecommendedAction::PrepareHostRuntime,
            format!(
                "Multiple stopped Podman machines ({}); prepare an Ato-managed machine.",
                names.join(", ")
            ),
        ),
        PodmanMachineStatus::Unavailable { reason } | PodmanMachineStatus::Unknown { reason } => (
            false,
            RecommendedAction::RepairHostRuntime,
            format!("Podman machine state could not be determined ({reason}); re-prepare Podman."),
        ),
    }
}

/// Detect a Docker-compatible daemon: detection-only. Ato never installs Docker
/// Desktop.
fn detect_docker_desktop() -> ToolStatus {
    if which::which("docker").is_err() {
        return ToolStatus::missing(
            ToolKind::DockerDesktop,
            RecommendedAction::OpenInstructions,
            "Docker Desktop is not installed. Ato does not install it automatically — see https://www.docker.com/products/docker-desktop/.",
        );
    }
    let version = tool_version("docker");
    let info_output = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output();
    let running = info_output
        .as_ref()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if running {
        return ToolStatus::ready(
            ToolKind::DockerDesktop,
            ToolSource::External,
            version,
            "Docker is installed and the daemon is running",
        );
    }

    // Check stderr for permission denied to give a more specific message.
    let stderr = info_output
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();
    let message = if stderr.to_ascii_lowercase().contains("permission denied")
        || stderr.to_ascii_lowercase().contains("access is denied")
    {
        "Docker is installed but permission was denied when connecting to the Docker socket. \
         Add your user to the 'docker' group or use 'sudo', or use Podman instead."
            .to_string()
    } else {
        "Docker is installed but the daemon is not running. Start Docker Desktop and try again."
            .to_string()
    };
    ToolStatus {
        kind: ToolKind::DockerDesktop,
        installed: true,
        version,
        supported: true,
        ready: false,
        source: ToolSource::External,
        action: RecommendedAction::StartService,
        message,
    }
}

/// Detect the bundled `ato` helper. When this code runs we *are* the helper, so
/// it is present by construction; report the running binary's version.
fn detect_ato_helper() -> ToolStatus {
    ToolStatus::ready(
        ToolKind::AtoHelper,
        ToolSource::Bundled,
        Some(env!("CARGO_PKG_VERSION").to_string()),
        "Ato helper is bundled and ready",
    )
}

/// Detect bundled `nacelle`: it must sit next to the running `ato` binary.
///
/// Deliberately NO PATH fallback. `nacelle` ships inside the desktop bundle, so
/// a missing sibling is a bundle-integrity error — reporting a `nacelle` found
/// elsewhere on `PATH` as "Bundled and ready" would mask broken Windows/MSI
/// packaging on the very machine where it must be caught. Version is read from
/// the sibling, never from a PATH copy.
fn detect_nacelle() -> ToolStatus {
    let sibling = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?;
        ["nacelle", "nacelle.exe"]
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    });
    match sibling {
        Some(path) => ToolStatus::ready(
            ToolKind::Nacelle,
            ToolSource::Bundled,
            tool_version_at(&path),
            "Nacelle is bundled and ready",
        ),
        None => ToolStatus {
            kind: ToolKind::Nacelle,
            installed: false,
            version: None,
            supported: false,
            ready: false,
            source: ToolSource::Missing,
            action: RecommendedAction::BundleRepairRequired,
            message: "Nacelle is missing from the Ato bundle. Reinstall Ato to repair it."
                .to_string(),
        },
    }
}

/// Emits one `InstallProgress` per fetcher event by translating the generic
/// `CapsuleReporter` callbacks into install phases. Honest by construction: a
/// phase is only emitted when the fetcher actually reaches it.
struct InstallReporter {
    tool: ToolKind,
    json: bool,
    /// Guards interleaved writes if the fetcher ever reports concurrently.
    lock: Mutex<()>,
}

impl InstallReporter {
    fn new(tool: ToolKind, json: bool) -> Self {
        InstallReporter {
            tool,
            json,
            lock: Mutex::new(()),
        }
    }

    fn emit(&self, phase: InstallPhase, message: impl Into<String>) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        emit_progress(self.tool, phase, message, self.json);
    }
}

#[async_trait::async_trait]
impl CapsuleReporter for InstallReporter {
    async fn notify(&self, _message: String) -> capsule::Result<()> {
        Ok(())
    }
    async fn warn(&self, _message: String) -> capsule::Result<()> {
        Ok(())
    }
    async fn progress_start(&self, label: String, _total: Option<u64>) -> capsule::Result<()> {
        self.emit(InstallPhase::Downloading, label);
        Ok(())
    }
    async fn progress_inc(&self, _amount: u64) -> capsule::Result<()> {
        Ok(())
    }
    async fn progress_finish(&self, _message: Option<String>) -> capsule::Result<()> {
        self.emit(InstallPhase::Installing, "Unpacking…");
        Ok(())
    }
}

/// Print a single progress event (JSON line for the desktop, or a human line).
/// Shared with `runtime_prepare` so install and prepare emit the identical
/// `InstallProgress` wire shape.
pub(crate) fn emit_progress(
    tool: ToolKind,
    phase: InstallPhase,
    message: impl Into<String>,
    json: bool,
) {
    emit_event(InstallProgress::new(tool, phase, message), json);
}

/// Emit a `Failed` event, tagging it `retryable` so a consuming UI can offer a
/// Retry action for transient conditions (e.g. a 504 download). Same wire shape
/// as [`emit_progress`], plus the `retryable` flag when true.
pub(crate) fn emit_failure(
    tool: ToolKind,
    message: impl Into<String>,
    retryable: bool,
    json: bool,
) {
    emit_event(
        InstallProgress::new(tool, InstallPhase::Failed, message).retryable(retryable),
        json,
    );
}

fn emit_event(event: InstallProgress, json: bool) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    if json {
        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(stdout, "{line}");
        }
    } else {
        let _ = writeln!(
            stdout,
            "[{}] {:?}: {}",
            event.tool.as_str(),
            event.phase,
            event.message
        );
    }
    let _ = stdout.flush();
}

/// Install Ato-managed copies of the requested tools, streaming progress.
///
/// Rejects any tool that is not managed-installable (Podman/Docker/ato/nacelle)
/// before doing any work, so a bad request can't half-install. Each tool's
/// failure is surfaced as a `Failed` event but does not abort the others; the
/// function returns an error only if at least one install failed.
pub(crate) fn install_tools(tools: Vec<ToolKind>, json: bool) -> Result<()> {
    if tools.is_empty() {
        return Err(anyhow!("no tools specified to install"));
    }
    // Reject unsupported tool names up front (transaction-safe: nothing runs
    // until the whole request is known-installable).
    let unsupported: Vec<&str> = tools
        .iter()
        .filter(|t| !t.is_managed_installable())
        .map(|t| t.as_str())
        .collect();
    if !unsupported.is_empty() {
        return Err(anyhow!(
            "these tools cannot be installed by Ato (detection-only / bundled): {}",
            unsupported.join(", ")
        ));
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("failed to build async runtime for install: {e}"))?;

    let mut failures = Vec::new();
    for tool in tools {
        emit_progress(tool, InstallPhase::Queued, "Queued", json);
        let reporter = Arc::new(InstallReporter::new(tool, json));
        let result = runtime.block_on(install_one(tool, reporter));
        match result {
            Ok(()) => emit_progress(tool, InstallPhase::Ready, "Ready", json),
            Err(err) => {
                emit_progress(tool, InstallPhase::Failed, err.to_string(), json);
                failures.push(format!("{}: {err}", tool.as_str()));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("runtime install failed: {}", failures.join("; ")))
    }
}

/// Install a single managed tool at its Ato-supported version.
async fn install_one(tool: ToolKind, reporter: Arc<InstallReporter>) -> Result<()> {
    let fetcher = RuntimeFetcher::new_with_reporter(reporter)
        .map_err(|e| anyhow!("failed to init runtime fetcher: {e}"))?;
    match tool {
        ToolKind::Node => {
            fetcher
                .ensure_node(SUPPORTED_NODE_VERSION)
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        ToolKind::Uv => {
            fetcher
                .ensure_uv(Some(SUPPORTED_UV_VERSION))
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        ToolKind::Python => {
            fetcher
                .ensure_python(SUPPORTED_PYTHON_VERSION)
                .await
                .map_err(|e| anyhow!("{e}"))?;
        }
        other => {
            return Err(anyhow!(
                "{} is not an Ato-managed installable tool",
                other.as_str()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_rejects_unsupported_tools() {
        for tool in [
            ToolKind::Podman,
            ToolKind::DockerDesktop,
            ToolKind::AtoHelper,
            ToolKind::Nacelle,
        ] {
            let err = install_tools(vec![tool], true).unwrap_err();
            assert!(
                err.to_string().contains("cannot be installed"),
                "expected rejection for {}, got: {err}",
                tool.as_str()
            );
        }
    }

    #[test]
    fn install_rejects_empty_request() {
        assert!(install_tools(vec![], true).is_err());
    }

    #[test]
    fn install_rejects_mixed_request_before_running() {
        // A request mixing an installable tool with an unsupported one must be
        // rejected wholesale — no partial install.
        let err = install_tools(vec![ToolKind::Node, ToolKind::Podman], true).unwrap_err();
        assert!(err.to_string().contains("podman"));
    }

    #[test]
    fn setup_status_reports_all_tools() {
        let status = collect_setup_status();
        for kind in [
            ToolKind::Podman,
            ToolKind::DockerDesktop,
            ToolKind::Node,
            ToolKind::Uv,
            ToolKind::Python,
            ToolKind::AtoHelper,
            ToolKind::Nacelle,
        ] {
            assert!(
                status.get(kind).is_some(),
                "status must include {}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn ato_helper_is_always_ready_with_version() {
        let helper = detect_ato_helper();
        assert!(helper.ready);
        assert_eq!(helper.source, ToolSource::Bundled);
        assert!(helper.version.is_some());
    }

    #[test]
    fn missing_managed_node_recommends_install() {
        // With no managed copy in a tool's cache, the recommended action is a
        // managed install (host PATH copies don't make it "ready"). Pin
        // ATO_HOME to an empty tempdir under the shared env lock so a real
        // (or concurrently mutated) managed cache can never flip the verdict
        // to UpgradeManaged.
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let ato_home = tempfile::tempdir().expect("ato home");
        let prior = std::env::var_os("ATO_HOME");
        unsafe {
            std::env::set_var("ATO_HOME", ato_home.path());
        }
        let status =
            detect_managed_language_tool(ToolKind::Node, &["definitely-not-a-real-bin-xyz"]);
        unsafe {
            match prior {
                Some(value) => std::env::set_var("ATO_HOME", value),
                None => std::env::remove_var("ATO_HOME"),
            }
        }
        assert!(!status.ready, "empty managed cache cannot be ready");
        assert_eq!(status.action, RecommendedAction::InstallManaged);
    }

    #[test]
    fn parse_numeric_version_extracts_dotted() {
        assert_eq!(
            parse_numeric_version("v22.11.0").as_deref(),
            Some("22.11.0")
        );
        assert_eq!(
            parse_numeric_version("Python 3.12.7").as_deref(),
            Some("3.12.7")
        );
        assert_eq!(
            parse_numeric_version("uv 0.4.19").as_deref(),
            Some("0.4.19")
        );
        assert!(parse_numeric_version("no digits here").is_none());
    }

    #[test]
    fn version_satisfies_enforces_policy() {
        // Node: major must match.
        assert!(version_satisfies(ToolKind::Node, "22.11.0", "22"));
        assert!(!version_satisfies(ToolKind::Node, "20.5.0", "22"));
        // Python: major.minor must match.
        assert!(version_satisfies(ToolKind::Python, "3.12.7", "3.12"));
        assert!(!version_satisfies(ToolKind::Python, "3.11.9", "3.12"));
        // uv: exact full version.
        assert!(version_satisfies(ToolKind::Uv, "0.4.19", "0.4.19"));
        assert!(!version_satisfies(ToolKind::Uv, "0.4.18", "0.4.19"));
    }

    /// Run `f` with `ATO_HOME` pointed at `home`, restoring the prior value.
    /// Callers use `#[serial]`; the shared lock also coordinates non-serial tests.
    fn with_ato_home<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
        // `serial_test` only coordinates tests using its own lock. The CLI test
        // suite also has a shared environment lock, so take both before changing
        // ATO_HOME to avoid another test restoring it during this probe.
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let prev = std::env::var_os("ATO_HOME");
        unsafe { std::env::set_var("ATO_HOME", home) };
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ATO_HOME", v),
                None => std::env::remove_var("ATO_HOME"),
            }
        }
        out
    }

    #[test]
    #[serial_test::serial]
    fn managed_corrupt_cache_is_not_ready() {
        // A version directory with no usable binary must NOT report Ready —
        // it is treated as a corrupt cache to reinstall.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("toolchains/node-22.11.0")).unwrap();
        let status = with_ato_home(tmp.path(), || {
            detect_managed_language_tool(ToolKind::Node, &["definitely-not-a-real-bin-xyz"])
        });
        assert!(!status.ready, "corrupt cache must not be Ready: {status:?}");
        assert_eq!(status.action, RecommendedAction::InstallManaged);
        assert!(
            status.message.contains("corrupt") || status.message.contains("incomplete"),
            "message should flag the broken cache: {}",
            status.message
        );
    }

    #[cfg(unix)]
    fn write_exec_script(path: &std::path::Path, version_line: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\necho {version_line}\n")).unwrap();
        let mut perm = std::fs::metadata(path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(path, perm).unwrap();
    }

    fn transient_busy_error() -> std::io::Error {
        #[cfg(windows)]
        {
            std::io::Error::from_raw_os_error(32)
        }
        #[cfg(not(windows))]
        {
            std::io::Error::from_raw_os_error(26)
        }
    }

    #[test]
    fn exec_retrying_busy_recovers_after_transient_busy() {
        let mut calls = 0u32;
        let result = exec_retrying_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(transient_busy_error())
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.ok(), Some(3), "should retry past a transient busy");
    }

    #[test]
    fn exec_retrying_busy_does_not_retry_other_errors() {
        let mut calls = 0u32;
        let result: std::io::Result<()> = exec_retrying_busy(|| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(2)) // ENOENT — a real failure
        });
        assert!(result.is_err());
        assert_eq!(calls, 1, "non-busy errors must surface immediately");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn managed_supported_node_is_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("toolchains/node-22.0.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_script(&bin.join("node"), "v22.0.0");
        let status = with_ato_home(tmp.path(), || {
            detect_managed_language_tool(ToolKind::Node, &[])
        });
        assert!(status.ready, "expected Ready, got {status:?}");
        assert_eq!(status.source, ToolSource::ManagedByAto);
        assert_eq!(status.version.as_deref(), Some("22.0.0"));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn managed_out_of_range_node_is_upgrade() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("toolchains/node-20.5.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_script(&bin.join("node"), "v20.5.0");
        let status = with_ato_home(tmp.path(), || {
            detect_managed_language_tool(ToolKind::Node, &[])
        });
        assert!(!status.ready, "old major must not be Ready: {status:?}");
        assert_eq!(status.action, RecommendedAction::UpgradeManaged);
        assert_eq!(status.version.as_deref(), Some("20.5.0"));
    }

    /// Run `f` with `ATO_PODMAN_BIN` pointed at `bin`, restoring the prior value.
    /// Serialised by callers (`#[serial]`) — it mutates a process-global var.
    fn with_podman_bin<T>(bin: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let prev = std::env::var_os("ATO_PODMAN_BIN");
        unsafe { std::env::set_var("ATO_PODMAN_BIN", bin) };
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ATO_PODMAN_BIN", v),
                None => std::env::remove_var("ATO_PODMAN_BIN"),
            }
        }
        out
    }

    /// Write `script` as an executable file at `path` (0o755).
    #[cfg(unix)]
    fn write_script(path: &std::path::Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).unwrap();
        let mut perm = std::fs::metadata(path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(path, perm).unwrap();
    }

    /// A resolvable podman binary whose `--version` succeeds but whose other
    /// subcommands fail must report installed-but-not-ready (never the false
    /// "missing binary"). `info`/`machine list` failing maps to a repair action.
    /// The `ATO_PODMAN_BIN` override makes resolution deterministic regardless
    /// of any real podman on the host.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn detect_podman_installed_but_broken_is_not_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("podman");
        // `--version` → version line; everything else (info / machine list) → fail.
        write_script(
            &bin,
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'podman version 9.9.9' ;;\n  *) exit 1 ;;\nesac\n",
        );

        let status = with_podman_bin(&bin, detect_podman);
        assert!(status.installed, "binary exists ⇒ installed: {status:?}");
        assert_eq!(status.source, ToolSource::External);
        assert_eq!(status.version.as_deref(), Some("podman version 9.9.9"));
        assert!(!status.ready);
        // Both the Linux (info fails) and macOS (machine list fails → Unknown)
        // paths surface a repair action — never a "missing"/install verdict.
        assert_eq!(status.action, RecommendedAction::RepairHostRuntime);
    }

    /// Passive status detection must be read-only: it may probe version / machine
    /// list / info, but must NEVER install or init/start a machine.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn setup_status_never_mutates_host() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("podman");
        let log = tmp.path().join("calls.log");
        // Log every invocation; answer reads so detection completes.
        write_script(
            &bin,
            &format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1\" in\n  --version) echo 'podman version 9.9.9' ;;\n  machine) echo '[]' ;;\n  info) echo 'arm64' ;;\n  *) ;;\nesac\n",
                log.display()
            ),
        );

        let prev = std::env::var_os("ATO_PODMAN_BIN");
        unsafe { std::env::set_var("ATO_PODMAN_BIN", &bin) };
        let _status = collect_setup_status();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ATO_PODMAN_BIN", v),
                None => std::env::remove_var("ATO_PODMAN_BIN"),
            }
        }

        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        for forbidden in ["machine init", "machine start", "install"] {
            assert!(
                !calls.contains(forbidden),
                "passive status must not run `{forbidden}`; calls were:\n{calls}"
            );
        }
    }

    #[test]
    fn classify_running_machine_with_info_ok_is_ready() {
        let machine = PodmanMachineStatus::Running {
            running_names: vec!["ato-podman".to_string()],
            all_names: vec!["ato-podman".to_string()],
        };
        let (ready, action, _) = classify_podman_machine(&machine, true);
        assert!(ready);
        assert_eq!(action, RecommendedAction::None);
    }

    #[test]
    fn classify_running_machine_with_info_fail_is_repair() {
        let machine = PodmanMachineStatus::Running {
            running_names: vec!["ato-podman".to_string()],
            all_names: vec!["ato-podman".to_string()],
        };
        let (ready, action, _) = classify_podman_machine(&machine, false);
        assert!(!ready);
        assert_eq!(action, RecommendedAction::RepairHostRuntime);
    }

    #[test]
    fn classify_no_machine_is_prepare() {
        let (ready, action, _) =
            classify_podman_machine(&PodmanMachineStatus::NotConfigured, false);
        assert!(!ready);
        assert_eq!(action, RecommendedAction::PrepareHostRuntime);
    }

    #[test]
    fn classify_single_stopped_machine_is_start_service() {
        let machine = PodmanMachineStatus::Stopped {
            names: vec!["ato-podman".to_string()],
        };
        let (ready, action, _) = classify_podman_machine(&machine, false);
        assert!(!ready);
        assert_eq!(action, RecommendedAction::StartService);
    }

    #[test]
    fn classify_multiple_stopped_machines_is_prepare() {
        let machine = PodmanMachineStatus::Stopped {
            names: vec!["a".to_string(), "b".to_string()],
        };
        let (_, action, _) = classify_podman_machine(&machine, false);
        assert_eq!(action, RecommendedAction::PrepareHostRuntime);
    }

    #[test]
    fn preferred_connection_prefers_ato_machine() {
        let names = vec![
            "podman-machine-default".to_string(),
            "ato-podman".to_string(),
        ];
        assert_eq!(preferred_running_connection(&names), Some("ato-podman"));
    }

    #[test]
    fn preferred_connection_falls_back_to_first_running() {
        let names = vec!["podman-machine-default".to_string()];
        assert_eq!(
            preferred_running_connection(&names),
            Some("podman-machine-default")
        );
        assert_eq!(preferred_running_connection(&[]), None);
    }

    #[test]
    fn classify_unknown_machine_state_is_repair() {
        let machine = PodmanMachineStatus::Unknown {
            reason: "boom".to_string(),
        };
        let (_, action, _) = classify_podman_machine(&machine, false);
        assert_eq!(action, RecommendedAction::RepairHostRuntime);
    }

    /// An invalid `ATO_PODMAN_BIN` path must report missing with an actionable
    /// message that names the bad path, not a generic "Podman is not installed".
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn detect_podman_invalid_override_reports_actionable_message() {
        let status = with_podman_bin(
            std::path::Path::new("/nonexistent/bad/podman"),
            detect_podman,
        );
        assert!(
            !status.installed,
            "bad override must not count as installed"
        );
        assert!(!status.ready);
        assert!(
            status.message.contains("ATO_PODMAN_BIN"),
            "message must mention ATO_PODMAN_BIN: {}",
            status.message
        );
        assert!(
            status.message.contains("/nonexistent/bad/podman"),
            "message must name the bad path: {}",
            status.message
        );
    }

    /// When `ATO_PODMAN_BIN` is not set and podman is not on PATH/known locations,
    /// the message should mention the install URL.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn detect_podman_not_found_suggests_install() {
        let _env = crate::tests::env_lock().lock().unwrap();
        // Temporarily clear ATO_PODMAN_BIN and set a PATH with no podman.
        let prev_bin = std::env::var_os("ATO_PODMAN_BIN");
        let prev_path = std::env::var_os("PATH");
        unsafe { std::env::remove_var("ATO_PODMAN_BIN") };
        // Use an empty PATH to ensure which::which finds nothing, and pick a
        // non-existent location for known paths.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PATH", tmp.path()) };
        // Resolution probes fixed install locations (e.g. /opt/homebrew/bin)
        // independently of PATH; on hosts with a real podman the NotFound
        // branch under test is unreachable, and `status` describes machine
        // state instead. Detect that while the scrubbed PATH is active.
        let host_podman_resolvable = capsule::podman::resolve_podman().is_ok();
        let status = detect_podman();
        unsafe {
            match prev_bin {
                Some(v) => std::env::set_var("ATO_PODMAN_BIN", v),
                None => std::env::remove_var("ATO_PODMAN_BIN"),
            }
            match prev_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        // Only assert when we're confident podman is not on the system (the
        // test may still find a real podman in known locations on developer
        // machines, which is fine — skip in that case).
        if host_podman_resolvable {
            return;
        }
        if !status.ready {
            assert!(
                status.message.contains("not installed")
                    || status.message.contains("ATO_PODMAN_BIN"),
                "message should indicate podman is not installed or missing: {}",
                status.message
            );
        }
    }

    // ── #460 Windows substrate (WSL) diagnostics ──────────────────────────────

    #[test]
    fn wsl_not_invocable_is_missing() {
        let s = classify_windows_substrate(false, None, None);
        assert_eq!(s.wsl, WslStatus::Missing);
    }

    #[test]
    fn wsl_list_with_version_2_is_ready() {
        let list = "  NAME                      STATE           VERSION\n\
                     * podman-machine-default    Running         2\n";
        let s = classify_windows_substrate(true, Some("Default Version: 2"), Some(list));
        assert_eq!(s.wsl, WslStatus::Ready);
    }

    #[test]
    fn wsl_default_version_2_without_distro_marker_is_ready() {
        let s = classify_windows_substrate(true, Some("Default Version: 2"), Some(""));
        assert_eq!(s.wsl, WslStatus::Ready);
    }

    #[test]
    fn wsl_no_installed_distributions_is_wsl2_unavailable() {
        let msg = "Windows Subsystem for Linux has no installed distributions.";
        let s = classify_windows_substrate(true, Some(msg), Some(msg));
        assert_eq!(s.wsl, WslStatus::Wsl2Unavailable);
    }

    #[test]
    fn wsl_only_version_1_distro_is_wsl2_unavailable() {
        let list = "  NAME            STATE           VERSION\n\
                     * Legacy          Stopped         1\n";
        let s = classify_windows_substrate(true, Some("Default Version: 1"), Some(list));
        assert_eq!(s.wsl, WslStatus::Wsl2Unavailable);
    }

    #[test]
    fn wsl_not_installed_text_is_missing() {
        let msg = "Windows Subsystem for Linux is not installed. Use `wsl --install`.";
        let s = classify_windows_substrate(true, Some(msg), None);
        assert_eq!(s.wsl, WslStatus::Missing);
    }

    #[test]
    fn wsl_reboot_required_is_detected() {
        let msg = "The requested operation is successful. Restart your computer to finish.";
        let s = classify_windows_substrate(true, Some(msg), None);
        assert_eq!(s.wsl, WslStatus::RebootRequired);
        assert!(s.reboot_required);
    }

    #[test]
    fn wsl_virtualization_disabled_is_flagged() {
        let msg = "Please enable the Virtual Machine Platform Windows feature and ensure \
                   virtualization is enabled in the BIOS.";
        let s = classify_windows_substrate(true, Some(msg), None);
        assert_eq!(s.virtualization, VirtualizationStatus::UnavailableOrUnknown);
    }

    #[test]
    fn wsl_ready_leaves_virtualization_unknown() {
        let list = "  NAME    STATE     VERSION\n* d      Running   2\n";
        let s = classify_windows_substrate(true, Some("Default Version: 2"), Some(list));
        assert_eq!(s.virtualization, VirtualizationStatus::Unknown);
        assert!(!s.reboot_required);
    }

    #[test]
    fn wsl_list_version_parsing_skips_header_and_reads_version_column() {
        let list = "  NAME                      STATE           VERSION\n\
                     * podman-machine-default    Running         2\n\
                       Other                     Stopped         1\n";
        assert!(list_reports_version_2(list));
        let versions: Vec<u32> = wsl_list_versions(list).collect();
        assert_eq!(versions, vec![2, 1]);
    }

    // ── #460 PR2: substrate action classification ─────────────────────────────

    #[test]
    fn action_wsl_missing_is_install_wsl() {
        let s = classify_windows_substrate(true, Some("is not installed. wsl --install"), None);
        assert_eq!(s.wsl, WslStatus::Missing);
        assert_eq!(s.action.kind, WindowsSubstrateActionKind::InstallWsl);
        assert!(s.action.requires_admin && s.action.requires_reboot);
        assert!(s.action.can_run_from_desktop);
    }

    #[test]
    fn action_wsl2_unavailable_is_enable_wsl2() {
        let s = classify_windows_substrate(
            true,
            Some("Windows Subsystem for Linux has no installed distributions."),
            None,
        );
        assert_eq!(s.wsl, WslStatus::Wsl2Unavailable);
        assert_eq!(s.action.kind, WindowsSubstrateActionKind::EnableWsl2);
        assert!(!s.action.requires_reboot);
    }

    #[test]
    fn action_reboot_required() {
        let s = classify_windows_substrate(true, Some("Restart your computer to finish."), None);
        assert_eq!(s.action.kind, WindowsSubstrateActionKind::RebootRequired);
        assert!(s.action.requires_reboot);
    }

    #[test]
    fn action_virtualization_disabled_opens_instructions_and_is_not_one_click() {
        let s = classify_windows_substrate(
            true,
            Some(
                "Please enable the Virtual Machine Platform; virtualization must be enabled in BIOS.",
            ),
            None,
        );
        assert_eq!(
            s.action.kind,
            WindowsSubstrateActionKind::OpenVirtualizationInstructions
        );
        // Firmware/BIOS cannot be fully automated → not a guaranteed one-click fix.
        assert!(!s.action.can_run_from_desktop);
    }

    #[test]
    fn action_ready_has_no_action() {
        let list = "  NAME    STATE     VERSION\n* d      Running   2\n";
        let s = classify_windows_substrate(true, Some("Default Version: 2"), Some(list));
        assert_eq!(s.action.kind, WindowsSubstrateActionKind::None);
    }

    #[test]
    fn decode_wsl_output_handles_utf16le() {
        // "Default Version: 2" as UTF-16LE.
        let text = "Default Version: 2";
        let mut bytes = Vec::new();
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_wsl_output(&bytes), text);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn windows_substrate_is_none_off_windows() {
        assert!(detect_windows_substrate().is_none());
    }
}
