//! Runtime-setup status & install models shared between the CLI (producer)
//! and the desktop onboarding/settings UI (consumer).
//!
//! The desktop never inspects the host directly; it shells out to
//! `ato internal runtime setup-status --json` and `ato internal runtime
//! install --json` and renders these structs. Keeping the wire model in
//! `capsule-core` (which both `ato-cli` and `ato-desktop` already depend on)
//! means the two sides cannot drift out of sync.
//!
//! Scope (issue #420 revision): this replaces the earlier "host device
//! detection" / GPU-scan concept. Nothing here scans CPU/GPU/hardware
//! capabilities — it reports whether the *runtime tools* a recipe needs are
//! installed and usable.

use serde::{Deserialize, Serialize};

/// Ato-supported Node major line used when installing an Ato-managed Node.
/// `RuntimeFetcher` resolves this to the latest matching `22.x` release.
pub const SUPPORTED_NODE_VERSION: &str = "22";
/// Ato-supported uv release used when installing an Ato-managed uv.
pub const SUPPORTED_UV_VERSION: &str = "0.4.19";
/// Ato-supported Python minor line used when installing an Ato-managed Python.
pub const SUPPORTED_PYTHON_VERSION: &str = "3.12";

/// The runtime tools Ato cares about when deciding whether a machine can run
/// recipes. Serialized in `snake_case` (`docker_desktop`, `ato_helper`, …) so
/// the React onboarding card can switch on `tool.kind` directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Podman,
    DockerDesktop,
    Node,
    Uv,
    Python,
    AtoHelper,
    Nacelle,
}

impl ToolKind {
    /// Stable lowercase token used on the `--tools` CLI flag and in the JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolKind::Podman => "podman",
            ToolKind::DockerDesktop => "docker_desktop",
            ToolKind::Node => "node",
            ToolKind::Uv => "uv",
            ToolKind::Python => "python",
            ToolKind::AtoHelper => "ato_helper",
            ToolKind::Nacelle => "nacelle",
        }
    }

    /// Parse a `--tools` token. Accepts the canonical token plus the common
    /// `nodejs` alias for [`ToolKind::Node`].
    pub fn parse_tool(token: &str) -> Option<ToolKind> {
        match token.trim().to_ascii_lowercase().as_str() {
            "podman" => Some(ToolKind::Podman),
            "docker_desktop" | "docker" => Some(ToolKind::DockerDesktop),
            "node" | "nodejs" => Some(ToolKind::Node),
            "uv" => Some(ToolKind::Uv),
            "python" | "python3" => Some(ToolKind::Python),
            "ato_helper" | "ato" => Some(ToolKind::AtoHelper),
            "nacelle" => Some(ToolKind::Nacelle),
            _ => None,
        }
    }

    /// Whether Ato can install an Ato-managed copy of this tool itself.
    ///
    /// Only the language runtimes are managed-installable today (issue #420
    /// revision): Node, uv and Python go into the Ato toolchain cache via
    /// `RuntimeFetcher`. Podman/Docker are host-managed container engines and
    /// `ato_helper`/`nacelle` ship inside the desktop bundle — none of those
    /// are installed through this path.
    pub fn is_managed_installable(&self) -> bool {
        matches!(self, ToolKind::Node | ToolKind::Uv | ToolKind::Python)
    }

    /// How Ato makes this tool available — see [`InstallStrategy`]. This is the
    /// single source of truth for routing in the install/prepare commands.
    pub fn install_strategy(&self) -> InstallStrategy {
        match self {
            ToolKind::Node | ToolKind::Uv | ToolKind::Python => InstallStrategy::ManagedToolchain,
            ToolKind::Podman => InstallStrategy::HostRuntime,
            ToolKind::DockerDesktop => InstallStrategy::DetectionOnly,
            ToolKind::AtoHelper | ToolKind::Nacelle => InstallStrategy::Bundled,
        }
    }

    /// Whether this tool is prepared as a *host runtime* (install + service/
    /// machine setup), as opposed to an Ato-managed toolchain. Only Podman
    /// today. Host runtimes go through `ato internal runtime prepare`, never
    /// `RuntimeFetcher`/the managed toolchain cache.
    pub fn is_host_runtime_prepareable(&self) -> bool {
        matches!(self.install_strategy(), InstallStrategy::HostRuntime)
    }
}

/// How Ato provisions a [`ToolKind`]. Keeps Podman (a host runtime that may need
/// install + machine init/start) distinct from the Ato-managed language
/// toolchains, so the two never share the `RuntimeFetcher`/cache path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStrategy {
    /// Node / uv / Python — fetched into the Ato toolchain cache.
    ManagedToolchain,
    /// Podman — a host container runtime; install + machine init/start.
    HostRuntime,
    /// Docker Desktop — detected only; Ato never installs it.
    DetectionOnly,
    /// `ato_helper` / `nacelle` — shipped inside the desktop bundle.
    Bundled,
}

/// Where a detected tool came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Shipped inside the desktop bundle (`ato_helper`, `nacelle`).
    Bundled,
    /// Installed into the Ato toolchain cache (`~/.ato/toolchains`).
    ManagedByAto,
    /// Found on the system `PATH`.
    SystemPath,
    /// A host-managed service (Podman machine, Docker Desktop daemon).
    External,
    /// Not found anywhere.
    Missing,
}

/// The single next step the UI should offer for a tool. Detection-only tools
/// never return [`RecommendedAction::InstallManaged`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    /// Ready — nothing to do.
    None,
    /// Offer to install an Ato-managed copy (Node/uv/Python only).
    InstallManaged,
    /// An Ato-managed copy exists but is below the supported version.
    UpgradeManaged,
    /// Installed but the backing service is not running (Podman machine /
    /// Docker daemon).
    StartService,
    /// Detection-only: show install/setup instructions, do not auto-install
    /// (Docker Desktop missing).
    OpenInstructions,
    /// A bundled tool is missing — the install is corrupt, reinstall Ato.
    BundleRepairRequired,
    /// A host runtime (Podman) needs explicit, opt-in preparation: install
    /// and/or initialize/start its Ato-managed machine. Run
    /// `ato internal runtime prepare`. Never triggered automatically.
    PrepareHostRuntime,
    /// A host runtime is installed but its machine/state is broken or
    /// ambiguous and needs a repair pass (re-init/recreate the Ato machine).
    RepairHostRuntime,
}

/// Per-tool readiness, as reported by `ato internal runtime setup-status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    pub kind: ToolKind,
    /// Present on the machine in any form.
    pub installed: bool,
    /// Detected version string, when one could be read.
    pub version: Option<String>,
    /// Whether the detected version is within Ato's supported range.
    pub supported: bool,
    /// Installed, supported, and (for services) running — safe to use now.
    pub ready: bool,
    pub source: ToolSource,
    pub action: RecommendedAction,
    /// Short, user-facing one-line explanation of the current state.
    pub message: String,
}

impl ToolStatus {
    /// A "ready" status for a tool found in `source` at `version`.
    pub fn ready(
        kind: ToolKind,
        source: ToolSource,
        version: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        ToolStatus {
            kind,
            installed: true,
            version,
            supported: true,
            ready: true,
            source,
            action: RecommendedAction::None,
            message: message.into(),
        }
    }

    /// A "missing" status with the given recommended action.
    pub fn missing(kind: ToolKind, action: RecommendedAction, message: impl Into<String>) -> Self {
        ToolStatus {
            kind,
            installed: false,
            version: None,
            supported: false,
            ready: false,
            source: ToolSource::Missing,
            action,
            message: message.into(),
        }
    }
}

/// Aggregate result of a host runtime-setup check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSetupStatus {
    pub tools: Vec<ToolStatus>,
    /// Windows-only substrate diagnostics for the local OCI engine (WSL /
    /// virtualization / reboot). `None` on non-Windows hosts and on older
    /// records written before this field existed. See #460.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_substrate: Option<WindowsSubstrateStatus>,
}

impl RuntimeSetupStatus {
    pub fn get(&self, kind: ToolKind) -> Option<&ToolStatus> {
        self.tools.iter().find(|t| t.kind == kind)
    }
}

/// WSL availability classification for the Windows OCI substrate (#460).
///
/// On Windows, a local Podman capsule runs inside a WSL2-backed `podman
/// machine`; the Desktop uses this to tell the user *what is missing* and never
/// requires them to open a shell. `NotApplicable` is used on non-Windows hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WslStatus {
    /// Non-Windows host — WSL is irrelevant.
    #[default]
    NotApplicable,
    /// `wsl.exe` is absent, or reports the Windows Subsystem for Linux is not
    /// installed.
    Missing,
    /// WSL is present but no WSL2-capable distribution is usable (no distro, or
    /// only version-1 distros / default version 1).
    Wsl2Unavailable,
    /// WSL setup completed but a reboot is pending before it can be used.
    RebootRequired,
    /// WSL2 is installed and a version-2 distribution is available.
    Ready,
    /// WSL state could not be determined from the probe output.
    Unknown,
}

/// Best-effort virtualization-platform signal for the Windows substrate (#460).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VirtualizationStatus {
    /// Could not determine virtualization state (the common, benign default).
    #[default]
    Unknown,
    /// Probe output indicates the Virtual Machine Platform / hardware
    /// virtualization is disabled (Windows feature off, or BIOS/firmware).
    UnavailableOrUnknown,
    /// Virtualization appears available.
    Available,
}

/// Windows substrate diagnostics surfaced to the Desktop Runtime Setup UI so it
/// can guide the user to a working OCI engine without any CLI/WSL hand-ops (#460).
///
/// Carries both the read-only diagnosis and the single recommended
/// [`WindowsSubstrateAction`] the Desktop should offer. The per-tool Podman
/// [`ToolStatus`] continues to own the *machine-level* readiness/repair action;
/// this struct owns the *substrate* (WSL / virtualization / reboot) action that
/// sits underneath Podman.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsSubstrateStatus {
    pub wsl: WslStatus,
    pub virtualization: VirtualizationStatus,
    /// A reboot is required before the substrate is usable.
    pub reboot_required: bool,
    /// Short, user-facing one-line explanation of the substrate state.
    pub message: String,
    /// The remediation the Desktop should offer for this state. `None`-kind when
    /// the substrate is ready or no Desktop-runnable action applies.
    #[serde(default)]
    pub action: WindowsSubstrateAction,
}

/// The kind of remediation the Desktop should offer for a Windows substrate
/// state (#460). Pairs with [`WindowsSubstrateAction`] metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowsSubstrateActionKind {
    /// Substrate is ready (or unknown) — nothing to offer.
    #[default]
    None,
    /// WSL is not installed — install it (`wsl --install`).
    InstallWsl,
    /// WSL present but not WSL2 — set the default version to 2.
    EnableWsl2,
    /// A reboot must complete before setup can continue.
    RebootRequired,
    /// Virtualization/VM-platform appears off — show guidance (some of which is
    /// firmware/BIOS and cannot be fully automated).
    OpenVirtualizationInstructions,
    /// The Ato-managed Podman machine is running but unhealthy — repair it.
    RepairPodmanMachine,
}

/// A single Desktop-presentable remediation for the Windows substrate (#460).
///
/// This is *descriptive metadata* the UI renders into a button + explanation;
/// executing it is a separate, capability-gated IPC command. `setup-status`
/// itself never mutates the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsSubstrateAction {
    pub kind: WindowsSubstrateActionKind,
    /// Button label, e.g. "Enable WSL".
    pub label: String,
    /// One-line description of what running the action does.
    pub description: String,
    /// Needs elevation (UAC) to run.
    pub requires_admin: bool,
    /// Completing the action requires an OS reboot.
    pub requires_reboot: bool,
    /// Running the action can destroy state (e.g. recreate a machine).
    pub destructive: bool,
    /// The Desktop can run this directly (vs. guidance the user must follow,
    /// e.g. firmware/BIOS virtualization changes).
    pub can_run_from_desktop: bool,
}

impl Default for WindowsSubstrateAction {
    fn default() -> Self {
        WindowsSubstrateAction {
            kind: WindowsSubstrateActionKind::None,
            label: String::new(),
            description: String::new(),
            requires_admin: false,
            requires_reboot: false,
            destructive: false,
            can_run_from_desktop: false,
        }
    }
}

impl WindowsSubstrateAction {
    /// The canonical action for a substrate kind. Pure so the Desktop and tests
    /// agree on labels/flags. See #460.
    pub fn for_kind(kind: WindowsSubstrateActionKind) -> Self {
        use WindowsSubstrateActionKind as K;
        match kind {
            K::None => WindowsSubstrateAction::default(),
            K::InstallWsl => WindowsSubstrateAction {
                kind,
                label: "Enable WSL".to_string(),
                description: "Install the Windows Subsystem for Linux so Ato can run \
                              containers. This needs administrator approval and a restart."
                    .to_string(),
                requires_admin: true,
                requires_reboot: true,
                destructive: false,
                can_run_from_desktop: true,
            },
            K::EnableWsl2 => WindowsSubstrateAction {
                kind,
                label: "Enable WSL 2".to_string(),
                description: "Set WSL's default version to 2, which Ato's container \
                              engine requires."
                    .to_string(),
                requires_admin: false,
                requires_reboot: false,
                destructive: false,
                can_run_from_desktop: true,
            },
            K::RebootRequired => WindowsSubstrateAction {
                kind,
                label: "Continue after restart".to_string(),
                description: "WSL setup needs a restart to finish. Ato will resume setup \
                              automatically after you restart."
                    .to_string(),
                requires_admin: false,
                requires_reboot: true,
                destructive: false,
                can_run_from_desktop: true,
            },
            K::OpenVirtualizationInstructions => WindowsSubstrateAction {
                kind,
                label: "Show virtualization steps".to_string(),
                description: "Virtualization (Virtual Machine Platform / firmware) appears \
                              disabled. Some steps may require firmware/BIOS changes Ato \
                              cannot make for you."
                    .to_string(),
                requires_admin: true,
                requires_reboot: false,
                destructive: false,
                // Windows-feature enablement can be Desktop-driven; firmware cannot,
                // so the UI presents guidance rather than a guaranteed one-click fix.
                can_run_from_desktop: false,
            },
            K::RepairPodmanMachine => WindowsSubstrateAction {
                kind,
                label: "Repair Ato Podman machine".to_string(),
                description: "Restart and re-verify the Ato-managed Podman machine.".to_string(),
                requires_admin: false,
                requires_reboot: false,
                destructive: false,
                can_run_from_desktop: true,
            },
        }
    }
}

/// Persisted marker that a Windows substrate remediation needs a reboot to
/// finish, so the Desktop can resume Runtime Setup on next launch (#460).
///
/// Written under `~/.ato/runtime-setup/resume.json` before a reboot-requiring
/// action; read on Desktop startup / Settings open, then cleared once the
/// substrate is ready.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSetupResumeMarker {
    pub schema_version: u32,
    /// The action that asked for the reboot.
    pub requested_action: WindowsSubstrateActionKind,
    /// Which surface initiated it (`"onboarding"` / `"settings"`).
    pub source_surface: String,
    /// Unix-ms timestamp the marker was written (stamped by the caller).
    pub created_at_unix_ms: u64,
    pub requires_reboot: bool,
    /// What the Desktop should do once back: continue podman prepare, or just
    /// re-check status.
    pub expected_next_step: RuntimeSetupResumeStep,
    /// Human-readable summary of the substrate state before the reboot.
    pub status_before_reboot: String,
}

/// What to do when resuming Runtime Setup after a reboot (#460).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSetupResumeStep {
    /// Re-check substrate status and show the next action.
    #[default]
    RecheckStatus,
    /// Substrate should be ready — continue creating/starting the Podman machine.
    ContinuePodmanPrepare,
}

/// Current resume-marker schema version.
pub const RUNTIME_SETUP_RESUME_SCHEMA_VERSION: u32 = 1;

/// Persisted intent to resume a capsule launch once Runtime Setup completes
/// (#460 PR3). When a launch is interrupted because the host runtime needs
/// setup — or when setup needs a reboot — the Desktop records what the user was
/// trying to open so it can return them there afterward, instead of stranding
/// them on the setup screen.
///
/// Written under `~/.ato/runtime-setup/launch-intent.json`; consumed (and the
/// marker cleared) once the substrate is ready. Advisory and self-healing: a
/// missing, corrupt, or stale intent is treated as "nothing to resume".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSetupLaunchIntent {
    pub schema_version: u32,
    /// Unix-ms timestamp the intent was written (stamped by the caller).
    pub created_at_unix_ms: u64,
    /// Which surface recorded it (`"onboarding"` / `"settings"` / `"launch_flow"`).
    pub source_surface: String,
    pub intent_kind: LaunchIntentKind,
    /// The launch input to replay — a capsule URL, sample slug, community recipe
    /// id, or source URL, interpreted per `intent_kind`.
    pub launch_input: String,
    pub expected_next_step: LaunchIntentNextStep,
    /// Correlates with the IPC request that recorded the intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Optional human-readable label for the pending launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
    /// Which Desktop display client to reattach when the launch resumes (#460
    /// PR3b). Desktop-only: the CLI round-trips it untouched. Absent on older
    /// markers and on CLI-written intents → the Desktop falls back to its
    /// default windowed client, preserving the original open mode on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_client: Option<LaunchClientKind>,
}

/// Which Desktop display client to reattach when a recorded launch resumes after
/// Runtime Setup (#460 PR3b). Carried on [`RuntimeSetupLaunchIntent`] so the
/// original `capsule_open_mode` (windowed vs. OS browser) survives the detour
/// through Runtime Setup. Desktop-only in practice; the CLI never sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaunchClientKind {
    /// Focus View top-level window — the Desktop default.
    #[default]
    AtoWindow,
    /// The user's OS default browser (no Ato pane).
    OsBrowser,
}

/// What kind of launch input a [`RuntimeSetupLaunchIntent`] carries (#460 PR3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaunchIntentKind {
    /// A bundled sample capsule (onboarding smoke).
    #[default]
    SampleCapsule,
    /// A `capsule://…` URL / handle.
    CapsuleUrl,
    /// A community recipe id (`ctoml_…`).
    CommunityTomlId,
    /// A source URL (e.g. a GitHub repo).
    SourceUrl,
}

/// What to do with a launch intent once Runtime Setup is ready (#460 PR3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaunchIntentNextStep {
    /// Resume the recorded capsule launch.
    #[default]
    ContinueLaunch,
    /// Return the user to onboarding (no direct launch).
    ReturnToOnboarding,
}

/// Current launch-intent schema version.
pub const RUNTIME_SETUP_LAUNCH_INTENT_SCHEMA_VERSION: u32 = 1;

/// Phases an install/prepare moves through. Emitted as a stream of JSON lines by
/// `ato internal runtime install --json` and `ato internal runtime prepare
/// --emit-json`.
///
/// Managed-toolchain installs use `Queued → Downloading → Installing → Ready`.
/// Host-runtime prepare (Podman) uses `Queued → Locating →
/// [Installing] → [InitializingMachine] → [StartingMachine] → Verifying →
/// Ready`, skipping the bracketed phases when the corresponding step is not
/// needed. Any phase may be followed by `Failed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    Queued,
    Downloading,
    /// Host-runtime prepare: resolving the binary / inspecting machine state.
    Locating,
    Installing,
    /// Host-runtime prepare: creating the Ato-managed Podman machine.
    InitializingMachine,
    /// Host-runtime prepare: starting the Ato-managed Podman machine.
    StartingMachine,
    Verifying,
    Ready,
    Failed,
}

/// One progress event for one tool's install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallProgress {
    pub tool: ToolKind,
    pub phase: InstallPhase,
    pub message: String,
    /// True when a `Failed` event is a transient condition the user can retry
    /// (e.g. a 504 from the release CDN), so a consuming UI can offer a Retry
    /// action instead of a dead end. Omitted from the wire when false to keep
    /// the existing event shape unchanged for the common case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
}

impl InstallProgress {
    pub fn new(tool: ToolKind, phase: InstallPhase, message: impl Into<String>) -> Self {
        InstallProgress {
            tool,
            phase,
            message: message.into(),
            retryable: false,
        }
    }

    /// Mark this event as a retryable (transient) failure.
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_progress_retryable_wire_shape() {
        // Default (false) is omitted from the wire — existing consumers unchanged.
        let plain = InstallProgress::new(ToolKind::Podman, InstallPhase::Failed, "boom");
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("retryable"), "false must be omitted: {json}");

        // A transient failure carries `retryable: true` for the UI's Retry action.
        let retry = InstallProgress::new(ToolKind::Podman, InstallPhase::Failed, "504").retryable(true);
        let json = serde_json::to_string(&retry).unwrap();
        assert!(json.contains("\"retryable\":true"), "true must be present: {json}");

        // Round-trips, and a missing field deserializes to false.
        let back: InstallProgress =
            serde_json::from_str(r#"{"tool":"podman","phase":"failed","message":"x"}"#).unwrap();
        assert!(!back.retryable);
    }

    #[test]
    fn install_strategy_routes_each_tool() {
        assert_eq!(
            ToolKind::Node.install_strategy(),
            InstallStrategy::ManagedToolchain
        );
        assert_eq!(
            ToolKind::Uv.install_strategy(),
            InstallStrategy::ManagedToolchain
        );
        assert_eq!(
            ToolKind::Python.install_strategy(),
            InstallStrategy::ManagedToolchain
        );
        assert_eq!(
            ToolKind::Podman.install_strategy(),
            InstallStrategy::HostRuntime
        );
        assert_eq!(
            ToolKind::DockerDesktop.install_strategy(),
            InstallStrategy::DetectionOnly
        );
        assert_eq!(
            ToolKind::AtoHelper.install_strategy(),
            InstallStrategy::Bundled
        );
        assert_eq!(
            ToolKind::Nacelle.install_strategy(),
            InstallStrategy::Bundled
        );
    }

    #[test]
    fn only_podman_is_host_runtime_prepareable() {
        assert!(ToolKind::Podman.is_host_runtime_prepareable());
        for kind in [
            ToolKind::Node,
            ToolKind::Uv,
            ToolKind::Python,
            ToolKind::DockerDesktop,
            ToolKind::AtoHelper,
            ToolKind::Nacelle,
        ] {
            assert!(!kind.is_host_runtime_prepareable(), "{}", kind.as_str());
        }
        // Podman is a host runtime, never an Ato-managed toolchain.
        assert!(!ToolKind::Podman.is_managed_installable());
    }

    #[test]
    fn tool_kind_roundtrips_through_tokens() {
        for kind in [
            ToolKind::Podman,
            ToolKind::DockerDesktop,
            ToolKind::Node,
            ToolKind::Uv,
            ToolKind::Python,
            ToolKind::AtoHelper,
            ToolKind::Nacelle,
        ] {
            assert_eq!(ToolKind::parse_tool(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn tool_kind_accepts_aliases() {
        assert_eq!(ToolKind::parse_tool("nodejs"), Some(ToolKind::Node));
        assert_eq!(
            ToolKind::parse_tool("docker"),
            Some(ToolKind::DockerDesktop)
        );
        assert_eq!(ToolKind::parse_tool(" UV "), Some(ToolKind::Uv));
        assert_eq!(ToolKind::parse_tool("gpu"), None);
    }

    #[test]
    fn only_language_runtimes_are_managed_installable() {
        assert!(ToolKind::Node.is_managed_installable());
        assert!(ToolKind::Uv.is_managed_installable());
        assert!(ToolKind::Python.is_managed_installable());
        assert!(!ToolKind::Podman.is_managed_installable());
        assert!(!ToolKind::DockerDesktop.is_managed_installable());
        assert!(!ToolKind::AtoHelper.is_managed_installable());
        assert!(!ToolKind::Nacelle.is_managed_installable());
    }

    #[test]
    fn status_serializes_snake_case_for_ui() {
        let status = ToolStatus::missing(
            ToolKind::Podman,
            RecommendedAction::OpenInstructions,
            "Podman is not installed",
        );
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["kind"], "podman");
        assert_eq!(json["action"], "open_instructions");
        assert_eq!(json["source"], "missing");
        assert_eq!(json["ready"], false);
    }

    #[test]
    fn aggregate_lookup_by_kind() {
        let status = RuntimeSetupStatus {
            tools: vec![ToolStatus::ready(
                ToolKind::Node,
                ToolSource::ManagedByAto,
                Some("22.11.0".to_string()),
                "ready",
            )],
            windows_substrate: None,
        };
        assert!(status.get(ToolKind::Node).is_some());
        assert!(status.get(ToolKind::Uv).is_none());
    }
}
