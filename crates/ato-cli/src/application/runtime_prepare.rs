//! Opt-in host-runtime preparation (Podman).
//!
//! This is the *only* path that may mutate host/runtime state for a container
//! engine: install Podman (explicit opt-in), create/start the Ato-managed
//! Podman machine (`ato-podman`), and verify readiness with `podman info`.
//! Passive status detection ([`crate::application::runtime_setup`]) never calls
//! into here — it only reads.
//!
//! Podman is a *host runtime*, not an Ato-managed toolchain: it is never routed
//! through `RuntimeFetcher` or the toolchain cache. Routing is decided by
//! [`ToolKind::install_strategy`]:
//! - `ManagedToolchain` (node/uv/python) → reuse [`install_tools`]
//! - `HostRuntime` (podman) → [`prepare_podman`]
//! - `DetectionOnly` (docker) / `Bundled` → rejected up front
//!
//! Backend only — no onboarding/settings UI (that is PR B-2).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, anyhow};

use capsule_core::podman::{self, ATO_PODMAN_MACHINE_NAME, PodmanResolveError, ResolvedPodman};
use capsule_core::runtime_setup::{InstallPhase, InstallStrategy, ToolKind};

use crate::adapters::runtime::podman_machine::{PodmanMachine, parse_machine_entries};
use crate::application::podman_install::{
    PodmanInstallError, PodmanInstallStrategy, ReqwestArtifactFetcher, install_ato_managed_podman,
    install_strategies, missing_helpers_for, pinned_artifact,
};
use crate::application::runtime_setup::{emit_failure, emit_progress, install_tools};

/// Public entry for `ato internal runtime prepare --tools … [--emit-json]`.
///
/// Rejects detection-only/bundled tools up front (transaction-safe). Managed
/// toolchains reuse [`install_tools`]; host runtimes (Podman) go through
/// [`prepare_podman`]. Returns an error if any tool failed.
pub(crate) fn prepare_tools(tools: Vec<ToolKind>, json: bool) -> Result<()> {
    if tools.is_empty() {
        return Err(anyhow!("no tools specified to prepare"));
    }
    let (managed, host) = classify_prepare_tools(&tools)
        .map_err(|reasons| anyhow!("these tools cannot be prepared: {}", reasons.join("; ")))?;

    let mut failures = Vec::new();

    // Managed language runtimes reuse the existing install path verbatim, so
    // `prepare --tools node` does not drift from `install --tools node`.
    if !managed.is_empty()
        && let Err(err) = install_tools(managed, json)
    {
        failures.push(err.to_string());
    }

    let env = SystemPrepareEnv;
    for tool in host {
        emit_progress(tool, InstallPhase::Queued, "Queued", json);
        let reporter = StreamReporter { tool, json };
        // Only Podman is a host runtime today; `prepare_podman` is generic over
        // the env so it stays unit-testable without a real podman.
        if let Err(err) = prepare_podman(&env, &reporter) {
            emit_failure(tool, err.to_string(), err.is_retryable(), json);
            failures.push(format!("{}: {err}", tool.as_str()));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("runtime prepare failed: {}", failures.join("; ")))
    }
}

/// Split the requested tools into (managed-toolchain, host-runtime) groups, or
/// return actionable reasons for any tool that cannot be prepared. Pure so the
/// routing policy is unit-testable.
fn classify_prepare_tools(
    tools: &[ToolKind],
) -> Result<(Vec<ToolKind>, Vec<ToolKind>), Vec<String>> {
    let mut managed = Vec::new();
    let mut host = Vec::new();
    let mut rejected = Vec::new();
    for &tool in tools {
        match tool.install_strategy() {
            InstallStrategy::ManagedToolchain => managed.push(tool),
            InstallStrategy::HostRuntime => host.push(tool),
            InstallStrategy::DetectionOnly => rejected.push(format!(
                "{} is detection-only; Ato never installs it",
                tool.as_str()
            )),
            InstallStrategy::Bundled => rejected.push(format!(
                "{} ships inside the Ato bundle and cannot be prepared",
                tool.as_str()
            )),
        }
    }
    if rejected.is_empty() {
        Ok((managed, host))
    } else {
        Err(rejected)
    }
}

/// Typed failures from a host-runtime prepare. Carries actionable messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrepareError {
    /// `ATO_PODMAN_BIN` was set but unusable — fail hard, never substitute.
    InvalidOverride(String),
    /// Podman is missing and Ato could not install it here (no strategy
    /// succeeded); carries Homebrew-free, actionable instructions.
    InstallUnavailable(String),
    /// The Ato-managed download failed with a *transient* error (e.g. repeated
    /// 504 from the release CDN) after retries. Kept distinct from
    /// `InstallUnavailable` so the UI can offer a Retry instead of telling the
    /// user to install Podman manually. Carries the actionable message.
    TransientRuntimeDownload(String),
    /// After installing, Podman still could not be resolved (likely a PATH
    /// refresh / restart is required).
    StillMissingAfterInstall(String),
    /// `podman machine list` failed or was unparseable.
    MachineQueryFailed(String),
    /// The Ato-managed Podman is missing a required `podman machine` helper
    /// binary (e.g. `gvproxy`, `vfkit`) and a self-repair reinstall did not (or
    /// could not) complete it, or it was mapped from a `could not find "gvproxy"`
    /// machine error. Surfaces a typed, actionable diagnostic instead of an
    /// opaque failure. This is an Ato packaging/runtime issue, not a user
    /// Homebrew/git issue.
    RuntimeProviderIncomplete { helper: String },
    /// `podman machine init ato-podman` failed.
    MachineInitFailed(String),
    /// `podman machine start ato-podman` failed.
    MachineStartFailed(String),
    /// `podman machine stop ato-podman` failed (repair flow, #460).
    MachineStopFailed(String),
    /// `podman info` verification failed after preparation.
    VerifyFailed(String),
    /// Podman preparation is not supported on this platform.
    Unsupported(String),
    /// Podman installed fine, but this host cannot start a Podman *machine*
    /// because the platform virtualization backend is unavailable — e.g. macOS
    /// `vfkit` is present and runnable but Apple's Virtualization.framework is
    /// not usable (no `kern.hv_support`, as in a nested/virtual-macOS environment
    /// without nested virtualization). This is an environment limitation, not an
    /// Ato packaging bug; carries the optional underlying message.
    RuntimeVirtualizationUnavailable(Option<String>),
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOverride(m) => write!(f, "{m}"),
            Self::InstallUnavailable(m) => write!(f, "{m}"),
            Self::TransientRuntimeDownload(m) => write!(f, "{m}"),
            Self::StillMissingAfterInstall(m) => write!(f, "{m}"),
            Self::MachineQueryFailed(m) => write!(f, "could not read podman machines: {m}"),
            Self::RuntimeProviderIncomplete { helper } => write!(
                f,
                "Ato-managed Podman is incomplete: required helper binary `{helper}` could not \
                 be installed, so the Podman machine runtime is not usable. This is an Ato \
                 packaging/runtime setup issue, not a user Homebrew/git issue."
            ),
            Self::MachineInitFailed(m) => {
                write!(
                    f,
                    "failed to create podman machine '{ATO_PODMAN_MACHINE_NAME}': {m}"
                )
            }
            Self::MachineStartFailed(m) => {
                write!(
                    f,
                    "failed to start podman machine '{ATO_PODMAN_MACHINE_NAME}': {m}"
                )
            }
            Self::MachineStopFailed(m) => {
                write!(
                    f,
                    "failed to stop podman machine '{ATO_PODMAN_MACHINE_NAME}': {m}"
                )
            }
            Self::VerifyFailed(m) => write!(f, "podman readiness check failed: {m}"),
            Self::Unsupported(m) => write!(f, "{m}"),
            Self::RuntimeVirtualizationUnavailable(detail) => {
                write!(
                    f,
                    "Podman installed successfully, but this macOS environment cannot start a \
                     Podman machine: hardware virtualization (Apple Virtualization.framework) is \
                     not available here. This usually means a virtual macOS without nested \
                     virtualization. Run on a physical Mac, or a VM host that exposes nested \
                     virtualization, then re-run runtime setup."
                )?;
                if let Some(detail) = detail {
                    write!(f, " (underlying error: {detail})")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PrepareError {}

impl PrepareError {
    /// Whether this failure is a transient condition the user can simply retry
    /// (vs. a dead end needing manual action). Drives the `retryable` flag on the
    /// emitted progress so a UI can show a Retry action.
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(self, Self::TransientRuntimeDownload(_))
    }
}

/// Host platform for prepare decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreparePlatform {
    Macos,
    Windows,
    Linux,
    Other,
}

impl PreparePlatform {
    fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
}

/// Output of a prepare subprocess.
#[derive(Clone, Debug)]
struct CmdOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl CmdOutput {
    fn success(&self) -> bool {
        self.status == 0
    }

    /// stderr if non-empty, else stdout — trimmed — for diagnostics.
    fn message(&self) -> String {
        if self.stderr.trim().is_empty() {
            self.stdout.trim().to_string()
        } else {
            self.stderr.trim().to_string()
        }
    }
}

/// Receives prepare progress. The real implementation streams `InstallProgress`
/// events; tests record the phases.
trait PrepareReporter {
    fn phase(&self, phase: InstallPhase, message: &str);
}

/// Streams `InstallProgress` lines for one tool via the shared emitter.
struct StreamReporter {
    tool: ToolKind,
    json: bool,
}

impl PrepareReporter for StreamReporter {
    fn phase(&self, phase: InstallPhase, message: &str) {
        emit_progress(self.tool, phase, message, self.json);
    }
}

/// Host operations a prepare needs. Injected so the orchestration is testable
/// without a real podman, brew, or machine.
trait PrepareEnv {
    fn platform(&self) -> PreparePlatform;
    /// Resolve Podman (no spawn). Mirrors [`podman::resolve_podman`] semantics,
    /// including the hard failure on an invalid `ATO_PODMAN_BIN`.
    fn resolve_podman(&self) -> Result<ResolvedPodman, PodmanResolveError>;
    /// Run a podman subcommand (args after the resolved binary).
    fn run_podman(&self, args: &[&str]) -> std::io::Result<CmdOutput>;
    /// A usable Homebrew `brew` binary, if present (macOS).
    fn brew_bin(&self) -> Option<PathBuf>;
    /// Run `brew install podman`.
    fn run_brew_install_podman(&self, brew: &Path) -> std::io::Result<CmdOutput>;
    /// Host OS, in `std::env::consts::OS` spelling.
    fn host_os(&self) -> &str;
    /// Host arch, in `std::env::consts::ARCH` spelling.
    fn host_arch(&self) -> &str;
    /// Whether Ato has a pinned managed Podman build for this host. Demotes
    /// Homebrew to optional: a brew-less host with a managed build still
    /// installs.
    fn managed_podman_available(&self) -> bool {
        pinned_artifact(self.host_os(), self.host_arch()).is_some()
    }
    /// Download, digest-verify, and extract an Ato-managed Podman into the tools
    /// cache. Returns the typed [`PodmanInstallError`] on failure so callers can
    /// distinguish a transient download failure (retryable) from a permanent
    /// one; the error is already actionable and never an "install Homebrew" one.
    fn install_ato_managed_podman(&self) -> Result<(), PodmanInstallError>;
    /// Whether to skip the Homebrew strategy and force the Ato-managed verified-
    /// download path even when brew is present (clean-VM testing / opt-out of a
    /// brew-managed Podman). Abstracted here so the strategy loop stays testable
    /// instead of reading process env directly.
    fn force_managed_podman(&self) -> bool {
        false
    }
    /// Names of required `podman machine` helper binaries that are MISSING from
    /// the resolved Podman's helper dir. Empty when complete or not applicable
    /// (a non-Ato-managed Podman is trusted to bring its own helpers). Used as a
    /// preflight before `machine init/start` so an incomplete Ato-managed
    /// runtime fails with a typed error instead of an opaque `could not find
    /// "gvproxy"`.
    fn missing_machine_helpers(&self) -> Vec<String> {
        Vec::new()
    }
    /// Whether this host can actually start a Podman *machine* — i.e. the
    /// platform's hardware-virtualization backend is usable. On macOS this is
    /// `kern.hv_support == 1` (Apple Virtualization.framework / vfkit). Default
    /// `true` so non-macOS hosts and tests are unaffected; only a host that can
    /// definitively answer "no" should return false, so we never block a host we
    /// can't assess.
    fn host_virtualization_available(&self) -> bool {
        true
    }
}

/// Real host environment: spawns processes, resolving podman through PR #436's
/// resolver so GUI-launched (minimal-PATH) invocations still find it.
struct SystemPrepareEnv;

impl PrepareEnv for SystemPrepareEnv {
    fn platform(&self) -> PreparePlatform {
        PreparePlatform::current()
    }

    fn resolve_podman(&self) -> Result<ResolvedPodman, PodmanResolveError> {
        podman::resolve_podman()
    }

    fn run_podman(&self, args: &[&str]) -> std::io::Result<CmdOutput> {
        let invocation = podman::podman_invocation();
        let mut command = Command::new(&invocation.program);
        if let Some(path_env) = &invocation.path_env {
            command.env("PATH", path_env);
        }
        // Point an Ato-managed Podman at its bundled gvproxy/vfkit helpers so
        // `podman machine init/start` works without Homebrew/system search paths.
        if let Some(containers_conf) = &invocation.containers_conf {
            command.env("CONTAINERS_CONF", containers_conf);
        }
        let output = command.args(args).output()?;
        Ok(CmdOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn brew_bin(&self) -> Option<PathBuf> {
        resolve_brew()
    }

    fn run_brew_install_podman(&self, brew: &Path) -> std::io::Result<CmdOutput> {
        let output = Command::new(brew).args(["install", "podman"]).output()?;
        Ok(CmdOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    fn host_os(&self) -> &str {
        std::env::consts::OS
    }

    fn host_arch(&self) -> &str {
        std::env::consts::ARCH
    }

    fn install_ato_managed_podman(&self) -> Result<(), PodmanInstallError> {
        let tools_dir = capsule_core::common::paths::ato_tools_dir().map_err(|err| {
            PodmanInstallError::Extract { message: err.to_string() }
        })?;
        std::fs::create_dir_all(&tools_dir).map_err(|err| PodmanInstallError::Extract {
            message: err.to_string(),
        })?;
        let fetcher = ReqwestArtifactFetcher;
        install_ato_managed_podman(&fetcher, self.host_os(), self.host_arch(), &tools_dir)
            .map(|_| ())
    }

    fn force_managed_podman(&self) -> bool {
        std::env::var_os("ATO_FORCE_MANAGED_PODMAN").is_some()
    }

    fn missing_machine_helpers(&self) -> Vec<String> {
        // Only meaningful for an Ato-managed install (we know its layout). If
        // podman can't be resolved, the install path handles that earlier; treat
        // it as "nothing to police" here.
        match podman::resolve_podman() {
            Ok(resolved) => {
                missing_helpers_for(&resolved.bin, self.host_os(), self.host_arch())
            }
            Err(_) => Vec::new(),
        }
    }

    fn host_virtualization_available(&self) -> bool {
        host_virtualization_available()
    }
}

/// Whether hardware virtualization is usable on this host. On macOS reads
/// `sysctl -n kern.hv_support` (1 = usable). On any non-macOS host, or if the
/// probe can't run / is unparseable, returns `true` so we never block a host we
/// cannot definitively assess (the actual `machine init/start` still guards us).
fn host_virtualization_available() -> bool {
    if std::env::consts::OS != "macos" {
        return true;
    }
    match Command::new("sysctl").args(["-n", "kern.hv_support"]).output() {
        Ok(out) if out.status.success() => {
            // "1" = supported, "0" = not. Anything unexpected → don't block.
            String::from_utf8_lossy(&out.stdout).trim() != "0"
        }
        _ => true,
    }
}

/// Locate a Homebrew `brew` binary: the two standard prefixes first (a GUI
/// launch may not have them on PATH), then `PATH`. Each candidate must be an
/// executable file, not merely present.
fn resolve_brew() -> Option<PathBuf> {
    for candidate in ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"] {
        let path = PathBuf::from(candidate);
        if is_executable_file(&path) {
            return Some(path);
        }
    }
    which::which("brew").ok()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// What to do with the Podman machine before verifying.
#[derive(Debug, PartialEq, Eq)]
enum MachinePlan {
    /// The Ato machine is already running → verify it explicitly.
    UseAto,
    /// The Ato machine exists but is stopped → start it (no init), verify it.
    StartAto,
    /// No usable Ato machine → create and start it, then verify it.
    InitAndStartAto,
    /// No Ato machine, but exactly one other machine is already running → use it
    /// through the default connection. Do not create or mutate anything.
    UseDefault,
}

impl MachinePlan {
    /// The connection `verify` (and, later, the provider) must target.
    ///
    /// Whenever Ato owns/creates the machine, verification is pinned to it by
    /// connection name (which Podman names after the machine). This is the crux:
    /// without it, `podman info` would follow the *global default* connection,
    /// so on a host whose default points at a different machine we could start
    /// `ato-podman` yet verify (or run capsules against) the wrong one. Ato never
    /// changes the global default, so it must address its machine explicitly.
    /// An existing user machine is verified through the default it already owns.
    fn verify_connection(&self) -> Option<&'static str> {
        match self {
            Self::UseAto | Self::StartAto | Self::InitAndStartAto => Some(ATO_PODMAN_MACHINE_NAME),
            Self::UseDefault => None,
        }
    }
}

/// Decide the machine action from the current machine list. Pure.
///
/// Policy: only ever mutate the Ato-managed machine. Never start, stop, or
/// reconfigure a user's own machine, and never change the global default
/// connection.
fn plan_machine(entries: &[PodmanMachine]) -> MachinePlan {
    if let Some(ato) = entries.iter().find(|m| m.name == ATO_PODMAN_MACHINE_NAME) {
        return if ato.running {
            MachinePlan::UseAto
        } else {
            MachinePlan::StartAto
        };
    }
    // No Ato machine. If exactly one (non-Ato) machine is already running, treat
    // it as usable rather than creating a redundant Ato machine. Otherwise
    // (nothing running, or an ambiguous multi-machine state) create our own.
    let running = entries.iter().filter(|m| m.running).count();
    if running == 1 {
        MachinePlan::UseDefault
    } else {
        MachinePlan::InitAndStartAto
    }
}

/// Prepare Podman end-to-end: resolve (install if missing & opted-in), set up
/// the Ato machine when the platform needs one, and verify with `podman info`.
fn prepare_podman<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    reporter.phase(InstallPhase::Locating, "Locating Podman…");

    let _resolved = match env.resolve_podman() {
        Ok(resolved) => resolved,
        Err(PodmanResolveError::InvalidEnvOverride { path }) => {
            return Err(invalid_override(&path));
        }
        Err(PodmanResolveError::NotFound { .. }) => {
            install_podman(env, reporter)?;
            // Re-resolve: a successful install must produce a resolvable binary.
            match env.resolve_podman() {
                Ok(resolved) => resolved,
                Err(PodmanResolveError::InvalidEnvOverride { path }) => {
                    return Err(invalid_override(&path));
                }
                Err(PodmanResolveError::NotFound { .. }) => {
                    return Err(PrepareError::StillMissingAfterInstall(
                        still_missing_message(env.platform()),
                    ));
                }
            }
        }
    };

    // The connection to verify against: the Ato machine when Ato owns it
    // (explicit, default-independent), or the default for native Linux / an
    // existing running user machine.
    let verify_connection = match env.platform() {
        // Native Linux Podman needs no machine; verify the default.
        PreparePlatform::Linux => None,
        PreparePlatform::Macos | PreparePlatform::Windows => prepare_machine(env, reporter)?,
        PreparePlatform::Other => {
            return Err(PrepareError::Unsupported(
                "Podman preparation is not supported on this platform".to_string(),
            ));
        }
    };

    verify(env, reporter, verify_connection)?;
    reporter.phase(InstallPhase::Ready, "Podman is ready");
    Ok(())
}

fn invalid_override(path: &Path) -> PrepareError {
    PrepareError::InvalidOverride(format!(
        "ATO_PODMAN_BIN points at '{}', which is not a usable executable; \
         fix or unset it before preparing Podman",
        path.display()
    ))
}

/// Install Podman after explicit opt-in.
///
/// macOS iterates the ordered strategies from [`install_strategies`]: an
/// already-present Homebrew first, then the **Ato-managed installer**
/// (download + digest-verify + extract into `~/.ato/tools`), then actionable
/// manual instructions. A missing Homebrew is **not** an error — it simply
/// falls through to the Ato-managed path so a clean VM with no brew still gets
/// Podman. The manual instructions never tell the user to install Homebrew.
///
/// Windows/Linux keep their package-manager instructions (they never required
/// Homebrew); an Ato-managed path for those targets is a documented follow-up.
fn install_podman<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    match env.platform() {
        PreparePlatform::Macos => install_podman_macos(env, reporter),
        PreparePlatform::Windows => Err(PrepareError::InstallUnavailable(
            "Podman is not installed. Install it (e.g. `winget install RedHat.Podman`) and \
             re-run; a sign-out/restart may be required before the CLI is visible."
                .to_string(),
        )),
        PreparePlatform::Linux => Err(PrepareError::InstallUnavailable(
            "Podman is not installed. Install it with your package manager (e.g. \
             `sudo apt install podman` or `sudo dnf install podman`) and re-run."
                .to_string(),
        )),
        PreparePlatform::Other => Err(PrepareError::Unsupported(
            "Podman preparation is not supported on this platform".to_string(),
        )),
    }
}

/// macOS strategy loop. Tries each strategy in order; a strategy that is merely
/// *unavailable* (e.g. brew absent) is skipped, while a strategy that *runs and
/// fails* records its error. Only when every strategy is exhausted is the
/// accumulated, brew-free actionable message returned.
fn install_podman_macos<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    // Forcing the managed path skips Homebrew even when brew is present (clean-VM
    // testing / opt-out of a brew-managed Podman). The decision is on `PrepareEnv`
    // so the strategy loop stays unit-testable instead of reading process env.
    let brew_present = env.brew_bin().is_some() && !env.force_managed_podman();
    let managed_available = env.managed_podman_available();
    let mut attempt_errors: Vec<String> = Vec::new();

    for strategy in install_strategies(brew_present, managed_available) {
        match strategy {
            PodmanInstallStrategy::Homebrew => {
                // Only reached when brew is present.
                let Some(brew) = env.brew_bin() else { continue };
                reporter.phase(InstallPhase::Installing, "Installing Podman via Homebrew…");
                match env.run_brew_install_podman(&brew) {
                    Ok(out) if out.success() => return Ok(()),
                    Ok(out) => attempt_errors.push(format!("Homebrew: {}", out.message())),
                    Err(err) => attempt_errors.push(format!("Homebrew: {err}")),
                }
            }
            PodmanInstallStrategy::AtoManaged => {
                reporter.phase(
                    InstallPhase::Downloading,
                    "Downloading a verified Podman build (no Homebrew required)…",
                );
                match env.install_ato_managed_podman() {
                    Ok(()) => return Ok(()),
                    // A transient download failure (e.g. repeated 504) is retryable
                    // — surface it structurally instead of collapsing it into the
                    // generic "install Podman manually" message. AtoManaged is the
                    // last install strategy before ManualInstructions, so there is
                    // nothing else to try anyway.
                    Err(err @ PodmanInstallError::TransientDownloadFailed { .. }) => {
                        return Err(PrepareError::TransientRuntimeDownload(err.to_string()));
                    }
                    Err(err) => attempt_errors.push(format!("Ato-managed install: {err}")),
                }
            }
            PodmanInstallStrategy::ManualInstructions => {
                return Err(PrepareError::InstallUnavailable(manual_install_message(
                    &attempt_errors,
                )));
            }
        }
    }

    // `install_strategies` always ends with ManualInstructions, so the loop
    // returns above; this is defensive.
    Err(PrepareError::InstallUnavailable(manual_install_message(
        &attempt_errors,
    )))
}

/// The last-resort, **Homebrew-free** instruction. Surfaces any earlier
/// strategy failures so the user can see *why* auto-install did not work, then
/// points at the official installer — never at `brew.sh`.
fn manual_install_message(attempt_errors: &[String]) -> String {
    let mut msg = String::from(
        "Ato could not automatically install a local container runtime. Install Podman \
         manually from https://podman.io/docs/installation and re-run.",
    );
    if !attempt_errors.is_empty() {
        msg.push_str(" (attempts: ");
        msg.push_str(&attempt_errors.join("; "));
        msg.push(')');
    }
    msg
}

fn still_missing_message(platform: PreparePlatform) -> String {
    match platform {
        PreparePlatform::Windows => "Podman was installed but is not yet visible. A sign-out or \
             restart may be required to refresh PATH; then re-run prepare."
            .to_string(),
        _ => "Podman was installed but could not be resolved afterward. Re-run prepare; if it \
             persists, check the install location."
            .to_string(),
    }
}

/// Set up the Ato-managed machine per [`plan_machine`], emitting the relevant
/// phases. Only the `ato-podman` machine is ever created/started. Returns the
/// connection that [`verify`] (and later the provider) must target.
fn prepare_machine<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<Option<&'static str>, PrepareError> {
    let list = env
        .run_podman(&["machine", "list", "--format", "json"])
        .map_err(|err| PrepareError::MachineQueryFailed(err.to_string()))?;
    if !list.success() {
        return Err(PrepareError::MachineQueryFailed(list.message()));
    }
    let entries = parse_machine_entries(&list.stdout).map_err(PrepareError::MachineQueryFailed)?;

    let plan = plan_machine(&entries);
    match plan {
        MachinePlan::UseAto | MachinePlan::UseDefault => {}
        MachinePlan::StartAto => {
            ensure_machine_helpers(env, reporter)?;
            ensure_virtualization_available(env)?;
            start_ato_machine(env, reporter)?;
        }
        MachinePlan::InitAndStartAto => {
            // Make the runtime complete BEFORE `machine init` so a fresh VM (or a
            // partial #577-era install) reaches a working machine, not an opaque
            // mid-init `could not find "gvproxy"`.
            ensure_machine_helpers(env, reporter)?;
            // Then make sure the host can actually boot a VM before we try —
            // otherwise a virtual macOS without nested virtualization fails deep
            // inside `machine init` (vfkit can't reach Virtualization.framework)
            // and the cause is easy to misread as an Ato packaging bug.
            ensure_virtualization_available(env)?;
            init_ato_machine(env, reporter)?;
            start_ato_machine(env, reporter)?;
        }
    }
    Ok(plan.verify_connection())
}

/// Ensure the resolved Podman has the `podman machine` helpers it needs,
/// **self-repairing** an incomplete Ato-managed install rather than dead-ending
/// the user.
///
/// A non-Ato-managed Podman never reports missing helpers (it is trusted to
/// bring its own), so this is a no-op there. For an Ato-managed install that is
/// missing helpers — exactly the #577 clean-VM state where only
/// `~/.ato/tools/podman-<ver>/usr/bin/podman` exists — re-running runtime setup
/// resolves the *same* incomplete binary and would otherwise loop on the same
/// error. So we reinstall the full bundle (download + digest-verify
/// podman + gvproxy + vfkit, atomically promoted), then re-check. Only if the
/// reinstall fails to produce a complete runtime do we surface a typed error.
fn ensure_machine_helpers<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    if env.missing_machine_helpers().is_empty() {
        return Ok(());
    }

    // `missing_machine_helpers` only reports for an Ato-managed install, and any
    // host with such an install has a pinned managed artifact — so a reinstall
    // is available. (The guard keeps us honest if that ever stops holding.)
    if !env.managed_podman_available() {
        let helper = env
            .missing_machine_helpers()
            .into_iter()
            .next()
            .unwrap_or_else(|| "gvproxy".to_string());
        return Err(PrepareError::RuntimeProviderIncomplete { helper });
    }

    reporter.phase(
        InstallPhase::Downloading,
        "Completing the Podman machine runtime (downloading verified helpers)…",
    );
    // Reinstall the full bundle in place. A transient download failure stays
    // retryable; any other failure means we genuinely could not complete the
    // runtime — report that as such, not as "re-run setup".
    env.install_ato_managed_podman().map_err(|e| match e {
        PodmanInstallError::TransientDownloadFailed { .. } => {
            PrepareError::TransientRuntimeDownload(e.to_string())
        }
        other => PrepareError::InstallUnavailable(other.to_string()),
    })?;

    // Re-resolve + re-check (each env call resolves fresh) — the freshly written
    // containers.conf is now in place too, so subsequent podman invocations pick
    // up the helper dir.
    if let Some(helper) = env.missing_machine_helpers().into_iter().next() {
        return Err(PrepareError::RuntimeProviderIncomplete { helper });
    }
    Ok(())
}

/// Map a `podman machine` error message that names a missing helper binary
/// (`could not find "gvproxy"`) to the typed runtime-incomplete category, so it
/// never surfaces as an opaque generic failure. Returns the helper name when the
/// message matches.
fn helper_name_in_machine_error(message: &str) -> Option<String> {
    let lower = message.to_lowercase();
    if !lower.contains("could not find") && !lower.contains("not find") {
        return None;
    }
    ["gvproxy", "vfkit"]
        .into_iter()
        .find(|helper| lower.contains(*helper))
        .map(|helper| helper.to_string())
}

/// Preflight: refuse to attempt `machine init/start` when the host's hardware
/// virtualization backend is unavailable, so a virtual-macOS-without-nested-virt
/// environment gets a clear, typed [`PrepareError::RuntimeVirtualizationUnavailable`]
/// instead of an opaque mid-init vfkit failure that reads like a packaging bug.
fn ensure_virtualization_available<E: PrepareEnv>(env: &E) -> Result<(), PrepareError> {
    if env.host_virtualization_available() {
        return Ok(());
    }
    Err(PrepareError::RuntimeVirtualizationUnavailable(None))
}

/// Whether a `podman machine` error indicates the platform virtualization layer
/// is the problem (vfkit/Virtualization.framework/hypervisor) rather than a
/// missing helper or generic failure. Defense-in-depth behind the preflight: a
/// host can report `kern.hv_support == 1` yet still fail to boot a VM (e.g.
/// restricted nested virt), and that should still read as an environment limit.
fn is_virtualization_machine_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    // vfkit/VZ-specific and generic virtualization-unavailable markers. Guard
    // against the helper-missing case (handled separately) so a `could not find
    // "vfkit"` packaging error is NOT misclassified as a virtualization limit.
    if lower.contains("could not find") || lower.contains("not find") {
        return false;
    }
    [
        "virtualization.framework",
        "hv_support",
        "hypervisor",
        "vz_error",
        "vzerror",
        "operation not permitted",
        "unsupported",
        "failed to start vm",
        "nested virtual",
        "no such hypervisor",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        // vfkit named together with a failure (but not a "could not find" miss).
        || (lower.contains("vfkit") && (lower.contains("exited") || lower.contains("failed")))
}

/// Map a failed `machine init/start` message to the most specific typed error:
/// a missing helper → [`PrepareError::RuntimeProviderIncomplete`]; a
/// virtualization-backend problem → [`PrepareError::RuntimeVirtualizationUnavailable`];
/// otherwise `fallback` (init- or start-specific).
fn classify_machine_error(
    message: String,
    fallback: impl FnOnce(String) -> PrepareError,
) -> PrepareError {
    if let Some(helper) = helper_name_in_machine_error(&message) {
        return PrepareError::RuntimeProviderIncomplete { helper };
    }
    if is_virtualization_machine_error(&message) {
        return PrepareError::RuntimeVirtualizationUnavailable(Some(message));
    }
    fallback(message)
}

fn init_ato_machine<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    reporter.phase(
        InstallPhase::InitializingMachine,
        &format!("Creating Podman machine '{ATO_PODMAN_MACHINE_NAME}'…"),
    );
    let out = env
        .run_podman(&["machine", "init", ATO_PODMAN_MACHINE_NAME])
        .map_err(|err| PrepareError::MachineInitFailed(err.to_string()))?;
    if !out.success() {
        return Err(classify_machine_error(
            out.message(),
            PrepareError::MachineInitFailed,
        ));
    }
    Ok(())
}

fn start_ato_machine<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    reporter.phase(
        InstallPhase::StartingMachine,
        &format!("Starting Podman machine '{ATO_PODMAN_MACHINE_NAME}'…"),
    );
    let out = env
        .run_podman(&["machine", "start", ATO_PODMAN_MACHINE_NAME])
        .map_err(|err| PrepareError::MachineStartFailed(err.to_string()))?;
    if !out.success() {
        return Err(classify_machine_error(
            out.message(),
            PrepareError::MachineStartFailed,
        ));
    }
    Ok(())
}

/// Verify readiness with `podman info`, pinned to `connection` when set so the
/// Ato machine is checked regardless of the host's global default connection.
fn verify<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
    connection: Option<&str>,
) -> Result<(), PrepareError> {
    reporter.phase(InstallPhase::Verifying, "Verifying Podman readiness…");
    let mut args: Vec<&str> = Vec::new();
    if let Some(connection) = connection {
        // Global flag — must precede the subcommand.
        args.push("--connection");
        args.push(connection);
    }
    args.extend_from_slice(&["info", "--format", "json"]);
    let out = env
        .run_podman(&args)
        .map_err(|err| PrepareError::VerifyFailed(err.to_string()))?;
    if !out.success() {
        return Err(PrepareError::VerifyFailed(out.message()));
    }
    Ok(())
}

/// Public entry for `ato internal runtime repair-host-runtime [--emit-json]`
/// (#460 PR2). Restart-and-verify the Ato-managed Podman machine — the
/// remediation for the "machine running but `podman info` fails" health-error
/// state. Only the `ato-podman` machine is ever touched.
pub(crate) fn repair_host_runtime(json: bool) -> Result<()> {
    let env = SystemPrepareEnv;
    let reporter = StreamReporter {
        tool: ToolKind::Podman,
        json,
    };
    emit_progress(ToolKind::Podman, InstallPhase::Queued, "Queued", json);
    match repair_ato_machine(&env, &reporter) {
        Ok(()) => Ok(()),
        Err(err) => {
            emit_progress(
                ToolKind::Podman,
                InstallPhase::Failed,
                err.to_string(),
                json,
            );
            Err(anyhow!("podman machine repair failed: {err}"))
        }
    }
}

/// Restart and re-verify the Ato-managed Podman machine (#460).
///
/// Repairs the health-error state (machine running but `podman info` fails) with
/// the least-destructive action: `machine stop ato-podman` → `machine start
/// ato-podman` → `info --connection ato-podman`. **Only `ato-podman` is
/// touched** — a user's own machine and the global default connection are never
/// stopped or reconfigured. Recreating the machine (destructive) is intentionally
/// out of scope here and is a separate, confirmation-gated follow-up.
fn repair_ato_machine<E: PrepareEnv, R: PrepareReporter>(
    env: &E,
    reporter: &R,
) -> Result<(), PrepareError> {
    if matches!(env.platform(), PreparePlatform::Linux) {
        // Native Linux Podman has no machine; "repair" is just re-verify.
        return verify(env, reporter, None);
    }

    reporter.phase(
        InstallPhase::StartingMachine,
        &format!("Restarting Podman machine '{ATO_PODMAN_MACHINE_NAME}'…"),
    );
    let stop = env
        .run_podman(&["machine", "stop", ATO_PODMAN_MACHINE_NAME])
        .map_err(|err| PrepareError::MachineStopFailed(err.to_string()))?;
    // A machine that is already stopped is fine to "stop" — only surface a hard
    // failure that is not the already-stopped case.
    if !stop.success() && !machine_already_stopped(&stop.message()) {
        return Err(PrepareError::MachineStopFailed(stop.message()));
    }

    start_ato_machine(env, reporter)?;
    verify(env, reporter, Some(ATO_PODMAN_MACHINE_NAME))?;
    reporter.phase(InstallPhase::Ready, "Podman machine repaired");
    Ok(())
}

/// Whether a `machine stop` error message indicates the machine was already
/// stopped (a benign no-op for the repair flow).
fn machine_already_stopped(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("already stopped") || lower.contains("not running")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::podman::PodmanBinarySource;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn resolved() -> ResolvedPodman {
        ResolvedPodman {
            bin: PathBuf::from("/opt/homebrew/bin/podman"),
            source: PodmanBinarySource::KnownLocation,
            version: None,
        }
    }

    /// Deterministic [`PrepareEnv`] that records every command invoked.
    struct FakeEnv {
        platform: PreparePlatform,
        /// Resolution results consumed front-to-back (initial, then re-resolve).
        resolves: RefCell<Vec<Result<ResolvedPodman, PodmanResolveError>>>,
        /// `podman <args joined>` → output.
        podman: HashMap<String, CmdOutput>,
        brew: Option<PathBuf>,
        brew_install: Option<CmdOutput>,
        /// Whether a pinned Ato-managed Podman build exists for this fake host.
        managed_available: bool,
        /// Result of the Ato-managed install (`Ok` = success). `None` means the
        /// strategy is never expected to run in this test.
        managed_install: Option<Result<(), PodmanInstallError>>,
        /// Force the managed path (skip Homebrew even when brew is present).
        force_managed: bool,
        /// Whether the fake host's virtualization backend is usable. Default true
        /// so existing machine tests are unaffected.
        virtualization_available: bool,
        /// Helper binaries the resolved Podman is missing (drives the machine
        /// preflight/repair). Empty = complete runtime. A successful Ato-managed
        /// (re)install clears it, simulating the bundle's helpers being placed.
        missing_helpers: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeEnv {
        fn new(platform: PreparePlatform) -> Self {
            FakeEnv {
                platform,
                resolves: RefCell::new(vec![Ok(resolved())]),
                podman: HashMap::new(),
                brew: None,
                brew_install: None,
                managed_available: false,
                managed_install: None,
                force_managed: false,
                virtualization_available: true,
                missing_helpers: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        /// Model a host whose virtualization backend is unavailable (e.g. a
        /// virtual macOS without nested virtualization).
        fn with_virtualization_available(mut self, available: bool) -> Self {
            self.virtualization_available = available;
            self
        }

        /// Configure the machine-helper preflight/repair: which required helpers
        /// the resolved Podman is missing (empty = complete). A successful
        /// managed (re)install clears this, modelling the bundle's helpers being
        /// installed.
        fn with_missing_helpers(self, helpers: &[&str]) -> Self {
            *self.missing_helpers.borrow_mut() = helpers.iter().map(|h| h.to_string()).collect();
            self
        }

        /// Configure the Ato-managed installer: whether a build is pinned for
        /// the host and, when run, whether it succeeds.
        fn with_managed(
            mut self,
            available: bool,
            result: Option<Result<(), PodmanInstallError>>,
        ) -> Self {
            self.managed_available = available;
            self.managed_install = result;
            self
        }

        /// Force the Ato-managed path even when brew is present.
        fn with_force_managed(mut self, force: bool) -> Self {
            self.force_managed = force;
            self
        }

        fn with_resolves(
            mut self,
            results: Vec<Result<ResolvedPodman, PodmanResolveError>>,
        ) -> Self {
            self.resolves = RefCell::new(results);
            self
        }

        fn with_podman(mut self, args: &str, status: i32, stdout: &str) -> Self {
            self.podman.insert(
                args.to_string(),
                CmdOutput {
                    status,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
            );
            self
        }

        fn with_brew(mut self, brew: Option<&str>, install_status: Option<i32>) -> Self {
            self.brew = brew.map(PathBuf::from);
            self.brew_install = install_status.map(|status| CmdOutput {
                status,
                stdout: String::new(),
                stderr: String::new(),
            });
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl PrepareEnv for FakeEnv {
        fn platform(&self) -> PreparePlatform {
            self.platform
        }

        fn resolve_podman(&self) -> Result<ResolvedPodman, PodmanResolveError> {
            let mut resolves = self.resolves.borrow_mut();
            if resolves.len() > 1 {
                resolves.remove(0)
            } else {
                resolves[0].clone()
            }
        }

        fn run_podman(&self, args: &[&str]) -> std::io::Result<CmdOutput> {
            let key = args.join(" ");
            self.calls.borrow_mut().push(format!("podman {key}"));
            self.podman.get(&key).cloned().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no fake podman output for: {key}"),
                )
            })
        }

        fn brew_bin(&self) -> Option<PathBuf> {
            self.brew.clone()
        }

        fn run_brew_install_podman(&self, brew: &Path) -> std::io::Result<CmdOutput> {
            self.calls
                .borrow_mut()
                .push(format!("brew install podman ({})", brew.display()));
            self.brew_install.clone().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "brew install not configured")
            })
        }

        fn host_os(&self) -> &str {
            // Map the fake platform to a `std::env::consts::OS` spelling so the
            // managed-availability default would behave; overridden directly.
            match self.platform {
                PreparePlatform::Macos => "macos",
                PreparePlatform::Windows => "windows",
                PreparePlatform::Linux => "linux",
                PreparePlatform::Other => "other",
            }
        }

        fn host_arch(&self) -> &str {
            "aarch64"
        }

        fn managed_podman_available(&self) -> bool {
            self.managed_available
        }

        fn install_ato_managed_podman(&self) -> Result<(), PodmanInstallError> {
            self.calls
                .borrow_mut()
                .push("ato-managed install podman".to_string());
            let result = self.managed_install.clone().unwrap_or_else(|| {
                Err(PodmanInstallError::Extract {
                    message: "ato-managed install not configured".to_string(),
                })
            });
            // A successful (re)install places the bundled helpers — model that by
            // clearing the missing set so the post-repair re-check passes.
            if result.is_ok() {
                self.missing_helpers.borrow_mut().clear();
            }
            result
        }

        fn force_managed_podman(&self) -> bool {
            self.force_managed
        }

        fn missing_machine_helpers(&self) -> Vec<String> {
            self.missing_helpers.borrow().clone()
        }

        fn host_virtualization_available(&self) -> bool {
            self.virtualization_available
        }
    }

    /// Records emitted phases for assertions.
    #[derive(Default)]
    struct RecordingReporter {
        phases: RefCell<Vec<InstallPhase>>,
    }

    impl RecordingReporter {
        fn phases(&self) -> Vec<InstallPhase> {
            self.phases.borrow().clone()
        }
    }

    impl PrepareReporter for RecordingReporter {
        fn phase(&self, phase: InstallPhase, _message: &str) {
            self.phases.borrow_mut().push(phase);
        }
    }

    // ── plan_machine ────────────────────────────────────────────────────────

    fn machine(name: &str, running: bool) -> PodmanMachine {
        PodmanMachine {
            name: name.to_string(),
            running,
        }
    }

    #[test]
    fn plan_no_machine_inits_and_starts() {
        assert_eq!(plan_machine(&[]), MachinePlan::InitAndStartAto);
    }

    #[test]
    fn plan_ato_stopped_starts_only() {
        assert_eq!(
            plan_machine(&[machine(ATO_PODMAN_MACHINE_NAME, false)]),
            MachinePlan::StartAto
        );
    }

    #[test]
    fn plan_ato_running_uses_ato() {
        let plan = plan_machine(&[machine(ATO_PODMAN_MACHINE_NAME, true)]);
        assert_eq!(plan, MachinePlan::UseAto);
        // The Ato machine is verified explicitly, never via the global default.
        assert_eq!(plan.verify_connection(), Some(ATO_PODMAN_MACHINE_NAME));
    }

    #[test]
    fn plan_single_running_non_ato_uses_default() {
        let plan = plan_machine(&[machine("podman-machine-default", true)]);
        assert_eq!(plan, MachinePlan::UseDefault);
        assert_eq!(plan.verify_connection(), None);
    }

    #[test]
    fn ato_plans_verify_the_ato_connection() {
        for plan in [
            MachinePlan::UseAto,
            MachinePlan::StartAto,
            MachinePlan::InitAndStartAto,
        ] {
            assert_eq!(plan.verify_connection(), Some(ATO_PODMAN_MACHINE_NAME));
        }
    }

    #[test]
    fn plan_multiple_stopped_non_ato_creates_ato() {
        assert_eq!(
            plan_machine(&[machine("a", false), machine("b", false)]),
            MachinePlan::InitAndStartAto
        );
    }

    #[test]
    fn plan_multiple_running_non_ato_creates_ato() {
        // Ambiguous (>1 running, no Ato machine): be deterministic, make our own.
        assert_eq!(
            plan_machine(&[machine("a", true), machine("b", true)]),
            MachinePlan::InitAndStartAto
        );
    }

    // ── prepare_podman ──────────────────────────────────────────────────────

    #[test]
    fn invalid_override_fails_hard_without_spawning() {
        let env = FakeEnv::new(PreparePlatform::Macos).with_resolves(vec![Err(
            PodmanResolveError::InvalidEnvOverride {
                path: PathBuf::from("/stale/podman"),
            },
        )]);
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("invalid override must fail");
        assert!(matches!(err, PrepareError::InvalidOverride(_)), "{err:?}");
        assert!(
            env.calls().is_empty(),
            "must not spawn anything: {:?}",
            env.calls()
        );
    }

    #[test]
    fn missing_podman_without_brew_or_managed_is_actionable_not_brew() {
        // No brew AND no managed build for this host: the only path left is the
        // manual instruction. It must be actionable and must NOT tell the user
        // to install Homebrew.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![Err(PodmanResolveError::NotFound {
                searched: Vec::new(),
            })])
            .with_brew(None, None)
            .with_managed(false, None);
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("nothing available => unavailable");
        let PrepareError::InstallUnavailable(msg) = &err else {
            panic!("expected InstallUnavailable, got {err:?}");
        };
        assert!(
            !msg.to_lowercase().contains("homebrew") && !msg.contains("brew.sh"),
            "must not instruct installing Homebrew: {msg}"
        );
        assert!(
            msg.contains("podman.io"),
            "should point at the official installer: {msg}"
        );
    }

    #[test]
    fn install_podman_falls_through_to_ato_managed_when_brew_missing() {
        // The headline clean-VM case: no Homebrew, but a pinned managed build
        // exists. Ato must install it itself rather than erroring on brew.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![
                Err(PodmanResolveError::NotFound {
                    searched: Vec::new(),
                }),
                Ok(resolved()),
            ])
            .with_brew(None, None)
            .with_managed(true, Some(Ok(())))
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("prepares via Ato-managed install");
        let calls = env.calls();
        assert!(
            calls.contains(&"ato-managed install podman".to_string()),
            "must use the Ato-managed installer: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("brew install")),
            "must not attempt brew when absent: {calls:?}"
        );
        assert!(
            reporter.phases().contains(&InstallPhase::Downloading),
            "managed install should emit a Downloading phase: {:?}",
            reporter.phases()
        );
    }

    #[test]
    fn install_podman_prefers_brew_when_present() {
        // When brew IS present, it is tried first and the managed installer is
        // never invoked.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![
                Err(PodmanResolveError::NotFound {
                    searched: Vec::new(),
                }),
                Ok(resolved()),
            ])
            .with_brew(Some("/opt/homebrew/bin/brew"), Some(0))
            .with_managed(true, Some(Ok(())))
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("prepares via brew");
        let calls = env.calls();
        assert!(calls.iter().any(|c| c.starts_with("brew install podman")));
        assert!(
            !calls.contains(&"ato-managed install podman".to_string()),
            "managed installer must not run when brew succeeds: {calls:?}"
        );
    }

    #[test]
    fn install_podman_falls_through_to_managed_when_brew_install_fails() {
        // brew present but `brew install` fails => fall through to the managed
        // installer rather than surfacing a brew error.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![
                Err(PodmanResolveError::NotFound {
                    searched: Vec::new(),
                }),
                Ok(resolved()),
            ])
            .with_brew(Some("/opt/homebrew/bin/brew"), Some(1))
            .with_managed(true, Some(Ok(())))
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("falls through to managed");
        let calls = env.calls();
        assert!(calls.iter().any(|c| c.starts_with("brew install podman")));
        assert!(calls.contains(&"ato-managed install podman".to_string()));
    }

    #[test]
    fn missing_provider_surfaces_actionable_error_not_brew_instruction() {
        // End-to-end: a clean host where the managed install itself fails (e.g.
        // offline) must still surface an actionable, Homebrew-free error — the
        // Runtime Setup card / CLI renders this, never "install Homebrew".
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![Err(PodmanResolveError::NotFound {
                searched: Vec::new(),
            })])
            .with_brew(None, None)
            .with_managed(
                true,
                Some(Err(PodmanInstallError::Fetch {
                    url: "https://example.test/pkg".to_string(),
                    message: "network unreachable".to_string(),
                })),
            );
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("managed install failed");
        let PrepareError::InstallUnavailable(msg) = &err else {
            panic!("expected InstallUnavailable, got {err:?}");
        };
        assert!(
            !msg.to_lowercase().contains("homebrew") && !msg.contains("brew.sh"),
            "missing provider must never instruct installing Homebrew: {msg}"
        );
        // The attempted strategy's failure is surfaced so the user can act.
        assert!(
            msg.contains("network unreachable"),
            "should surface the attempt: {msg}"
        );
        assert!(
            msg.contains("podman.io"),
            "should point at official installer: {msg}"
        );
    }

    #[test]
    fn force_managed_skips_homebrew_even_when_present() {
        // brew IS present, but force-managed must skip it and use the Ato-managed
        // verified-download path instead.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![
                Err(PodmanResolveError::NotFound { searched: Vec::new() }),
                Ok(resolved()),
            ])
            .with_brew(Some("/opt/homebrew/bin/brew"), Some(0))
            .with_force_managed(true)
            .with_managed(true, Some(Ok(())))
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("force-managed install should prepare");

        let calls = env.calls();
        assert!(
            calls.iter().any(|c| c == "ato-managed install podman"),
            "managed path must run: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("brew install")),
            "Homebrew must be skipped when force-managed: {calls:?}"
        );
    }

    #[test]
    fn transient_managed_download_failure_is_typed_not_install_manually() {
        // A repeated-504-style transient download failure must surface as a
        // distinct, retryable PrepareError — not collapsed into the generic
        // "install Podman manually" InstallUnavailable.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![Err(PodmanResolveError::NotFound { searched: Vec::new() })])
            .with_brew(None, None)
            .with_managed(
                true,
                Some(Err(PodmanInstallError::TransientDownloadFailed {
                    url: "https://example.test/pkg".to_string(),
                    attempts: 4,
                    message: "HTTP 504 Gateway Timeout".to_string(),
                })),
            );
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("transient download must error");
        assert!(
            matches!(err, PrepareError::TransientRuntimeDownload(_)),
            "expected TransientRuntimeDownload, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("re-run runtime setup"),
            "must carry the retryable hint: {msg}"
        );
        assert!(
            !msg.contains("Install Podman manually"),
            "must NOT be the generic install-manually message: {msg}"
        );
    }

    #[test]
    fn missing_podman_installs_via_brew_then_prepares() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_resolves(vec![
                Err(PodmanResolveError::NotFound {
                    searched: Vec::new(),
                }),
                Ok(resolved()),
            ])
            .with_brew(Some("/opt/homebrew/bin/brew"), Some(0))
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("prepares after install");
        let calls = env.calls();
        assert!(calls.iter().any(|c| c.starts_with("brew install podman")));
        assert!(calls.contains(&"podman machine init ato-podman".to_string()));
        assert!(calls.contains(&"podman machine start ato-podman".to_string()));
        // Verify is pinned to the Ato machine, not the global default.
        assert!(
            calls.contains(&"podman --connection ato-podman info --format json".to_string()),
            "verify must target ato-podman: {calls:?}"
        );
        assert_eq!(
            reporter.phases(),
            vec![
                InstallPhase::Locating,
                InstallPhase::Installing,
                InstallPhase::InitializingMachine,
                InstallPhase::StartingMachine,
                InstallPhase::Verifying,
                InstallPhase::Ready,
            ]
        );
    }

    #[test]
    fn podman_present_no_machine_inits_starts_verifies() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("prepares");
        let calls = env.calls();
        assert!(!calls.iter().any(|c| c.starts_with("brew install")));
        assert_eq!(
            calls,
            vec![
                "podman machine list --format json",
                "podman machine init ato-podman",
                "podman machine start ato-podman",
                "podman --connection ato-podman info --format json",
            ]
        );
    }

    #[test]
    fn no_virtualization_blocks_machine_init_with_typed_error() {
        // vfkit etc. are present (no missing helpers), but the host can't run a
        // VM. Preflight must fail with the typed virtualization error BEFORE
        // attempting `machine init`, so it never reads as a packaging bug.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_virtualization_available(false)
            .with_podman("machine list --format json", 0, "[]");
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("must refuse without virtualization");
        assert!(
            matches!(err, PrepareError::RuntimeVirtualizationUnavailable(_)),
            "expected RuntimeVirtualizationUnavailable, got {err:?}"
        );
        // We must NOT have attempted to boot a VM.
        let calls = env.calls();
        assert!(
            !calls.iter().any(|c| c.contains("machine init")),
            "machine init must not run when virtualization is unavailable: {calls:?}"
        );
        // Message is environment-oriented, not "Ato packaging bug".
        let msg = err.to_string();
        assert!(msg.contains("virtualization"), "{msg}");
        assert!(msg.contains("physical Mac"), "{msg}");
        assert!(!err.is_retryable(), "environment limit is not a simple retry");
    }

    #[test]
    fn machine_init_virtualization_error_is_reclassified() {
        // Even when the preflight passed (host reports virtualization), a vfkit /
        // Virtualization.framework failure during init must surface as the typed
        // virtualization error, not a generic MachineInitFailed.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman("machine list --format json", 0, "[]")
            .with_podman(
                "machine init ato-podman",
                1,
                "vfkit exited unexpectedly: Virtualization.framework: operation not permitted",
            );
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("init should fail");
        assert!(
            matches!(err, PrepareError::RuntimeVirtualizationUnavailable(Some(_))),
            "vfkit/VZ failure must map to virtualization error, got {err:?}"
        );
    }

    #[test]
    fn virtualization_error_classifier_distinguishes_cases() {
        // Virtualization-backend failures → true.
        for msg in [
            "vfkit exited unexpectedly with exit code 1",
            "could not access Virtualization.framework",
            "operation not permitted",
            "failed to start VM",
        ] {
            assert!(is_virtualization_machine_error(msg), "should match: {msg}");
        }
        // A missing-helper packaging error must NOT be misread as virtualization.
        assert!(
            !is_virtualization_machine_error("could not find \"vfkit\""),
            "missing-helper error is a packaging issue, not virtualization"
        );
        // A generic/unrelated failure → false.
        assert!(!is_virtualization_machine_error("disk image is corrupt"));
    }

    #[test]
    fn incomplete_managed_install_self_repairs_then_inits() {
        // The #577 clean-VM state: an Ato-managed Podman is resolvable but its
        // machine helpers are missing. Runtime prepare must REINSTALL the full
        // bundle (not just error out), then proceed to machine init — no
        // "re-run setup" dead-end.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_managed(true, Some(Ok(())))
            .with_missing_helpers(&["gvproxy", "vfkit"])
            .with_podman("machine list --format json", 0, "[]")
            .with_podman("machine init ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("self-repairs then prepares");
        let calls = env.calls();
        assert!(
            calls.contains(&"ato-managed install podman".to_string()),
            "must reinstall the bundle to repair: {calls:?}"
        );
        assert!(
            calls.contains(&"podman machine init ato-podman".to_string()),
            "must init the machine after repair: {calls:?}"
        );
        // Repaired exactly once — no loop.
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains("install podman"))
                .count(),
            1,
            "reinstall must run exactly once: {calls:?}"
        );
    }

    #[test]
    fn incomplete_managed_install_failed_repair_is_typed_and_does_not_loop() {
        // If the repair reinstall itself fails, surface a typed error and never
        // attempt machine init — and never loop on reinstall.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_managed(
                true,
                Some(Err(PodmanInstallError::Extract {
                    message: "download failed".to_string(),
                })),
            )
            .with_missing_helpers(&["gvproxy"])
            .with_podman("machine list --format json", 0, "[]");
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("failed repair must error");
        // A reinstall failure is reported as an install problem, not "re-run".
        assert!(
            matches!(err, PrepareError::InstallUnavailable(_)),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(
            !msg.to_lowercase().contains("re-run runtime setup"),
            "must not give a misleading retry hint: {msg}"
        );
        let calls = env.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains("install podman"))
                .count(),
            1,
            "reinstall attempted once, no loop: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.contains("machine init")),
            "init must not run when repair failed: {calls:?}"
        );
    }

    #[test]
    fn incomplete_runtime_without_managed_artifact_is_typed_no_init() {
        // Defensive branch: a missing-helper report with no pinned managed
        // artifact (cannot repair) → typed error, no init, never "re-run setup".
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_missing_helpers(&["gvproxy"]) // managed_available defaults false
            .with_podman("machine list --format json", 0, "[]");
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("incomplete runtime must fail");
        assert!(
            matches!(err, PrepareError::RuntimeProviderIncomplete { ref helper } if helper == "gvproxy"),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("gvproxy"), "{msg}");
        assert!(
            msg.contains("Ato packaging") || msg.contains("Ato-managed"),
            "must blame Ato packaging, not the user: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("install homebrew"),
            "must never tell the user to install Homebrew: {msg}"
        );
        let calls = env.calls();
        assert!(
            !calls.iter().any(|c| c.contains("install podman")),
            "no managed artifact → no reinstall attempt: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.contains("machine init")),
            "init must not run when helpers are missing: {calls:?}"
        );
    }

    #[test]
    fn machine_init_missing_gvproxy_maps_to_runtime_incomplete_not_generic() {
        // Even if preflight passes (helpers report complete), a podman machine
        // init that fails with `could not find "gvproxy"` is mapped to the typed
        // runtime-incomplete category — never an opaque MachineInitFailed.
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman("machine list --format json", 0, "[]")
            .with_podman(
                "machine init ato-podman",
                125,
                "Error: could not find \"gvproxy\" in one of [...]",
            );
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("machine init helper error");
        assert!(
            matches!(err, PrepareError::RuntimeProviderIncomplete { ref helper } if helper == "gvproxy"),
            "{err:?}"
        );
    }

    #[test]
    fn helper_name_in_machine_error_matches_gvproxy_and_vfkit() {
        assert_eq!(
            helper_name_in_machine_error("could not find \"gvproxy\" in [...]").as_deref(),
            Some("gvproxy")
        );
        assert_eq!(
            helper_name_in_machine_error("Could Not Find vfkit anywhere").as_deref(),
            Some("vfkit")
        );
        // A generic machine failure is left alone (mapped to MachineInitFailed).
        assert_eq!(
            helper_name_in_machine_error("vm already exists"),
            None
        );
        assert_eq!(
            helper_name_in_machine_error("could not find machine ato-podman"),
            None
        );
    }

    #[test]
    fn ato_machine_stopped_starts_only_no_init() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman(
                "machine list --format json",
                0,
                r#"[{"Name":"ato-podman","Running":false}]"#,
            )
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("starts");
        let calls = env.calls();
        assert!(
            !calls.contains(&"podman machine init ato-podman".to_string()),
            "must not init when machine exists: {calls:?}"
        );
        assert!(calls.contains(&"podman machine start ato-podman".to_string()));
    }

    #[test]
    fn ato_machine_running_verifies_only() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman(
                "machine list --format json",
                0,
                r#"[{"Name":"ato-podman","Running":true}]"#,
            )
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("verifies");
        let calls = env.calls();
        assert!(!calls.iter().any(|c| c.contains("machine init")));
        assert!(!calls.iter().any(|c| c.contains("machine start")));
        // Even when only verifying, the Ato machine is targeted explicitly.
        assert!(
            calls.contains(&"podman --connection ato-podman info --format json".to_string()),
            "{calls:?}"
        );
    }

    #[test]
    fn single_running_user_machine_is_used_without_creating_ato() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman(
                "machine list --format json",
                0,
                r#"[{"Name":"podman-machine-default","Running":true}]"#,
            )
            .with_podman("info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("uses existing");
        let calls = env.calls();
        assert!(
            !calls.iter().any(|c| c.contains("ato-podman")),
            "must not touch ato-podman when a user machine runs: {calls:?}"
        );
    }

    #[test]
    fn linux_skips_machine_and_verifies() {
        let env = FakeEnv::new(PreparePlatform::Linux).with_podman("info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        prepare_podman(&env, &reporter).expect("linux native");
        let calls = env.calls();
        assert!(!calls.iter().any(|c| c.contains("machine")));
        assert_eq!(calls, vec!["podman info --format json"]);
    }

    #[test]
    fn verify_failure_surfaces_as_verify_failed() {
        let env = FakeEnv::new(PreparePlatform::Macos)
            .with_podman(
                "machine list --format json",
                0,
                r#"[{"Name":"ato-podman","Running":true}]"#,
            )
            .with_podman("--connection ato-podman info --format json", 125, "");
        let reporter = RecordingReporter::default();
        let err = prepare_podman(&env, &reporter).expect_err("info fails");
        assert!(matches!(err, PrepareError::VerifyFailed(_)), "{err:?}");
    }

    // ── classify_prepare_tools ────────────────────────────────────────────────

    #[test]
    fn classify_splits_managed_and_host() {
        let (managed, host) =
            classify_prepare_tools(&[ToolKind::Node, ToolKind::Podman, ToolKind::Uv])
                .expect("valid");
        assert_eq!(managed, vec![ToolKind::Node, ToolKind::Uv]);
        assert_eq!(host, vec![ToolKind::Podman]);
    }

    #[test]
    fn classify_rejects_docker_and_bundled() {
        let err = classify_prepare_tools(&[ToolKind::DockerDesktop]).unwrap_err();
        assert!(err.iter().any(|m| m.contains("detection-only")), "{err:?}");
        let err = classify_prepare_tools(&[ToolKind::Nacelle]).unwrap_err();
        assert!(err.iter().any(|m| m.contains("bundle")), "{err:?}");
    }

    #[test]
    fn prepare_tools_rejects_docker_before_any_work() {
        let err = prepare_tools(vec![ToolKind::DockerDesktop], true).unwrap_err();
        assert!(err.to_string().contains("cannot be prepared"), "{err}");
    }

    #[test]
    fn prepare_tools_rejects_empty() {
        assert!(prepare_tools(vec![], true).is_err());
    }

    // ── #460 PR2: repair flow ─────────────────────────────────────────────────

    #[test]
    fn repair_restarts_ato_machine_in_order() {
        let env = FakeEnv::new(PreparePlatform::Windows)
            .with_podman("machine stop ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        repair_ato_machine(&env, &reporter).expect("repairs");
        assert_eq!(
            env.calls(),
            vec![
                "podman machine stop ato-podman",
                "podman machine start ato-podman",
                "podman --connection ato-podman info --format json",
            ]
        );
    }

    #[test]
    fn repair_only_touches_ato_machine() {
        let env = FakeEnv::new(PreparePlatform::Windows)
            .with_podman("machine stop ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        repair_ato_machine(&env, &reporter).expect("repairs");
        // Every machine command names ato-podman; no user/default machine touched.
        for call in env.calls() {
            if call.contains("machine stop") || call.contains("machine start") {
                assert!(
                    call.ends_with("ato-podman"),
                    "repair must only mutate ato-podman, got: {call}"
                );
            }
        }
    }

    #[test]
    fn repair_tolerates_already_stopped_machine() {
        let mut env = FakeEnv::new(PreparePlatform::Windows)
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 0, "{}");
        // `machine stop` reports already-stopped (non-zero) → treated as benign.
        env.podman.insert(
            "machine stop ato-podman".to_string(),
            CmdOutput {
                status: 125,
                stdout: String::new(),
                stderr: "Error: machine ato-podman is already stopped".to_string(),
            },
        );
        let reporter = RecordingReporter::default();
        repair_ato_machine(&env, &reporter).expect("repairs despite already-stopped");
        assert!(
            env.calls()
                .contains(&"podman machine start ato-podman".to_string())
        );
    }

    #[test]
    fn repair_fails_when_verify_fails() {
        let env = FakeEnv::new(PreparePlatform::Windows)
            .with_podman("machine stop ato-podman", 0, "")
            .with_podman("machine start ato-podman", 0, "")
            .with_podman("--connection ato-podman info --format json", 1, "");
        let reporter = RecordingReporter::default();
        let err = repair_ato_machine(&env, &reporter).expect_err("verify fails");
        assert!(matches!(err, PrepareError::VerifyFailed(_)), "{err:?}");
    }

    #[test]
    fn repair_on_linux_only_verifies() {
        let env = FakeEnv::new(PreparePlatform::Linux).with_podman("info --format json", 0, "{}");
        let reporter = RecordingReporter::default();
        repair_ato_machine(&env, &reporter).expect("verifies");
        let calls = env.calls();
        assert!(!calls.iter().any(|c| c.contains("machine")), "{calls:?}");
        assert_eq!(calls, vec!["podman info --format json"]);
    }
}
