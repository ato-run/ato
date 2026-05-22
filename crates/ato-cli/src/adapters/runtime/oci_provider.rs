#![allow(dead_code)]

use async_trait::async_trait;
use capsule_core::runtime::oci::{
    BollardOciRuntimeClient, OciContainerInspect, OciContainerRequest, OciLogChunk,
    OciNetworkRequest, OciRuntimeClient,
};
use capsule_core::types::{
    OciImageResolution, OciPlatform, OciProviderKind, OciProviderMode, OciProviderSemantics,
    OciProviderSubstrate,
};
use std::process::Command;
use thiserror::Error;
use tokio::sync::mpsc;

const PODMAN_POLICY_PROFILE_V1: &str = "oci-podman-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciProviderProbe {
    pub ready: bool,
    pub semantics: OciProviderSemantics,
    pub inventory: OciProviderInventory,
    pub detail: Option<String>,
}

impl OciProviderProbe {
    pub(crate) fn require_ready(self) -> Result<Self, OciProviderError> {
        if self.ready {
            return Ok(self);
        }
        let provider = match self.inventory.kind {
            OciProviderKind::Podman => "podman",
            OciProviderKind::DockerCompatible => "docker-compatible",
            OciProviderKind::AtoNative => "ato-native",
        };
        Err(OciProviderError::NotReady {
            provider,
            reason: self
                .detail
                .clone()
                .unwrap_or_else(|| "provider readiness probe reported not ready".to_string()),
            inventory: Some(self.inventory.clone()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciImageResolveRequest {
    pub declared_ref: String,
    pub platform: Option<OciPlatform>,
    pub importer_input_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciProviderInventory {
    pub kind: OciProviderKind,
    pub binary: OciProviderBinaryStatus,
    pub version: Option<String>,
    pub mode: OciProviderMode,
    pub machine: OciProviderMachineStatus,
    pub semantics: OciProviderSemantics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciProviderBinaryStatus {
    Found,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciProviderMachineStatus {
    NativeLinux,
    MachineRequired,
    MachineRunning,
    MachineNotRunning,
    MachineUnknown,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum OciProviderError {
    #[error("OCI provider '{provider}' is missing required binary '{binary}'")]
    Missing {
        provider: &'static str,
        binary: &'static str,
    },
    #[error("OCI provider '{provider}' is not ready: {reason}")]
    NotReady {
        provider: &'static str,
        reason: String,
        inventory: Option<OciProviderInventory>,
    },
    #[error("OCI provider '{provider}' probe failed: {message}")]
    ProbeFailed {
        provider: &'static str,
        message: String,
    },
    #[error("OCI provider '{provider}' does not support platform '{platform}'")]
    UnsupportedPlatform {
        provider: &'static str,
        platform: String,
    },
    #[error(
        "OCI provider '{provider}' cannot satisfy required capability '{capability}': detected {detected}"
    )]
    CapabilityUnsupported {
        provider: &'static str,
        capability: &'static str,
        detected: String,
    },
    #[error("OCI provider '{provider}' command failed: {command}: {message}")]
    CommandFailed {
        provider: &'static str,
        command: String,
        status: Option<i32>,
        message: String,
    },
    #[error("OCI provider operation '{operation}' failed: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
    #[error("OCI provider does not support operation '{0}'")]
    Unsupported(&'static str),
}

impl OciProviderError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "oci_provider_missing",
            Self::NotReady { .. } => "oci_provider_not_ready",
            Self::ProbeFailed { .. } => "oci_provider_probe_failed",
            Self::UnsupportedPlatform { .. } => "oci_provider_unsupported_platform",
            Self::CapabilityUnsupported { .. } => "oci_provider_capability_unsupported",
            Self::CommandFailed { .. } | Self::Operation { .. } => "oci_provider_command_failed",
            Self::Unsupported(_) => "oci_provider_unsupported_operation",
        }
    }

    fn operation(operation: &'static str, err: capsule_core::CapsuleError) -> Self {
        Self::Operation {
            operation,
            message: err.to_string(),
        }
    }
}

#[async_trait]
pub(crate) trait OciProvider: Send + Sync {
    fn semantics(&self) -> &OciProviderSemantics;

    async fn probe(&self) -> Result<OciProviderProbe, OciProviderError>;

    async fn resolve_image(
        &self,
        _request: &OciImageResolveRequest,
    ) -> Result<OciImageResolution, OciProviderError> {
        Err(OciProviderError::Unsupported("resolve_image"))
    }

    async fn pull_image(&self, image: &OciImageResolution) -> Result<(), OciProviderError>;

    async fn create_network(&self, request: &OciNetworkRequest)
        -> Result<String, OciProviderError>;

    async fn remove_network(&self, network_name: &str) -> Result<(), OciProviderError>;

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> Result<String, OciProviderError>;

    async fn start_container(&self, container_id: &str) -> Result<(), OciProviderError>;

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError>;

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError>;

    async fn wait_container(&self, container_id: &str) -> Result<i64, OciProviderError>;

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> Result<(), OciProviderError>;

    async fn remove_container(
        &self,
        container_id: &str,
        force: bool,
    ) -> Result<(), OciProviderError>;
}

pub(crate) trait OciProviderSelector: Send + Sync {
    type Provider: OciProvider;

    fn select_provider(&self) -> Self::Provider;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DefaultOciProviderSelector;

impl OciProviderSelector for DefaultOciProviderSelector {
    type Provider = PodmanProvider<SystemCommandRunner>;

    fn select_provider(&self) -> Self::Provider {
        PodmanProvider::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    fn success(&self) -> bool {
        self.status == 0
    }
}

pub(crate) trait OciCommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemCommandRunner;

impl OciCommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let output = Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PodmanProbePlatform {
    Linux,
    Macos,
    Windows,
    Unsupported(String),
}

impl PodmanProbePlatform {
    fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Self::Linux,
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            other => Self::Unsupported(other.to_string()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Unsupported(value) => value.as_str(),
        }
    }
}

pub(crate) struct PodmanProvider<R = SystemCommandRunner> {
    runner: R,
    platform: PodmanProbePlatform,
    semantics: OciProviderSemantics,
}

impl PodmanProvider<SystemCommandRunner> {
    pub(crate) fn new() -> Self {
        Self::with_runner(SystemCommandRunner, PodmanProbePlatform::current())
    }
}

impl<R> PodmanProvider<R> {
    pub(crate) fn with_runner(runner: R, platform: PodmanProbePlatform) -> Self {
        Self {
            runner,
            platform,
            semantics: podman_semantics(OciProviderMode::Unknown, OciProviderSubstrate::Unknown),
        }
    }
}

#[async_trait]
impl<R> OciProvider for PodmanProvider<R>
where
    R: OciCommandRunner + Send + Sync,
{
    fn semantics(&self) -> &OciProviderSemantics {
        &self.semantics
    }

    async fn probe(&self) -> Result<OciProviderProbe, OciProviderError> {
        let version_output = run_provider_command(&self.runner, "podman", &["--version"])?;
        if !version_output.success() {
            return Err(command_failed("podman --version", version_output));
        }
        let version = parse_podman_version(&version_output.stdout);

        match &self.platform {
            PodmanProbePlatform::Linux => {
                let mode = detect_linux_podman_mode(&self.runner);
                let semantics = podman_semantics(mode, OciProviderSubstrate::NativeLinux);
                let inventory = OciProviderInventory {
                    kind: OciProviderKind::Podman,
                    binary: OciProviderBinaryStatus::Found,
                    version,
                    mode,
                    machine: OciProviderMachineStatus::NativeLinux,
                    semantics: semantics.clone(),
                };
                Ok(OciProviderProbe {
                    ready: true,
                    semantics,
                    inventory,
                    detail: None,
                })
            }
            PodmanProbePlatform::Macos | PodmanProbePlatform::Windows => {
                let machine_output = run_provider_command(
                    &self.runner,
                    "podman",
                    &["machine", "list", "--format", "json"],
                )?;
                if !machine_output.success() {
                    return Err(command_failed(
                        "podman machine list --format json",
                        machine_output,
                    ));
                }
                let machine = parse_machine_status(&machine_output.stdout);
                let ready = matches!(machine, OciProviderMachineStatus::MachineRunning);
                let substrate = match machine {
                    OciProviderMachineStatus::MachineRunning
                    | OciProviderMachineStatus::MachineNotRunning
                    | OciProviderMachineStatus::MachineRequired => {
                        OciProviderSubstrate::PodmanMachine
                    }
                    _ => OciProviderSubstrate::Unknown,
                };
                let semantics = podman_semantics(OciProviderMode::Unknown, substrate);
                let detail = match machine {
                    OciProviderMachineStatus::MachineRunning => None,
                    OciProviderMachineStatus::MachineNotRunning => {
                        Some("podman machine exists but is not running".to_string())
                    }
                    OciProviderMachineStatus::MachineRequired => {
                        Some("podman machine is required but no machine is configured".to_string())
                    }
                    OciProviderMachineStatus::MachineUnknown => {
                        Some("podman machine list output was not recognized".to_string())
                    }
                    OciProviderMachineStatus::NativeLinux => None,
                };
                let inventory = OciProviderInventory {
                    kind: OciProviderKind::Podman,
                    binary: OciProviderBinaryStatus::Found,
                    version,
                    mode: OciProviderMode::Unknown,
                    machine,
                    semantics: semantics.clone(),
                };
                Ok(OciProviderProbe {
                    ready,
                    semantics,
                    inventory,
                    detail,
                })
            }
            PodmanProbePlatform::Unsupported(platform) => {
                Err(OciProviderError::UnsupportedPlatform {
                    provider: "podman",
                    platform: platform.clone(),
                })
            }
        }
    }

    async fn pull_image(&self, _image: &OciImageResolution) -> Result<(), OciProviderError> {
        Err(OciProviderError::Unsupported("pull_image"))
    }

    async fn create_network(
        &self,
        _request: &OciNetworkRequest,
    ) -> Result<String, OciProviderError> {
        Err(OciProviderError::Unsupported("create_network"))
    }

    async fn remove_network(&self, _network_name: &str) -> Result<(), OciProviderError> {
        Err(OciProviderError::Unsupported("remove_network"))
    }

    async fn create_container(
        &self,
        _request: &OciContainerRequest,
    ) -> Result<String, OciProviderError> {
        Err(OciProviderError::Unsupported("create_container"))
    }

    async fn start_container(&self, _container_id: &str) -> Result<(), OciProviderError> {
        Err(OciProviderError::Unsupported("start_container"))
    }

    async fn inspect_container(
        &self,
        _container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError> {
        Err(OciProviderError::Unsupported("inspect_container"))
    }

    async fn logs(
        &self,
        _container_id: &str,
        _follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError> {
        Err(OciProviderError::Unsupported("logs"))
    }

    async fn wait_container(&self, _container_id: &str) -> Result<i64, OciProviderError> {
        Err(OciProviderError::Unsupported("wait_container"))
    }

    async fn stop_container(
        &self,
        _container_id: &str,
        _timeout_secs: i64,
    ) -> Result<(), OciProviderError> {
        Err(OciProviderError::Unsupported("stop_container"))
    }

    async fn remove_container(
        &self,
        _container_id: &str,
        _force: bool,
    ) -> Result<(), OciProviderError> {
        Err(OciProviderError::Unsupported("remove_container"))
    }
}

fn run_provider_command<R: OciCommandRunner>(
    runner: &R,
    program: &'static str,
    args: &[&str],
) -> Result<CommandOutput, OciProviderError> {
    runner.run(program, args).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            OciProviderError::Missing {
                provider: "podman",
                binary: program,
            }
        } else {
            OciProviderError::ProbeFailed {
                provider: "podman",
                message: format!("failed to run {} {}: {err}", program, args.join(" ")),
            }
        }
    })
}

fn command_failed(command: &'static str, output: CommandOutput) -> OciProviderError {
    let message = if output.stderr.trim().is_empty() {
        output.stdout.trim().to_string()
    } else {
        output.stderr.trim().to_string()
    };
    OciProviderError::CommandFailed {
        provider: "podman",
        command: command.to_string(),
        status: Some(output.status),
        message,
    }
}

fn podman_semantics(
    mode: OciProviderMode,
    substrate: OciProviderSubstrate,
) -> OciProviderSemantics {
    OciProviderSemantics {
        kind: OciProviderKind::Podman,
        mode,
        substrate,
        policy_profile: PODMAN_POLICY_PROFILE_V1.to_string(),
    }
}

fn parse_podman_version(stdout: &str) -> Option<String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    line.strip_prefix("podman version ")
        .or_else(|| line.strip_prefix("Podman version "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn detect_linux_podman_mode<R: OciCommandRunner>(runner: &R) -> OciProviderMode {
    let Ok(output) = runner.run(
        "podman",
        &["info", "--format", "{{.Host.Security.Rootless}}"],
    ) else {
        return OciProviderMode::Unknown;
    };
    if !output.success() {
        return OciProviderMode::Unknown;
    }
    match output.stdout.trim().to_ascii_lowercase().as_str() {
        "true" => OciProviderMode::Rootless,
        "false" => OciProviderMode::Rootful,
        _ => OciProviderMode::Unknown,
    }
}

fn parse_machine_status(stdout: &str) -> OciProviderMachineStatus {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return OciProviderMachineStatus::MachineUnknown;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return OciProviderMachineStatus::MachineUnknown;
    };
    let Some(machines) = value.as_array() else {
        return OciProviderMachineStatus::MachineUnknown;
    };
    if machines.is_empty() {
        return OciProviderMachineStatus::MachineRequired;
    }
    if machines.iter().any(machine_running) {
        return OciProviderMachineStatus::MachineRunning;
    }
    OciProviderMachineStatus::MachineNotRunning
}

fn machine_running(value: &serde_json::Value) -> bool {
    value
        .get("Running")
        .or_else(|| value.get("running"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub(crate) struct DockerCompatibleOciProvider<C> {
    client: C,
    semantics: OciProviderSemantics,
}

impl<C> DockerCompatibleOciProvider<C> {
    pub(crate) fn new(client: C, semantics: OciProviderSemantics) -> Self {
        Self { client, semantics }
    }
}

impl DockerCompatibleOciProvider<BollardOciRuntimeClient> {
    pub(crate) fn connect_default(
        semantics: OciProviderSemantics,
    ) -> Result<Self, OciProviderError> {
        let client = BollardOciRuntimeClient::connect_default().map_err(|err| {
            OciProviderError::ProbeFailed {
                provider: "docker-compatible",
                message: err.to_string(),
            }
        })?;
        Ok(Self { client, semantics })
    }
}

#[async_trait]
impl<C> OciProvider for DockerCompatibleOciProvider<C>
where
    C: OciRuntimeClient + Send + Sync,
{
    fn semantics(&self) -> &OciProviderSemantics {
        &self.semantics
    }

    async fn probe(&self) -> Result<OciProviderProbe, OciProviderError> {
        let inventory = OciProviderInventory {
            kind: self.semantics.kind,
            binary: OciProviderBinaryStatus::Found,
            version: None,
            mode: self.semantics.mode,
            machine: OciProviderMachineStatus::MachineUnknown,
            semantics: self.semantics.clone(),
        };
        Ok(OciProviderProbe {
            ready: true,
            semantics: self.semantics.clone(),
            inventory,
            detail: None,
        })
    }

    async fn pull_image(&self, image: &OciImageResolution) -> Result<(), OciProviderError> {
        self.client
            .pull_image(&image.declared_ref)
            .await
            .map_err(|err| OciProviderError::operation("pull_image", err))
    }

    async fn create_network(
        &self,
        request: &OciNetworkRequest,
    ) -> Result<String, OciProviderError> {
        self.client
            .create_network(request)
            .await
            .map_err(|err| OciProviderError::operation("create_network", err))
    }

    async fn remove_network(&self, network_name: &str) -> Result<(), OciProviderError> {
        self.client
            .remove_network(network_name)
            .await
            .map_err(|err| OciProviderError::operation("remove_network", err))
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> Result<String, OciProviderError> {
        self.client
            .create_container(request)
            .await
            .map_err(|err| OciProviderError::operation("create_container", err))
    }

    async fn start_container(&self, container_id: &str) -> Result<(), OciProviderError> {
        self.client
            .start_container(container_id)
            .await
            .map_err(|err| OciProviderError::operation("start_container", err))
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError> {
        self.client
            .inspect_container(container_id)
            .await
            .map_err(|err| OciProviderError::operation("inspect_container", err))
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError> {
        self.client
            .logs(container_id, follow)
            .await
            .map_err(|err| OciProviderError::operation("logs", err))
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64, OciProviderError> {
        self.client
            .wait_container(container_id)
            .await
            .map_err(|err| OciProviderError::operation("wait_container", err))
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> Result<(), OciProviderError> {
        self.client
            .stop_container(container_id, timeout_secs)
            .await
            .map_err(|err| OciProviderError::operation("stop_container", err))
    }

    async fn remove_container(
        &self,
        container_id: &str,
        force: bool,
    ) -> Result<(), OciProviderError> {
        self.client
            .remove_container(container_id, force)
            .await
            .map_err(|err| OciProviderError::operation("remove_container", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::runtime::oci::{OciContainerInspect, OciMountSpec, OciPortSpec};
    use capsule_core::types::{OciProviderKind, OciProviderMode, OciProviderSubstrate};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeRunner {
        outputs: Arc<Mutex<HashMap<String, std::io::Result<CommandOutput>>>>,
    }

    impl FakeRunner {
        fn with_output(self, args: &[&str], output: CommandOutput) -> Self {
            self.outputs
                .lock()
                .unwrap()
                .insert(args.join(" "), Ok(output));
            self
        }

        fn with_error(self, args: &[&str], error: std::io::Error) -> Self {
            self.outputs
                .lock()
                .unwrap()
                .insert(args.join(" "), Err(error));
            self
        }
    }

    impl OciCommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let key = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            let mut outputs = self.outputs.lock().unwrap();
            match outputs.remove(&key) {
                Some(result) => result,
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("missing fake command: {key}"),
                )),
            }
        }
    }

    fn output(status: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[derive(Clone, Default)]
    struct FakeClient {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl OciRuntimeClient for FakeClient {
        async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
            self.events.lock().unwrap().push(format!("pull:{image}"));
            Ok(())
        }

        async fn create_network(
            &self,
            request: &OciNetworkRequest,
        ) -> capsule_core::Result<String> {
            Ok(request.name.clone())
        }

        async fn remove_network(&self, _network_name: &str) -> capsule_core::Result<()> {
            Ok(())
        }

        async fn create_container(
            &self,
            request: &OciContainerRequest,
        ) -> capsule_core::Result<String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("create:{}", request.image));
            Ok(request.name.clone())
        }

        async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("start:{container_id}"));
            Ok(())
        }

        async fn inspect_container(
            &self,
            _container_id: &str,
        ) -> capsule_core::Result<OciContainerInspect> {
            Ok(OciContainerInspect::default())
        }

        async fn logs(
            &self,
            _container_id: &str,
            _follow: bool,
        ) -> capsule_core::Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }

        async fn wait_container(&self, _container_id: &str) -> capsule_core::Result<i64> {
            Ok(0)
        }

        async fn stop_container(
            &self,
            _container_id: &str,
            _timeout_secs: i64,
        ) -> capsule_core::Result<()> {
            Ok(())
        }

        async fn remove_container(
            &self,
            _container_id: &str,
            _force: bool,
        ) -> capsule_core::Result<()> {
            Ok(())
        }
    }

    fn semantics() -> OciProviderSemantics {
        OciProviderSemantics {
            kind: OciProviderKind::DockerCompatible,
            mode: OciProviderMode::Rootless,
            substrate: OciProviderSubstrate::Unknown,
            policy_profile: "oci-docker-compatible-v1".to_string(),
        }
    }

    fn image() -> OciImageResolution {
        OciImageResolution {
            declared_ref: "ghcr.io/acme/app:latest".to_string(),
            resolved_digest: "sha256:abc".to_string(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "arm64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        }
    }

    #[tokio::test]
    async fn podman_probe_reports_missing_binary_as_typed_error() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_error(
                &["podman", "--version"],
                std::io::Error::new(std::io::ErrorKind::NotFound, "missing podman"),
            ),
            PodmanProbePlatform::Linux,
        );

        let error = provider.probe().await.expect_err("missing podman");

        assert_eq!(error.code(), "oci_provider_missing");
        assert!(matches!(
            error,
            OciProviderError::Missing {
                provider: "podman",
                binary: "podman"
            }
        ));
    }

    #[tokio::test]
    async fn podman_probe_linux_reports_version_native_and_rootless() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(0, "true\n", ""),
                ),
            PodmanProbePlatform::Linux,
        );

        let probe = provider.probe().await.expect("probe");

        assert!(probe.ready);
        assert_eq!(probe.inventory.kind, OciProviderKind::Podman);
        assert_eq!(probe.inventory.binary, OciProviderBinaryStatus::Found);
        assert_eq!(probe.inventory.version.as_deref(), Some("5.2.1"));
        assert_eq!(probe.inventory.mode, OciProviderMode::Rootless);
        assert_eq!(
            probe.inventory.machine,
            OciProviderMachineStatus::NativeLinux
        );
        assert_eq!(probe.semantics.kind, OciProviderKind::Podman);
        assert_eq!(probe.semantics.mode, OciProviderMode::Rootless);
        assert_eq!(probe.semantics.substrate, OciProviderSubstrate::NativeLinux);
        assert_eq!(probe.semantics.policy_profile, "oci-podman-v1");
    }

    #[tokio::test]
    async fn podman_probe_macos_reports_machine_running() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(
                        0,
                        r#"[{"Name":"podman-machine-default","MachineId":"volatile-id","Running":true}]"#,
                        "",
                    ),
                ),
            PodmanProbePlatform::Macos,
        );

        let probe = provider.probe().await.expect("probe");
        let semantics = serde_json::to_string(&probe.semantics).expect("semantics");

        assert!(probe.ready);
        assert_eq!(
            probe.inventory.machine,
            OciProviderMachineStatus::MachineRunning
        );
        assert_eq!(probe.semantics.kind, OciProviderKind::Podman);
        assert_eq!(probe.semantics.mode, OciProviderMode::Unknown);
        assert_eq!(
            probe.semantics.substrate,
            OciProviderSubstrate::PodmanMachine
        );
        assert!(!semantics.contains("podman-machine-default"));
        assert!(!semantics.contains("volatile-id"));
    }

    #[tokio::test]
    async fn podman_probe_windows_reports_machine_not_running() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(
                        0,
                        r#"[{"Name":"podman-machine-default","Running":false}]"#,
                        "",
                    ),
                ),
            PodmanProbePlatform::Windows,
        );

        let probe = provider.probe().await.expect("probe");

        assert!(!probe.ready);
        assert_eq!(
            probe.inventory.machine,
            OciProviderMachineStatus::MachineNotRunning
        );
        assert_eq!(
            probe.semantics.substrate,
            OciProviderSubstrate::PodmanMachine
        );
        assert!(probe
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("not running")));
        assert_eq!(
            probe.clone().require_ready().expect_err("not ready").code(),
            "oci_provider_not_ready"
        );
    }

    #[tokio::test]
    async fn podman_probe_malformed_machine_output_does_not_panic() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, "not json", ""),
                ),
            PodmanProbePlatform::Macos,
        );

        let probe = provider.probe().await.expect("probe");

        assert!(!probe.ready);
        assert_eq!(
            probe.inventory.machine,
            OciProviderMachineStatus::MachineUnknown
        );
        assert_eq!(probe.semantics.substrate, OciProviderSubstrate::Unknown);
    }

    #[tokio::test]
    async fn podman_probe_empty_machine_list_reports_machine_required() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, "[]", ""),
                ),
            PodmanProbePlatform::Macos,
        );

        let probe = provider.probe().await.expect("probe");

        assert!(!probe.ready);
        assert_eq!(
            probe.inventory.machine,
            OciProviderMachineStatus::MachineRequired
        );
        assert_eq!(
            probe.semantics.substrate,
            OciProviderSubstrate::PodmanMachine
        );
    }

    #[test]
    fn oci_provider_selector_defaults_to_official_podman_provider() {
        let selector = DefaultOciProviderSelector;
        let provider = selector.select_provider();
        let semantics = provider.semantics();

        assert_eq!(semantics.kind, OciProviderKind::Podman);
        assert_eq!(semantics.mode, OciProviderMode::Unknown);
        assert_eq!(semantics.substrate, OciProviderSubstrate::Unknown);
        assert_eq!(semantics.policy_profile, "oci-podman-v1");
    }

    #[tokio::test]
    async fn oci_provider_trait_delegates_docker_compatible_adapter_without_bollard_types() {
        let client = FakeClient::default();
        let events = client.events.clone();
        let provider = DockerCompatibleOciProvider::new(client, semantics());

        let probe = provider.probe().await.expect("probe");
        assert!(probe.ready);
        assert_eq!(probe.semantics.policy_profile, "oci-docker-compatible-v1");

        provider.pull_image(&image()).await.expect("pull");
        let container_id = provider
            .create_container(&OciContainerRequest {
                name: "ato-test".to_string(),
                image: "ghcr.io/acme/app:latest".to_string(),
                cmd: vec!["serve".to_string()],
                env: HashMap::new(),
                working_dir: None,
                labels: HashMap::new(),
                mounts: vec![OciMountSpec {
                    source: "state://app".to_string(),
                    target: "/data".to_string(),
                    readonly: false,
                }],
                ports: vec![OciPortSpec {
                    container_port: 3000,
                    host_port: None,
                    protocol: "tcp".to_string(),
                    host_ip: Some("127.0.0.1".to_string()),
                }],
                network: None,
                aliases: Vec::new(),
            })
            .await
            .expect("create");
        provider
            .start_container(&container_id)
            .await
            .expect("start");

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "pull:ghcr.io/acme/app:latest".to_string(),
                "create:ghcr.io/acme/app:latest".to_string(),
                "start:ato-test".to_string(),
            ]
        );
    }
}
