//! Runner-owned Docker/OCI execution.
//!
//! This is an Adapter, not a Capsule kind. The workload can describe an
//! immutable image and ordinary container arguments, but it never receives a
//! Docker socket, daemon credential, or arbitrary Docker CLI access.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

mod ownership;
mod stop;

pub use ownership::{
    LABEL_INCARNATION, LABEL_LEASE_ID, LABEL_MANAGED, LABEL_RUN_ID, LABEL_RUNNER_ID, LABEL_SERVICE,
    LABEL_SLOT_ID, OciOwner, OwnedResource, OwnedResourceScanner, OwnedResources, is_label_value,
};
pub use stop::{StopBudget, StopOutcome};

/// A launch that failed after Docker may already have created its container,
/// and whose removal could not be confirmed. The container — and the network
/// it is attached to — are left for recovery: nothing may treat them as gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnCleanupUnconfirmed {
    /// Why the launch failed.
    pub launch_error: String,
    /// The container id, or its name when Docker returned no id.
    pub container: String,
    /// Networks kept because the container may still be attached.
    pub networks: Vec<String>,
    /// Why its removal is not confirmed.
    pub reason: String,
}

impl std::fmt::Display for SpawnCleanupUnconfirmed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; the container {} it may have created is not confirmed removed ({}); kept \
             for recovery",
            self.launch_error, self.container, self.reason
        )
    }
}

impl std::error::Error for SpawnCleanupUnconfirmed {}

/// Remove a container a failed launch may have left, and confirm it is gone.
fn discard_failed_launch(docker: &Path, container: &str) -> std::result::Result<(), String> {
    let removed = stop::docker_output(
        docker,
        ["rm", "--force", container],
        stop::DOCKER_CALL_TIMEOUT,
    );
    let inspected = stop::docker_output(
        docker,
        ["container", "inspect", "--format", "{{.Id}}", container],
        stop::DOCKER_CALL_TIMEOUT,
    );
    match inspected {
        Ok(output)
            if !output.status.success()
                && String::from_utf8_lossy(&output.stderr)
                    .to_ascii_lowercase()
                    .contains("no such container:") =>
        {
            Ok(())
        }
        Ok(output) if output.status.success() => Err(format!(
            "it still exists after removal{}",
            match removed {
                Ok(removed) if !removed.status.success() =>
                    format!(": {}", bounded_stderr(&removed)),
                Err(error) => format!(": {error:#}"),
                _ => String::new(),
            }
        )),
        Ok(output) => Err(format!(
            "inspect after removal failed: {}",
            bounded_stderr(&output)
        )),
        Err(error) => Err(format!("{error:#}")),
    }
}

/// The launch error, or — when the container it may have created cannot be
/// confirmed removed — [`SpawnCleanupUnconfirmed`] carrying both.
fn failed_launch(docker: &Path, container: &str, error: anyhow::Error) -> anyhow::Error {
    match discard_failed_launch(docker, container) {
        Ok(()) => error,
        Err(reason) => anyhow::Error::new(SpawnCleanupUnconfirmed {
            launch_error: format!("{error:#}"),
            container: container.to_owned(),
            networks: Vec::new(),
            reason,
        }),
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciEndpoint {
    pub host_port: u16,
    pub guest_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciResourceLimits {
    pub memory_bytes: u64,
    pub cpu_limit_millis: u64,
    pub pids_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciMount {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciSpec {
    pub id: String,
    pub image: String,
    pub platform: String,
    pub entrypoint: Option<String>,
    pub argv: Vec<String>,
    pub working_dir: String,
    pub workspace_mount_path: String,
    pub environment: BTreeMap<String, String>,
    pub endpoints: Vec<OciEndpoint>,
    pub mounts: Vec<OciMount>,
    pub limits: OciResourceLimits,
    pub stop_timeout_seconds: u64,
    /// Non-secret ownership labels (`run.ato.dev/*`). Empty outside a Runner.
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciAdmission {
    pub docker_version: String,
    pub image: String,
    pub platform: String,
}

pub struct DockerOciAdapter {
    docker: PathBuf,
    spec: OciSpec,
    offline_image: Option<OfflineImage>,
}

struct OfflineImage {
    archive: Vec<u8>,
    config_reference: String,
}

impl DockerOciAdapter {
    pub fn new(spec: OciSpec) -> Result<Self> {
        validate_spec(&spec)?;
        ensure!(
            cfg!(target_os = "linux") || spec.endpoints.is_empty(),
            "OCI runtime admission failed: isolated HTTP endpoints currently require a native Linux host; Docker Desktop keeps the internal bridge inside its VM"
        );
        let docker = find_on_path("docker").context(
            "OCI runtime admission failed: Docker CLI is not installed or is not on PATH",
        )?;
        Ok(Self {
            docker,
            spec,
            offline_image: None,
        })
    }

    /// The caller has already verified the archive's registry manifest,
    /// config, layers, platform, and pinned image digest. Docker may load it,
    /// but must not resolve an absent image through the network.
    pub fn new_offline(spec: OciSpec, archive: Vec<u8>, config_reference: String) -> Result<Self> {
        ensure!(
            config_reference
                .strip_prefix("sha256:")
                .is_some_and(|digest| digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
            "offline OCI config reference is invalid"
        );
        let mut adapter = Self::new(spec)?;
        adapter.offline_image = Some(OfflineImage {
            archive,
            config_reference,
        });
        Ok(adapter)
    }

    /// Admit the route and acquire its immutable image. Pulling is explicit:
    /// callers can report that this materialization is network-dependent.
    pub fn admit(&self) -> Result<OciAdmission> {
        self.admit_image().map(|(admission, _)| admission)
    }

    fn admit_image(&self) -> Result<(OciAdmission, String)> {
        let version = run_checked(
            &self.docker,
            ["version", "--format", "{{.Server.Version}}"],
            "query Docker Engine version",
        )?;
        let image_for_run = if let Some(offline) = &self.offline_image {
            // A tagless `docker image load` does not necessarily install a
            // repository@digest alias. The archive's verified manifest and
            // config hashes are the only local image IDs we may launch.
            self.inspect_loaded_image(&offline.config_reference)?
        } else {
            if self.inspect_image().is_err() {
                run_checked(
                    &self.docker,
                    [
                        "pull",
                        "--platform",
                        self.spec.platform.as_str(),
                        self.spec.image.as_str(),
                    ],
                    "pull immutable OCI image",
                )?;
            }
            self.inspect_image()?;
            self.spec.image.clone()
        };
        Ok((
            OciAdmission {
                docker_version: version.trim().to_owned(),
                image: self.spec.image.clone(),
                platform: self.spec.platform.clone(),
            },
            image_for_run,
        ))
    }

    /// Launch in a new `--internal` network owned by the returned handle. The
    /// single-container route: nothing else ever joins that network.
    pub fn spawn(&self, workspace: &Path, runtime_root: &Path) -> Result<OciHandle> {
        let network =
            OciNetwork::create_with(&self.docker, &self.spec.id, &self.spec.labels, "ator")?;
        match self.spawn_in_network(workspace, runtime_root, &network, None) {
            Ok(mut handle) => {
                handle.network = Some(network);
                Ok(handle)
            }
            Err(error) => match error.downcast::<SpawnCleanupUnconfirmed>() {
                // The container may still be attached: keep its network too.
                Ok(mut unconfirmed) => {
                    unconfirmed.networks.push(network.name().to_owned());
                    network.forget();
                    Err(anyhow::Error::new(unconfirmed))
                }
                Err(error) => Err(error),
            },
        }
    }

    /// Launch into a network the caller owns, optionally under a network
    /// alias sibling containers resolve by DNS. The handle does NOT own the
    /// network: an [`OciServiceGroup`] removes it after its last service.
    pub fn spawn_in_network(
        &self,
        workspace: &Path,
        runtime_root: &Path,
        network: &OciNetwork,
        alias: Option<&str>,
    ) -> Result<OciHandle> {
        ensure!(workspace.is_dir(), "OCI workspace does not exist");
        if let Some(alias) = alias {
            ensure!(
                !alias.is_empty()
                    && alias.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'),
                "OCI network alias is not a DNS label"
            );
        }
        fs::create_dir_all(runtime_root).context("create OCI runtime directory")?;
        if let Some(offline) = &self.offline_image {
            let path = runtime_root.join("verified-oci-image.tar");
            fs::write(&path, &offline.archive).context("write verified OCI archive")?;
            run_checked(
                &self.docker,
                [
                    "image",
                    "load",
                    "--platform",
                    self.spec.platform.as_str(),
                    "--input",
                    path.to_str().context("OCI archive path is not UTF-8")?,
                ],
                "load verified offline OCI image",
            )?;
        }
        let (_, image_for_run) = self.admit_image()?;

        let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        // A service's alias is kept whole in its container name so an operator
        // can tell the containers of one group apart; the id is shortened to
        // leave room for it.
        let stem = match alias {
            Some(alias) => format!(
                "{}-{alias}",
                safe_name(&self.spec.id)
                    .chars()
                    .take(24)
                    .collect::<String>()
            ),
            None => safe_name(&self.spec.id),
        };
        let container_name = format!("ato-{stem}-{}-{suffix}", std::process::id());

        let env_file = runtime_root.join("environment.list");
        let environment = self
            .spec
            .environment
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect::<String>();
        fs::write(&env_file, environment).context("write OCI environment file")?;

        let argv = docker_run_arguments(
            &self.spec,
            workspace,
            &env_file,
            &container_name,
            network.name(),
            alias,
            &image_for_run,
        )?;
        // Persist the chosen name before Docker can create it. A worker crash
        // must not erase the identity needed to inspect an external workload.
        fs::write(
            runtime_root.join("ownership.json"),
            serde_json::to_vec(&serde_json::json!({
                "container": container_name, "network": network.name(),
            }))?,
        )
        .context("record OCI launch identity")?;
        let launched = Command::new(&self.docker).args(&argv).output();
        // Docker has consumed the file once `docker run` returns. It may hold
        // runtime Binding values, so it must not become part of a durable Run
        // directory or survive a failed launch.
        let removed_environment = fs::remove_file(&env_file);
        let launched = match launched {
            Ok(launched) => launched,
            Err(error) => {
                return Err(failed_launch(
                    &self.docker,
                    &container_name,
                    anyhow::Error::from(error).context("start OCI container"),
                ));
            }
        };
        // From here on Docker may have created the container. Every failure
        // removes it and confirms it is gone, or reports that it could not.
        let launched_id = String::from_utf8(launched.stdout.clone())
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| launched.status.success() && !id.is_empty());
        let created = launched_id
            .clone()
            .unwrap_or_else(|| container_name.clone());
        if let Err(error) = removed_environment {
            return Err(failed_launch(
                &self.docker,
                &created,
                anyhow::Error::new(error).context("remove OCI environment file after launch"),
            ));
        }
        if !launched.status.success() {
            // `docker run` may create the named container before runc rejects
            // its process. Remove by the Runner-owned name because no stdout
            // container ID is available on this failure path.
            return Err(failed_launch(
                &self.docker,
                &container_name,
                anyhow::anyhow!("start OCI container failed: {}", bounded_stderr(&launched)),
            ));
        }
        let Some(container_id) = launched_id else {
            return Err(failed_launch(
                &self.docker,
                &container_name,
                anyhow::anyhow!("Docker returned no container id"),
            ));
        };
        let container_address = match inspect_container_address(&self.docker, &container_id) {
            Ok(address) => address,
            Err(error) => return Err(failed_launch(&self.docker, &container_id, error)),
        };
        let mut forwarders = Vec::with_capacity(self.spec.endpoints.len());
        for endpoint in &self.spec.endpoints {
            match PortForwarder::start(endpoint.host_port, container_address, endpoint.guest_port) {
                Ok(forwarder) => forwarders.push(forwarder),
                Err(error) => {
                    drop(forwarders);
                    return Err(failed_launch(
                        &self.docker,
                        &container_id,
                        error.context("start OCI loopback Port forwarder"),
                    ));
                }
            }
        }
        Ok(OciHandle {
            docker: self.docker.clone(),
            container_id,
            container_address,
            container_name,
            network: None,
            image: self.spec.image.clone(),
            platform: self.spec.platform.clone(),
            endpoints: self.spec.endpoints.clone(),
            forwarders,
            outcome: None,
        })
    }

    fn inspect_image(&self) -> Result<()> {
        let output = Command::new(&self.docker)
            .args([
                "image",
                "inspect",
                "--format",
                "{{json .RepoDigests}}|{{.Os}}/{{.Architecture}}",
                &self.spec.image,
            ])
            .output()
            .context("inspect OCI image")?;
        ensure!(
            output.status.success(),
            "OCI image is not available: {}",
            bounded_stderr(&output)
        );
        let inspected =
            String::from_utf8(output.stdout).context("invalid Docker inspect output")?;
        validate_inspected_image(&self.spec, inspected.trim())
    }

    fn inspect_loaded_image(&self, config_reference: &str) -> Result<String> {
        // Docker's executable image ID is the verified config digest. Some
        // containerd-backed daemons also make the manifest digest inspectable,
        // but launching that alias from an otherwise empty namespace can
        // produce a rootfs-less container. Prefer config; retain the manifest
        // fallback for daemons that expose only the loaded manifest alias.
        for reference in offline_image_reference_candidates(&self.spec.image, config_reference)? {
            let output = Command::new(&self.docker)
                .args([
                    "image",
                    "inspect",
                    "--format",
                    "{{.Id}}|{{.Os}}/{{.Architecture}}",
                    reference,
                ])
                .output()
                .context("inspect loaded offline OCI image")?;
            if output.status.success() {
                let inspected = String::from_utf8(output.stdout)
                    .context("invalid Docker offline image inspect output")?;
                validate_loaded_image(&self.spec, reference, inspected.trim(), config_reference)?;
                return Ok(reference.to_owned());
            }
        }
        bail!("verified offline OCI image was not available after archive load")
    }
}

fn offline_image_reference_candidates<'a>(
    image: &'a str,
    config_reference: &'a str,
) -> Result<[&'a str; 2]> {
    let manifest_reference = image
        .rsplit_once('@')
        .map(|(_, reference)| reference)
        .context("validated OCI image omitted manifest digest")?;
    Ok([config_reference, manifest_reference])
}

/// Report whether this host can honestly accept an OCI HTTP workload now.
///
/// Finding a Docker client is insufficient: a stopped or unreachable daemon
/// would make the scheduler issue a lease that can only fail. The Connected
/// Runner uses this probe for its heartbeat capability advertisement; launch
/// admission still repeats the check and validates the pinned platform/image.
pub fn docker_runtime_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Some(docker) = find_on_path("docker") else {
        return false;
    };
    Command::new(docker)
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn validate_inspected_image(spec: &OciSpec, inspected: &str) -> Result<()> {
    let (digests, platform) = inspected
        .rsplit_once('|')
        .context("Docker inspect omitted image platform")?;
    let expected_digest = spec
        .image
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .context("validated image omitted digest")?;
    ensure!(
        digests.contains(expected_digest),
        "local OCI image does not carry declared digest {expected_digest}"
    );
    ensure!(
        platform == spec.platform,
        "OCI platform mismatch: route requires {}, image is {platform}",
        spec.platform
    );
    Ok(())
}

fn validate_loaded_image(
    spec: &OciSpec,
    reference: &str,
    inspected: &str,
    config_reference: &str,
) -> Result<()> {
    let manifest_reference = spec
        .image
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .context("validated OCI image omitted manifest digest")?;
    ensure!(
        reference == manifest_reference || reference == config_reference,
        "offline OCI image is not the verified manifest or config"
    );
    let (image_id, platform) = inspected
        .split_once('|')
        .context("Docker inspect omitted offline image platform")?;
    ensure!(
        image_id == reference,
        "loaded OCI image ID differs from the verified archive"
    );
    ensure!(
        platform == spec.platform,
        "OCI platform mismatch: route requires {}, image is {platform}",
        spec.platform
    );
    Ok(())
}

/// One `--internal` bridge for one Run. Removed on [`OciNetwork::remove`] or,
/// best effort, on drop — so a failed launch never leaks a network.
pub struct OciNetwork {
    docker: PathBuf,
    name: String,
    bridge_name: String,
    removed: bool,
}

impl OciNetwork {
    /// Create the network a group of services shares.
    pub fn create(label: &str, labels: &BTreeMap<String, String>) -> Result<Self> {
        let docker = find_on_path("docker").context(
            "OCI runtime admission failed: Docker CLI is not installed or is not on PATH",
        )?;
        Self::create_with(&docker, label, labels, "ator")
    }

    /// Create the dedicated bridge used only to reach the Runner-owned TCP
    /// egress broker. Its interface prefix is part of the firewall admission
    /// contract and is deliberately distinct from ordinary Run bridges.
    pub fn create_egress(label: &str, labels: &BTreeMap<String, String>) -> Result<Self> {
        let docker = find_on_path("docker").context(
            "OCI runtime admission failed: Docker CLI is not installed or is not on PATH",
        )?;
        Self::create_with(&docker, label, labels, "atoe")
    }

    fn create_with(
        docker: &Path,
        label: &str,
        labels: &BTreeMap<String, String>,
        bridge_prefix: &str,
    ) -> Result<Self> {
        let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "ato-{}-{}-{suffix}-net",
            safe_name(label),
            std::process::id()
        );
        let bridge_name = bridge_name(bridge_prefix, suffix);
        let mut arguments = vec![
            "network".to_owned(),
            "create".to_owned(),
            "--driver".to_owned(),
            "bridge".to_owned(),
            "--internal".to_owned(),
            "--opt".to_owned(),
            format!("com.docker.network.bridge.name={bridge_name}"),
        ];
        for (key, value) in labels {
            ensure!(
                key.starts_with("run.ato.dev/") && is_label_value(value),
                "OCI network ownership label is invalid"
            );
            arguments.extend(["--label".to_owned(), format!("{key}={value}")]);
        }
        arguments.push(name.clone());
        run_checked(
            docker,
            arguments.iter().map(String::as_str),
            "create isolated OCI network",
        )?;
        Ok(Self {
            docker: docker.to_path_buf(),
            name,
            bridge_name,
            removed: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn bridge_name(&self) -> &str {
        &self.bridge_name
    }

    /// Address of the host-side bridge endpoint. An internal network has no
    /// external forwarding, but containers may reach a Runner-owned broker on
    /// this exact address.
    pub fn gateway_address(&self) -> Result<IpAddr> {
        let output = run_checked(
            &self.docker,
            [
                "network",
                "inspect",
                "--format",
                "{{(index .IPAM.Config 0).Gateway}}",
                self.name.as_str(),
            ],
            "inspect isolated OCI network gateway",
        )?;
        output
            .trim()
            .parse()
            .context("isolated OCI network gateway is not an IP address")
    }

    /// Attach one already-running owned container. The network is internal,
    /// so this adds access only to host-side listeners on its bridge.
    pub fn connect_container(&self, container_id: &str) -> Result<()> {
        ensure!(!container_id.trim().is_empty(), "container id is empty");
        run_checked(
            &self.docker,
            ["network", "connect", self.name.as_str(), container_id],
            "attach OCI container to isolated broker network",
        )?;
        Ok(())
    }

    pub fn remove(mut self) -> Result<()> {
        self.removed = true;
        remove_network(&self.docker, &self.name)
    }

    /// Give up ownership without removing: the network still has a container
    /// whose stop was not confirmed, and recovery finds it by its labels.
    fn forget(mut self) {
        self.removed = true;
    }
}

impl Drop for OciNetwork {
    fn drop(&mut self) {
        if !self.removed {
            let _ = remove_network(&self.docker, &self.name);
        }
    }
}

/// The containers of one OCI service group and the network they share.
///
/// Services are held in start order and always stopped in reverse, then the
/// network is removed. Dropping the group does the same, best effort, so a
/// group that fails half-way through its start leaves nothing behind.
pub struct OciServiceGroup {
    network: Option<OciNetwork>,
    auxiliary_networks: Vec<OciNetwork>,
    services: Vec<(String, OciHandle)>,
    retained_resources: Vec<Box<dyn Send>>,
}

impl OciServiceGroup {
    pub fn new(network: OciNetwork) -> Self {
        Self {
            network: Some(network),
            auxiliary_networks: Vec::new(),
            services: Vec::new(),
            retained_resources: Vec::new(),
        }
    }

    pub fn network(&self) -> &OciNetwork {
        self.network
            .as_ref()
            .expect("a live service group always owns its network")
    }

    pub fn push(&mut self, name: String, handle: OciHandle) {
        self.services.push((name, handle));
    }

    /// Keep an additional internal bridge owned by this group. It is removed
    /// only after every service is confirmed stopped.
    pub fn push_auxiliary_network(&mut self, network: OciNetwork) {
        self.auxiliary_networks.push(network);
    }

    /// Keep a Runner-owned broker or similar guard alive for exactly this
    /// group's lifetime. Resources are dropped after services stop.
    pub fn retain_resource(&mut self, resource: Box<dyn Send>) {
        self.retained_resources.push(resource);
    }

    /// Every network this group owns: its own, then the auxiliary ones.
    pub fn network_names(&self) -> Vec<String> {
        self.network
            .iter()
            .chain(&self.auxiliary_networks)
            .map(|network| network.name().to_owned())
            .collect()
    }

    /// Preserve network identities when a failed Docker run may have attached
    /// an endpoint for which no live handle was returned.
    pub fn retain_networks_for_recovery(&mut self) {
        if let Some(network) = self.network.take() {
            network.forget();
        }
        for network in self.auxiliary_networks.drain(..) {
            network.forget();
        }
    }

    pub fn services(&self) -> impl Iterator<Item = (&str, &OciHandle)> {
        self.services
            .iter()
            .map(|(name, handle)| (name.as_str(), handle))
    }

    /// The first service, in start order, that is no longer running.
    pub fn exited_service(&self) -> Result<Option<(&str, i32)>> {
        for (name, handle) in &self.services {
            if let Some(code) = handle.exit_code()? {
                return Ok(Some((name.as_str(), code)));
            }
        }
        Ok(None)
    }

    /// Stop every service in reverse start order with a grace period each,
    /// then remove the network — but only if every service is confirmed
    /// stopped. Every service is attempted even after a failure.
    pub fn stop_gracefully(mut self, budget: StopBudget) -> GroupStopReport {
        self.stop_all(budget)
    }

    /// `stop_gracefully` for callers that only need success or failure.
    pub fn stop(self) -> Result<()> {
        let report = self.stop_gracefully(StopBudget::DEFAULT);
        match report.overall() {
            Some(StopOutcome::Unconfirmed { reason }) => {
                bail!("OCI service group stop is unconfirmed: {reason}")
            }
            _ => Ok(()),
        }
    }

    fn stop_all(&mut self, budget: StopBudget) -> GroupStopReport {
        let mut services = Vec::new();
        while let Some((name, handle)) = self.services.pop() {
            services.push((name, handle.stop_gracefully(budget)));
        }
        self.retained_resources.clear();
        let all_confirmed = services.iter().all(|(_, outcome)| outcome.is_confirmed());
        let network_removed = match self.network.take() {
            // A network with a live endpoint cannot be removed, and removing
            // it is not how a Run ends; leave it for recovery.
            Some(network) if all_confirmed => network.remove().is_ok(),
            Some(network) => {
                network.forget();
                false
            }
            None => true,
        };
        let auxiliary_removed = if all_confirmed {
            self.auxiliary_networks
                .drain(..)
                .all(|network| network.remove().is_ok())
        } else {
            for network in self.auxiliary_networks.drain(..) {
                network.forget();
            }
            false
        };
        GroupStopReport {
            services,
            network_removed: network_removed && auxiliary_removed,
        }
    }
}

/// How each service of a group stopped, in stop (reverse start) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupStopReport {
    pub services: Vec<(String, StopOutcome)>,
    pub network_removed: bool,
}

impl GroupStopReport {
    /// The least favourable service outcome; `None` for an empty group.
    pub fn overall(&self) -> Option<StopOutcome> {
        StopOutcome::worst(self.services.iter().map(|(_, outcome)| outcome))
    }
}

impl Drop for OciServiceGroup {
    fn drop(&mut self) {
        if !self.services.is_empty() || self.network.is_some() {
            let _ = self.stop_all(StopBudget::DEFAULT);
        }
    }
}

pub struct OciHandle {
    docker: PathBuf,
    container_id: String,
    container_address: IpAddr,
    container_name: String,
    /// Owned only by a single-container route; a group owns its network.
    network: Option<OciNetwork>,
    image: String,
    platform: String,
    endpoints: Vec<OciEndpoint>,
    forwarders: Vec<PortForwarder>,
    /// Set once a stop has been attempted, so drop never stops twice.
    outcome: Option<StopOutcome>,
}

impl OciHandle {
    pub fn network_name(&self) -> Option<&str> {
        self.network.as_ref().map(OciNetwork::name)
    }

    pub fn container_id(&self) -> &str {
        &self.container_id
    }

    /// The bridge address and exact Port mapping used by this Run's forwarder.
    /// Diagnostic only: neither value participates in Derivation identity.
    pub fn port_mapping(&self) -> (IpAddr, &[OciEndpoint]) {
        (self.container_address, &self.endpoints)
    }

    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// The container's address on its Run network. Used to probe internal
    /// Endpoints from the Runner, which is the trusted execution substrate.
    pub fn container_address(&self) -> IpAddr {
        self.container_address
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn exit_code(&self) -> Result<Option<i32>> {
        let output = Command::new(&self.docker)
            .args([
                "inspect",
                "--format",
                "{{.State.Running}}|{{.State.ExitCode}}",
                &self.container_id,
            ])
            .output()
            .context("inspect OCI container state")?;
        ensure!(
            output.status.success(),
            "inspect OCI container state failed: {}",
            bounded_stderr(&output)
        );
        let value = String::from_utf8(output.stdout).context("invalid container state output")?;
        let (running, code) = value
            .trim()
            .split_once('|')
            .context("Docker inspect omitted container state")?;
        if running == "true" {
            return Ok(None);
        }
        Ok(Some(code.parse().context("invalid OCI exit code")?))
    }

    /// Stop signal, grace, SIGKILL, confirm — then remove only a confirmed
    /// stop. The forwarder goes first so no new request reaches a workload
    /// that is shutting down.
    pub fn stop_gracefully(mut self, budget: StopBudget) -> StopOutcome {
        self.shutdown(budget)
    }

    /// `stop_gracefully` for callers that only need success or failure.
    pub fn stop(self) -> Result<()> {
        match self.stop_gracefully(StopBudget::DEFAULT) {
            StopOutcome::Unconfirmed { reason } => {
                bail!("OCI container stop is unconfirmed: {reason}")
            }
            _ => Ok(()),
        }
    }

    fn shutdown(&mut self, budget: StopBudget) -> StopOutcome {
        if let Some(outcome) = self.outcome.clone() {
            return outcome;
        }
        self.forwarders.clear();
        let outcome = stop::stop_container(&self.docker, &self.container_id, budget);
        if outcome.is_confirmed() {
            let _ = stop::remove_stopped_container(&self.docker, &self.container_id);
            // Docker refuses to remove a network with an endpoint; a failure
            // here leaves a labelled network for recovery, not a live writer.
            if let Some(network) = self.network.take() {
                let _ = network.remove();
            }
        } else if let Some(network) = self.network.take() {
            network.forget();
        }
        self.outcome = Some(outcome.clone());
        outcome
    }
}

struct PortForwarder {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PortForwarder {
    fn start(host_port: u16, container_address: IpAddr, guest_port: u16) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, host_port))
            .with_context(|| format!("bind loopback Port {host_port}"))?;
        listener
            .set_nonblocking(true)
            .context("make OCI Port forwarder non-blocking")?;
        let target = SocketAddr::new(container_address, guest_port);
        let (stop, stopped) = mpsc::channel();
        let thread = thread::Builder::new()
            .name(format!("ato-oci-port-{host_port}"))
            .spawn(move || run_port_forwarder(listener, target, stopped))
            .context("spawn OCI Port forwarder")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_port_forwarder(listener: TcpListener, target: SocketAddr, stopped: Receiver<()>) {
    loop {
        match stopped.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        match listener.accept() {
            Ok((client, _)) => {
                let _ = thread::Builder::new()
                    .name("ato-oci-port-connection".to_owned())
                    .spawn(move || forward_connection(client, target));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}

fn forward_connection(client: TcpStream, target: SocketAddr) {
    let Ok(upstream) = TcpStream::connect_timeout(&target, Duration::from_secs(2)) else {
        return;
    };
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let upload = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let mut upstream_read = upstream;
    let mut client_write = client;
    let _ = io::copy(&mut upstream_read, &mut client_write);
    // An HTTP server may close an idle keep-alive connection while the proxy
    // keeps its read half open. Forward that EOF immediately: waiting for the
    // upload thread first leaves an apparently live socket in the proxy pool,
    // where the next request hangs until the public ingress times out.
    let _ = client_write.shutdown(Shutdown::Write);
    let _ = client_write.shutdown(Shutdown::Read);
    let _ = upload.join();
}

fn inspect_container_address(docker: &Path, container_id: &str) -> Result<IpAddr> {
    let value = run_checked(
        docker,
        [
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            container_id,
        ],
        "inspect OCI container network address",
    )?;
    value
        .trim()
        .parse()
        .context("Docker returned an invalid container network address")
}

impl Drop for OciHandle {
    fn drop(&mut self) {
        let _ = self.shutdown(StopBudget::DEFAULT);
    }
}

fn validate_spec(spec: &OciSpec) -> Result<()> {
    ensure!(!spec.id.trim().is_empty(), "OCI id is empty");
    let (repository, digest) = spec
        .image
        .rsplit_once("@sha256:")
        .context("OCI image must be repository@sha256:<digest>")?;
    ensure!(!repository.is_empty(), "OCI image repository is empty");
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "OCI image digest is invalid"
    );
    ensure!(
        matches!(spec.platform.as_str(), "linux/amd64" | "linux/arm64"),
        "OCI platform is unsupported"
    );
    ensure!(
        !spec.argv.iter().any(|value| value.contains('\0')),
        "OCI argv contains NUL"
    );
    ensure!(
        spec.entrypoint.as_ref().is_none_or(|entrypoint| {
            entrypoint.starts_with('/')
                && !entrypoint.contains(['\0', '\\', ','])
                && entrypoint
                    .split('/')
                    .skip(1)
                    .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        }),
        "OCI entrypoint is invalid"
    );
    ensure!(
        spec.working_dir == "/app",
        "OCI working directory must be /app"
    );
    ensure!(
        valid_guest_path(&spec.workspace_mount_path),
        "OCI workspace mount target is invalid"
    );
    ensure!(
        spec.limits.memory_bytes > 0
            && spec.limits.cpu_limit_millis > 0
            && spec.limits.pids_limit > 0,
        "OCI resource limits must be positive"
    );
    for (key, value) in &spec.labels {
        ensure!(
            key.starts_with("run.ato.dev/") && is_label_value(value),
            "OCI ownership label is invalid"
        );
    }
    for (name, value) in &spec.environment {
        ensure!(
            !name.is_empty() && !name.contains('=') && !value.contains(['\0', '\n', '\r']),
            "OCI environment is invalid"
        );
    }
    let mut host_ports = BTreeSet::new();
    for endpoint in &spec.endpoints {
        ensure!(
            endpoint.host_port > 0 && endpoint.guest_port > 0,
            "OCI endpoint Port must be non-zero"
        );
        ensure!(
            host_ports.insert(endpoint.host_port),
            "OCI host Port is declared more than once"
        );
    }
    let mut guest_mounts = BTreeSet::new();
    for mount in &spec.mounts {
        ensure!(
            mount.host_path.is_dir(),
            "OCI mount source is not a directory"
        );
        ensure!(
            valid_guest_path(&mount.guest_path)
                && mount.guest_path != "/app"
                && mount.guest_path != spec.workspace_mount_path,
            "OCI mount target is invalid"
        );
        ensure!(
            guest_mounts.insert(mount.guest_path.as_str()),
            "OCI mount target is declared more than once"
        );
    }
    Ok(())
}

fn valid_guest_path(target: &str) -> bool {
    target.starts_with('/')
        && target != "/"
        && !target.contains(['\0', '\\', ','])
        && target
            .split('/')
            .skip(1)
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn docker_run_arguments(
    spec: &OciSpec,
    workspace: &Path,
    env_file: &Path,
    container_name: &str,
    network_name: &str,
    alias: Option<&str>,
    image_reference: &str,
) -> Result<Vec<String>> {
    let workspace = workspace
        .canonicalize()
        .context("canonicalize OCI workspace")?;
    let env_file = env_file
        .canonicalize()
        .context("canonicalize OCI environment file")?;
    let writable_mount_user = writable_mount_user(spec)?;
    let mut argv = vec![
        "run".to_owned(),
        "--detach".to_owned(),
        "--pull=never".to_owned(),
        "--name".to_owned(),
        container_name.to_owned(),
        "--platform".to_owned(),
        spec.platform.clone(),
        "--network".to_owned(),
        network_name.to_owned(),
        "--read-only".to_owned(),
        "--cap-drop=ALL".to_owned(),
        "--security-opt=no-new-privileges".to_owned(),
        "--pids-limit".to_owned(),
        spec.limits.pids_limit.to_string(),
        "--memory".to_owned(),
        spec.limits.memory_bytes.to_string(),
        "--cpus".to_owned(),
        format!("{:.3}", spec.limits.cpu_limit_millis as f64 / 1000.0),
        "--stop-timeout".to_owned(),
        spec.stop_timeout_seconds.to_string(),
        "--tmpfs".to_owned(),
        "/tmp:rw,noexec,nosuid,size=67108864".to_owned(),
        "--mount".to_owned(),
        format!(
            "type=bind,src={},dst={},readonly",
            workspace.display(),
            spec.workspace_mount_path
        ),
        "--workdir".to_owned(),
        spec.working_dir.clone(),
        "--env-file".to_owned(),
        env_file.display().to_string(),
        "--label".to_owned(),
        format!("run.ato.dev/id={}", spec.id),
    ];
    for (key, value) in &spec.labels {
        argv.extend(["--label".to_owned(), format!("{key}={value}")]);
    }
    if let Some(alias) = alias {
        argv.extend(["--network-alias".to_owned(), alias.to_owned()]);
    }
    if let Some(user) = writable_mount_user {
        argv.extend(["--user".to_owned(), user]);
    }
    if let Some(entrypoint) = &spec.entrypoint {
        argv.extend(["--entrypoint".to_owned(), entrypoint.clone()]);
    }
    for mount in &spec.mounts {
        let source = mount
            .host_path
            .canonicalize()
            .context("canonicalize OCI mount source")?;
        let source = source
            .to_str()
            .context("OCI mount source is not valid UTF-8")?;
        ensure!(
            !source.contains([',', '\0']),
            "OCI mount source cannot be represented safely"
        );
        argv.extend([
            "--mount".to_owned(),
            format!(
                "type=bind,src={source},dst={}{}",
                mount.guest_path,
                if mount.writable { "" } else { ",readonly" }
            ),
        ]);
    }
    argv.push(image_reference.to_owned());
    argv.extend(spec.argv.iter().cloned());
    Ok(argv)
}

#[cfg(unix)]
fn writable_mount_user(spec: &OciSpec) -> Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;

    let mut owners = BTreeSet::new();
    for mount in spec.mounts.iter().filter(|mount| mount.writable) {
        let metadata = fs::metadata(&mount.host_path)
            .with_context(|| format!("inspect OCI mount source {}", mount.host_path.display()))?;
        owners.insert((metadata.uid(), metadata.gid()));
    }
    ensure!(
        owners.len() <= 1,
        "writable OCI mount sources must have one host owner"
    );
    Ok(owners
        .into_iter()
        .next()
        .map(|(uid, gid)| format!("{uid}:{gid}")))
}

#[cfg(not(unix))]
fn writable_mount_user(spec: &OciSpec) -> Result<Option<String>> {
    ensure!(
        !spec.mounts.iter().any(|mount| mount.writable),
        "writable OCI mounts require a Unix host"
    );
    Ok(None)
}

pub(crate) fn remove_network(docker: &Path, network: &str) -> Result<()> {
    let output = stop::docker_output(
        docker,
        ["network", "rm", network],
        stop::DOCKER_CALL_TIMEOUT,
    )
    .context("remove isolated OCI network")?;
    ensure!(
        output.status.success(),
        "remove isolated OCI network failed: {}",
        bounded_stderr(&output)
    );
    Ok(())
}

fn run_checked<'a>(
    executable: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
    operation: &str,
) -> Result<String> {
    let output = Command::new(executable)
        .args(arguments)
        .output()
        .with_context(|| operation.to_owned())?;
    ensure!(
        output.status.success(),
        "{operation} failed: {}",
        bounded_stderr(&output)
    );
    String::from_utf8(output.stdout).with_context(|| format!("{operation} returned non-UTF-8"))
}

fn bounded_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .chars()
        .take(2048)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn safe_name(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(40)
        .collect::<String>();
    if value.is_empty() {
        "run".to_owned()
    } else {
        value
    }
}

fn bridge_name(prefix: &str, suffix: u64) -> String {
    // Linux interface names are limited to 15 bytes. Five hex digits for the
    // PID and sequence keep concurrently created bridges distinct while
    // retaining the firewall-significant prefix.
    format!(
        "{prefix}{:05x}{:05x}",
        std::process::id() & 0x0f_ffff,
        suffix & 0x0f_ffff
    )
}

pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|search| {
        std::env::split_paths(&search)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn spec() -> OciSpec {
        OciSpec {
            id: "run_1".to_owned(),
            image: format!("docker.io/example/app@sha256:{}", "a".repeat(64)),
            platform: "linux/amd64".to_owned(),
            entrypoint: None,
            argv: vec!["--serve".to_owned()],
            working_dir: "/app".to_owned(),
            workspace_mount_path: "/app".to_owned(),
            environment: BTreeMap::new(),
            endpoints: vec![OciEndpoint {
                host_port: 49152,
                guest_port: 8000,
            }],
            mounts: vec![],
            limits: OciResourceLimits {
                memory_bytes: 256 * 1024 * 1024,
                cpu_limit_millis: 1000,
                pids_limit: 128,
            },
            stop_timeout_seconds: 5,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn egress_bridge_names_fit_linux_and_keep_the_firewall_prefix() {
        let first = bridge_name("atoe", 1);
        let second = bridge_name("atoe", 2);
        assert!(first.starts_with("atoe"));
        assert!(first.len() <= 15);
        assert_ne!(first, second);
    }

    #[test]
    fn mutable_image_is_refused() {
        let mut invalid = spec();
        invalid.image = "docker.io/example/app:latest".to_owned();
        assert!(validate_spec(&invalid).is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn vm_backed_docker_cannot_admit_an_isolated_http_endpoint() {
        let error = DockerOciAdapter::new(spec()).err().unwrap();
        assert!(error.to_string().contains("native Linux host"));
    }

    #[test]
    fn inspected_image_must_match_both_digest_and_platform() {
        let valid = spec();
        let digest = valid.image.rsplit_once('@').unwrap().1;
        validate_inspected_image(
            &valid,
            &format!("[\"docker.io/example/app@{digest}\"]|linux/amd64"),
        )
        .unwrap();
        assert!(
            validate_inspected_image(
                &valid,
                &format!("[\"docker.io/example/app@{digest}\"]|linux/arm64"),
            )
            .unwrap_err()
            .to_string()
            .contains("platform mismatch")
        );
        assert!(
            validate_inspected_image(
                &valid,
                &format!(
                    "[\"docker.io/example/app@sha256:{}\"]|linux/amd64",
                    "cd".repeat(32)
                ),
            )
            .unwrap_err()
            .to_string()
            .contains("does not carry declared digest")
        );
    }

    #[test]
    fn offline_image_can_use_only_the_verified_local_manifest_or_config() {
        let valid = spec();
        let manifest = valid.image.rsplit_once('@').unwrap().1;
        let config = format!("sha256:{}", "b".repeat(64));
        assert!(
            validate_loaded_image(
                &valid,
                manifest,
                &format!("{manifest}|linux/amd64"),
                &config
            )
            .is_ok()
        );
        assert!(
            validate_loaded_image(&valid, &config, &format!("{config}|linux/amd64"), &config)
                .is_ok()
        );
        assert!(
            validate_loaded_image(&valid, manifest, &format!("{config}|linux/amd64"), &config)
                .is_err()
        );
        assert!(
            validate_loaded_image(
                &valid,
                manifest,
                &format!("{manifest}|linux/arm64"),
                &config
            )
            .is_err()
        );
        let unrelated = format!("sha256:{}", "c".repeat(64));
        assert!(
            validate_loaded_image(
                &valid,
                &unrelated,
                &format!("{unrelated}|linux/amd64"),
                &config
            )
            .is_err()
        );
    }

    #[test]
    fn offline_image_launch_prefers_the_verified_config_id() {
        let config = format!("sha256:{}", "b".repeat(64));
        let manifest = format!("sha256:{}", "a".repeat(64));
        let image = format!("docker.io/example/app@{manifest}");

        assert_eq!(
            offline_image_reference_candidates(&image, &config).unwrap(),
            [config.as_str(), manifest.as_str()]
        );
    }

    #[test]
    fn run_arguments_enforce_isolation_and_limits() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let args = docker_run_arguments(
            &spec(),
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-test-net",
            None,
            "sha256:verified-local-id",
        )
        .unwrap();
        let rendered = args.join(" ");
        for required in [
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit 128",
            "--memory 268435456",
            "--network ato-test-net",
            "--pull=never",
            "dst=/app,readonly",
        ] {
            assert!(
                rendered.contains(required),
                "missing {required}: {rendered}"
            );
        }
        assert!(!rendered.contains("docker.sock"));
        assert!(!rendered.contains("--privileged"));
        assert!(!rendered.contains("--publish"));
        assert_eq!(args[args.len() - 2], "sha256:verified-local-id");
    }

    #[test]
    fn a_service_joins_the_shared_network_under_its_alias_with_the_same_hardening() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let alone = docker_run_arguments(
            &spec(),
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-group-net",
            None,
            "sha256:verified-local-id",
        )
        .unwrap();
        let aliased = docker_run_arguments(
            &spec(),
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-group-net",
            Some("backend"),
            "sha256:verified-local-id",
        )
        .unwrap();
        let rendered = aliased.join(" ");
        assert!(rendered.contains("--network ato-group-net --read-only"));
        assert!(rendered.contains("--network-alias backend"));
        assert!(!alone.join(" ").contains("--network-alias"));
        // The alias adds exactly two arguments and removes nothing.
        assert_eq!(aliased.len(), alone.len() + 2);
        assert!(!rendered.contains("--publish"));
    }

    #[test]
    fn run_arguments_mount_workspace_at_the_declared_guest_path() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let mut declared = spec();
        declared.workspace_mount_path = "/ato/workspace".to_owned();

        let args = docker_run_arguments(
            &declared,
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-test-net",
            None,
            "sha256:verified-local-id",
        )
        .unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains("dst=/ato/workspace,readonly"));
        assert!(!rendered.contains("dst=/app,readonly"));
    }

    #[test]
    fn run_arguments_apply_a_declared_guest_entrypoint() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let mut declared = spec();
        declared.entrypoint = Some("/usr/local/bin/app".to_owned());
        let args = docker_run_arguments(
            &declared,
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-test-net",
            None,
            "sha256:verified-local-id",
        )
        .unwrap();
        let entrypoint = args
            .iter()
            .position(|argument| argument == "--entrypoint")
            .unwrap();
        assert_eq!(args[entrypoint + 1], "/usr/local/bin/app");
        assert!(entrypoint < args.len() - 2);
    }

    #[cfg(unix)]
    #[test]
    fn run_arguments_mount_declared_state_with_requested_access() {
        use std::os::unix::fs::MetadataExt;

        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let writable = tempfile::tempdir().unwrap();
        let read_only = tempfile::tempdir().unwrap();
        let env_file = runtime.path().join("environment.list");
        fs::write(&env_file, "").unwrap();
        let mut value = spec();
        value.mounts = vec![
            OciMount {
                host_path: writable.path().to_path_buf(),
                guest_path: "/data".to_owned(),
                writable: true,
            },
            OciMount {
                host_path: read_only.path().to_path_buf(),
                guest_path: "/seed".to_owned(),
                writable: false,
            },
        ];
        let args = docker_run_arguments(
            &value,
            workspace.path(),
            &env_file,
            "ato-test",
            "ato-test-net",
            None,
            "sha256:verified-local-id",
        )
        .unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains("dst=/data"));
        assert!(!rendered.contains("dst=/data,readonly"));
        assert!(rendered.contains("dst=/seed,readonly"));
        let user = args
            .iter()
            .position(|value| value == "--user")
            .map(|index| args[index + 1].as_str());
        let metadata = fs::metadata(writable.path()).unwrap();
        let expected_user = format!("{}:{}", metadata.uid(), metadata.gid());
        assert_eq!(user, Some(expected_user.as_str()));
    }

    #[test]
    fn upstream_close_reaches_a_keepalive_client_without_waiting_for_its_close() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let target = upstream.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").unwrap();
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy = thread::spawn(move || {
            let (client, _) = listener.accept().unwrap();
            forward_connection(client, target);
        });

        let mut client = TcpStream::connect(proxy_address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(b"ping").unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(response, b"pong");
        server.join().unwrap();
        proxy.join().unwrap();
    }

    #[test]
    fn endpoint_ports_must_be_non_zero() {
        let mut invalid_host = spec();
        invalid_host.endpoints[0].host_port = 0;
        assert!(
            validate_spec(&invalid_host)
                .unwrap_err()
                .to_string()
                .contains("non-zero")
        );

        let mut invalid_guest = spec();
        invalid_guest.endpoints[0].guest_port = 0;
        assert!(
            validate_spec(&invalid_guest)
                .unwrap_err()
                .to_string()
                .contains("non-zero")
        );
    }

    #[test]
    fn host_port_may_be_published_only_once() {
        let mut invalid = spec();
        invalid.endpoints.push(OciEndpoint {
            host_port: invalid.endpoints[0].host_port,
            guest_port: 8001,
        });
        assert!(
            validate_spec(&invalid)
                .unwrap_err()
                .to_string()
                .contains("more than once")
        );
    }

    #[cfg(unix)]
    fn fake_docker(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, fake)
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_launch_whose_container_is_gone_reports_only_the_launch_error() {
        let (_dir, docker) = fake_docker(
            "#!/bin/sh\ncase \"$1\" in container) echo 'Error: No such container: c1' >&2; exit 1;; *) exit 0;; esac\n",
        );
        let error = failed_launch(&docker, "c1", anyhow::anyhow!("forwarder failed"));
        assert!(error.downcast_ref::<SpawnCleanupUnconfirmed>().is_none());
        assert_eq!(format!("{error:#}"), "forwarder failed");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_launch_whose_container_survives_removal_is_kept_for_recovery() {
        // `rm` is refused and the container is still there afterwards.
        let (_dir, docker) = fake_docker(
            "#!/bin/sh\ncase \"$1\" in rm) echo 'daemon refused' >&2; exit 1;; container) echo abc; exit 0;; *) exit 0;; esac\n",
        );
        let error = failed_launch(&docker, "c1", anyhow::anyhow!("forwarder failed"));
        let unconfirmed = error
            .downcast_ref::<SpawnCleanupUnconfirmed>()
            .expect("the leftover is reported, not dropped");
        assert_eq!(unconfirmed.container, "c1");
        assert_eq!(unconfirmed.launch_error, "forwarder failed");
        assert!(
            unconfirmed.reason.contains("still exists"),
            "{unconfirmed:?}"
        );
        assert!(
            unconfirmed.reason.contains("daemon refused"),
            "{unconfirmed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_daemon_that_cannot_answer_after_a_failed_launch_is_not_a_removal() {
        let (_dir, docker) = fake_docker(
            "#!/bin/sh\ncase \"$1\" in container) echo 'Cannot connect to the Docker daemon' >&2; exit 1;; *) exit 0;; esac\n",
        );
        let error = failed_launch(&docker, "c1", anyhow::anyhow!("inspect failed"));
        assert!(error.downcast_ref::<SpawnCleanupUnconfirmed>().is_some());
    }
}
