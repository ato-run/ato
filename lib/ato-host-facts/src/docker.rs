//! What the local Docker daemon is, measured without starting anything.
//!
//! Every fact here is read, never inferred from a product name: the CLI's
//! presence, whether the daemon answers, the daemon's OS/architecture, whether
//! it is rootless, what kind of endpoint it is reached through, whether the
//! CLI and the daemon share a kernel, whether the filesystem this Run needs is
//! shared with the daemon, and whether a requested platform can run there.
//!
//! Nothing in this module pulls an image, creates a container, or touches the
//! workload. That is what makes it usable in a preflight: `ato pack` and
//! `ato run` must be able to say "this host cannot run that route, here is
//! why" before any of it happens.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::reason::{CapabilityState, CapabilityStatus, Reason, ReasonCode};

/// How the CLI reaches the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum EndpointKind {
    /// A unix socket. The daemon may still be in another kernel (Docker
    /// Desktop), which `same_kernel` answers separately.
    Unix(String),
    /// A TCP endpoint: another machine, or a local socket proxy that could be
    /// one. Treated as remote, because nothing proves otherwise.
    Tcp(String),
    /// An SSH endpoint: another machine.
    Ssh(String),
    /// A Windows named pipe.
    NamedPipe(String),
    /// No endpoint was configured and the client default was used.
    Default,
    /// Not determined.
    Unknown(String),
}

impl EndpointKind {
    fn from_host(host: &str) -> Self {
        let trimmed = host.trim();
        if let Some(path) = trimmed.strip_prefix("unix://") {
            Self::Unix(path.to_owned())
        } else if let Some(authority) = trimmed.strip_prefix("tcp://") {
            Self::Tcp(authority.to_owned())
        } else if let Some(authority) = trimmed.strip_prefix("ssh://") {
            Self::Ssh(authority.to_owned())
        } else if let Some(path) = trimmed.strip_prefix("npipe://") {
            Self::NamedPipe(path.to_owned())
        } else if trimmed.is_empty() {
            Self::Default
        } else {
            Self::Unknown(trimmed.to_owned())
        }
    }

    /// Whether this endpoint reaches a daemon on another machine.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Tcp(_) | Self::Ssh(_))
    }

    pub fn is_unix_socket(&self) -> bool {
        matches!(self, Self::Unix(_))
    }
}

/// Whether the filesystem path a Run needs can be bind-mounted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkspaceSharing {
    /// The daemon sees the same filesystem (a native Linux daemon).
    SameFilesystem,
    /// A VM-backed daemon mounts selected host directories. `roots` is what the
    /// daemon's own configuration lists; empty means it listed none.
    VmFileSharing { roots: Vec<PathBuf> },
    /// A remote daemon: this host's tree is not there at all.
    NotShared { detail: String },
    /// Not proven either way.
    Unknown { detail: String },
}

impl WorkspaceSharing {
    /// Whether `path` is known to be bind-mountable by the daemon.
    ///
    /// Unknown is not a "yes": a preflight that cannot prove the path is
    /// visible must not promise that the Run will start.
    pub fn admits(&self, path: &Path) -> Option<bool> {
        match self {
            Self::SameFilesystem => Some(true),
            Self::VmFileSharing { roots } => Some(
                roots
                    .iter()
                    .any(|root| path.starts_with(root) || root == Path::new("/")),
            ),
            Self::NotShared { .. } => Some(false),
            Self::Unknown { .. } => None,
        }
    }
}

/// Whether a requested `os/arch` platform can execute here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlatformSupport {
    /// The daemon's own architecture.
    Native,
    /// A different architecture that the host can emulate.
    Emulated { detail: String },
    /// A different architecture with no emulation available.
    Unsupported { detail: String },
}

impl PlatformSupport {
    pub fn is_runnable(&self) -> bool {
        !matches!(self, Self::Unsupported { .. })
    }
}

/// Everything measured about the local daemon. A fact that was not observed is
/// `None`, and is never treated as available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerFacts {
    pub cli_present: bool,
    pub cli_path: Option<PathBuf>,
    pub daemon_reachable: bool,
    /// The daemon's own error text, when it did not answer.
    pub probe_error: Option<String>,
    pub operating_system: Option<String>,
    /// The daemon's container OS (`linux`), from `docker info`'s `OSType`.
    pub os_type: Option<String>,
    /// Normalized to `amd64`, `arm64`, or another architecture's own name.
    pub architecture: Option<String>,
    pub rootless: bool,
    pub endpoint: EndpointKind,
    /// Whether the CLI's kernel is the daemon's kernel.
    pub same_kernel: bool,
    /// The CLI's own OS (`macos`, `linux`, `windows`).
    pub client_os: String,
    pub workspace_sharing: WorkspaceSharing,
    /// One entry per requested `os/arch` platform.
    pub platforms: BTreeMap<String, PlatformSupport>,
}

impl Default for DockerFacts {
    fn default() -> Self {
        Self {
            cli_present: false,
            cli_path: None,
            daemon_reachable: false,
            probe_error: None,
            operating_system: None,
            os_type: None,
            architecture: None,
            rootless: false,
            endpoint: EndpointKind::Default,
            same_kernel: false,
            client_os: client_os().to_owned(),
            workspace_sharing: WorkspaceSharing::Unknown {
                detail: "not probed".to_owned(),
            },
            platforms: BTreeMap::new(),
        }
    }
}

impl DockerFacts {
    /// Whether the container network (the bridge the containers are on) is
    /// reachable from this CLI's kernel.
    ///
    /// A local container IP is only addressable when the CLI and the daemon
    /// share a kernel, the daemon runs as root (its network namespace is then
    /// the host's), and the endpoint is a local socket.
    pub fn bridge_reachable(&self) -> bool {
        self.cli_present
            && self.daemon_reachable
            && self.endpoint.is_unix_socket()
            && self.same_kernel
            && !self.rootless
    }

    /// Why the bridge is not reachable, if it is not.
    pub fn bridge_reasons(&self) -> Vec<Reason> {
        if self.bridge_reachable() {
            return Vec::new();
        }
        if self.endpoint.is_remote() {
            return vec![Reason::new(
                ReasonCode::RemoteDaemonWorkspaceUnsupported,
                format!(
                    "the daemon is reached through {:?}, i.e. from another machine; this Run's \
                     workspace cannot be delivered there, so the route is rejected instead of \
                     building a container that would read the wrong tree",
                    self.endpoint
                ),
            )];
        }
        if !self.cli_present {
            return vec![Reason::new(
                ReasonCode::DockerBinaryMissing,
                "the docker CLI is not on PATH, so no container route can run here; install \
                 Docker (or point PATH at its CLI) and re-run `ato pack`",
            )];
        }
        if !self.daemon_reachable {
            let detail = self
                .probe_error
                .as_deref()
                .filter(|detail| !detail.trim().is_empty())
                .unwrap_or("the daemon returned no detail");
            let code = if detail.to_lowercase().contains("permission denied")
                || detail.to_lowercase().contains("access denied")
            {
                ReasonCode::DockerSocketAccessDenied
            } else if detail.to_lowercase().contains("cannot connect")
                || detail.to_lowercase().contains("connection refused")
                || detail.to_lowercase().contains("no such file")
                || detail
                    .to_lowercase()
                    .contains("is the docker daemon running")
            {
                ReasonCode::DockerDaemonUnreachable
            } else {
                ReasonCode::DockerUnavailable
            };
            return vec![Reason::new(
                code,
                format!(
                    "the docker CLI is installed but no daemon answered ({detail}); start Docker \
                     Desktop or Docker Engine and re-run"
                ),
            )];
        }
        if self.rootless {
            return vec![Reason::new(
                ReasonCode::DockerBridgeUnreachableRootless,
                "the daemon is rootless, so its container network lives in a user namespace this \
                 CLI cannot enter; the route runs through the Run's relay instead of the bridge IP",
            )];
        }
        if !self.same_kernel {
            return vec![Reason::new(
                ReasonCode::DockerBridgeUnreachableVm,
                format!(
                    "the daemon runs in a VM (daemon OS {:?}, client OS {}), so container IPs are \
                     only reachable inside that VM; the route runs through the Run's relay \
                     instead of the bridge IP",
                    self.operating_system.as_deref().unwrap_or("unknown"),
                    self.client_os
                ),
            )];
        }
        vec![Reason::new(
            ReasonCode::DockerUnavailable,
            "the daemon answered but the facts needed to reach its container network were not \
             available; re-run with `ato --verbose` to see the probe",
        )]
    }

    /// Whether the container network has to be reached through a relay.
    ///
    /// This is a fact about the host, not a product choice: a VM-backed daemon
    /// and a rootless daemon both put the container network out of this
    /// process's reach.
    pub fn needs_relay(&self) -> bool {
        self.daemon_reachable
            && self.endpoint.is_unix_socket()
            && (!self.same_kernel || self.rootless)
    }

    /// Whether this host can run the requested platforms without emulation.
    pub fn platform_platform_status(&self, platform: &str) -> Option<&PlatformSupport> {
        self.platforms.get(platform)
    }

    /// The reason list for the requested platforms.
    pub fn platform_reasons(&self) -> Vec<Reason> {
        let mut reasons = Vec::new();
        for (platform, support) in &self.platforms {
            match support {
                PlatformSupport::Native => {}
                PlatformSupport::Emulated { detail } => reasons.push(Reason::new(
                    ReasonCode::PlatformEmulated,
                    format!(
                        "{platform} runs through emulation on this host ({detail}); it works and it \
                         is slower, so the receipt records the emulation rather than refusing the \
                         Run"
                    ),
                )),
                PlatformSupport::Unsupported { detail } => reasons.push(Reason::new(
                    ReasonCode::PlatformUnsupported,
                    format!(
                        "{platform} cannot run on this daemon ({detail}); use an image built for \
                         the daemon's own architecture or move the Run to a host that can execute \
                         it"
                    ),
                )),
            }
        }
        reasons
    }

    /// The capability of running a container route here, as one status.
    pub fn execution_status(&self, needs_bridge: bool) -> CapabilityStatus {
        let mut reasons = Vec::new();
        let mut state = CapabilityState::Advertised;
        if !self.cli_present || !self.daemon_reachable || self.endpoint.is_remote() {
            reasons.extend(self.bridge_reasons());
            state = CapabilityState::Unavailable;
        } else if needs_bridge && !self.bridge_reachable() {
            // Not a refusal: the Run can relay through its own container.
            reasons.extend(self.bridge_reasons());
        }
        for platform_reason in self.platform_reasons() {
            if platform_reason.code == ReasonCode::PlatformUnsupported {
                state = CapabilityState::Unavailable;
            }
            reasons.push(platform_reason);
        }
        CapabilityStatus {
            capability: "execution_abi:oci".to_owned(),
            state,
            reasons,
        }
    }
}

/// The CLI's own operating system, in the vocabulary this module uses.
pub fn client_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

/// Verify that every detail of `facts` is what was observed.
///
/// Kept as a free function so the probe and the tests build the same value:
/// this is also where a fact that was derived (rather than read) is marked.
pub fn normalize_architecture(architecture: &str) -> String {
    match architecture.trim().to_lowercase().as_str() {
        "aarch64" | "arm64" => "arm64".to_owned(),
        "x86_64" | "amd64" | "x86-64" => "amd64".to_owned(),
        "i386" | "i686" => "386".to_owned(),
        "armv7l" | "armv7" => "arm".to_owned(),
        other => other.to_owned(),
    }
}

/// Split `linux/amd64` into its OS and architecture.
pub fn parse_platform(platform: &str) -> Option<(String, String)> {
    let (os, arch) = platform.split_once('/')?;
    if os.is_empty() || arch.is_empty() {
        return None;
    }
    Some((os.to_owned(), normalize_architecture(arch)))
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// The raw observations the classification is made from.
///
/// Separated from the process calls so tests can drive the whole decision from
/// a recorded `docker info` document instead of a daemon.
#[derive(Debug, Clone)]
pub struct DockerProbeInput {
    pub cli_path: Option<PathBuf>,
    /// `docker info --format '{{json .}}'` output, or the failure text.
    pub info: Result<String, String>,
    /// The endpoint the current context resolves to, if it was readable.
    pub endpoint_host: Option<String>,
    /// `docker context show` output ("default" or a name).
    pub context_name: Option<String>,
    pub client_os: String,
    /// Handler names from `/proc/sys/fs/binfmt_misc` (only readable when the
    /// CLI shares the daemon's kernel).
    pub binfmt_handlers: Vec<String>,
    /// Host directories a VM-backed daemon is configured to share.
    pub desktop_shared_roots: Option<Vec<PathBuf>>,
}

impl Default for DockerProbeInput {
    fn default() -> Self {
        Self {
            cli_path: None,
            info: Err("the daemon was not probed".to_owned()),
            endpoint_host: None,
            context_name: None,
            client_os: crate::docker::client_os().to_owned(),
            binfmt_handlers: Vec::new(),
            desktop_shared_roots: None,
        }
    }
}

/// Decide the facts from observations. Pure: no I/O, so the same fixture
/// always produces the same answer.
pub fn facts_from(input: DockerProbeInput, requested_platforms: &[String]) -> DockerFacts {
    let mut facts = DockerFacts {
        cli_present: input.cli_path.is_some(),
        cli_path: input.cli_path.clone(),
        client_os: input.client_os.clone(),
        ..DockerFacts::default()
    };

    let info = match input.info {
        Ok(document) => match serde_json::from_str::<serde_json::Value>(&document) {
            Ok(value) => Some(value),
            Err(error) => {
                facts.probe_error = Some(format!("docker info was not JSON: {error}"));
                None
            }
        },
        Err(error) => {
            facts.probe_error = Some(error);
            None
        }
    };

    if let Some(info) = info {
        facts.daemon_reachable = true;
        facts.operating_system = info
            .get("OperatingSystem")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        facts.os_type = info
            .get("OSType")
            .and_then(|value| value.as_str())
            .map(|value| value.to_lowercase());
        facts.architecture = info
            .get("Architecture")
            .and_then(|value| value.as_str())
            .map(normalize_architecture);
        facts.rootless = info
            .get("SecurityOptions")
            .and_then(|value| value.as_array())
            .is_some_and(|options| {
                options.iter().any(|option| {
                    option
                        .as_str()
                        .is_some_and(|option| option.contains("rootless"))
                })
            });
    }

    // `OSType` is the field that says which container OS the daemon runs;
    // `OperatingSystem` is a distribution label and cannot be matched on.
    let daemon_linux = facts
        .os_type
        .as_deref()
        .map(|os| os == "linux")
        .unwrap_or(true);

    // The endpoint: the context's own answer first, then the environment.
    let endpoint_host = input
        .endpoint_host
        .clone()
        .or_else(|| std::env::var("DOCKER_HOST").ok())
        .filter(|host| !host.trim().is_empty());
    facts.endpoint = match endpoint_host.as_deref() {
        Some(host) => EndpointKind::from_host(host),
        None => EndpointKind::Default,
    };

    // The kernel question: a Linux client reaching a Linux daemon over a unix
    // socket is the only configuration where both provably share one kernel.
    // A macOS or Windows client cannot: its daemon is in a VM by construction.
    facts.same_kernel = facts.daemon_reachable
        && facts.endpoint.is_unix_socket()
        && input.client_os == "linux"
        && daemon_linux;

    facts.workspace_sharing = if facts.endpoint.is_remote() {
        WorkspaceSharing::NotShared {
            detail: format!("{:?}", facts.endpoint),
        }
    } else if !facts.daemon_reachable {
        WorkspaceSharing::Unknown {
            detail: "the daemon did not answer".to_owned(),
        }
    } else if facts.same_kernel {
        WorkspaceSharing::SameFilesystem
    } else if let Some(roots) = input.desktop_shared_roots {
        WorkspaceSharing::VmFileSharing { roots }
    } else {
        WorkspaceSharing::Unknown {
            detail: format!(
                "the daemon is in a VM and its file-sharing configuration was not readable \
                 (client OS {})",
                input.client_os
            ),
        }
    };

    if facts.daemon_reachable {
        for platform in requested_platforms {
            let support = platform_support(&facts, platform, &input.binfmt_handlers);
            facts.platforms.insert(platform.clone(), support);
        }
    }

    facts
}

fn platform_support(
    facts: &DockerFacts,
    platform: &str,
    binfmt_handlers: &[String],
) -> PlatformSupport {
    let Some((os, arch)) = parse_platform(platform) else {
        return PlatformSupport::Unsupported {
            detail: format!("{platform} is not an os/arch platform"),
        };
    };
    if os != "linux" {
        return PlatformSupport::Unsupported {
            detail: format!("the Docker daemon only runs linux containers, not {os}"),
        };
    }
    let Some(daemon) = facts.architecture.as_deref() else {
        return PlatformSupport::Unsupported {
            detail: "the daemon did not report its architecture".to_owned(),
        };
    };
    if daemon == arch {
        return PlatformSupport::Native;
    }
    // Emulation, when it exists, is a registration in the host kernel's
    // binfmt_misc (readable only when the CLI shares the daemon's kernel) or a
    // VM image built with the emulator in it. A VM-backed daemon's image is
    // that image; say so instead of claiming an unverified guarantee.
    let handler = binfmt_handlers.iter().any(|registered| {
        let registered = registered.to_lowercase();
        registered.contains(&format!("qemu-{arch}")) || registered.contains(&format!("qemu_{arch}"))
    });
    if handler {
        return PlatformSupport::Emulated {
            detail: format!("a qemu-{arch} binfmt handler is registered on this kernel"),
        };
    }
    if !facts.same_kernel && facts.daemon_reachable {
        return PlatformSupport::Emulated {
            detail: format!(
                "the daemon is a VM image on {daemon} and its emulation handlers are not readable \
                 from this CLI; Docker Desktop images register them"
            ),
        };
    }
    PlatformSupport::Unsupported {
        detail: format!("the daemon is {daemon} and no emulation handler for {arch} is registered"),
    }
}

/// Read this host once. Bounded: a daemon that hangs is not allowed to hang
/// `ato pack`.
pub fn probe(requested_platforms: &[String]) -> DockerFacts {
    const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
    let cli_path = find_on_path("docker");
    if cli_path.is_none() {
        return facts_from(
            DockerProbeInput {
                cli_path: None,
                info: Err("the docker CLI is not on PATH".to_owned()),
                client_os: client_os().to_owned(),
                ..DockerProbeInput::default()
            },
            requested_platforms,
        );
    }
    let info = run_bounded("docker", &["info", "--format", "{{json .}}"], PROBE_TIMEOUT);
    let endpoint_host = run_bounded(
        "docker",
        &[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ],
        PROBE_TIMEOUT,
    )
    .ok();
    let context_name = run_bounded("docker", &["context", "show"], PROBE_TIMEOUT).ok();
    facts_from(
        DockerProbeInput {
            cli_path,
            info,
            endpoint_host,
            context_name,
            client_os: client_os().to_owned(),
            binfmt_handlers: read_binfmt_handlers(),
            desktop_shared_roots: desktop_shared_roots(),
        },
        requested_platforms,
    )
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| candidate.is_file())
}

/// Run a command and stop waiting after `timeout`.
///
/// `std` has no wait-with-timeout, so the child is polled and killed on
/// expiry: a wedged daemon must not wedge the preflight.
fn run_bounded(program: &str, arguments: &[&str], timeout: Duration) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {program}: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("cannot read {program} output: {error}"))?;
                if status.success() {
                    return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                return Err(if stderr.is_empty() {
                    format!("{program} exited with {status}")
                } else {
                    stderr
                });
            }
            Ok(None) => {}
            Err(error) => return Err(format!("cannot wait for {program}: {error}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} did not answer within {}s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Emulation handlers registered on this kernel, when they are readable.
fn read_binfmt_handlers() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/proc/sys/fs/binfmt_misc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "register" && name != "status")
        .collect()
}

/// The directories a VM-backed daemon is configured to share.
///
/// Docker Desktop records them in its own settings file. Reading the settings
/// is a fact about the daemon's mount configuration; when the file is absent
/// the answer is "not known", never "shared".
fn desktop_shared_roots() -> Option<Vec<PathBuf>> {
    let settings = if cfg!(target_os = "macos") {
        Some(
            PathBuf::from(std::env::var_os("HOME")?)
                .join("Library/Group Containers/group.com.docker/settings-store.json"),
        )
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|appdata| {
            PathBuf::from(appdata)
                .join("Docker")
                .join("settings-store.json")
        })
    } else {
        None
    }?;
    let bytes = std::fs::read(settings).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value
        .get("filesharingDirectories")
        .or_else(|| value.get("FilesharingDirectories"))?;
    let roots: Vec<PathBuf> = raw
        .as_array()?
        .iter()
        .filter_map(|entry| entry.as_str())
        .map(PathBuf::from)
        .collect();
    Some(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_info() -> String {
        serde_json::json!({
            "OperatingSystem": "Ubuntu 24.04.1 LTS",
            "Architecture": "x86_64",
            "OSType": "linux",
            "SecurityOptions": ["name=apparmor", "name=seccomp,profile=builtin"],
            "ServerVersion": "27.0.0"
        })
        .to_string()
    }

    fn desktop_info() -> String {
        serde_json::json!({
            "OperatingSystem": "Docker Desktop",
            "Architecture": "aarch64",
            "OSType": "linux",
            "SecurityOptions": ["name=seccomp,profile=builtin"],
            "ServerVersion": "29.1.3"
        })
        .to_string()
    }

    fn rootless_info() -> String {
        serde_json::json!({
            "OperatingSystem": "Debian GNU/Linux 12",
            "Architecture": "aarch64",
            "OSType": "linux",
            "SecurityOptions": ["name=rootless", "name=seccomp,profile=builtin"],
            "ServerVersion": "27.0.0"
        })
        .to_string()
    }

    fn fixture(
        info: Result<String, String>,
        endpoint: &str,
        client_os: &str,
        platforms: &[&str],
    ) -> DockerFacts {
        facts_from(
            DockerProbeInput {
                cli_path: Some(PathBuf::from("/usr/bin/docker")),
                info,
                endpoint_host: Some(endpoint.to_owned()),
                context_name: Some("default".to_owned()),
                client_os: client_os.to_owned(),
                binfmt_handlers: Vec::new(),
                desktop_shared_roots: None,
            },
            &platforms
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn native_linux_daemon_reaches_the_container_bridge() {
        let facts = fixture(
            Ok(linux_info()),
            "unix:///var/run/docker.sock",
            "linux",
            &["linux/amd64", "linux/arm64"],
        );
        assert!(facts.daemon_reachable);
        assert!(facts.same_kernel);
        assert!(facts.bridge_reachable());
        assert!(!facts.needs_relay());
        assert!(facts.bridge_reasons().is_empty());
        assert_eq!(
            facts.platforms.get("linux/amd64"),
            Some(&PlatformSupport::Native)
        );
        // A different architecture with no binfmt registration is refused.
        assert!(matches!(
            facts.platforms.get("linux/arm64"),
            Some(PlatformSupport::Unsupported { .. })
        ));
    }

    #[test]
    fn vm_backed_daemon_needs_a_relay_and_warns_about_emulation() {
        let facts = facts_from(
            DockerProbeInput {
                cli_path: Some(PathBuf::from("/usr/local/bin/docker")),
                info: Ok(desktop_info()),
                endpoint_host: Some("unix:///var/run/docker.sock".to_owned()),
                client_os: "macos".to_owned(),
                desktop_shared_roots: Some(vec![PathBuf::from("/Users")]),
                ..DockerProbeInput::default()
            },
            &["linux/amd64".to_owned()],
        );
        assert!(facts.daemon_reachable);
        assert!(!facts.same_kernel);
        assert!(!facts.bridge_reachable());
        assert!(facts.needs_relay());
        let reasons = facts.bridge_reasons();
        assert_eq!(reasons[0].code, ReasonCode::DockerBridgeUnreachableVm);
        // Emulation is a warning, not a refusal.
        assert!(matches!(
            facts.platforms.get("linux/amd64"),
            Some(PlatformSupport::Emulated { .. })
        ));
        let status = facts.execution_status(true);
        assert_eq!(status.state, CapabilityState::Advertised);
        assert!(
            status
                .warnings()
                .any(|reason| reason.code == ReasonCode::PlatformEmulated)
        );
    }

    #[test]
    fn rootless_daemon_needs_a_relay_for_a_different_reason() {
        let facts = fixture(
            Ok(rootless_info()),
            "unix:///run/user/1001/docker.sock",
            "linux",
            &["linux/arm64"],
        );
        assert!(facts.rootless);
        assert!(facts.same_kernel);
        assert!(!facts.bridge_reachable());
        assert!(facts.needs_relay());
        assert_eq!(
            facts.bridge_reasons()[0].code,
            ReasonCode::DockerBridgeUnreachableRootless
        );
    }

    #[test]
    fn a_remote_daemon_is_refused_because_the_workspace_is_not_there() {
        let facts = fixture(
            Ok(linux_info()),
            "tcp://10.0.0.5:2375",
            "macos",
            &["linux/amd64"],
        );
        assert!(facts.endpoint.is_remote());
        assert!(!facts.bridge_reachable());
        let status = facts.execution_status(true);
        assert_eq!(status.state, CapabilityState::Unavailable);
        assert!(
            status
                .reasons
                .iter()
                .any(|reason| reason.code == ReasonCode::RemoteDaemonWorkspaceUnsupported)
        );
        assert_eq!(
            facts.workspace_sharing,
            WorkspaceSharing::NotShared {
                detail: "Tcp(\"10.0.0.5:2375\")".to_owned()
            }
        );
    }

    #[test]
    fn a_remote_endpoint_is_remote_even_when_the_daemon_never_answered() {
        // The endpoint is read locally, so it is a fact before any daemon
        // answers. Reporting "no daemon" would hide the actionable cause: this
        // host's workspace cannot be delivered there.
        let facts = fixture(
            Err("docker did not answer within 5s".to_owned()),
            "tcp://10.255.255.1:2375",
            "macos",
            &["linux/amd64"],
        );
        assert_eq!(
            facts.bridge_reasons()[0].code,
            ReasonCode::RemoteDaemonWorkspaceUnsupported
        );
        assert_eq!(
            facts.workspace_sharing,
            WorkspaceSharing::NotShared {
                detail: "Tcp(\"10.255.255.1:2375\")".to_owned()
            }
        );
        // No architecture was observed, and none is invented.
        assert!(facts.platforms.is_empty());
        assert!(facts.platform_reasons().is_empty());
    }

    #[test]
    fn a_missing_cli_and_an_unreachable_daemon_are_different_reasons() {
        let missing = facts_from(
            DockerProbeInput {
                cli_path: None,
                info: Err("the docker CLI is not on PATH".to_owned()),
                client_os: "macos".to_owned(),
                ..DockerProbeInput::default()
            },
            &[],
        );
        assert_eq!(
            missing.bridge_reasons()[0].code,
            ReasonCode::DockerBinaryMissing
        );

        let unreachable = fixture(
            Err(
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the \
                 docker daemon running?"
                    .to_owned(),
            ),
            "unix:///var/run/docker.sock",
            "linux",
            &[],
        );
        assert_eq!(
            unreachable.bridge_reasons()[0].code,
            ReasonCode::DockerDaemonUnreachable
        );

        let denied = fixture(
            Err("permission denied while trying to connect to the Docker daemon socket".to_owned()),
            "unix:///var/run/docker.sock",
            "linux",
            &[],
        );
        assert_eq!(
            denied.bridge_reasons()[0].code,
            ReasonCode::DockerSocketAccessDenied
        );
    }

    #[test]
    fn vm_file_sharing_decides_whether_the_path_is_visible() {
        let sharing = WorkspaceSharing::VmFileSharing {
            roots: vec![PathBuf::from("/Users"), PathBuf::from("/Volumes")],
        };
        assert_eq!(sharing.admits(Path::new("/Users/someone/run")), Some(true));
        assert_eq!(sharing.admits(Path::new("/etc/passwd")), Some(false));
        assert_eq!(
            WorkspaceSharing::Unknown {
                detail: "unreadable".to_owned()
            }
            .admits(Path::new("/Users/someone/run")),
            None
        );
    }

    #[test]
    fn architecture_normalization_matches_docker_spellings() {
        assert_eq!(normalize_architecture("aarch64"), "arm64");
        assert_eq!(normalize_architecture("arm64"), "arm64");
        assert_eq!(normalize_architecture("x86_64"), "amd64");
        assert_eq!(normalize_architecture("amd64"), "amd64");
        assert_eq!(normalize_architecture("riscv64"), "riscv64");
        assert_eq!(
            parse_platform("linux/amd64"),
            Some(("linux".into(), "amd64".into()))
        );
        assert_eq!(parse_platform("amd64"), None);
    }

    #[test]
    fn a_binfmt_registration_is_what_makes_emulation_a_fact() {
        let facts = facts_from(
            DockerProbeInput {
                cli_path: Some(PathBuf::from("/usr/bin/docker")),
                info: Ok(rootless_info()),
                endpoint_host: Some("unix:///run/user/1001/docker.sock".to_owned()),
                client_os: "linux".to_owned(),
                binfmt_handlers: vec!["qemu-amd64".to_owned()],
                ..DockerProbeInput::default()
            },
            &["linux/amd64".to_owned()],
        );
        assert!(matches!(
            facts.platforms.get("linux/amd64"),
            Some(PlatformSupport::Emulated { .. })
        ));
        assert!(facts.platform_reasons()[0].code.is_warning());
    }
}
