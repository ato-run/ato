//! The decision: can this host run this declared route?
//!
//! Facts are observed once ([`HostFacts::probe`]); the decision is pure over
//! them. That split is what lets `ato pack` report the same answer `ato run`
//! will admit on, and lets both be tested from a recorded probe instead of a
//! live daemon.

use std::path::PathBuf;

use crate::docker::{DockerFacts, PlatformSupport, WorkspaceSharing};
use crate::reason::{CapabilityState, CapabilityStatus, Reason, ReasonCode};

use serde::Serialize;

/// What a route needs from the host. Read from the Derivation, never guessed
/// from the argv.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExecutionRequirements {
    /// The `os/arch` platform the route's images are pinned to, when it declares
    /// one.
    pub platform: Option<String>,
    /// The `os/arch` a native desktop Derivation is built for, when the
    /// selected D is one. Kept apart from `platform` (an OCI image platform)
    /// because the two are matched against different facts.
    pub native_platform: Option<String>,
    /// Whether the route declares filesystem state.
    pub needs_state: bool,
    /// Whether the route's realization is a container realization.
    pub needs_container: bool,
    /// The paths this Run will bind into the daemon (its runtime root and any
    /// state root).
    pub bound_paths: Vec<PathBuf>,
}

impl ExecutionRequirements {
    pub fn container(platform: Option<String>) -> Self {
        Self {
            platform,
            needs_container: true,
            ..Self::default()
        }
    }

    pub fn process() -> Self {
        Self::default()
    }

    /// A native desktop route: the executable is built for exactly this
    /// `os/arch`, and its mandatory policies need the host's native Adapter.
    pub fn native_desktop(os: &str, arch: &str) -> Self {
        Self {
            native_platform: Some(format!("{os}/{arch}")),
            ..Self::default()
        }
    }

    pub fn with_state(mut self) -> Self {
        self.needs_state = true;
        self
    }

    pub fn binding(mut self, path: impl Into<PathBuf>) -> Self {
        self.bound_paths.push(path.into());
        self
    }
}

/// How the OCI adapter reaches a route's logical Ports on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorChoice {
    /// The container's address on the run's internal network is reachable from
    /// this process: connect to it directly.
    Direct,
    /// It is not (a VM-backed or rootless daemon): the Run starts its own relay
    /// container on the same internal network and multiplexes its Ports over
    /// one stdio stream.
    Relay,
}

impl ConnectorChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

/// Everything measured about this host, in the shape the decisions need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostFacts {
    /// The CLI's own OS, in this crate's vocabulary.
    pub os: String,
    /// The CLI's own architecture.
    pub arch: String,
    pub docker: DockerFacts,
    /// Whether this host has the Linux containment a process route with state
    /// needs (bubblewrap + user namespaces).
    pub linux_sandbox: bool,
    /// The OS-level tools a native desktop route's mandatory policies need.
    pub native: NativeFacts,
}

/// What a native desktop Adapter can enforce on this host.
///
/// Measured, not assumed. A native route whose signature or sandbox policy
/// cannot be enforced before launch is refused at admission: recording the gap
/// would not satisfy the mandatory condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeFacts {
    /// The containment tool (macOS Seatbelt's `sandbox-exec`).
    pub sandbox_tool: bool,
    /// The tooling that produces a signature verdict (macOS `codesign` and
    /// `spctl`). Windows Authenticode is an OS API, not a CLI tool.
    pub signature_tools: bool,
}

impl NativeFacts {
    pub fn probe() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                sandbox_tool: std::path::Path::new("/usr/bin/sandbox-exec").is_file(),
                signature_tools: path_tool("codesign") && path_tool("spctl"),
            }
        }
        #[cfg(target_os = "windows")]
        {
            Self {
                // AppContainer/filesystem policy is not implemented yet, so the
                // mandatory set is not enforceable here.
                sandbox_tool: false,
                // WinVerifyTrust is part of the OS.
                signature_tools: true,
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self {
                sandbox_tool: false,
                signature_tools: false,
            }
        }
    }
}

impl Default for NativeFacts {
    fn default() -> Self {
        Self::probe()
    }
}

fn path_tool(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| {
            let candidate = directory.join(name);
            candidate.is_file()
        })
    })
}

/// `arm64`/`amd64` are the same architectures under different spellings; a
/// native descriptor uses `aarch64`/`x86_64`.
fn normalize_arch(arch: &str) -> &str {
    match arch {
        "arm64" => "aarch64",
        "amd64" => "x86_64",
        other => other,
    }
}

impl Default for HostFacts {
    fn default() -> Self {
        Self {
            os: crate::docker::client_os().to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            docker: DockerFacts::default(),
            linux_sandbox: cfg!(target_os = "linux"),
            native: NativeFacts::probe(),
        }
    }
}

impl HostFacts {
    /// Probe this host once for the given platforms, without starting anything.
    pub fn probe(requirements: &[ExecutionRequirements]) -> Self {
        let platforms: Vec<String> = requirements
            .iter()
            .filter_map(|requirement| requirement.platform.clone())
            .collect();
        let docker = crate::docker::probe(&platforms);
        Self {
            os: crate::docker::client_os().to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            docker,
            linux_sandbox: cfg!(target_os = "linux"),
            native: NativeFacts::probe(),
        }
    }

    /// How the OCI adapter should reach this route's Ports.
    ///
    /// Direct only when the bridge is provably reachable; a relay when the
    /// daemon is local but the bridge is not; neither otherwise (the caller
    /// must refuse before realizing).
    pub fn connector_choice(&self) -> Option<ConnectorChoice> {
        if !self.docker.cli_present || !self.docker.daemon_reachable {
            return None;
        }
        if self.docker.endpoint.is_remote() {
            return None;
        }
        if self.docker.bridge_reachable() {
            Some(ConnectorChoice::Direct)
        } else {
            Some(ConnectorChoice::Relay)
        }
    }

    /// The reason list for the paths this Run binds, if any are not provably
    /// visible to the daemon.
    pub fn workspace_reasons(&self, requirements: &ExecutionRequirements) -> Vec<Reason> {
        let mut reasons = Vec::new();
        for path in &requirements.bound_paths {
            match self.docker.workspace_sharing.admits(path) {
                Some(true) => {}
                Some(false) => {
                    let detail = match &self.docker.workspace_sharing {
                        WorkspaceSharing::NotShared { detail } => detail.clone(),
                        _ => format!(
                            "{} is outside the daemon's shared directories",
                            path.display()
                        ),
                    };
                    reasons.push(Reason::new(
                        ReasonCode::DockerWorkspaceNotShared,
                        format!(
                            "the daemon cannot see {} ({detail}); add that path to the daemon's \
                             file sharing, or move the Run's runtime root under a shared directory",
                            path.display()
                        ),
                    ));
                }
                None => {}
            }
        }
        reasons
    }
}

/// The one decision `ato pack` reports and `ato run` admits on.
pub fn execution_status(
    host: &HostFacts,
    requirements: &ExecutionRequirements,
) -> CapabilityStatus {
    let mut reasons = Vec::new();
    let mut state = CapabilityState::Advertised;

    // Only a *process* route mounts state through a sandbox path. A container
    // route carries state as a volume the daemon mounts, so the sandbox
    // question does not apply to it.
    if requirements.needs_state && !requirements.needs_container && !host.linux_sandbox {
        state = CapabilityState::Unavailable;
        reasons.push(Reason::new(
            ReasonCode::StateNeedsLinuxSandbox,
            "the route declares filesystem state that is mounted into a process, which needs the \
             Linux bubblewrap sandbox; container routes carry state as a volume instead, and \
             `ato run` refuses before realizing anything on a host without it",
        ));
    }

    if requirements.needs_container {
        let container = host
            .docker
            .execution_status(host.connector_choice() != Some(ConnectorChoice::Direct));
        if container.state != CapabilityState::Advertised {
            state = CapabilityState::Unavailable;
        }
        reasons.extend(container.reasons);
        let workspace = host.workspace_reasons(requirements);
        if !workspace.is_empty() {
            // Unknown sharing is not a promise: a bound path that cannot be
            // proven visible is a refusal, because the alternative is a
            // container that silently reads an empty directory.
            let blocked = requirements
                .bound_paths
                .iter()
                .any(|path| host.docker.workspace_sharing.admits(path).is_none());
            if blocked {
                state = CapabilityState::Unavailable;
                reasons.extend(workspace);
            } else {
                reasons.extend(workspace);
            }
        }
        if reasons.iter().any(|reason| {
            matches!(
                reason.code,
                ReasonCode::DockerWorkspaceNotShared | ReasonCode::RemoteDaemonWorkspaceUnsupported
            )
        }) {
            state = CapabilityState::Unavailable;
        }
    }

    // A platform that no fact proves runnable is a refusal, not a warning.
    if let Some(platform) = requirements.platform.as_deref() {
        match host.docker.platforms.get(platform) {
            Some(PlatformSupport::Native) | Some(PlatformSupport::Emulated { .. }) => {}
            Some(PlatformSupport::Unsupported { detail }) => {
                state = CapabilityState::Unavailable;
                // The docker status may already carry this reason; the route
                // status must not repeat it.
                if !reasons
                    .iter()
                    .any(|reason| reason.code == ReasonCode::PlatformUnsupported)
                {
                    reasons.push(Reason::new(
                        ReasonCode::PlatformUnsupported,
                        format!("{platform} cannot run on this daemon: {detail}"),
                    ));
                }
            }
            // Nothing was probed because nothing could be: the docker
            // reasons already say why, and a second "no architecture"
            // refusal hides the actionable one.
            None => {}
        }
    }

    // A native desktop D names the exact os/arch its artifact was built for,
    // and its mandatory policies need the host's native Adapter. Both are
    // refusals, never warnings: the D either cannot run, or cannot run with
    // the containment its declaration requires.
    if let Some(native) = requirements.native_platform.as_deref() {
        let (os, arch) = native.split_once('/').unwrap_or((native, ""));
        if host.os != os || normalize_arch(&host.arch) != normalize_arch(arch) {
            state = CapabilityState::Unavailable;
            reasons.push(Reason::new(
                ReasonCode::NativePlatformMismatch,
                format!(
                    "the selected native Derivation is built for {native}; this host is \
                     {}/{}",
                    host.os, host.arch
                ),
            ));
        } else {
            if !host.native.sandbox_tool {
                state = CapabilityState::Unavailable;
                reasons.push(Reason::new(
                    ReasonCode::NativeSandboxUnavailable,
                    "the native route's mandatory filesystem/state/egress/lifecycle policy \
                     cannot be enforced on this host, so it is not launched",
                ));
            }
            if !host.native.signature_tools {
                state = CapabilityState::Unavailable;
                reasons.push(Reason::new(
                    ReasonCode::NativeSignatureVerificationUnavailable,
                    "the native route's signature policy cannot be verified on this host, so \
                     no verdict could be recorded before launch",
                ));
            }
        }
    }

    CapabilityStatus {
        capability: "route:execution".to_owned(),
        state,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{DockerProbeInput, facts_from};

    fn desktop_host() -> HostFacts {
        let docker = facts_from(
            DockerProbeInput {
                cli_path: Some(PathBuf::from("/usr/local/bin/docker")),
                info: Ok(serde_json::json!({
                    "OperatingSystem": "Docker Desktop",
                    "Architecture": "aarch64",
                    "OSType": "linux",
                    "SecurityOptions": ["name=seccomp,profile=builtin"]
                })
                .to_string()),
                endpoint_host: Some("unix:///var/run/docker.sock".to_owned()),
                client_os: "macos".to_owned(),
                desktop_shared_roots: Some(vec![PathBuf::from("/Users")]),
                ..DockerProbeInput::default()
            },
            &["linux/amd64".to_owned()],
        );
        HostFacts {
            os: "macos".to_owned(),
            arch: "arm64".to_owned(),
            docker,
            linux_sandbox: false,
            native: NativeFacts {
                sandbox_tool: true,
                signature_tools: true,
            },
        }
    }

    #[test]
    fn a_macos_docker_desktop_host_relays_and_does_not_refuse_the_route() {
        let host = desktop_host();
        let status = execution_status(
            &host,
            &ExecutionRequirements::container(Some("linux/amd64".to_owned()))
                .binding("/Users/me/run"),
        );
        assert_eq!(status.state, CapabilityState::Advertised);
        assert_eq!(host.connector_choice(), Some(ConnectorChoice::Relay));
        assert!(
            status
                .warnings()
                .any(|reason| reason.code == ReasonCode::PlatformEmulated)
        );
    }

    #[test]
    fn a_bound_path_outside_the_daemons_file_sharing_is_refused() {
        let host = desktop_host();
        let status = execution_status(
            &host,
            &ExecutionRequirements::container(Some("linux/amd64".to_owned()))
                .binding("/private/var/run/ato"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::DockerWorkspaceNotShared)
        );
    }

    #[test]
    fn a_process_route_with_state_needs_the_linux_sandbox() {
        let host = desktop_host();
        let status = execution_status(&host, &ExecutionRequirements::process().with_state());
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::StateNeedsLinuxSandbox)
        );
    }

    #[test]
    fn a_remote_daemon_has_no_connector_at_all() {
        let docker = facts_from(
            DockerProbeInput {
                cli_path: Some(PathBuf::from("/usr/bin/docker")),
                info: Ok(serde_json::json!({
                    "OperatingSystem": "Ubuntu",
                    "Architecture": "x86_64",
                    "SecurityOptions": []
                })
                .to_string()),
                endpoint_host: Some("tcp://10.0.0.5:2375".to_owned()),
                client_os: "linux".to_owned(),
                ..DockerProbeInput::default()
            },
            &["linux/amd64".to_owned()],
        );
        let host = HostFacts {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            docker,
            linux_sandbox: true,
            native: NativeFacts::default(),
        };
        assert_eq!(host.connector_choice(), None);
        let status = execution_status(
            &host,
            &ExecutionRequirements::container(Some("linux/amd64".to_owned()))
                .binding("/home/me/run"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::RemoteDaemonWorkspaceUnsupported)
        );
    }

    #[test]
    fn a_native_route_is_placed_only_on_the_platform_it_was_built_for() {
        let host = desktop_host();
        let status = execution_status(
            &host,
            &ExecutionRequirements::native_desktop("macos", "aarch64"),
        );
        assert_eq!(status.state, CapabilityState::Advertised);

        let status = execution_status(
            &host,
            &ExecutionRequirements::native_desktop("macos", "x86_64"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::NativePlatformMismatch)
        );

        let status = execution_status(
            &host,
            &ExecutionRequirements::native_desktop("windows", "x86_64"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::NativePlatformMismatch)
        );
    }

    #[test]
    fn a_native_route_whose_mandatory_policy_cannot_be_enforced_is_refused() {
        let mut host = desktop_host();
        host.native.sandbox_tool = false;
        let status = execution_status(
            &host,
            &ExecutionRequirements::native_desktop("macos", "aarch64"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::NativeSandboxUnavailable)
        );

        let mut host = desktop_host();
        host.native.signature_tools = false;
        let status = execution_status(
            &host,
            &ExecutionRequirements::native_desktop("macos", "aarch64"),
        );
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status.reasons.iter().any(|reason| {
                reason.code == ReasonCode::NativeSignatureVerificationUnavailable
            })
        );
    }
}
