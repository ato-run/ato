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
    /// (Podman missing, Docker Desktop missing).
    OpenInstructions,
    /// A bundled tool is missing — the install is corrupt, reinstall Ato.
    BundleRepairRequired,
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
    pub fn ready(kind: ToolKind, source: ToolSource, version: Option<String>, message: impl Into<String>) -> Self {
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
}

impl RuntimeSetupStatus {
    pub fn get(&self, kind: ToolKind) -> Option<&ToolStatus> {
        self.tools.iter().find(|t| t.kind == kind)
    }
}

/// Phases an install moves through. Emitted as a stream of JSON lines by
/// `ato internal runtime install --json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    Queued,
    Downloading,
    Verifying,
    Installing,
    Ready,
    Failed,
}

/// One progress event for one tool's install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallProgress {
    pub tool: ToolKind,
    pub phase: InstallPhase,
    pub message: String,
}

impl InstallProgress {
    pub fn new(tool: ToolKind, phase: InstallPhase, message: impl Into<String>) -> Self {
        InstallProgress {
            tool,
            phase,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(ToolKind::parse_tool("docker"), Some(ToolKind::DockerDesktop));
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
        };
        assert!(status.get(ToolKind::Node).is_some());
        assert!(status.get(ToolKind::Uv).is_none());
    }
}
