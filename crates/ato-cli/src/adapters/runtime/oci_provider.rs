#![allow(dead_code)]

use async_trait::async_trait;
use capsule_core::CapsuleError;
use capsule_core::runtime::oci::{
    BollardOciRuntimeClient, OciContainerInspect, OciContainerRequest, OciLogChunk,
    OciNetworkRequest, OciRuntimeClient,
};
use capsule_core::types::{
    OciImageResolution, OciPlatform, OciProviderKind, OciProviderMode, OciProviderSemantics,
    OciProviderSubstrate,
};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use thiserror::Error;
use tokio::sync::mpsc;

use capsule_core::podman::ATO_PODMAN_MACHINE_NAME;

use crate::adapters::runtime::podman_machine::{
    PodmanMachine, PodmanMachineStatus, parse_machine_entries, parse_podman_machine_list,
};
use crate::application::provider_projection::oci::{
    OciMountProjection, OciPortProjection, OciProjectionPlan, OciRenderError,
    render_podman_mount_value, render_podman_port_value,
};

// In tests use zero timeouts so the poll loop exits immediately without sleeping.
#[cfg(not(test))]
const MACHINE_READY_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
#[cfg(not(test))]
const MACHINE_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(test)]
const MACHINE_READY_POLL_TIMEOUT: std::time::Duration = std::time::Duration::ZERO;
#[cfg(test)]
const MACHINE_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::ZERO;

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
pub(crate) struct OciImageResolutionRequest {
    pub target_label: String,
    pub declared_ref: String,
    pub requested_platform: Option<OciPlatform>,
    pub resolution_mode: OciImageResolutionMode,
    pub importer_input_hash: Option<String>,
    /// Platform emulation policy for this target.
    /// NativeOnly (default): reject images whose platform does not match the host.
    /// AllowEmulation: allow pulling and running non-native images (e.g., amd64 on arm64).
    pub platform_policy: OciPlatformPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OciPlatformPolicy {
    /// Only accept images matching the host platform. Default.
    #[default]
    NativeOnly,
    /// Accept images for a non-native platform; provider must pass `--platform` to pull/create.
    AllowEmulation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciImageResolutionMode {
    Required,
    BestEffort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciResolvedImage {
    pub declared_ref: String,
    pub resolved_digest: String,
    pub platform: OciPlatform,
    pub media_type: Option<String>,
    pub provider_semantics: OciProviderSemantics,
}

impl OciResolvedImage {
    pub(crate) fn into_lock_resolution(self) -> OciImageResolution {
        OciImageResolution {
            declared_ref: self.declared_ref,
            resolved_digest: self.resolved_digest,
            platform: self.platform,
            importer_input_hash: None,
        }
    }
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

    #[error("OCI image reference '{declared_ref}' is malformed: {reason}")]
    ImageRefMalformed {
        declared_ref: String,
        reason: String,
    },

    #[error("OCI image '{declared_ref}' resolve failed: {reason}")]
    ImageResolveFailed {
        declared_ref: String,
        reason: String,
    },

    #[error("OCI image '{declared_ref}' does not support platform '{platform}'")]
    ImagePlatformUnsupported {
        declared_ref: String,
        platform: String,
    },

    #[error("OCI registry authentication required for '{declared_ref}'")]
    RegistryAuthRequired { declared_ref: String },

    #[error("OCI policy envelope is missing from the execution plan")]
    OciPolicyEnvelopeMissing,

    #[error(
        "OCI image resolution required before execution: '{declared_ref}' has no resolved digest"
    )]
    OciImageResolutionRequired { declared_ref: String },

    #[error("OCI execution gate failed: {reason}")]
    OciExecutionGateFailed { reason: String },

    #[error("OCI container '{container_name}' failed to start: {message}")]
    OciContainerStartFailed {
        container_name: String,
        message: String,
    },

    #[error("OCI cleanup operation '{operation}' failed: {message}")]
    OciCleanupFailed { operation: String, message: String },

    #[error("Podman machine is not configured. Run: podman machine init && podman machine start")]
    MachineNotConfigured,

    #[error(
        "Multiple Podman machines are stopped and Ato cannot decide which one to start. \
         Machines: {names}. Start the desired machine manually: podman machine start <name>"
    )]
    MachineAmbiguous { names: String },

    #[error("Failed to start Podman machine '{machine_name}': {reason}")]
    MachineStartFailed {
        machine_name: String,
        reason: String,
    },

    #[error(
        "Podman machine '{machine_name}' did not become ready within {elapsed_secs}s after start"
    )]
    MachineReadyTimeout {
        machine_name: String,
        elapsed_secs: u64,
    },

    #[error(
        "This recipe needs a container runtime, but Podman is disabled in Ato settings. \
         Enable Podman in Settings, then try again."
    )]
    PodmanDisabled,

    #[error(
        "mount '{target}' is read-only but declares an ownership init; \
         read-only mounts cannot be re-owned by the engine"
    )]
    ReadOnlyOwnershipConflict { target: String },

    #[error(
        "ATO_PODMAN_BIN is set to '{path}' but that path is not a usable executable. \
         Unset ATO_PODMAN_BIN or set it to a valid podman binary path."
    )]
    InvalidBinaryOverride { path: String },

    #[error(
        "Podman storage or graph driver reported an error. \
         This is a provider health issue, not a recipe error.\n\
         Detail: {reason}\n\
         Fix: podman system reset (warning: destroys all containers and images)."
    )]
    StorageCorrupted { reason: String },

    #[error(
        "Docker-compatible daemon is not reachable: {reason}. \
         Start Docker Desktop or switch the container_runtime setting to podman."
    )]
    DockerDaemonUnavailable { reason: String },

    #[error(
        "Permission denied when connecting to the Docker socket. \
         Add your user to the 'docker' group or use 'sudo', or use Podman instead."
    )]
    DockerPermissionDenied,
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
            Self::ImageRefMalformed { .. } => "oci_image_ref_malformed",
            Self::ImageResolveFailed { .. } => "oci_image_resolve_failed",
            Self::ImagePlatformUnsupported { .. } => "oci_image_platform_unsupported",
            Self::RegistryAuthRequired { .. } => "oci_registry_auth_required",
            Self::OciPolicyEnvelopeMissing => "oci_policy_envelope_missing",
            Self::OciImageResolutionRequired { .. } => "oci_image_resolution_required",
            Self::OciExecutionGateFailed { .. } => "oci_execution_gate_failed",
            Self::OciContainerStartFailed { .. } => "oci_container_start_failed",
            Self::OciCleanupFailed { .. } => "oci_cleanup_failed",
            Self::MachineNotConfigured => "oci_machine_not_configured",
            Self::MachineAmbiguous { .. } => "oci_machine_ambiguous",
            Self::MachineStartFailed { .. } => "oci_machine_start_failed",
            Self::MachineReadyTimeout { .. } => "oci_machine_ready_timeout",
            Self::PodmanDisabled => "oci_podman_disabled",
            Self::ReadOnlyOwnershipConflict { .. } => "oci_readonly_ownership_conflict",
            Self::InvalidBinaryOverride { .. } => "oci_invalid_binary_override",
            Self::StorageCorrupted { .. } => "oci_storage_corrupted",
            Self::DockerDaemonUnavailable { .. } => "oci_docker_daemon_unavailable",
            Self::DockerPermissionDenied => "oci_docker_permission_denied",
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

    /// Ensure the OCI provider is ready to accept container operations.
    ///
    /// On macOS/Windows this may auto-start a stopped Podman machine.
    /// The default implementation simply delegates to `probe` + `require_ready`.
    async fn ensure_ready(&self) -> Result<(), OciProviderError> {
        self.probe().await?.require_ready().map(|_| ())
    }

    async fn resolve_image(
        &self,
        _request: &OciImageResolutionRequest,
    ) -> Result<OciResolvedImage, OciProviderError> {
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

    /// Remove an engine-managed named volume.
    ///
    /// Only meaningful for engines that manage volumes (Podman/Docker); the
    /// default is a no-op so providers that never create volumes need not
    /// implement it. Used by cleanup to delete ephemeral state volumes; see #444.
    async fn remove_volume(&self, _volume_name: &str) -> Result<(), OciProviderError> {
        Ok(())
    }
}

pub(crate) trait OciProviderSelector: Send + Sync {
    type Provider: OciProvider;

    fn select_provider(&self) -> Self::Provider;
}

/// Diagnostic report describing which OCI provider was selected and why.
///
/// Returned alongside the provider from [`select_ready_runtime_oci_provider_with_report`].
/// Useful for CLI diagnostics and runtime-setup status commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciProviderSelectionReport {
    /// The provider kind that was selected, or `None` when no provider was ready.
    pub selected: Option<OciProviderKind>,
    /// Human-readable reason for the selection outcome.
    pub reason: String,
    /// A provider that could have been used but wasn't selected (e.g. Docker when Podman wins).
    pub fallback_candidate: Option<OciProviderKind>,
    /// The readiness error from the Podman probe, when Podman was not selected.
    pub podman_error: Option<OciProviderError>,
    /// The readiness error from the Docker-compatible probe, when Docker was not selected.
    pub docker_error: Option<OciProviderError>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DefaultOciProviderSelector;

impl OciProviderSelector for DefaultOciProviderSelector {
    type Provider = PodmanProvider<SystemCommandRunner>;

    fn select_provider(&self) -> Self::Provider {
        PodmanProvider::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeOciProviderChoice {
    Podman,
    DockerCompatible,
}

fn choose_runtime_oci_provider(
    podman_ready: Result<(), OciProviderError>,
    docker_ready: Result<(), OciProviderError>,
) -> Result<RuntimeOciProviderChoice, OciProviderError> {
    match podman_ready {
        Ok(()) => Ok(RuntimeOciProviderChoice::Podman),
        Err(podman_error) => match docker_ready {
            Ok(()) => Ok(RuntimeOciProviderChoice::DockerCompatible),
            Err(_) => Err(podman_error),
        },
    }
}

#[derive(Clone)]
pub(crate) enum RuntimeOciProvider {
    Podman(PodmanProvider<SystemCommandRunner>),
    DockerCompatible(DockerCompatibleOciProvider<BollardOciRuntimeClient>),
}

/// Returns `false` only when Podman has been explicitly disabled via
/// `ATO_PODMAN_ENABLED=0` (the desktop sets this from the onboarding opt-out
/// toggle). Unset or any other value leaves Podman enabled — the opt-out
/// default. This is the interim Desktop → CLI carrier; a structured launch
/// profile should replace it later.
pub(crate) fn podman_enabled() -> bool {
    !matches!(
        std::env::var("ATO_PODMAN_ENABLED").ok().as_deref(),
        Some("0")
    )
}

pub(crate) async fn select_ready_runtime_oci_provider()
-> Result<RuntimeOciProvider, OciProviderError> {
    let (result, _report) = select_ready_runtime_oci_provider_with_report().await;
    result
}

/// Select a ready OCI provider, returning both the provider result and a
/// diagnostic report describing what was tried and why the selection was made.
///
/// This is the primary selection entry point; [`select_ready_runtime_oci_provider`]
/// is a thin wrapper that discards the report.
pub(crate) async fn select_ready_runtime_oci_provider_with_report() -> (
    Result<RuntimeOciProvider, OciProviderError>,
    OciProviderSelectionReport,
) {
    if !podman_enabled() {
        tracing::info!("Podman disabled via ATO_PODMAN_ENABLED=0; skipping Podman provider");
        return match connect_ready_docker_compatible_provider().await {
            Ok(docker) => {
                tracing::info!(
                    chosen_runtime = "docker-compatible",
                    "selected Docker-compatible OCI runtime provider (Podman disabled)"
                );
                let report = OciProviderSelectionReport {
                    selected: Some(OciProviderKind::DockerCompatible),
                    reason: "Podman disabled via ATO_PODMAN_ENABLED=0; Docker-compatible is ready"
                        .to_string(),
                    fallback_candidate: None,
                    podman_error: Some(OciProviderError::PodmanDisabled),
                    docker_error: None,
                };
                (Ok(RuntimeOciProvider::DockerCompatible(docker)), report)
            }
            Err(docker_error) => {
                tracing::warn!(
                    docker_error = %docker_error,
                    "Podman disabled and no ready Docker-compatible provider; surfacing PodmanDisabled"
                );
                let report = OciProviderSelectionReport {
                    selected: None,
                    reason: "Podman disabled and Docker-compatible is not ready".to_string(),
                    fallback_candidate: None,
                    podman_error: Some(OciProviderError::PodmanDisabled),
                    docker_error: Some(docker_error),
                };
                (Err(OciProviderError::PodmanDisabled), report)
            }
        };
    }

    let podman = PodmanProvider::new();
    let podman_ready = podman.ensure_ready().await;
    if podman_ready.is_ok() {
        tracing::debug!(
            chosen_runtime = "podman",
            "selected ready OCI runtime provider"
        );
        let report = OciProviderSelectionReport {
            selected: Some(OciProviderKind::Podman),
            reason: "Podman is installed and ready".to_string(),
            fallback_candidate: None,
            podman_error: None,
            docker_error: None,
        };
        return (Ok(RuntimeOciProvider::Podman(podman)), report);
    }
    let podman_error = podman_ready.expect_err("checked podman readiness failure");

    let docker_ready = connect_ready_docker_compatible_provider().await;
    match choose_runtime_oci_provider(
        Err(podman_error.clone()),
        docker_ready.as_ref().map(|_| ()).map_err(Clone::clone),
    ) {
        Ok(RuntimeOciProviderChoice::DockerCompatible) => {
            let docker = docker_ready.expect("checked ready Docker-compatible provider");
            tracing::info!(
                chosen_runtime = "docker-compatible",
                podman_error = %podman_error,
                "selected Docker-compatible OCI runtime provider after Podman was not ready"
            );
            let reason = format!(
                "Podman not ready ({}); Docker-compatible is ready",
                podman_error.code()
            );
            let report = OciProviderSelectionReport {
                selected: Some(OciProviderKind::DockerCompatible),
                reason,
                fallback_candidate: None,
                podman_error: Some(podman_error),
                docker_error: None,
            };
            (Ok(RuntimeOciProvider::DockerCompatible(docker)), report)
        }
        Ok(RuntimeOciProviderChoice::Podman) => unreachable!("podman readiness already failed"),
        Err(err) => {
            let docker_error = match docker_ready {
                Ok(_) => unreachable!("checked Docker-compatible readiness failure"),
                Err(docker_error) => docker_error,
            };
            let reason = format!(
                "no provider ready: Podman {} ({}), Docker {} ({})",
                podman_error.code(),
                podman_error,
                docker_error.code(),
                docker_error
            );
            tracing::warn!(
                podman_error = %podman_error,
                docker_error = %docker_error,
                reason = %reason,
                "no ready OCI provider found"
            );
            let report = OciProviderSelectionReport {
                selected: None,
                reason,
                fallback_candidate: None,
                podman_error: Some(err.clone()),
                docker_error: Some(docker_error),
            };
            (Err(err), report)
        }
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
        // Resolve the logical "podman" program to an absolute binary (+ PATH
        // override) so GUI-launched processes with a minimal PATH still find
        // Homebrew/known-location Podman. Other programs run unchanged.
        let mut command = if program == "podman" {
            let invocation = capsule_core::podman::podman_invocation();
            let mut command = Command::new(&invocation.program);
            if let Some(path_env) = &invocation.path_env {
                command.env("PATH", path_env);
            }
            // Direct an Ato-managed Podman at its bundled machine helpers via the
            // containers.conf the installer wrote next to it.
            if let Some(containers_conf) = &invocation.containers_conf {
                command.env("CONTAINERS_CONF", containers_conf);
            }
            command
        } else {
            Command::new(program)
        };
        let output = command.args(args).output()?;
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

/// Returns the option suffix for a `-v` mount argument (`""`, `":ro"`, or
/// `":U"`). Thin adapter over the projection renderer
/// `provider_projection::oci::podman_mount_opts`; retained for its unit tests.
fn podman_mount_opts(mount: &capsule_core::runtime::oci::OciMountSpec) -> &'static str {
    crate::application::provider_projection::oci::podman_mount_opts(
        mount.readonly,
        mount.ownership.is_some(),
    )
}

/// Render an `OciMountSpec` into the value passed after `-v` to `podman create`.
/// Thin adapter over the projection renderer
/// `provider_projection::oci::render_podman_mount_value` (#444 bind-path vs
/// engine-volume logic lives there); retained for its unit tests.
fn podman_mount_arg(mount: &capsule_core::runtime::oci::OciMountSpec) -> String {
    render_podman_mount_value(&OciMountProjection::from_spec(mount))
}

/// Render an `OciPortSpec` into the value passed after `-p` to `podman create`.
/// Thin adapter over the projection renderer
/// `provider_projection::oci::render_podman_port_value`; retained for its
/// unit tests.
fn podman_publish_port_arg(port: &capsule_core::runtime::oci::OciPortSpec) -> String {
    render_podman_port_value(&OciPortProjection::from_spec(port))
}

#[derive(Clone)]
pub(crate) struct PodmanProvider<R = SystemCommandRunner> {
    runner: R,
    platform: PodmanProbePlatform,
    semantics: OciProviderSemantics,
    /// Podman connection selected during readiness (e.g. `ato-podman`). Shared
    /// across clones and set once, so every later container/image/network op
    /// targets the *same* machine that readiness verified — never whatever the
    /// host's global default connection happens to point at. `None` (unset, or
    /// set to `None`) preserves the prior default-connection behavior.
    connection: Arc<OnceLock<Option<String>>>,
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
            connection: Arc::new(OnceLock::new()),
        }
    }

    /// The connection selected so far (cached), if any. `None` if unset or
    /// explicitly set to no-connection.
    fn cached_connection(&self) -> Option<String> {
        self.connection.get().cloned().flatten()
    }

    /// Record the selected connection (idempotent; first write wins).
    fn set_connection(&self, connection: Option<String>) {
        let _ = self.connection.set(connection);
    }
}

impl<R: OciCommandRunner + Send + Sync> PodmanProvider<R> {
    /// This provider's Podman connection, computed once and cached.
    ///
    /// Each provider instance resolves its own connection: the OCI executors
    /// construct fresh `PodmanProvider`s (via `DefaultOciProviderSelector`) that
    /// never saw `ensure_ready`, so the connection cannot live only on the
    /// session-start instance — it must be (re)derivable on demand. On
    /// macOS/Windows it is `ato-podman` whenever that machine is running, else
    /// `None` (default connection). Linux is always `None`. Reads `podman
    /// machine list` at most once per instance.
    fn resolved_connection(&self) -> Option<String> {
        if let Some(cached) = self.connection.get() {
            return cached.clone();
        }
        let selected = self.compute_connection();
        let _ = self.connection.set(selected.clone());
        selected
    }

    fn compute_connection(&self) -> Option<String> {
        match self.platform {
            PodmanProbePlatform::Macos | PodmanProbePlatform::Windows => {
                let list = run_provider_command(
                    &self.runner,
                    "podman",
                    &["machine", "list", "--format", "json"],
                )
                .ok()?;
                if !list.success() {
                    return None;
                }
                match parse_machine_entries(&list.stdout) {
                    Ok(entries) if ato_machine_running(&entries) => {
                        Some(ATO_PODMAN_MACHINE_NAME.to_string())
                    }
                    _ => None,
                }
            }
            // Native Linux / unsupported: no machine connection.
            _ => None,
        }
    }

    /// Run a podman subcommand pinned to this provider's selected connection
    /// (when any). Use for daemon operations (info, manifest inspect, pull,
    /// image inspect) — NOT for `podman machine …`/`--version`, which are
    /// connection-independent.
    fn run_podman(&self, args: &[&str]) -> Result<CommandOutput, OciProviderError> {
        let connection = self.resolved_connection();
        let full = prepend_connection(connection.as_deref(), args);
        run_provider_command(&self.runner, "podman", &full)
    }

    /// Build a tokio `Command` for spawning podman, resolved to an absolute
    /// binary with a `PATH` override so GUI-launched (minimal-PATH) processes
    /// find Homebrew/known-location Podman. Falls back to the bare `"podman"`
    /// name when resolution fails.
    ///
    /// `--connection <name>` is injected as a global flag (before any subcommand
    /// the caller appends) whenever a connection was selected, so every
    /// container/image/network op targets the same machine readiness verified.
    fn podman_command(&self) -> tokio::process::Command {
        let invocation = capsule_core::podman::podman_invocation();
        let mut command = tokio::process::Command::new(&invocation.program);
        if let Some(path_env) = &invocation.path_env {
            command.env("PATH", path_env);
        }
        if let Some(connection) = self.resolved_connection() {
            command.arg("--connection").arg(connection);
        }
        command
    }

    /// Run `podman info` (optionally pinned to `connection`) and classify its
    /// failure mode; called after confirming the machine is Running to verify
    /// the daemon is actually healthy.
    fn check_podman_info_health(&self, connection: Option<&str>) -> Result<(), OciProviderError> {
        let args = prepend_connection(connection, &["info"]);
        let info_out = run_provider_command(&self.runner, "podman", &args)?;
        if info_out.success() {
            return Ok(());
        }
        let combined = format!("{} {}", info_out.stdout, info_out.stderr);
        if is_storage_corrupted(&combined) {
            Err(OciProviderError::StorageCorrupted {
                reason: combined.trim().to_string(),
            })
        } else {
            Err(OciProviderError::ProbeFailed {
                provider: "podman",
                message: format!("podman info failed: {}", combined.trim()),
            })
        }
    }

    /// macOS/Windows: inspect the machine list, select the Podman connection, and
    /// start the machine if exactly one is stopped.
    ///
    /// The Ato-managed machine (`ato-podman`, created by `runtime prepare`) is
    /// preferred whenever it is running: the connection is pinned to it and it is
    /// verified explicitly, even if other machines coexist — so a different
    /// global default cannot mask its readiness, and a running `ato-podman`
    /// alongside another machine is no longer reported as ambiguous. When
    /// `ato-podman` is not running, behavior is unchanged and no connection is
    /// pinned (the default connection is used, as before).
    async fn ensure_machine_ready(&self) -> Result<(), OciProviderError> {
        let ver_out = run_provider_command(&self.runner, "podman", &["--version"])?;
        if !ver_out.success() {
            return Err(command_failed("podman --version", ver_out));
        }
        let list_out = run_provider_command(
            &self.runner,
            "podman",
            &["machine", "list", "--format", "json"],
        )?;
        if !list_out.success() {
            return Err(command_failed(
                "podman machine list --format json",
                list_out,
            ));
        }

        // Prefer the Ato machine when it is running: pin the connection to it and
        // verify it explicitly, regardless of other machines or the host default.
        if let Ok(entries) = parse_machine_entries(&list_out.stdout)
            && ato_machine_running(&entries)
        {
            self.set_connection(Some(ATO_PODMAN_MACHINE_NAME.to_string()));
            return self.check_podman_info_health(Some(ATO_PODMAN_MACHINE_NAME));
        }

        // No running Ato machine: preserve prior behavior, default connection.
        match parse_podman_machine_list(&list_out.stdout) {
            PodmanMachineStatus::Running { all_names, .. } if all_names.len() > 1 => {
                Err(OciProviderError::MachineAmbiguous {
                    names: all_names.join(", "),
                })
            }
            PodmanMachineStatus::Running { .. } => self.check_podman_info_health(None),
            PodmanMachineStatus::NotConfigured => Err(OciProviderError::MachineNotConfigured),
            PodmanMachineStatus::Stopped { names } if names.len() > 1 => {
                Err(OciProviderError::MachineAmbiguous {
                    names: names.join(", "),
                })
            }
            PodmanMachineStatus::Stopped { names } => {
                let machine_name = names.into_iter().next().unwrap_or_default();
                // If the single stopped machine is the Ato machine, pin the
                // connection so the readiness poll (and later ops) target it
                // rather than the host's default connection.
                let connection = if machine_name == ATO_PODMAN_MACHINE_NAME {
                    self.set_connection(Some(ATO_PODMAN_MACHINE_NAME.to_string()));
                    Some(ATO_PODMAN_MACHINE_NAME)
                } else {
                    None
                };
                self.start_machine_and_wait(&machine_name, connection).await
            }
            PodmanMachineStatus::Unknown { reason }
            | PodmanMachineStatus::Unavailable { reason } => Err(OciProviderError::ProbeFailed {
                provider: "podman",
                message: format!("podman machine list: {reason}"),
            }),
        }
    }

    /// Start a single stopped machine and poll `podman info` until it is ready.
    /// The poll is pinned to `connection` when set (the Ato machine), so it does
    /// not check the host's default connection instead of the machine we started.
    async fn start_machine_and_wait(
        &self,
        machine_name: &str,
        connection: Option<&str>,
    ) -> Result<(), OciProviderError> {
        let start_out =
            run_provider_command(&self.runner, "podman", &["machine", "start", machine_name])?;
        if !start_out.success() {
            let reason = if start_out.stderr.trim().is_empty() {
                start_out.stdout.trim().to_string()
            } else {
                start_out.stderr.trim().to_string()
            };
            return Err(OciProviderError::MachineStartFailed {
                machine_name: machine_name.to_string(),
                reason,
            });
        }
        // Poll `podman info` (pinned to `connection` when set) until the machine
        // daemon is up.
        let info_args = prepend_connection(connection, &["info"]);
        let start = std::time::Instant::now();
        loop {
            let info_out = self.runner.run("podman", &info_args).map_err(|err| {
                OciProviderError::ProbeFailed {
                    provider: "podman",
                    message: format!("podman info poll: {err}"),
                }
            })?;
            if info_out.success() {
                return Ok(());
            }
            if start.elapsed() >= MACHINE_READY_POLL_TIMEOUT {
                return Err(OciProviderError::MachineReadyTimeout {
                    machine_name: machine_name.to_string(),
                    elapsed_secs: MACHINE_READY_POLL_TIMEOUT.as_secs(),
                });
            }
            tokio::time::sleep(MACHINE_READY_POLL_INTERVAL).await;
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
                let mode = detect_linux_podman_mode(&self.runner)?;
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

    async fn ensure_ready(&self) -> Result<(), OciProviderError> {
        match &self.platform {
            PodmanProbePlatform::Linux => {
                // Native Linux Podman never needs machine management.
                self.probe().await?.require_ready().map(|_| ())
            }
            PodmanProbePlatform::Macos | PodmanProbePlatform::Windows => {
                self.ensure_machine_ready().await
            }
            PodmanProbePlatform::Unsupported(platform) => {
                Err(OciProviderError::UnsupportedPlatform {
                    provider: "podman",
                    platform: platform.clone(),
                })
            }
        }
    }

    async fn resolve_image(
        &self,
        request: &OciImageResolutionRequest,
    ) -> Result<OciResolvedImage, OciProviderError> {
        let declared_ref = &request.declared_ref;

        // Validate basic format.
        if declared_ref.trim().is_empty()
            || declared_ref
                .bytes()
                .any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return Err(OciProviderError::ImageRefMalformed {
                declared_ref: declared_ref.clone(),
                reason: "image reference must be non-empty and must not contain whitespace"
                    .to_string(),
            });
        }

        // Extract digest if already embedded in the ref (e.g. "image@sha256:...").
        let ref_digest = extract_digest_from_ref(declared_ref);
        if let Some(digest) = &ref_digest
            && let Err(reason) = validate_oci_digest_format(digest)
        {
            return Err(OciProviderError::ImageRefMalformed {
                declared_ref: declared_ref.clone(),
                reason,
            });
        }

        // Always call `podman manifest inspect` to get manifest information.
        // This is required for platform discovery in all cases.
        let output = self.run_podman(&["manifest", "inspect", declared_ref])?;

        if !output.success() {
            let combined = format!("{} {}", output.stdout, output.stderr);
            if is_registry_auth_error(&combined) {
                return Err(OciProviderError::RegistryAuthRequired {
                    declared_ref: declared_ref.clone(),
                });
            }
            let reason = if output.stderr.trim().is_empty() {
                output.stdout.trim().to_string()
            } else {
                output.stderr.trim().to_string()
            };
            return Err(OciProviderError::ImageResolveFailed {
                declared_ref: declared_ref.clone(),
                reason,
            });
        }

        let parsed = parse_manifest_inspect(&output.stdout).map_err(|reason| {
            OciProviderError::ImageResolveFailed {
                declared_ref: declared_ref.clone(),
                reason: format!("could not parse manifest inspect output: {reason}"),
            }
        })?;

        if parsed.is_list {
            // Manifest list: select the platform entry and use its child digest.
            // When no platform is requested, auto-select based on the host
            // architecture so that `postgres:14` (16 platforms) just works
            // without requiring an explicit `requested_platform`.
            let auto_platform;
            let (effective_requested, is_auto) = if request.requested_platform.is_some() {
                (request.requested_platform.as_ref(), false)
            } else {
                auto_platform = auto_select_platform();
                (Some(&auto_platform), true)
            };
            // Fallback to linux/amd64 only when the platform was auto-selected (not when
            // the caller explicitly requested a specific platform that is not available).
            let entry = if is_auto {
                select_platform_entry(&parsed.entries, effective_requested, declared_ref).or_else(
                    |_| {
                        let fallback = OciPlatform {
                            os: "linux".to_string(),
                            architecture: "amd64".to_string(),
                            variant: None,
                        };
                        select_platform_entry(&parsed.entries, Some(&fallback), declared_ref)
                    },
                )?
            } else {
                select_platform_entry(&parsed.entries, effective_requested, declared_ref)?
            };
            Ok(OciResolvedImage {
                declared_ref: declared_ref.clone(),
                resolved_digest: entry.digest.clone(),
                platform: entry.platform.clone(),
                media_type: entry.media_type.clone(),
                provider_semantics: self.semantics.clone(),
            })
        } else {
            // Single-arch manifest: only usable when we already have the digest from the ref.
            if let Some(digest) = &ref_digest {
                let platform = request.requested_platform.clone().ok_or_else(|| {
                    OciProviderError::ImagePlatformUnsupported {
                        declared_ref: declared_ref.clone(),
                        platform: "single-platform manifest: specify requested_platform to resolve"
                            .to_string(),
                    }
                })?;
                Ok(OciResolvedImage {
                    declared_ref: declared_ref.clone(),
                    resolved_digest: digest.clone(),
                    platform,
                    media_type: parsed.media_type,
                    provider_semantics: self.semantics.clone(),
                })
            } else {
                // Mutable tag + single-arch: we must pull to resolve the digest.
                // Under NativeOnly policy this fails with an actionable diagnostic.
                // Under AllowEmulation we pull with --platform linux/amd64 and inspect
                // for the digest so the lock entry is stable.
                if request.platform_policy == OciPlatformPolicy::NativeOnly {
                    return Err(OciProviderError::ImagePlatformUnsupported {
                        declared_ref: declared_ref.clone(),
                        platform: format!(
                            "single-platform image '{declared_ref}' has a mutable tag and no \
                             multi-arch manifest list; cannot determine image platform without \
                             pulling. If this image is linux/amd64-only on an arm64 host, add \
                             `allow_emulation = true` to the recipe target in capsule.toml."
                        ),
                    });
                }
                // AllowEmulation: pull with --platform linux/amd64 then inspect for digest.
                let pull_output =
                    self.run_podman(&["pull", "--platform", "linux/amd64", declared_ref])?;
                if !pull_output.success() {
                    let combined = format!("{} {}", pull_output.stdout, pull_output.stderr);
                    if is_registry_auth_error(&combined) {
                        return Err(OciProviderError::RegistryAuthRequired {
                            declared_ref: declared_ref.clone(),
                        });
                    }
                    let reason = if pull_output.stderr.trim().is_empty() {
                        pull_output.stdout.trim().to_string()
                    } else {
                        pull_output.stderr.trim().to_string()
                    };
                    return Err(OciProviderError::ImageResolveFailed {
                        declared_ref: declared_ref.clone(),
                        reason,
                    });
                }
                // Get the digest of the pulled image.
                let inspect_output = self.run_podman(&[
                    "image",
                    "inspect",
                    "--format",
                    "{{index .RepoDigests 0}}",
                    declared_ref,
                ])?;
                let raw_digest = inspect_output.stdout.trim().to_string();
                let digest =
                    extract_digest_from_ref(&raw_digest).unwrap_or_else(|| raw_digest.clone());
                if digest.is_empty() || !digest.starts_with("sha256:") {
                    return Err(OciProviderError::ImageResolveFailed {
                        declared_ref: declared_ref.clone(),
                        reason: format!(
                            "could not resolve digest for '{declared_ref}' after pull; \
                             inspect returned: {raw_digest}"
                        ),
                    });
                }
                let emulated_platform = OciPlatform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                    variant: None,
                };
                tracing::warn!(
                    target: "oci_provider",
                    "Emulating linux/amd64 image '{}' on non-native host (allow_emulation=true). \
                     Performance may be reduced.",
                    declared_ref
                );
                Ok(OciResolvedImage {
                    declared_ref: declared_ref.clone(),
                    resolved_digest: digest,
                    platform: emulated_platform,
                    media_type: parsed.media_type,
                    provider_semantics: self.semantics.clone(),
                })
            }
        }
    }

    async fn pull_image(&self, image: &OciImageResolution) -> Result<(), OciProviderError> {
        let pull_ref = build_digest_pull_ref(image);
        let host = auto_select_platform();
        let mut args = vec!["pull".to_string()];
        if image.platform.architecture != host.architecture {
            // Emulated platform: pass --platform so Podman fetches the right arch.
            args.push("--platform".to_string());
            args.push(format!("linux/{}", image.platform.architecture));
        }
        args.push(pull_ref.clone());
        let output = self
            .podman_command()
            .args(&args)
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman pull", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if is_registry_auth_error(&stderr) {
                return Err(OciProviderError::RegistryAuthRequired {
                    declared_ref: image.declared_ref.clone(),
                });
            }
            return Err(OciProviderError::CommandFailed {
                provider: "podman",
                command: format!("podman pull {pull_ref}"),
                status: output.status.code(),
                message: stderr,
            });
        }
        Ok(())
    }

    async fn create_network(
        &self,
        request: &OciNetworkRequest,
    ) -> Result<String, OciProviderError> {
        let mut args: Vec<String> = vec!["network".into(), "create".into()];
        for (k, v) in &request.labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        args.push(request.name.clone());
        let output = self
            .podman_command()
            .args(&args)
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman network create", e))?;
        if !output.status.success() {
            return Err(OciProviderError::CommandFailed {
                provider: "podman",
                command: "podman network create".into(),
                status: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn remove_network(&self, network_name: &str) -> Result<(), OciProviderError> {
        // `--force` disconnects any lingering endpoint before removing the
        // network so cleanup does not leave an orphaned `ato-*` network when a
        // container is still attached at removal time (#450).
        let output = self
            .podman_command()
            .args(["network", "rm", "--force", network_name])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman network rm", e))?;
        if !output.status.success() {
            return Err(OciProviderError::OciCleanupFailed {
                operation: "remove_network".into(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(())
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> Result<String, OciProviderError> {
        // Provider projection boundary (#501): the resolved launch conditions
        // become an `OciProjectionPlan` (the source of truth), and the
        // `podman create` argv is *derived* from that plan rather than built ad
        // hoc here. The plan carries no runtime evidence (container id, pid, log
        // path); those are produced below and are not part of plan identity.
        let plan = OciProjectionPlan::from_container_request(request);
        let args = plan
            .render_podman_create_argv(&auto_select_platform())
            .map_err(|e| match e {
                OciRenderError::ReadOnlyOwnershipConflict { target } => {
                    OciProviderError::ReadOnlyOwnershipConflict { target }
                }
            })?;
        let output = self
            .podman_command()
            .args(&args)
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman create", e))?;
        if !output.status.success() {
            return Err(OciProviderError::CommandFailed {
                provider: "podman",
                command: "podman create".into(),
                status: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn start_container(&self, container_id: &str) -> Result<(), OciProviderError> {
        let output = self
            .podman_command()
            .args(["start", container_id])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman start", e))?;
        if !output.status.success() {
            return Err(OciProviderError::OciContainerStartFailed {
                container_name: container_id.to_string(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(())
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError> {
        let output = self
            .podman_command()
            .args(["inspect", "--format", "json", container_id])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman inspect", e))?;
        if !output.status.success() {
            return Err(OciProviderError::CommandFailed {
                provider: "podman",
                command: "podman inspect".into(),
                status: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        parse_podman_inspect(&String::from_utf8_lossy(&output.stdout))
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError> {
        use tokio::io::AsyncBufReadExt;
        let mut args = vec!["logs".to_string()];
        if follow {
            args.push("--follow".into());
        }
        args.push(container_id.to_string());
        let mut child = self
            .podman_command()
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| podman_async_io_error("podman logs", e))?;
        let (tx, rx) = mpsc::channel(64);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let tx_err = tx.clone();
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let chunk = OciLogChunk {
                    stderr: false,
                    message: (line + "\n").into_bytes(),
                };
                if tx.send(Ok(chunk)).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let chunk = OciLogChunk {
                    stderr: true,
                    message: (line + "\n").into_bytes(),
                };
                if tx_err.send(Ok(chunk)).await.is_err() {
                    break;
                }
            }
        });
        // Drive child to completion so it doesn't become a zombie.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(rx)
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64, OciProviderError> {
        let output = self
            .podman_command()
            .args(["wait", container_id])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman wait", e))?;
        if !output.status.success() {
            return Err(OciProviderError::CommandFailed {
                provider: "podman",
                command: "podman wait".into(),
                status: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        let code = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        Ok(code)
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> Result<(), OciProviderError> {
        let timeout = timeout_secs.to_string();
        let output = self
            .podman_command()
            .args(["stop", "--time", &timeout, container_id])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman stop", e))?;
        if !output.status.success() {
            return Err(OciProviderError::OciCleanupFailed {
                operation: "stop_container".into(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(())
    }

    async fn remove_container(
        &self,
        container_id: &str,
        force: bool,
    ) -> Result<(), OciProviderError> {
        let mut args = vec!["rm".to_string()];
        if force {
            args.push("--force".into());
        }
        args.push(container_id.to_string());
        let output = self
            .podman_command()
            .args(&args)
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman rm", e))?;
        if !output.status.success() {
            return Err(OciProviderError::OciCleanupFailed {
                operation: "remove_container".into(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(())
    }

    async fn remove_volume(&self, volume_name: &str) -> Result<(), OciProviderError> {
        // `--force` so removing a not-yet-created volume (e.g. a session that
        // failed before the container started) is a no-op rather than an error.
        let output = self
            .podman_command()
            .args(["volume", "rm", "--force", volume_name])
            .output()
            .await
            .map_err(|e| podman_async_io_error("podman volume rm", e))?;
        if !output.status.success() {
            return Err(OciProviderError::OciCleanupFailed {
                operation: "remove_volume".into(),
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl OciRuntimeClient for PodmanProvider<SystemCommandRunner> {
    async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
        // For a digest-pinned reference (`...@sha256:...`) the image is
        // immutable — if it's already in the local store, the registry round
        // trip can never produce a different result. Skip the pull in that
        // case so sessions still start when the registry is unreachable
        // (offline laptop, DNS hiccup, etc.). This restores the offline
        // resilience that PR #289's broader "never skip" rule accidentally
        // removed for digest-pinned refs.
        if image.contains("@sha256:") {
            let exists = self
                .podman_command()
                .args(["image", "exists", image])
                .status()
                .await
                .map_err(|err| {
                    CapsuleError::ContainerEngine(format!(
                        "failed to run podman image exists: {err}"
                    ))
                })?;
            if exists.success() {
                return Ok(());
            }
        }

        // For mutable tags (`:latest`, `:main`, etc.) always attempt the pull
        // so the cached image refreshes against the registry, matching the
        // prior bollard semantics. The resolved-digest path lives on
        // `OciProvider::pull_image(&OciImageResolution)`.
        let output = self
            .podman_command()
            .args(["pull", image])
            .output()
            .await
            .map_err(|err| {
                CapsuleError::ContainerEngine(format!("failed to run podman pull: {err}"))
            })?;
        if output.status.success() {
            return Ok(());
        }

        // If the pull failed but a local copy exists, fall back to it with a
        // structured warning rather than failing the session. Common cause:
        // transient registry/DNS outage. For mutable tags this means the
        // session runs against possibly-stale content, which is preferable to
        // refusing to start at all.
        let cached = self
            .podman_command()
            .args(["image", "exists", image])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if cached {
            tracing::warn!(
                image = %image,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "podman pull failed; falling back to locally cached image"
            );
            return Ok(());
        }

        Err(CapsuleError::Runtime(format!(
            "podman pull failed for '{}': {}",
            image,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    async fn create_network(&self, request: &OciNetworkRequest) -> capsule_core::Result<String> {
        <Self as OciProvider>::create_network(self, request)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn remove_network(&self, network_name: &str) -> capsule_core::Result<()> {
        <Self as OciProvider>::remove_network(self, network_name)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> capsule_core::Result<String> {
        <Self as OciProvider>::create_container(self, request)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
        <Self as OciProvider>::start_container(self, container_id)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> capsule_core::Result<OciContainerInspect> {
        <Self as OciProvider>::inspect_container(self, container_id)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> capsule_core::Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>> {
        <Self as OciProvider>::logs(self, container_id, follow)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn exec_container(
        &self,
        container_id: &str,
        cmd: &[String],
    ) -> capsule_core::Result<i64> {
        let output = self
            .podman_command()
            .arg("exec")
            .arg(container_id)
            .args(cmd)
            .output()
            .await
            .map_err(|err| {
                CapsuleError::ContainerEngine(format!("failed to run podman exec: {err}"))
            })?;
        Ok(output.status.code().unwrap_or(1) as i64)
    }

    async fn wait_container(&self, container_id: &str) -> capsule_core::Result<i64> {
        <Self as OciProvider>::wait_container(self, container_id)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> capsule_core::Result<()> {
        <Self as OciProvider>::stop_container(self, container_id, timeout_secs)
            .await
            .map_err(provider_error_to_capsule_error)
    }

    async fn remove_container(&self, container_id: &str, force: bool) -> capsule_core::Result<()> {
        <Self as OciProvider>::remove_container(self, container_id, force)
            .await
            .map_err(provider_error_to_capsule_error)
    }
}

fn provider_error_to_capsule_error(error: OciProviderError) -> CapsuleError {
    match error {
        OciProviderError::Missing { .. } | OciProviderError::NotReady { .. } => {
            CapsuleError::ContainerEngine(error.to_string())
        }
        _ => CapsuleError::Runtime(error.to_string()),
    }
}

/// Prepend `--connection <name>` as a Podman global flag (before the subcommand)
/// when a connection is selected. Podman requires `--connection` to precede the
/// subcommand: `podman --connection ato-podman info`, never `podman info
/// --connection ato-podman`.
fn prepend_connection<'a>(connection: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    match connection {
        Some(name) => {
            let mut out = Vec::with_capacity(args.len() + 2);
            out.push("--connection");
            out.push(name);
            out.extend_from_slice(args);
            out
        }
        None => args.to_vec(),
    }
}

/// Whether the Ato-managed machine (`ato-podman`) is present and running.
fn ato_machine_running(entries: &[PodmanMachine]) -> bool {
    entries
        .iter()
        .any(|m| m.name == ATO_PODMAN_MACHINE_NAME && m.running)
}

fn run_provider_command<R: OciCommandRunner>(
    runner: &R,
    program: &'static str,
    args: &[&str],
) -> Result<CommandOutput, OciProviderError> {
    runner.run(program, args).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound && program == "podman" {
            // Distinguish an invalid ATO_PODMAN_BIN override from a genuinely
            // absent binary so the user gets an actionable message.
            if let Err(capsule_core::podman::PodmanResolveError::InvalidEnvOverride { path }) =
                capsule_core::podman::resolve_podman()
            {
                return OciProviderError::InvalidBinaryOverride {
                    path: path.display().to_string(),
                };
            }
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

/// Detect whether a `podman info` failure is caused by a storage or graph
/// driver error rather than a transient daemon state.
fn is_storage_corrupted(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("storage")
        && (lower.contains("graph") || lower.contains("driver") || lower.contains("corrupt"))
        || lower.contains("graphdriver")
        || lower.contains("overlay") && (lower.contains("corrupt") || lower.contains("invalid"))
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

fn detect_linux_podman_mode<R: OciCommandRunner>(
    runner: &R,
) -> Result<OciProviderMode, OciProviderError> {
    let output = runner
        .run(
            "podman",
            &["info", "--format", "{{.Host.Security.Rootless}}"],
        )
        .map_err(|err| OciProviderError::ProbeFailed {
            provider: "podman",
            message: format!("podman info: {err}"),
        })?;
    if !output.success() {
        let combined = format!("{} {}", output.stdout, output.stderr);
        if is_storage_corrupted(&combined) {
            return Err(OciProviderError::StorageCorrupted {
                reason: combined.trim().to_string(),
            });
        }
        return Err(OciProviderError::ProbeFailed {
            provider: "podman",
            message: format!("podman info failed: {}", combined.trim()),
        });
    }
    Ok(match output.stdout.trim().to_ascii_lowercase().as_str() {
        "true" => OciProviderMode::Rootless,
        "false" => OciProviderMode::Rootful,
        _ => OciProviderMode::Unknown,
    })
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

// ── Manifest inspect helpers ─────────────────────────────────────────────────

struct ManifestEntry {
    digest: String,
    platform: OciPlatform,
    media_type: Option<String>,
}

struct ManifestInspectParsed {
    is_list: bool,
    entries: Vec<ManifestEntry>,
    media_type: Option<String>,
}

/// Parse `podman manifest inspect` JSON output.
///
/// Returns a manifest list (with per-platform entries) when the JSON contains
/// a `"manifests"` array.  Returns a single-arch sentinel when it does not.
fn parse_manifest_inspect(json: &str) -> Result<ManifestInspectParsed, String> {
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("invalid JSON: {e}"))?;

    let media_type = value
        .get("mediaType")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if let Some(manifests_arr) = value.get("manifests").and_then(|v| v.as_array()) {
        let mut entries = Vec::with_capacity(manifests_arr.len());
        for (i, m) in manifests_arr.iter().enumerate() {
            let digest = m
                .get("digest")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("manifests[{i}] missing 'digest'"))?
                .to_string();
            let platform_json = m
                .get("platform")
                .ok_or_else(|| format!("manifests[{i}] missing 'platform'"))?;
            let os = platform_json
                .get("os")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("manifests[{i}].platform missing 'os'"))?
                .to_string();
            let architecture = platform_json
                .get("architecture")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("manifests[{i}].platform missing 'architecture'"))?
                .to_string();
            let variant = platform_json
                .get("variant")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let entry_media_type = m
                .get("mediaType")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            entries.push(ManifestEntry {
                digest,
                platform: OciPlatform {
                    os,
                    architecture,
                    variant,
                },
                media_type: entry_media_type,
            });
        }
        Ok(ManifestInspectParsed {
            is_list: true,
            entries,
            media_type,
        })
    } else {
        Ok(ManifestInspectParsed {
            is_list: false,
            entries: Vec::new(),
            media_type,
        })
    }
}

/// Select the manifest entry that best matches `requested`.
///
/// - If `requested` is `Some`: find an exact `os/architecture[/variant]` match or fail.
/// - If `requested` is `None` and there is exactly one entry: return it.
/// - If `requested` is `None` and there are multiple entries: fail with an ambiguity error.
fn select_platform_entry<'a>(
    entries: &'a [ManifestEntry],
    requested: Option<&OciPlatform>,
    declared_ref: &str,
) -> Result<&'a ManifestEntry, OciProviderError> {
    if let Some(requested) = requested {
        entries
            .iter()
            .find(|e| {
                e.platform.os == requested.os
                    && e.platform.architecture == requested.architecture
                    && (requested.variant.is_none() || e.platform.variant == requested.variant)
            })
            .ok_or_else(|| {
                let requested_platform = format!(
                    "{}/{}{}",
                    requested.os,
                    requested.architecture,
                    requested
                        .variant
                        .as_deref()
                        .map(|v| format!("/{v}"))
                        .unwrap_or_default()
                );
                OciProviderError::ImagePlatformUnsupported {
                    declared_ref: declared_ref.to_string(),
                    platform: requested_platform,
                }
            })
    } else if entries.len() == 1 {
        Ok(&entries[0])
    } else {
        let platforms: Vec<_> = entries
            .iter()
            .map(|e| format!("{}/{}", e.platform.os, e.platform.architecture))
            .collect();
        Err(OciProviderError::ImagePlatformUnsupported {
            declared_ref: declared_ref.to_string(),
            platform: format!(
                "ambiguous: {} platform(s) available ({}); specify requested_platform",
                entries.len(),
                platforms.join(", ")
            ),
        })
    }
}

/// Extract the `sha256:...` digest embedded in `image@sha256:...` style refs.
fn extract_digest_from_ref(declared_ref: &str) -> Option<String> {
    declared_ref
        .find("@sha256:")
        .map(|pos| declared_ref[pos + 1..].to_string())
}

/// Auto-select a platform for the current host when no `requested_platform`
/// was specified.  Podman containers always run Linux, so OS is always
/// `linux`; architecture is mapped from the Rust `ARCH` constant.
fn auto_select_platform() -> OciPlatform {
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        "arm" => "arm",
        other => other,
    };
    OciPlatform {
        os: "linux".to_string(),
        architecture: architecture.to_string(),
        variant: None,
    }
}

/// Require `sha256:` prefix followed by exactly 64 lowercase hex characters.
fn validate_oci_digest_format(digest: &str) -> Result<(), String> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("digest must start with 'sha256:', got '{digest}'"))?;
    if hex.len() != 64 {
        return Err(format!(
            "sha256 digest must be 64 hex characters, got {}",
            hex.len()
        ));
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("sha256 digest must contain only hexadecimal characters".to_string());
    }
    Ok(())
}

fn is_registry_auth_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("authentication required")
        || lower.contains("auth required")
}

/// Build a canonical pull reference that pins to the resolved digest.
///
/// `postgres:14` + `sha256:abc...` → `postgres@sha256:abc...`
/// `myimage@sha256:abc...` → returned as-is (already a digest ref)
pub(crate) fn build_digest_pull_ref(image: &OciImageResolution) -> String {
    if image.declared_ref.contains('@') {
        return image.declared_ref.clone();
    }
    let base = image
        .declared_ref
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .map(|(base, _)| base)
        .unwrap_or(&image.declared_ref);
    format!("{}@{}", base, image.resolved_digest)
}

fn podman_async_io_error(command: &'static str, err: std::io::Error) -> OciProviderError {
    if err.kind() == std::io::ErrorKind::NotFound {
        if let Err(capsule_core::podman::PodmanResolveError::InvalidEnvOverride { path }) =
            capsule_core::podman::resolve_podman()
        {
            return OciProviderError::InvalidBinaryOverride {
                path: path.display().to_string(),
            };
        }
        OciProviderError::Missing {
            provider: "podman",
            binary: "podman",
        }
    } else {
        OciProviderError::CommandFailed {
            provider: "podman",
            command: command.to_string(),
            status: None,
            message: err.to_string(),
        }
    }
}

/// Parse `podman inspect --format json <id>` output into [`OciContainerInspect`].
fn parse_podman_inspect(json: &str) -> Result<OciContainerInspect, OciProviderError> {
    use std::collections::HashMap;
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| OciProviderError::ProbeFailed {
            provider: "podman",
            message: format!("failed to parse inspect output: {e}"),
        })?;
    let item = value
        .as_array()
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let running = item["State"]["Status"]
        .as_str()
        .map(|s| s == "running")
        .unwrap_or(false);
    let exit_code = item["State"]["ExitCode"].as_i64();
    let mut host_ports: HashMap<u16, u16> = HashMap::new();
    if let Some(ports) = item["NetworkSettings"]["Ports"].as_object() {
        for (key, bindings) in ports {
            let Some((port_raw, _)) = key.split_once('/') else {
                continue;
            };
            let Ok(container_port) = port_raw.parse::<u16>() else {
                continue;
            };
            if let Some(binding) = bindings.as_array().and_then(|arr| arr.first())
                && let Some(hp) = binding["HostPort"]
                    .as_str()
                    .and_then(|s| s.parse::<u16>().ok())
            {
                host_ports.insert(container_port, hp);
            }
        }
    }
    Ok(OciContainerInspect {
        running,
        exit_code,
        host_ports,
    })
}

#[derive(Clone)]
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

async fn connect_ready_docker_compatible_provider()
-> Result<DockerCompatibleOciProvider<BollardOciRuntimeClient>, OciProviderError> {
    let provider = DockerCompatibleOciProvider::connect_default(docker_compatible_semantics())
        .map_err(classify_docker_connect_error)?;
    let version = provider
        .client
        .docker()
        .version()
        .await
        .map_err(|err| classify_docker_error_message(&err.to_string()))?;

    let platform_name = version
        .platform
        .as_ref()
        .map(|platform| platform.name.as_str())
        .unwrap_or_default();
    let engine_version = version.version.as_deref().unwrap_or_default();
    let engine_marker = format!("{platform_name} {engine_version}").to_ascii_lowercase();
    if engine_marker.contains("podman") {
        let semantics = docker_compatible_semantics();
        return Err(OciProviderError::NotReady {
            provider: "docker-compatible",
            reason:
                "Docker-compatible endpoint resolves to Podman; use the Podman provider setup path"
                    .to_string(),
            inventory: Some(OciProviderInventory {
                kind: OciProviderKind::DockerCompatible,
                binary: OciProviderBinaryStatus::Found,
                version: version.version,
                mode: semantics.mode,
                machine: OciProviderMachineStatus::MachineUnknown,
                semantics,
            }),
        });
    }

    Ok(provider)
}

fn docker_compatible_semantics() -> OciProviderSemantics {
    OciProviderSemantics {
        kind: OciProviderKind::DockerCompatible,
        mode: OciProviderMode::Unknown,
        substrate: OciProviderSubstrate::Unknown,
        policy_profile: "oci-docker-compatible-v1".to_string(),
    }
}

fn classify_docker_connect_error(err: OciProviderError) -> OciProviderError {
    if let OciProviderError::ProbeFailed { message, .. } = &err {
        return classify_docker_error_message(message);
    }
    err
}

fn classify_docker_error_message(message: &str) -> OciProviderError {
    if is_permission_denied(message) {
        return OciProviderError::DockerPermissionDenied;
    }
    if is_daemon_unavailable(message) {
        return OciProviderError::DockerDaemonUnavailable {
            reason: message.to_string(),
        };
    }
    OciProviderError::ProbeFailed {
        provider: "docker-compatible",
        message: format!("docker-compatible engine version probe failed: {message}"),
    }
}

fn is_permission_denied(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("permission denied") || lower.contains("access is denied")
}

fn is_daemon_unavailable(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no such file or directory")
        || lower.contains("connection refused")
        || lower.contains("cannot connect")
        || lower.contains("daemon is not running")
        || lower.contains("is docker running")
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
        // Docker-compatible providers do not support engine-delegated ownership
        // (:U is Podman-specific). Emit a warning but continue — the container
        // user must match bind-mount permissions or the recipe will fail at
        // runtime.
        for mount in &request.mounts {
            if mount.ownership.is_some() {
                tracing::warn!(
                    target = %mount.target,
                    "state binding owner requested but docker-compatible provider does not \
                     support engine-delegated ownership; relying on bind mount permissions"
                );
            }
        }
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

#[async_trait]
impl<C> OciRuntimeClient for DockerCompatibleOciProvider<C>
where
    C: OciRuntimeClient + Send + Sync,
{
    async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
        self.client.pull_image(image).await
    }

    async fn create_network(&self, request: &OciNetworkRequest) -> capsule_core::Result<String> {
        self.client.create_network(request).await
    }

    async fn remove_network(&self, network_name: &str) -> capsule_core::Result<()> {
        self.client.remove_network(network_name).await
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> capsule_core::Result<String> {
        self.client.create_container(request).await
    }

    async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
        self.client.start_container(container_id).await
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> capsule_core::Result<OciContainerInspect> {
        self.client.inspect_container(container_id).await
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> capsule_core::Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>> {
        self.client.logs(container_id, follow).await
    }

    async fn wait_container(&self, container_id: &str) -> capsule_core::Result<i64> {
        self.client.wait_container(container_id).await
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> capsule_core::Result<()> {
        self.client.stop_container(container_id, timeout_secs).await
    }

    async fn remove_container(&self, container_id: &str, force: bool) -> capsule_core::Result<()> {
        self.client.remove_container(container_id, force).await
    }

    async fn exec_container(
        &self,
        container_id: &str,
        cmd: &[String],
    ) -> capsule_core::Result<i64> {
        self.client.exec_container(container_id, cmd).await
    }
}

#[async_trait]
impl OciProvider for RuntimeOciProvider {
    fn semantics(&self) -> &OciProviderSemantics {
        match self {
            Self::Podman(provider) => provider.semantics(),
            Self::DockerCompatible(provider) => provider.semantics(),
        }
    }

    async fn probe(&self) -> Result<OciProviderProbe, OciProviderError> {
        match self {
            Self::Podman(provider) => provider.probe().await,
            Self::DockerCompatible(provider) => provider.probe().await,
        }
    }

    async fn resolve_image(
        &self,
        request: &OciImageResolutionRequest,
    ) -> Result<OciResolvedImage, OciProviderError> {
        match self {
            Self::Podman(provider) => provider.resolve_image(request).await,
            Self::DockerCompatible(provider) => provider.resolve_image(request).await,
        }
    }

    async fn pull_image(&self, image: &OciImageResolution) -> Result<(), OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::pull_image(provider, image).await,
            Self::DockerCompatible(provider) => OciProvider::pull_image(provider, image).await,
        }
    }

    async fn create_network(
        &self,
        request: &OciNetworkRequest,
    ) -> Result<String, OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::create_network(provider, request).await,
            Self::DockerCompatible(provider) => {
                OciProvider::create_network(provider, request).await
            }
        }
    }

    async fn remove_network(&self, network_name: &str) -> Result<(), OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::remove_network(provider, network_name).await,
            Self::DockerCompatible(provider) => {
                OciProvider::remove_network(provider, network_name).await
            }
        }
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> Result<String, OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::create_container(provider, request).await,
            Self::DockerCompatible(provider) => {
                OciProvider::create_container(provider, request).await
            }
        }
    }

    async fn start_container(&self, container_id: &str) -> Result<(), OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::start_container(provider, container_id).await,
            Self::DockerCompatible(provider) => {
                OciProvider::start_container(provider, container_id).await
            }
        }
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::inspect_container(provider, container_id).await,
            Self::DockerCompatible(provider) => {
                OciProvider::inspect_container(provider, container_id).await
            }
        }
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::logs(provider, container_id, follow).await,
            Self::DockerCompatible(provider) => {
                OciProvider::logs(provider, container_id, follow).await
            }
        }
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64, OciProviderError> {
        match self {
            Self::Podman(provider) => OciProvider::wait_container(provider, container_id).await,
            Self::DockerCompatible(provider) => {
                OciProvider::wait_container(provider, container_id).await
            }
        }
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> Result<(), OciProviderError> {
        match self {
            Self::Podman(provider) => {
                OciProvider::stop_container(provider, container_id, timeout_secs).await
            }
            Self::DockerCompatible(provider) => {
                OciProvider::stop_container(provider, container_id, timeout_secs).await
            }
        }
    }

    async fn remove_container(
        &self,
        container_id: &str,
        force: bool,
    ) -> Result<(), OciProviderError> {
        match self {
            Self::Podman(provider) => {
                OciProvider::remove_container(provider, container_id, force).await
            }
            Self::DockerCompatible(provider) => {
                OciProvider::remove_container(provider, container_id, force).await
            }
        }
    }
}

#[async_trait]
impl OciRuntimeClient for RuntimeOciProvider {
    async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
        match self {
            Self::Podman(provider) => OciRuntimeClient::pull_image(provider, image).await,
            Self::DockerCompatible(provider) => OciRuntimeClient::pull_image(provider, image).await,
        }
    }

    async fn create_network(&self, request: &OciNetworkRequest) -> capsule_core::Result<String> {
        match self {
            Self::Podman(provider) => OciRuntimeClient::create_network(provider, request).await,
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::create_network(provider, request).await
            }
        }
    }

    async fn remove_network(&self, network_name: &str) -> capsule_core::Result<()> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::remove_network(provider, network_name).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::remove_network(provider, network_name).await
            }
        }
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> capsule_core::Result<String> {
        match self {
            Self::Podman(provider) => OciRuntimeClient::create_container(provider, request).await,
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::create_container(provider, request).await
            }
        }
    }

    async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::start_container(provider, container_id).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::start_container(provider, container_id).await
            }
        }
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> capsule_core::Result<OciContainerInspect> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::inspect_container(provider, container_id).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::inspect_container(provider, container_id).await
            }
        }
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> capsule_core::Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>> {
        match self {
            Self::Podman(provider) => OciRuntimeClient::logs(provider, container_id, follow).await,
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::logs(provider, container_id, follow).await
            }
        }
    }

    async fn wait_container(&self, container_id: &str) -> capsule_core::Result<i64> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::wait_container(provider, container_id).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::wait_container(provider, container_id).await
            }
        }
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_secs: i64,
    ) -> capsule_core::Result<()> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::stop_container(provider, container_id, timeout_secs).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::stop_container(provider, container_id, timeout_secs).await
            }
        }
    }

    async fn remove_container(&self, container_id: &str, force: bool) -> capsule_core::Result<()> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::remove_container(provider, container_id, force).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::remove_container(provider, container_id, force).await
            }
        }
    }

    async fn exec_container(
        &self,
        container_id: &str,
        cmd: &[String],
    ) -> capsule_core::Result<i64> {
        match self {
            Self::Podman(provider) => {
                OciRuntimeClient::exec_container(provider, container_id, cmd).await
            }
            Self::DockerCompatible(provider) => {
                OciRuntimeClient::exec_container(provider, container_id, cmd).await
            }
        }
    }
}

// ── FakeOciProvider ───────────────────────────────────────────────────────────
// A fully-controllable in-process OCI provider for use in unit tests.
// All lifecycle results are set up front; no real Podman is required.
//
// Call tracking:
// - `call_log` records each method call as "<method>:<key_arg>" in order.
// - `create_container_queue` and `start_result_queue` allow per-call result queues.
//   When the queue is empty the flat result field is used as fallback.
// - `create_container_requests` captures every OciContainerRequest for inspection.

#[derive(Clone)]
pub(crate) struct FakeOciProvider {
    pub probe_result: Result<OciProviderProbe, OciProviderError>,
    /// When `Some`, every `resolve_image` call returns this error instead of the fake digest.
    pub resolve_error: Option<OciProviderError>,
    pub pull_result: Result<(), OciProviderError>,
    pub create_container_result: Result<String, OciProviderError>,
    pub start_result: Result<(), OciProviderError>,
    pub inspect_result: Result<OciContainerInspect, OciProviderError>,
    pub log_chunks: Vec<OciLogChunk>,
    pub wait_result: Result<i64, OciProviderError>,
    pub stop_result: Result<(), OciProviderError>,
    pub remove_result: Result<(), OciProviderError>,
    pub semantics: OciProviderSemantics,
    // ── call tracking (shared so clones share state) ──────────────────────────
    pub call_log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pub create_container_queue: std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<Result<String, OciProviderError>>>,
    >,
    pub create_container_requests: std::sync::Arc<std::sync::Mutex<Vec<OciContainerRequest>>>,
    pub start_result_queue:
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Result<(), OciProviderError>>>>,
    /// Per-call wait results.  When the queue is non-empty the front entry is
    /// consumed on each `wait_container` call; when empty `wait_result` is used.
    pub wait_result_queue:
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Result<i64, OciProviderError>>>>,
    /// Optional artificial delay (in milliseconds) before `wait_container`
    /// returns.  Used by run_once timeout tests to simulate a container that
    /// runs longer than the configured timeout without burning real wall-clock.
    pub wait_block_ms: std::sync::Arc<std::sync::Mutex<Option<u64>>>,
}

impl FakeOciProvider {
    pub(crate) fn ready() -> Self {
        Self {
            probe_result: Ok(fake_oci_probe_ready()),
            resolve_error: None,
            pull_result: Ok(()),
            create_container_result: Ok("fake-container-id".to_string()),
            start_result: Ok(()),
            inspect_result: Ok(OciContainerInspect {
                running: true,
                exit_code: None,
                host_ports: std::collections::HashMap::from([(8080u16, 45678u16)]),
            }),
            log_chunks: vec![],
            wait_result: Ok(0),
            stop_result: Ok(()),
            remove_result: Ok(()),
            semantics: fake_oci_semantics(),
            call_log: std::sync::Arc::new(std::sync::Mutex::new(vec![])),
            create_container_queue: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            create_container_requests: std::sync::Arc::new(std::sync::Mutex::new(vec![])),
            start_result_queue: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            wait_result_queue: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            wait_block_ms: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub(crate) fn with_probe_missing() -> Self {
        let mut f = Self::ready();
        f.probe_result = Err(OciProviderError::Missing {
            provider: "podman",
            binary: "podman",
        });
        f
    }

    pub(crate) fn with_probe_not_ready() -> Self {
        let mut f = Self::ready();
        let sem = fake_oci_semantics();
        f.probe_result = Ok(OciProviderProbe {
            ready: false,
            semantics: sem.clone(),
            inventory: OciProviderInventory {
                kind: OciProviderKind::Podman,
                binary: OciProviderBinaryStatus::Found,
                version: None,
                mode: OciProviderMode::Unknown,
                machine: OciProviderMachineStatus::MachineNotRunning,
                semantics: sem,
            },
            detail: Some("podman machine is not running".to_string()),
        });
        f
    }

    /// Simulate an image resolution failure for all resolve_image calls.
    pub(crate) fn with_resolve_error(error: OciProviderError) -> Self {
        let mut f = Self::ready();
        f.resolve_error = Some(error);
        f
    }

    /// Simulate a pull failure for all pull_image calls.
    pub(crate) fn with_pull_failure(error: OciProviderError) -> Self {
        let mut f = Self::ready();
        f.pull_result = Err(error);
        f
    }
}

pub(crate) fn fake_oci_semantics() -> OciProviderSemantics {
    OciProviderSemantics {
        kind: OciProviderKind::Podman,
        mode: OciProviderMode::Rootless,
        substrate: OciProviderSubstrate::NativeLinux,
        policy_profile: PODMAN_POLICY_PROFILE_V1.to_string(),
    }
}

pub(crate) fn fake_oci_probe_ready() -> OciProviderProbe {
    let sem = fake_oci_semantics();
    OciProviderProbe {
        ready: true,
        semantics: sem.clone(),
        inventory: OciProviderInventory {
            kind: OciProviderKind::Podman,
            binary: OciProviderBinaryStatus::Found,
            version: Some("4.0.0".to_string()),
            mode: OciProviderMode::Rootless,
            machine: OciProviderMachineStatus::NativeLinux,
            semantics: sem,
        },
        detail: None,
    }
}

#[async_trait]
impl OciProvider for FakeOciProvider {
    fn semantics(&self) -> &OciProviderSemantics {
        &self.semantics
    }

    async fn probe(&self) -> Result<OciProviderProbe, OciProviderError> {
        self.probe_result.clone()
    }

    async fn resolve_image(
        &self,
        request: &OciImageResolutionRequest,
    ) -> Result<OciResolvedImage, OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("resolve:{}", request.declared_ref));
        if let Some(err) = &self.resolve_error {
            return Err(err.clone());
        }
        Ok(OciResolvedImage {
            declared_ref: request.declared_ref.clone(),
            resolved_digest: format!("sha256:{}", "b".repeat(64)),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            media_type: None,
            provider_semantics: self.semantics.clone(),
        })
    }

    async fn pull_image(&self, image: &OciImageResolution) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("pull:{}", image.declared_ref));
        self.pull_result.clone()
    }

    async fn create_network(
        &self,
        request: &OciNetworkRequest,
    ) -> Result<String, OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("create_network:{}", request.name));
        Ok(format!("fake-network-{}", request.name))
    }

    async fn remove_network(&self, network_name: &str) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("remove_network:{}", network_name));
        Ok(())
    }

    async fn create_container(
        &self,
        request: &OciContainerRequest,
    ) -> Result<String, OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("create:{}", request.name));
        self.create_container_requests
            .lock()
            .unwrap()
            .push(request.clone());
        let mut queue = self.create_container_queue.lock().unwrap();
        if let Some(result) = queue.pop_front() {
            result
        } else {
            self.create_container_result.clone()
        }
    }

    async fn start_container(&self, container_id: &str) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("start:{}", container_id));
        let mut queue = self.start_result_queue.lock().unwrap();
        if let Some(result) = queue.pop_front() {
            result
        } else {
            self.start_result.clone()
        }
    }

    async fn inspect_container(
        &self,
        container_id: &str,
    ) -> Result<OciContainerInspect, OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("inspect:{}", container_id));
        self.inspect_result.clone()
    }

    async fn logs(
        &self,
        _container_id: &str,
        _follow: bool,
    ) -> Result<mpsc::Receiver<capsule_core::Result<OciLogChunk>>, OciProviderError> {
        let (tx, rx) = mpsc::channel(64);
        for chunk in &self.log_chunks {
            let _ = tx.try_send(Ok(chunk.clone()));
        }
        Ok(rx)
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64, OciProviderError> {
        // Optional artificial block (used by run_once timeout tests).
        let block_ms = *self.wait_block_ms.lock().unwrap();
        if let Some(ms) = block_ms {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        self.call_log
            .lock()
            .unwrap()
            .push(format!("wait_container:{container_id}"));
        if let Some(result) = self.wait_result_queue.lock().unwrap().pop_front() {
            return result;
        }
        self.wait_result.clone()
    }

    async fn stop_container(
        &self,
        container_id: &str,
        _timeout_secs: i64,
    ) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("stop:{}", container_id));
        self.stop_result.clone()
    }

    async fn remove_container(
        &self,
        container_id: &str,
        _force: bool,
    ) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("remove:{}", container_id));
        self.remove_result.clone()
    }

    async fn remove_volume(&self, volume_name: &str) -> Result<(), OciProviderError> {
        self.call_log
            .lock()
            .unwrap()
            .push(format!("remove_volume:{}", volume_name));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::runtime::oci::{
        OciContainerInspect, OciMountSourceKind, OciMountSpec, OciPortSpec,
    };
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

    #[test]
    fn runtime_provider_selection_keeps_ready_podman() {
        let choice =
            choose_runtime_oci_provider(Ok(()), Ok(())).expect("ready podman should be selected");

        assert_eq!(choice, RuntimeOciProviderChoice::Podman);
    }

    #[test]
    fn runtime_provider_selection_prefers_ready_docker_over_unconfigured_podman() {
        let choice =
            choose_runtime_oci_provider(Err(OciProviderError::MachineNotConfigured), Ok(()))
                .expect("ready docker-compatible provider should be selected as fallback");

        assert_eq!(choice, RuntimeOciProviderChoice::DockerCompatible);
    }

    #[test]
    fn runtime_provider_selection_preserves_podman_setup_error_without_ready_docker() {
        let err = choose_runtime_oci_provider(
            Err(OciProviderError::MachineNotConfigured),
            Err(OciProviderError::ProbeFailed {
                provider: "docker-compatible",
                message: "cannot connect to Docker daemon".to_string(),
            }),
        )
        .expect_err("podman setup error should remain actionable when Docker is not ready");

        assert_eq!(err, OciProviderError::MachineNotConfigured);
        assert_eq!(err.code(), "oci_machine_not_configured");
        assert!(err.to_string().contains("podman machine init"));
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

        async fn exec_container(
            &self,
            _container_id: &str,
            _cmd: &[String],
        ) -> capsule_core::Result<i64> {
            Ok(0)
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
        assert!(
            probe
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("not running"))
        );
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
    fn podman_publish_port_arg_honors_loopback_host_ip() {
        use capsule_core::runtime::oci::OciPortSpec;

        let fixed = OciPortSpec {
            container_port: 8080,
            host_port: Some(45678),
            protocol: "tcp".to_string(),
            host_ip: Some("127.0.0.1".to_string()),
        };
        assert_eq!(
            super::podman_publish_port_arg(&fixed),
            "127.0.0.1:45678:8080/tcp"
        );

        let dynamic = OciPortSpec {
            container_port: 8080,
            host_port: None,
            protocol: "tcp".to_string(),
            host_ip: Some("127.0.0.1".to_string()),
        };
        assert_eq!(
            super::podman_publish_port_arg(&dynamic),
            "127.0.0.1::8080/tcp"
        );

        let host_port_only = OciPortSpec {
            container_port: 8080,
            host_port: Some(45678),
            protocol: "tcp".to_string(),
            host_ip: None,
        };
        assert_eq!(
            super::podman_publish_port_arg(&host_port_only),
            "45678:8080/tcp"
        );

        let neither = OciPortSpec {
            container_port: 8080,
            host_port: None,
            protocol: "udp".to_string(),
            host_ip: None,
        };
        assert_eq!(super::podman_publish_port_arg(&neither), "8080/udp");
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

        OciProvider::pull_image(&provider, &image())
            .await
            .expect("pull");
        let container_id = OciProvider::create_container(
            &provider,
            &OciContainerRequest {
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
                    ownership: None,
                    source_kind: OciMountSourceKind::default(),
                }],
                ports: vec![OciPortSpec {
                    container_port: 3000,
                    host_port: None,
                    protocol: "tcp".to_string(),
                    host_ip: Some("127.0.0.1".to_string()),
                }],
                network: None,
                aliases: Vec::new(),
                platform: None,
                extra_hosts: vec![],
                user: None,
            },
        )
        .await
        .expect("create");
        OciProvider::start_container(&provider, &container_id)
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

    // ── Image resolution tests ────────────────────────────────────────────────

    fn multi_arch_manifest_json() -> &'static str {
        r#"{
          "schemaVersion": 2,
          "mediaType": "application/vnd.docker.distribution.manifest.list.v2+json",
          "manifests": [
            {
              "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
              "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "platform": { "os": "linux", "architecture": "amd64" }
            },
            {
              "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
              "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "platform": { "os": "linux", "architecture": "arm64", "variant": "v8" }
            }
          ]
        }"#
    }

    fn single_arch_manifest_json() -> &'static str {
        r#"{
          "schemaVersion": 2,
          "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
          "config": { "mediaType": "application/vnd.docker.container.image.v1+json" }
        }"#
    }

    fn fake_semantics() -> OciProviderSemantics {
        OciProviderSemantics {
            kind: OciProviderKind::Podman,
            mode: OciProviderMode::Rootless,
            substrate: OciProviderSubstrate::NativeLinux,
            policy_profile: "oci-podman-v1".to_string(),
        }
    }

    #[tokio::test]
    async fn resolves_tag_to_digest_and_platform_with_fake_provider() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "manifest", "inspect", "postgres:14"],
                output(0, multi_arch_manifest_json(), ""),
            ),
            PodmanProbePlatform::Linux,
        );

        let request = OciImageResolutionRequest {
            target_label: "db".to_string(),
            declared_ref: "postgres:14".to_string(),
            requested_platform: Some(OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            }),
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let resolved = provider.resolve_image(&request).await.expect("resolve");
        assert_eq!(
            resolved.resolved_digest,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(resolved.platform.os, "linux");
        assert_eq!(resolved.platform.architecture, "amd64");
        assert_eq!(resolved.declared_ref, "postgres:14");
    }

    #[tokio::test]
    async fn digest_ref_with_platform_resolves_single_arch_manifest() {
        let digest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let declared_ref = format!("postgres@{digest}");
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "manifest", "inspect", &declared_ref],
                output(0, single_arch_manifest_json(), ""),
            ),
            PodmanProbePlatform::Linux,
        );

        let request = OciImageResolutionRequest {
            target_label: "db".to_string(),
            declared_ref: declared_ref.clone(),
            requested_platform: Some(OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            }),
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let resolved = provider.resolve_image(&request).await.expect("resolve");
        assert_eq!(resolved.resolved_digest, digest);
        assert_eq!(resolved.platform.architecture, "amd64");
    }

    #[tokio::test]
    async fn mutable_tag_single_arch_manifest_fails_with_typed_error() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "manifest", "inspect", "myimage:latest"],
                output(0, single_arch_manifest_json(), ""),
            ),
            PodmanProbePlatform::Linux,
        );

        let request = OciImageResolutionRequest {
            target_label: "app".to_string(),
            declared_ref: "myimage:latest".to_string(),
            requested_platform: Some(OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            }),
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let err = provider.resolve_image(&request).await.expect_err(
            "should fail for mutable tag on single-arch manifest with NativeOnly policy",
        );
        assert_eq!(err.code(), "oci_image_platform_unsupported");
        assert!(
            err.to_string().contains("allow_emulation"),
            "error should mention allow_emulation: {err}"
        );
    }

    #[tokio::test]
    async fn malformed_image_ref_returns_typed_error() {
        let provider =
            PodmanProvider::with_runner(FakeRunner::default(), PodmanProbePlatform::Linux);

        let request = OciImageResolutionRequest {
            target_label: "app".to_string(),
            declared_ref: "has space".to_string(),
            requested_platform: None,
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let err = provider
            .resolve_image(&request)
            .await
            .expect_err("malformed ref must fail");
        assert_eq!(err.code(), "oci_image_ref_malformed");
    }

    #[tokio::test]
    async fn multi_arch_manifest_auto_selects_host_platform_when_no_platform_requested() {
        // With no requested_platform, the provider should auto-select linux/<host_arch>
        // (or fall back to linux/amd64) rather than failing with "ambiguous".
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "manifest", "inspect", "postgres:14"],
                output(0, multi_arch_manifest_json(), ""),
            ),
            PodmanProbePlatform::Linux,
        );

        let request = OciImageResolutionRequest {
            target_label: "db".to_string(),
            declared_ref: "postgres:14".to_string(),
            requested_platform: None, // auto-select host platform
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let resolved = provider
            .resolve_image(&request)
            .await
            .expect("auto-selection must succeed on multi-arch manifest");
        // The resolved platform must be linux/amd64 or linux/arm64 (both are in the fixture).
        assert_eq!(resolved.platform.os, "linux");
        assert!(
            resolved.platform.architecture == "amd64" || resolved.platform.architecture == "arm64",
            "unexpected platform architecture: {}",
            resolved.platform.architecture
        );
    }

    #[tokio::test]
    async fn unsupported_platform_returns_typed_error_when_not_found() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "manifest", "inspect", "postgres:14"],
                output(0, multi_arch_manifest_json(), ""),
            ),
            PodmanProbePlatform::Linux,
        );

        let request = OciImageResolutionRequest {
            target_label: "db".to_string(),
            declared_ref: "postgres:14".to_string(),
            requested_platform: Some(OciPlatform {
                os: "windows".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            }),
            resolution_mode: OciImageResolutionMode::Required,
            importer_input_hash: None,
            platform_policy: OciPlatformPolicy::NativeOnly,
        };

        let err = provider
            .resolve_image(&request)
            .await
            .expect_err("unsupported platform must fail");
        assert_eq!(err.code(), "oci_image_platform_unsupported");
    }

    #[test]
    fn resolved_image_converts_to_lock_resolution() {
        let resolved = OciResolvedImage {
            declared_ref: "postgres:14".to_string(),
            resolved_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            media_type: Some("application/vnd.docker.distribution.manifest.v2+json".to_string()),
            provider_semantics: fake_semantics(),
        };

        let lock = resolved.into_lock_resolution();
        assert_eq!(lock.declared_ref, "postgres:14");
        assert_eq!(
            lock.resolved_digest,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(lock.platform.os, "linux");
        assert_eq!(lock.platform.architecture, "amd64");
        assert!(lock.importer_input_hash.is_none());
    }

    // ----- ensure_ready() tests -----

    #[tokio::test]
    async fn ensure_ready_macos_already_running_returns_ok() {
        let machine_json = r#"[{"Name":"podman-machine-default","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                .with_output(&["podman", "info"], output(0, "{}", "")),
            PodmanProbePlatform::Macos,
        );
        provider
            .ensure_ready()
            .await
            .expect("already running must be ok");
        // No Ato machine present → no connection pinned (default preserved).
        assert_eq!(provider.cached_connection(), None);
    }

    #[tokio::test]
    async fn ensure_ready_macos_ato_running_pins_connection_and_verifies_it() {
        let machine_json = r#"[{"Name":"ato-podman","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.8.2\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                // Readiness must verify via the Ato connection, NOT plain `info`.
                .with_output(
                    &["podman", "--connection", "ato-podman", "info"],
                    output(0, "{}", ""),
                ),
            PodmanProbePlatform::Macos,
        );
        provider
            .ensure_ready()
            .await
            .expect("ato-podman running must be ready via its connection");
        assert_eq!(provider.cached_connection(), Some("ato-podman".to_string()));
    }

    #[tokio::test]
    async fn ensure_ready_macos_ato_running_alongside_other_is_not_ambiguous() {
        // ato-podman running + another machine running: prior code would report
        // MachineAmbiguous; now the Ato machine wins and is used explicitly.
        let machine_json = r#"[{"Name":"podman-machine-default","Running":true},{"Name":"ato-podman","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.8.2\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                .with_output(
                    &["podman", "--connection", "ato-podman", "info"],
                    output(0, "{}", ""),
                ),
            PodmanProbePlatform::Macos,
        );
        provider
            .ensure_ready()
            .await
            .expect("running ato-podman must win over ambiguity");
        assert_eq!(provider.cached_connection(), Some("ato-podman".to_string()));
    }

    #[test]
    fn prepend_connection_puts_flag_before_subcommand() {
        // Test #7: `--connection` must precede the subcommand.
        assert_eq!(
            prepend_connection(Some("ato-podman"), &["info"]),
            vec!["--connection", "ato-podman", "info"]
        );
        assert_eq!(
            prepend_connection(Some("ato-podman"), &["pull", "img"]),
            vec!["--connection", "ato-podman", "pull", "img"]
        );
        assert_eq!(prepend_connection(None, &["info"]), vec!["info"]);
    }

    #[test]
    fn ato_machine_running_detects_running_ato_only() {
        let mk = |name: &str, running: bool| PodmanMachine {
            name: name.to_string(),
            running,
        };
        assert!(ato_machine_running(&[mk("ato-podman", true)]));
        assert!(ato_machine_running(&[
            mk("other", true),
            mk("ato-podman", true)
        ]));
        assert!(!ato_machine_running(&[mk("ato-podman", false)]));
        assert!(!ato_machine_running(&[mk("other", true)]));
        assert!(!ato_machine_running(&[]));
    }

    #[test]
    fn fresh_provider_lazily_resolves_ato_connection_for_daemon_ops() {
        // An executor builds its own provider that never saw ensure_ready; it
        // must still resolve and pin the Ato connection on first daemon use.
        let machine_json = r#"[{"Name":"ato-podman","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                .with_output(
                    &[
                        "podman",
                        "--connection",
                        "ato-podman",
                        "manifest",
                        "inspect",
                        "img:1",
                    ],
                    output(0, "{}", ""),
                ),
            PodmanProbePlatform::Macos,
        );
        assert_eq!(
            provider.resolved_connection(),
            Some("ato-podman".to_string())
        );
        // A daemon op (e.g. manifest inspect during resolve_image) is pinned.
        let out = provider
            .run_podman(&["manifest", "inspect", "img:1"])
            .expect("manifest inspect runs against the Ato connection");
        assert!(out.success());
    }

    #[test]
    fn fresh_provider_without_ato_resolves_no_connection() {
        let machine_json = r#"[{"Name":"podman-machine-default","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default().with_output(
                &["podman", "machine", "list", "--format", "json"],
                output(0, machine_json, ""),
            ),
            PodmanProbePlatform::Macos,
        );
        // No Ato machine → no pin (default connection preserved).
        assert_eq!(provider.resolved_connection(), None);
    }

    #[test]
    fn linux_provider_resolves_no_connection_without_probing() {
        let provider =
            PodmanProvider::with_runner(FakeRunner::default(), PodmanProbePlatform::Linux);
        // Linux never injects a connection and must not even read machine list.
        assert_eq!(provider.resolved_connection(), None);
    }

    #[tokio::test]
    async fn ensure_ready_macos_single_stopped_starts_and_returns_ok() {
        let stopped_json = r#"[{"Name":"podman-machine-default","Running":false}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, stopped_json, ""),
                )
                .with_output(
                    &["podman", "machine", "start", "podman-machine-default"],
                    output(
                        0,
                        "Machine \"podman-machine-default\" started successfully\n",
                        "",
                    ),
                )
                .with_output(&["podman", "info"], output(0, "{}", "")),
            PodmanProbePlatform::Macos,
        );
        provider
            .ensure_ready()
            .await
            .expect("single stopped machine should start and become ready");
        // Non-Ato machine → no connection pin (default preserved).
        assert_eq!(provider.cached_connection(), None);
    }

    #[tokio::test]
    async fn ensure_ready_macos_ato_stopped_starts_and_polls_ato_connection() {
        // A stopped ato-podman must be started AND its readiness poll pinned to
        // `--connection ato-podman` — not the host default (which may point at a
        // different/stopped machine).
        let stopped_json = r#"[{"Name":"ato-podman","Running":false}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.8.2\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, stopped_json, ""),
                )
                .with_output(
                    &["podman", "machine", "start", "ato-podman"],
                    output(0, "Machine \"ato-podman\" started successfully\n", ""),
                )
                // The poll must target the Ato connection, never plain `info`.
                .with_output(
                    &["podman", "--connection", "ato-podman", "info"],
                    output(0, "{}", ""),
                ),
            PodmanProbePlatform::Macos,
        );
        provider
            .ensure_ready()
            .await
            .expect("stopped ato-podman should start and verify via its connection");
        assert_eq!(provider.cached_connection(), Some("ato-podman".to_string()));
    }

    #[tokio::test]
    async fn ensure_ready_macos_no_machines_returns_not_configured_error() {
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
        let err = provider
            .ensure_ready()
            .await
            .expect_err("no machines must fail");
        assert_eq!(err.code(), "oci_machine_not_configured");
    }

    #[tokio::test]
    async fn ensure_ready_macos_multiple_stopped_returns_ambiguous_error() {
        let two_stopped =
            r#"[{"Name":"machine-a","Running":false},{"Name":"machine-b","Running":false}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, two_stopped, ""),
                ),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("multiple stopped machines must fail");
        assert_eq!(err.code(), "oci_machine_ambiguous");
    }

    #[tokio::test]
    async fn ensure_ready_macos_multiple_running_returns_ambiguous_error() {
        let two_running =
            r#"[{"Name":"machine-a","Running":true},{"Name":"machine-b","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, two_running, ""),
                ),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("multiple running machines must fail");
        assert_eq!(err.code(), "oci_machine_ambiguous");
    }

    #[tokio::test]
    async fn ensure_ready_macos_one_running_one_stopped_returns_ambiguous_error() {
        let mixed_state =
            r#"[{"Name":"machine-a","Running":false},{"Name":"machine-b","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, mixed_state, ""),
                ),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("mixed-state machines must fail");
        assert_eq!(err.code(), "oci_machine_ambiguous");
    }

    #[tokio::test]
    async fn ensure_ready_missing_podman_binary_returns_missing_error() {
        // FakeRunner returns NotFound for any unregistered command, which
        // run_provider_command maps to OciProviderError::Missing.
        let provider =
            PodmanProvider::with_runner(FakeRunner::default(), PodmanProbePlatform::Macos);
        let err = provider
            .ensure_ready()
            .await
            .expect_err("missing binary must fail");
        assert_eq!(err.code(), "oci_provider_missing");
    }

    #[tokio::test]
    async fn ensure_ready_start_succeeds_but_podman_info_fails_returns_timeout() {
        // With #[cfg(test)] MACHINE_READY_POLL_TIMEOUT = Duration::ZERO, the
        // first failing `podman info` response immediately triggers timeout.
        let stopped_json = r#"[{"Name":"podman-machine-default","Running":false}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, stopped_json, ""),
                )
                .with_output(
                    &["podman", "machine", "start", "podman-machine-default"],
                    output(0, "started\n", ""),
                )
                .with_output(&["podman", "info"], output(1, "", "daemon not ready")),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("failing podman info must time out");
        assert_eq!(err.code(), "oci_machine_ready_timeout");
    }

    #[tokio::test]
    async fn ensure_ready_linux_delegates_to_probe() {
        // On Linux, ensure_ready calls probe + require_ready. Native Linux is
        // always ready, so no machine management is attempted.
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 4.9.0\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(0, "true\n", ""),
                ),
            PodmanProbePlatform::Linux,
        );
        provider
            .ensure_ready()
            .await
            .expect("linux native podman is always ready");
    }

    #[test]
    fn podman_disabled_error_has_stable_code() {
        assert_eq!(
            OciProviderError::PodmanDisabled.code(),
            "oci_podman_disabled"
        );
    }

    /// `ATO_PODMAN_ENABLED` is process-global. `#[serial]` keeps this off the
    /// parallel test scheduler so it can't perturb other tests that exercise
    /// `select_ready_runtime_oci_provider()`; the test also restores the prior
    /// value on the way out.
    #[serial_test::serial]
    #[test]
    fn podman_enabled_reads_env_opt_out() {
        let previous = std::env::var_os("ATO_PODMAN_ENABLED");

        unsafe { std::env::set_var("ATO_PODMAN_ENABLED", "0") };
        assert!(!podman_enabled(), "\"0\" must disable Podman");

        unsafe { std::env::set_var("ATO_PODMAN_ENABLED", "1") };
        assert!(podman_enabled(), "\"1\" must enable Podman");

        unsafe { std::env::remove_var("ATO_PODMAN_ENABLED") };
        assert!(podman_enabled(), "unset must default to enabled (opt-out)");

        match previous {
            Some(value) => unsafe { std::env::set_var("ATO_PODMAN_ENABLED", value) },
            None => unsafe { std::env::remove_var("ATO_PODMAN_ENABLED") },
        }
    }

    // ── Podman mount :U / ReadOnlyOwnershipConflict tests (#428 followup) ────

    fn test_ownership() -> capsule_core::types::MountOwnership {
        capsule_core::types::MountOwnership {
            uid: Some(1001),
            gid: Some(1001),
            recursive: false,
            mode: Some(0o755),
        }
    }

    fn oci_mount_spec(
        target: &str,
        readonly: bool,
        ownership: Option<capsule_core::types::MountOwnership>,
    ) -> OciMountSpec {
        OciMountSpec {
            source: "/host/src".to_string(),
            target: target.to_string(),
            readonly,
            ownership,
            source_kind: OciMountSourceKind::default(),
        }
    }

    #[test]
    fn podman_mount_opts_writable_without_ownership_is_empty() {
        let m = oci_mount_spec("/app/data", false, None);
        assert_eq!(podman_mount_opts(&m), "");
    }

    #[test]
    fn podman_mount_opts_readonly_without_ownership_is_ro() {
        let m = oci_mount_spec("/app/cfg", true, None);
        assert_eq!(podman_mount_opts(&m), ":ro");
    }

    #[test]
    fn podman_mount_opts_writable_with_ownership_is_colon_u() {
        let m = oci_mount_spec("/app/data", false, Some(test_ownership()));
        assert_eq!(podman_mount_opts(&m), ":U");
    }

    #[test]
    fn podman_mount_opts_readonly_with_ownership_errors() {
        // Confirm the predicate that triggers ReadOnlyOwnershipConflict.
        let m = oci_mount_spec("/app/data", true, Some(test_ownership()));
        assert!(
            m.readonly && m.ownership.is_some(),
            "readonly + ownership must be detectable as a conflict"
        );
        // podman_mount_opts would return :U but the caller guards this before calling.
        assert_ne!(podman_mount_opts(&m), ":ro");
    }

    // ── podman_mount_arg: BindPath vs EngineVolume command builder (#444) ──────

    #[test]
    fn podman_mount_arg_bind_path_uses_source_path() {
        // A non-existent host path can't be canonicalized, so the literal source
        // is used — `source:target` with no extra options.
        let m = OciMountSpec {
            source: "/host/state".to_string(),
            target: "/data".to_string(),
            readonly: false,
            ownership: None,
            source_kind: OciMountSourceKind::BindPath,
        };
        assert_eq!(podman_mount_arg(&m), "/host/state:/data");
    }

    #[test]
    fn podman_mount_arg_engine_volume_uses_volume_name_verbatim() {
        // Engine volume source is a name, not a path: never canonicalized.
        let m = OciMountSpec {
            source: "ato-state-deadbeef0000-data".to_string(),
            target: "/data".to_string(),
            readonly: false,
            ownership: None,
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: false,
            },
        };
        assert_eq!(podman_mount_arg(&m), "ato-state-deadbeef0000-data:/data");
    }

    #[test]
    fn podman_mount_arg_engine_volume_with_ownership_appends_colon_u() {
        let m = OciMountSpec {
            source: "ato-state-deadbeef0000-pgdata".to_string(),
            target: "/var/lib/postgresql/data".to_string(),
            readonly: false,
            ownership: Some(test_ownership()),
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: true,
            },
        };
        assert_eq!(
            podman_mount_arg(&m),
            "ato-state-deadbeef0000-pgdata:/var/lib/postgresql/data:U"
        );
    }

    // ── Provider health diagnostics tests (#430) ──────────────────────────────

    #[test]
    fn invalid_binary_override_has_stable_code() {
        let err = OciProviderError::InvalidBinaryOverride {
            path: "/bad/path".to_string(),
        };
        assert_eq!(err.code(), "oci_invalid_binary_override");
        assert!(err.to_string().contains("/bad/path"));
        assert!(err.to_string().contains("ATO_PODMAN_BIN"));
    }

    #[test]
    fn storage_corrupted_has_stable_code() {
        let err = OciProviderError::StorageCorrupted {
            reason: "overlay graphDriver: no such file".to_string(),
        };
        assert_eq!(err.code(), "oci_storage_corrupted");
        assert!(err.to_string().contains("storage or graph driver"));
        assert!(err.to_string().contains("podman system reset"));
    }

    #[test]
    fn docker_daemon_unavailable_has_stable_code() {
        let err = OciProviderError::DockerDaemonUnavailable {
            reason: "connection refused".to_string(),
        };
        assert_eq!(err.code(), "oci_docker_daemon_unavailable");
        assert!(err.to_string().contains("not reachable"));
    }

    #[test]
    fn docker_permission_denied_has_stable_code() {
        assert_eq!(
            OciProviderError::DockerPermissionDenied.code(),
            "oci_docker_permission_denied"
        );
        assert!(
            OciProviderError::DockerPermissionDenied
                .to_string()
                .contains("Permission denied")
        );
    }

    #[test]
    fn is_storage_corrupted_classifies_known_patterns() {
        assert!(is_storage_corrupted(
            "Error: storage: graphDriver not initialized"
        ));
        assert!(is_storage_corrupted(
            "overlay: storage corrupt: no such file"
        ));
        assert!(is_storage_corrupted("graphdriver error"));
        assert!(!is_storage_corrupted("connection refused"));
        assert!(!is_storage_corrupted("permission denied"));
    }

    #[test]
    fn is_permission_denied_classifies_docker_socket_error() {
        assert!(is_permission_denied(
            "Got permission denied while trying to connect to the Docker daemon socket"
        ));
        assert!(is_permission_denied("Access is denied (OS error 5)"));
        assert!(!is_permission_denied("connection refused"));
    }

    #[test]
    fn is_daemon_unavailable_classifies_connection_errors() {
        assert!(is_daemon_unavailable(
            "error during connect: no such file or directory"
        ));
        assert!(is_daemon_unavailable("connection refused"));
        assert!(is_daemon_unavailable("Is Docker running?"));
        assert!(!is_daemon_unavailable("permission denied"));
    }

    #[test]
    fn classify_docker_error_message_maps_permission_denied() {
        let err = classify_docker_error_message("permission denied to Docker socket");
        assert_eq!(err.code(), "oci_docker_permission_denied");
    }

    #[test]
    fn classify_docker_error_message_maps_daemon_unavailable() {
        let err = classify_docker_error_message("connection refused to Docker daemon");
        assert_eq!(err.code(), "oci_docker_daemon_unavailable");
    }

    #[test]
    fn classify_docker_error_message_maps_unknown_to_probe_failed() {
        let err = classify_docker_error_message("some unexpected error");
        assert_eq!(err.code(), "oci_provider_probe_failed");
    }

    // ── StorageCorrupted in real readiness paths ──────────────────────────────

    #[tokio::test]
    async fn ensure_ready_macos_running_storage_error_returns_storage_corrupted() {
        let machine_json = r#"[{"Name":"podman-machine-default","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                .with_output(
                    &["podman", "info"],
                    output(1, "", "Error: storage: graphDriver not initialized"),
                ),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("storage error must fail");
        assert_eq!(err.code(), "oci_storage_corrupted");
    }

    #[tokio::test]
    async fn ensure_ready_macos_running_connection_refused_is_probe_failed_not_storage_corrupted() {
        let machine_json = r#"[{"Name":"podman-machine-default","Running":true}]"#;
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 5.2.1\n", ""),
                )
                .with_output(
                    &["podman", "machine", "list", "--format", "json"],
                    output(0, machine_json, ""),
                )
                .with_output(&["podman", "info"], output(1, "", "connection refused")),
            PodmanProbePlatform::Macos,
        );
        let err = provider
            .ensure_ready()
            .await
            .expect_err("failed podman info must fail");
        assert_eq!(err.code(), "oci_provider_probe_failed");
        assert_ne!(err.code(), "oci_storage_corrupted");
    }

    #[tokio::test]
    async fn podman_probe_linux_storage_error_returns_storage_corrupted() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 4.9.0\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(1, "", "Error: storage: graphDriver not initialized"),
                ),
            PodmanProbePlatform::Linux,
        );
        let err = provider
            .probe()
            .await
            .expect_err("storage error must fail probe");
        assert_eq!(err.code(), "oci_storage_corrupted");
    }

    #[tokio::test]
    async fn podman_probe_linux_permission_denied_is_not_storage_corrupted() {
        let provider = PodmanProvider::with_runner(
            FakeRunner::default()
                .with_output(
                    &["podman", "--version"],
                    output(0, "podman version 4.9.0\n", ""),
                )
                .with_output(
                    &["podman", "info", "--format", "{{.Host.Security.Rootless}}"],
                    output(1, "", "permission denied"),
                ),
            PodmanProbePlatform::Linux,
        );
        let err = provider
            .probe()
            .await
            .expect_err("permission denied must fail probe");
        assert_ne!(err.code(), "oci_storage_corrupted");
    }

    // ── select_ready_runtime_oci_provider_with_report actual return values ────

    #[serial_test::serial]
    #[tokio::test]
    async fn select_ready_runtime_with_report_podman_disabled_sets_podman_error_in_report() {
        let previous = std::env::var_os("ATO_PODMAN_ENABLED");
        unsafe { std::env::set_var("ATO_PODMAN_ENABLED", "0") };

        let (_result, report) = select_ready_runtime_oci_provider_with_report().await;

        match previous {
            Some(v) => unsafe { std::env::set_var("ATO_PODMAN_ENABLED", v) },
            None => unsafe { std::env::remove_var("ATO_PODMAN_ENABLED") },
        }

        assert_eq!(
            report.podman_error.as_ref().map(|e| e.code()),
            Some("oci_podman_disabled"),
            "report must record PodmanDisabled when opt-out is set"
        );
        assert_ne!(
            report.selected,
            Some(OciProviderKind::Podman),
            "Podman must not be selected when disabled"
        );
        assert!(!report.reason.is_empty(), "report reason must be non-empty");
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn select_ready_runtime_with_report_no_provider_emits_both_errors_in_report() {
        // Force both providers to be unavailable: disable Podman and suppress
        // Docker by pointing ATO_PODMAN_BIN at an intentionally absent path so
        // the Docker path is exercised without Podman ever starting.
        // We only check the Podman-disabled path here since Docker availability
        // is not controllable in the test environment without a real daemon.
        let previous = std::env::var_os("ATO_PODMAN_ENABLED");
        unsafe { std::env::set_var("ATO_PODMAN_ENABLED", "0") };

        let (result, report) = select_ready_runtime_oci_provider_with_report().await;

        match previous {
            Some(v) => unsafe { std::env::set_var("ATO_PODMAN_ENABLED", v) },
            None => unsafe { std::env::remove_var("ATO_PODMAN_ENABLED") },
        }

        // podman_error is always PodmanDisabled in the opt-out path.
        assert_eq!(
            report.podman_error.as_ref().map(|e| e.code()),
            Some("oci_podman_disabled")
        );
        // If Docker is also unavailable, result is Err and selected is None.
        // If Docker is available, result is Ok and selected is DockerCompatible.
        // Either way, the report must have a non-empty reason.
        assert!(!report.reason.is_empty());
        match result {
            Ok(_) => assert_eq!(report.selected, Some(OciProviderKind::DockerCompatible)),
            Err(_) => assert_eq!(report.selected, None),
        }
    }

    #[test]
    fn provider_selection_report_auto_podman_ready() {
        let report = OciProviderSelectionReport {
            selected: Some(OciProviderKind::Podman),
            reason: "Podman is installed and ready".to_string(),
            fallback_candidate: None,
            podman_error: None,
            docker_error: None,
        };
        assert_eq!(report.selected, Some(OciProviderKind::Podman));
        assert!(report.podman_error.is_none());
        assert!(report.docker_error.is_none());
        assert!(!report.reason.is_empty());
    }

    #[test]
    fn provider_selection_report_fallback_to_docker() {
        let report = OciProviderSelectionReport {
            selected: Some(OciProviderKind::DockerCompatible),
            reason: "Podman not ready; Docker-compatible is ready".to_string(),
            fallback_candidate: None,
            podman_error: Some(OciProviderError::MachineNotConfigured),
            docker_error: None,
        };
        assert_eq!(report.selected, Some(OciProviderKind::DockerCompatible));
        assert!(report.podman_error.is_some());
        assert!(report.docker_error.is_none());
    }

    #[test]
    fn provider_selection_report_no_provider() {
        let report = OciProviderSelectionReport {
            selected: None,
            reason: "no provider ready: Podman missing, Docker unavailable".to_string(),
            fallback_candidate: None,
            podman_error: Some(OciProviderError::Missing {
                provider: "podman",
                binary: "podman",
            }),
            docker_error: Some(OciProviderError::DockerDaemonUnavailable {
                reason: "connection refused".to_string(),
            }),
        };
        assert!(report.selected.is_none());
        assert!(report.podman_error.is_some());
        assert!(report.docker_error.is_some());
    }
}
