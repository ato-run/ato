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

use capsule_core::packers::runtime_fetcher::{RuntimeFetcher, locate_runtime_binary};
use capsule_core::reporter::CapsuleReporter;
use capsule_core::runtime_setup::{
    InstallPhase, InstallProgress, RecommendedAction, RuntimeSetupStatus, SUPPORTED_NODE_VERSION,
    SUPPORTED_PYTHON_VERSION, SUPPORTED_UV_VERSION, ToolKind, ToolSource, ToolStatus,
};

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
    }
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
    let Ok(cache_dir) = capsule_core::common::paths::toolchain_cache_dir() else {
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

/// Read a resolved binary's `--version`, trimmed to a single line. Running the
/// binary doubles as an executability probe.
fn tool_version_at(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
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
    let cache = capsule_core::common::paths::toolchain_cache_dir().ok()?;
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
    let output = Command::new(bin).arg("--version").output().ok()?;
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

/// Detect Podman: detection-only (we never auto-install a container engine).
fn detect_podman() -> ToolStatus {
    if which::which("podman").is_err() {
        return ToolStatus::missing(
            ToolKind::Podman,
            RecommendedAction::OpenInstructions,
            "Podman is not installed. See https://podman.io/docs/installation to set it up.",
        );
    }
    let version = tool_version("podman");
    // `podman info` returns non-zero quickly when no machine/daemon is running,
    // so it doubles as a readiness probe without auto-starting anything.
    let running = Command::new("podman")
        .args(["info", "--format", "{{.Host.Arch}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if running {
        ToolStatus::ready(
            ToolKind::Podman,
            ToolSource::External,
            version,
            "Podman is installed and running",
        )
    } else {
        ToolStatus {
            kind: ToolKind::Podman,
            installed: true,
            version,
            supported: true,
            ready: false,
            source: ToolSource::External,
            action: RecommendedAction::StartService,
            message: "Podman is installed but no machine is running. Start it with `podman machine start`.".to_string(),
        }
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
    let running = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if running {
        ToolStatus::ready(
            ToolKind::DockerDesktop,
            ToolSource::External,
            version,
            "Docker is installed and the daemon is running",
        )
    } else {
        ToolStatus {
            kind: ToolKind::DockerDesktop,
            installed: true,
            version,
            supported: true,
            ready: false,
            source: ToolSource::External,
            action: RecommendedAction::StartService,
            message: "Docker is installed but the daemon is not running. Start Docker Desktop and try again.".to_string(),
        }
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
    async fn notify(&self, _message: String) -> capsule_core::Result<()> {
        Ok(())
    }
    async fn warn(&self, _message: String) -> capsule_core::Result<()> {
        Ok(())
    }
    async fn progress_start(&self, label: String, _total: Option<u64>) -> capsule_core::Result<()> {
        self.emit(InstallPhase::Downloading, label);
        Ok(())
    }
    async fn progress_inc(&self, _amount: u64) -> capsule_core::Result<()> {
        Ok(())
    }
    async fn progress_finish(&self, _message: Option<String>) -> capsule_core::Result<()> {
        self.emit(InstallPhase::Installing, "Unpacking…");
        Ok(())
    }
}

/// Print a single progress event (JSON line for the desktop, or a human line).
fn emit_progress(tool: ToolKind, phase: InstallPhase, message: impl Into<String>, json: bool) {
    use std::io::Write;
    let event = InstallProgress::new(tool, phase, message);
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
        // managed install (host PATH copies don't make it "ready").
        let status =
            detect_managed_language_tool(ToolKind::Node, &["definitely-not-a-real-bin-xyz"]);
        if !status.ready {
            assert_eq!(status.action, RecommendedAction::InstallManaged);
        }
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
    /// Serialised by callers (`#[serial]`) — it mutates a process-global var.
    fn with_ato_home<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
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
}
