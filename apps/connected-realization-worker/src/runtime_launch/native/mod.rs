//! Runner-owned Native startup and cleanup. Kept behind dispatch admission:
//! transport readiness never authorizes publication or satisfies Contract K.
mod producer;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use ato_adapter_oci::{DockerOciAdapter, OciHandle, OciOwner, StopBudget, StopOutcome};
use ato_ipc::{
    runtime_launch::{LaunchRealizationV1, RuntimeLaunchSpecV1},
    session_surface::{SessionSurfaceDescriptor, SurfaceCapabilityValue, SurfaceExecutionScope},
};
use ato_native_surface_gateway::{
    NativeProbeHandle, NativeProbeTarget, capture_and_start_native_probe,
};
use ato_runtime_attempt::{
    launch::{oci::container_oci_spec, resolved::ResolvedRuntimeLaunchContext},
    native_launch::NativeLaunchDeclaration,
};
use netd::{
    native_surface::{
        NativeSurfaceConfig, NativeSurfaceHandle, NativeSurfaceProbe, start_ready_native_surface,
    },
    pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring},
    surface_authorization::SurfaceGatewayScope,
    terminal_gateway::TerminalGatewayConfig,
};
use producer::TerminalProducer;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

/// Runtime inputs only. Keys, host addresses and owner identities never enter D.
/// No Debug/Serialize: verification_key must not reach logs or durable evidence.
pub struct NativeLeaseSurfaceConfig {
    pub scope: SurfaceGatewayScope,
    pub verification_key: String,
    pub origin: String,
    pub surface_listen: SocketAddr,
    pub startup_timeout: Duration,
    pub idle_timeout: Duration,
    pub maximum_lifetime: Duration,
}

pub struct OwnedNativeTerminalRun {
    surface_addr: SocketAddr,
    observation_addr: SocketAddr,
    execution: super::lease::RuntimeExecutionEvidence,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<NativeCompletion>>,
}
/// Physical termination evidence; it never supplies a Contract verdict.
pub struct NativeCompletion {
    pub stop: StopOutcome,
    pub cause: &'static str,
}
impl OwnedNativeTerminalRun {
    pub fn surface_addr(&self) -> SocketAddr {
        self.surface_addr
    }
    /// Private ingress must bind this exact endpoint to the declared probe Port.
    pub fn observation_addr(&self) -> SocketAddr {
        self.observation_addr
    }
    pub fn container_id(&self) -> &str {
        self.execution
            .container_id
            .as_deref()
            .expect("owned container identity")
    }
    pub fn execution_evidence(&self) -> &super::lease::RuntimeExecutionEvidence {
        &self.execution
    }
    pub async fn stop(mut self) -> NativeCompletion {
        self.stop.take();
        self.join().await
    }
    pub async fn wait(mut self) -> NativeCompletion {
        self.join().await
    }
    async fn join(&mut self) -> NativeCompletion {
        match self.task.take().expect("owned lifecycle task").await {
            Ok(outcome) => outcome,
            Err(_) => NativeCompletion {
                stop: StopOutcome::Unconfirmed {
                    reason: "native lifecycle task did not finish".into(),
                },
                cause: "lifecycle_task_failed",
            },
        }
    }
}
impl Drop for OwnedNativeTerminalRun {
    fn drop(&mut self) {
        self.stop.take();
    }
}

struct Parts {
    container: Option<OciHandle>,
    producer: Option<TerminalProducer>,
    probe: Option<NativeProbeHandle>,
    surface: Option<NativeSurfaceHandle>,
    budget: StopBudget,
    // Keep the Linux directory alias alive through every UDS connection.
    _socket_directory: std::fs::File,
}
impl Parts {
    async fn shutdown(mut self) -> StopOutcome {
        let mut failures = Vec::new();
        if let Some(surface) = self.surface.take()
            && !matches!(
                tokio::time::timeout(Duration::from_secs(3), surface.stop()).await,
                Ok(Ok(()))
            )
        {
            failures.push("surface cleanup");
        }
        if let Some(mut probe) = self.probe.take()
            && probe.stop().await.is_err()
        {
            failures.push("observation cleanup");
        }
        if let Some(mut producer) = self.producer.take()
            && producer.stop().await.is_err()
        {
            failures.push("terminal cleanup");
        }
        let outcome = self
            .container
            .take()
            .map(|container| container.stop_gracefully(self.budget))
            .unwrap_or(StopOutcome::AlreadyExited { exit_code: 0 });
        if !failures.is_empty() {
            return StopOutcome::Unconfirmed {
                reason: format!(
                    "native cleanup not confirmed: {}; container={outcome:?}",
                    failures.join(", ")
                ),
            };
        }
        outcome
    }
}

/// Executes a validated native D on this lease, without publishing a ready Run.
/// The caller still uses the existing private HTTP observer/Rust K verifier.
/// Production dispatch stays guarded until receipt-gated publication is wired.
#[allow(clippy::too_many_arguments)]
pub async fn start_terminal(
    spec: &RuntimeLaunchSpecV1,
    selected_derivation_ref: &str,
    descriptor: &SessionSurfaceDescriptor,
    context: &ResolvedRuntimeLaunchContext,
    owner: &OciOwner,
    config: NativeLeaseSurfaceConfig,
    mut owner_alive: watch::Receiver<bool>,
) -> std::result::Result<OwnedNativeTerminalRun, super::lease::StartFailure> {
    let started_at = Instant::now();
    let (
        declaration,
        cols,
        rows,
        runtime_root,
        gateway_config,
        keys,
        execution,
        authorizer,
        adapter,
    ) = (|| -> Result<_> {
        spec.validate().map_err(anyhow::Error::new)?;
        ensure!(*owner_alive.borrow(), "native owner already disconnected");
        ensure!(
            owner.run_id == spec.context.run_id,
            "native owner Run mismatch"
        );
        owner.labels(None)?;
        let LaunchRealizationV1::Oci(oci) = &spec.realization else {
            anyhow::bail!("native terminal requires OCI")
        };
        let declaration = NativeLaunchDeclaration::decode(
            oci.native_surface
                .as_ref()
                .context("missing native declaration")?,
        )?;
        declaration.validate_selection(selected_derivation_ref, descriptor)?;
        declaration.require_host_terminal_ports()?;
        ensure!(
            declaration.adapter.surface.kind() == "terminal",
            "native pixel lifecycle is unavailable"
        );
        let SessionSurfaceDescriptor::Terminal { capabilities, .. } = descriptor else {
            unreachable!()
        };
        let size = |key| match capabilities.get(key) {
            Some(SurfaceCapabilityValue::Integer(value)) => *value as u16,
            _ => unreachable!(),
        };
        let (cols, rows) = (size("cols"), size("rows"));
        ensure!(
            config.scope.surface_id.as_str() == descriptor.surface_id().unwrap_or_default(),
            "native surface owner mismatch"
        );
        ensure!(
            (Duration::from_secs(1)..=Duration::from_secs(120)).contains(&config.startup_timeout)
                && (Duration::from_secs(1)..=Duration::from_secs(86_400))
                    .contains(&config.idle_timeout)
                && (Duration::from_secs(1)..=Duration::from_secs(86_400))
                    .contains(&config.maximum_lifetime),
            "invalid bounded native lifetime"
        );
        ensure!(
            config.surface_listen.ip().is_loopback() && config.surface_listen.port() != 0,
            "native gateway requires private assigned listener"
        );
        ensure!(
            spec.endpoints.is_empty(),
            "native Terminal does not admit OCI network endpoints"
        );
        ensure!(
            spec.lifecycle.graceful_shutdown_ms <= 10_000
                && spec.lifecycle.force_kill_after_ms <= 15_000,
            "native cleanup exceeds supported budget"
        );
        let runtime_root = ato_runtime_attempt::launch::oci::lease_runtime_root(context)?;
        let socket = runtime_root.join("terminal.sock");
        let gateway_config = TerminalGatewayConfig {
            listen_addr: config.surface_listen,
            firecracker_vsock_uds: socket.clone(),
            guest_connect_timeout: config.startup_timeout,
            scope: config.scope.clone(),
            allowed_origins: BTreeSet::from([config.origin.clone()]),
        };
        gateway_config.validate()?;
        let keys = Arc::new(SurfaceAssertionKeyring::new(BTreeMap::from([(
            "ato.runner-surface@1".into(),
            config.verification_key,
        )]))?);
        let execution = SurfaceExecutionScope {
            run_id: owner.run_id.clone(),
            lease_id: owner.lease_id.clone(),
            runner_id: owner.runner_id.clone(),
        };
        let authorizer = Arc::new(
            HmacSurfaceAccessAuthorizer::new(Arc::clone(&keys))
                .with_execution_scope(execution.clone()),
        );
        // Strip the already validated physical declaration only for the existing
        // ordinary OCI conversion. The immutable selected D and K stay untouched.
        let mut physical = oci.clone();
        physical.native_surface = None;
        let oci_spec = container_oci_spec(spec, &physical, context, owner.labels(None)?)?;
        let deadline = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64
            + config
                .startup_timeout
                .min(config.maximum_lifetime)
                .min(Duration::from_secs(5))
                .as_millis() as u64;
        let adapter = DockerOciAdapter::new(oci_spec)?
            .with_deadline(deadline)
            .with_attached_terminal()?;
        Ok((
            declaration,
            cols,
            rows,
            runtime_root,
            gateway_config,
            keys,
            execution,
            authorizer,
            adapter,
        ))
    })()?;
    let budget = super::recovery::stop_budget(&spec.lifecycle);
    // Same safe long-path technique as the existing Runtime network bridge:
    // hold a directory fd instead of chdir or publishing a host path.
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&runtime_root).context("prepare native runtime root")?;
    std::fs::set_permissions(&runtime_root, std::fs::Permissions::from_mode(0o700))
        .context("protect native runtime root")?;
    let socket_directory =
        std::fs::File::open(&runtime_root).context("open owned native runtime root")?;
    let socket = std::path::PathBuf::from(format!(
        "/proc/self/fd/{}/terminal.sock",
        socket_directory.as_raw_fd()
    ));
    let mut gateway_config = gateway_config;
    gateway_config.firecracker_vsock_uds = socket.clone();
    let container = match adapter.spawn(context.workspace_root(), &runtime_root) {
        Ok(container) => container,
        Err(error) => {
            return Err(super::lease::StartFailure {
                stop: super::lease::settle_failed_oci_start(owner, budget, &error),
                error,
                process: None,
            });
        }
    };
    let mut parts = Parts {
        container: Some(container),
        producer: None,
        probe: None,
        surface: None,
        budget: super::recovery::stop_budget(&spec.lifecycle),
        _socket_directory: socket_directory,
    };
    let lifetime_deadline = started_at + config.maximum_lifetime;
    let startup = async {
        let terminal = parts
            .container
            .as_mut()
            .unwrap()
            .take_terminal()
            .context("container omitted owned PTY")?;
        parts.producer = Some(TerminalProducer::start(terminal, socket.clone(), cols, rows).await?);
        parts.probe = Some(
            capture_and_start_native_probe(
                "127.0.0.1:0".parse().unwrap(),
                NativeProbeTarget::Terminal(socket),
                &declaration.adapter,
            )
            .await?,
        );
        let assertion =
            keys.issue_readiness_assertion_for_execution(&config.scope, Some(execution))?;
        let connect_url = format!("http://{}/__ato/surface", config.surface_listen);
        parts.surface = Some(
            start_ready_native_surface(
                NativeSurfaceConfig::Terminal(gateway_config),
                authorizer,
                NativeSurfaceProbe {
                    connect_url: &connect_url,
                    origin: &config.origin,
                    assertion: assertion.as_str(),
                },
                config.startup_timeout,
                config.idle_timeout,
            )
            .await?,
        );
        ensure!(
            parts
                .container
                .as_ref()
                .unwrap()
                .exit_code_bounded(Duration::from_millis(500))?
                .is_none(),
            "native workload exited during startup"
        );
        ensure!(
            !parts.producer.as_ref().unwrap().failed(),
            "native producer exited during startup"
        );
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {
        biased;
        _ = owner_alive.changed() => Err(anyhow::anyhow!("native owner disconnected during startup")),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(
            started_at + config.startup_timeout.min(config.maximum_lifetime)
        )) => Err(anyhow::anyhow!("native startup timeout")),
        result = startup => result,
    };
    if let Err(error) = result {
        let stop = parts.shutdown().await;
        return Err(super::lease::StartFailure {
            error,
            stop,
            process: None,
        });
    }
    let surface_addr = parts.surface.as_ref().unwrap().local_addr();
    let observation_addr = parts.probe.as_ref().unwrap().local_addr();
    let execution =
        super::lease::RuntimeExecutionEvidence::for_container(parts.container.as_ref().unwrap());
    let (stop, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        let cause = loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break "requested_stop",
                _ = owner_alive.changed() => break "owner_eof",
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if Instant::now() >= lifetime_deadline { break "maximum_lifetime"; }
                    if parts.surface.as_ref().unwrap().idle_expired() { break "idle_timeout"; }
                    match parts.container.as_ref().unwrap().exit_code_bounded(Duration::from_millis(500)) {
                        Ok(Some(_)) => break "workload_exit",
                        Err(_) => break "workload_status_unavailable",
                        Ok(None) => {}
                    }
                    if parts.producer.as_ref().unwrap().failed() { break "producer_failure"; }
                }
            }
        };
        NativeCompletion {
            stop: parts.shutdown().await,
            cause,
        }
    });
    Ok(OwnedNativeTerminalRun {
        surface_addr,
        observation_addr,
        execution,
        stop: Some(stop),
        task: Some(task),
    })
}
