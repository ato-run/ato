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

use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use capsule_core::packers::runtime_fetcher::RuntimeFetcher;
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

/// Detect a managed language runtime (Node/uv/Python). Managed-first: ready iff
/// an Ato-managed copy exists in the toolchain cache; otherwise recommend a
/// managed install, noting any host PATH copy for context only.
fn detect_managed_language_tool(tool: ToolKind, path_bins: &[&str]) -> ToolStatus {
    let label = tool.as_str();
    let managed = managed_versions(tool);
    if let Some(version) = managed.last() {
        return ToolStatus::ready(
            tool,
            ToolSource::ManagedByAto,
            Some(version.clone()),
            format!("Ato-managed {label} {version} is ready"),
        );
    }

    // No managed copy. Note a host PATH copy if present, but still recommend a
    // managed install — host toolchains are not used for reproducible launches.
    let host = path_bins.iter().find_map(|bin| which::which(bin).ok());
    let message = match &host {
        Some(_) => format!(
            "A system {label} was found, but Ato installs its own managed copy for reproducible launches"
        ),
        None => format!("{label} is not installed; Ato can install a managed copy"),
    };
    ToolStatus {
        kind: tool,
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

/// Detect bundled `nacelle`: a sibling of the running `ato` binary, falling back
/// to PATH. Missing is a bundle-integrity error, never a download.
fn detect_nacelle() -> ToolStatus {
    let sibling = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?;
        for name in ["nacelle", "nacelle.exe"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    });
    let found = sibling.is_some() || which::which("nacelle").is_ok();
    if found {
        ToolStatus::ready(
            ToolKind::Nacelle,
            ToolSource::Bundled,
            tool_version("nacelle"),
            "Nacelle is bundled and ready",
        )
    } else {
        ToolStatus {
            kind: ToolKind::Nacelle,
            installed: false,
            version: None,
            supported: false,
            ready: false,
            source: ToolSource::Missing,
            action: RecommendedAction::BundleRepairRequired,
            message: "Nacelle is missing from the Ato bundle. Reinstall Ato to repair it."
                .to_string(),
        }
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
        let status = detect_managed_language_tool(ToolKind::Node, &["definitely-not-a-real-bin-xyz"]);
        if !status.ready {
            assert_eq!(status.action, RecommendedAction::InstallManaged);
        }
    }
}
