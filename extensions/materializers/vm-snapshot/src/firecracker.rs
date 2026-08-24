//! Low-level Firecracker process and snapshot-load backend.
//!
//! The process/API/socket cleanup mechanics are adapted from the historical
//! Ready-State backend; legacy Store identity and launch APIs are not reused.

use std::fs;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use ato_materializer_api::RunnerCapabilities;
#[cfg(target_os = "linux")]
use tempfile::TempDir;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use crate::ArtifactRole;
use crate::{
    CaptureGuard, CaptureProvenance, CapturedArtifact, CapturedVm, CpuContract, DeviceContract,
    HostBackendContract, MemoryContract, NetworkContract, VmBackendSession, VmCaptureRequest,
    VmRestoreRequest, VmSnapshotBackend, VmSnapshotError, VsockContract,
};

pub trait ActiveVmCaptureSource: Send + Sync {
    fn capture_active(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError>;
}

/// Lease returned by the Record Writer while submissions remain paused at a
/// sealed causal cut. Dropping the lease resumes normal asynchronous writes.
pub trait FirecrackerRecordCaptureLease: Send {
    fn frontier_ref(&self) -> &ato_computation::ContentRef;
}

pub trait FirecrackerRecordCaptureBarrier: Send + Sync {
    fn pause_and_seal(&self) -> Result<Box<dyn FirecrackerRecordCaptureLease>, VmSnapshotError>;
}

/// Capture facts owned by the active Firecracker runtime. They describe the
/// physical backend and never participate in ComputationRef identity.
#[derive(Debug, Clone)]
pub struct ActiveFirecrackerCaptureSpec {
    pub captured_at: String,
    pub snapshot_format: String,
    pub architecture: String,
    pub guest_os: String,
    pub host_backend_contract: HostBackendContract,
    pub cpu_contract: CpuContract,
    pub firecracker_version: String,
    pub device_contract: DeviceContract,
    pub network_contract: NetworkContract,
    pub vsock_contract: VsockContract,
    pub memory_contract: MemoryContract,
    pub state_contract_refs: Vec<ato_computation::ContentRef>,
    pub placement_hint: Option<String>,
    pub restore_layout: FirecrackerRestoreLayout,
}

/// The runtime owner supplies lifecycle and low-level Firecracker operations.
/// This keeps the generic Materializer independent from a particular Runner.
pub trait ActiveFirecrackerRealization: Send {
    fn target(&self) -> &ato_computation::ComputationRef;
    fn realization_id(&self) -> &str;
    fn capture_spec(&self) -> ActiveFirecrackerCaptureSpec;
    fn freeze_ingress(&mut self) -> Result<(), VmSnapshotError>;
    fn quiesce_interactions(&mut self) -> Result<(), VmSnapshotError>;
    fn pause_vm(&mut self) -> Result<(), VmSnapshotError>;
    fn create_full_snapshot(
        &mut self,
        memory_path: &Path,
        vmstate_path: &Path,
    ) -> Result<(), VmSnapshotError>;
    fn copy_rootfs_backing(&mut self, destination: &Path) -> Result<(), VmSnapshotError>;
    fn resume_vm(&mut self) -> Result<(), VmSnapshotError>;
    fn unfreeze_ingress(&mut self) -> Result<(), VmSnapshotError>;
}

/// Active VM capture integration used by a hosted Runner.
///
/// The source serializes captures for one active Realization and owns rollback
/// for every state transition. The Record Writer is reached only after the VM
/// and interaction ingress have been quiesced.
pub struct FirecrackerActiveVmCaptureSource {
    active: Mutex<Box<dyn ActiveFirecrackerRealization>>,
    barrier: Arc<dyn FirecrackerRecordCaptureBarrier>,
    capture_root: PathBuf,
}

impl FirecrackerActiveVmCaptureSource {
    pub fn new(
        active: Box<dyn ActiveFirecrackerRealization>,
        barrier: Arc<dyn FirecrackerRecordCaptureBarrier>,
        capture_root: PathBuf,
    ) -> Self {
        Self {
            active: Mutex::new(active),
            barrier,
            capture_root,
        }
    }
}

impl ActiveVmCaptureSource for FirecrackerActiveVmCaptureSource {
    fn capture_active(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| VmSnapshotError::Backend("active VM capture lock poisoned".to_owned()))?;
        if active.target() != &request.target {
            return Err(VmSnapshotError::Backend(
                "active VM target does not match requested ComputationRef".to_owned(),
            ));
        }
        let realization_id = active.realization_id().to_owned();
        let spec = active.capture_spec();
        spec.restore_layout.validate()?;
        if spec.vsock_contract.uds_path.as_deref() != spec.restore_layout.vsock_uds_path.as_deref()
        {
            return Err(VmSnapshotError::Backend(
                "active VM vsock path does not match restore layout".to_owned(),
            ));
        }

        active.freeze_ingress()?;
        let mut ingress_frozen = true;
        let mut vm_paused = false;
        let capture_result = (|| {
            active.quiesce_interactions()?;
            active.pause_vm()?;
            vm_paused = true;
            let record_lease = self.barrier.pause_and_seal()?;
            let record_frontier_ref = record_lease.frontier_ref().clone();

            fs::create_dir_all(&self.capture_root)?;
            let capture_dir = tempfile::Builder::new()
                .prefix("firecracker-capture-")
                .tempdir_in(&self.capture_root)?;
            let memory_path = capture_dir.path().join(&spec.restore_layout.memory_path);
            let vmstate_path = capture_dir.path().join(&spec.restore_layout.vmstate_path);
            let rootfs_path = capture_dir
                .path()
                .join(&spec.restore_layout.rootfs_backing_path);
            let metadata_path = capture_dir.path().join("restore-layout.json");
            for path in [&memory_path, &vmstate_path, &rootfs_path, &metadata_path] {
                create_parent(path)?;
            }
            active.create_full_snapshot(&memory_path, &vmstate_path)?;
            active.copy_rootfs_backing(&rootfs_path)?;
            fs::write(&metadata_path, spec.restore_layout.encode()?)?;
            sync_capture_artifact(&memory_path)?;
            sync_capture_artifact(&vmstate_path)?;
            sync_capture_artifact(&rootfs_path)?;
            sync_capture_artifact(&metadata_path)?;

            drop(record_lease);
            active.resume_vm()?;
            vm_paused = false;
            active.unfreeze_ingress()?;
            ingress_frozen = false;

            Ok(CapturedVm {
                target: request.target.clone(),
                record_frontier_ref,
                snapshot_format: spec.snapshot_format,
                architecture: spec.architecture,
                guest_os: spec.guest_os,
                host_backend_contract: spec.host_backend_contract,
                cpu_contract: spec.cpu_contract,
                firecracker_version: spec.firecracker_version,
                device_contract: spec.device_contract,
                network_contract: spec.network_contract,
                vsock_contract: spec.vsock_contract,
                memory_contract: spec.memory_contract,
                artifacts: vec![
                    CapturedArtifact {
                        role: crate::ArtifactRole::Memory,
                        path: memory_path,
                    },
                    CapturedArtifact {
                        role: crate::ArtifactRole::Rootfs,
                        path: rootfs_path,
                    },
                    CapturedArtifact {
                        role: crate::ArtifactRole::Vmstate,
                        path: vmstate_path,
                    },
                    CapturedArtifact {
                        role: crate::ArtifactRole::Metadata,
                        path: metadata_path,
                    },
                ],
                state_contract_refs: spec.state_contract_refs,
                provenance: CaptureProvenance {
                    captured_at: spec.captured_at,
                    backend_implementation_id: "firecracker.active-vm@1".to_owned(),
                    source_realization_id: realization_id,
                    capture_barrier_complete: true,
                    realization_quiesced: true,
                    placement_hint: spec.placement_hint,
                },
                guard: Box::new(FirecrackerCaptureGuard {
                    capture_dir: Some(capture_dir),
                }),
            })
        })();

        match capture_result {
            Ok(captured) => Ok(captured),
            Err(error) => {
                let mut rollback_errors = Vec::new();
                if vm_paused && let Err(rollback) = active.resume_vm() {
                    rollback_errors.push(format!("resume VM: {rollback}"));
                }
                if ingress_frozen && let Err(rollback) = active.unfreeze_ingress() {
                    rollback_errors.push(format!("unfreeze ingress: {rollback}"));
                }
                if rollback_errors.is_empty() {
                    Err(error)
                } else {
                    Err(VmSnapshotError::Backend(format!(
                        "{error}; capture rollback failed: {}",
                        rollback_errors.join(", ")
                    )))
                }
            }
        }
    }
}

struct FirecrackerCaptureGuard {
    capture_dir: Option<tempfile::TempDir>,
}

impl CaptureGuard for FirecrackerCaptureGuard {
    fn cleanup(&mut self) -> Result<(), VmSnapshotError> {
        if let Some(capture_dir) = self.capture_dir.take() {
            capture_dir.close()?;
        }
        Ok(())
    }
}

fn sync_capture_artifact(path: &Path) -> Result<(), VmSnapshotError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(VmSnapshotError::Backend(format!(
            "capture artifact is missing or empty: {}",
            path.display()
        )));
    }
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Backend-owned logical host paths persisted as the metadata artifact.
///
/// Firecracker stores block backing paths in vmstate.  All paths here are
/// normalized relative to the per-realization session root, allowing restore
/// to reproduce the capture layout without admitting a host-specific absolute
/// path into ComputationRef identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirecrackerRestoreLayout {
    pub version: u32,
    pub memory_path: String,
    pub vmstate_path: String,
    pub rootfs_backing_path: String,
    pub api_socket_path: String,
    pub console_log_path: String,
    pub vsock_uds_path: Option<String>,
}

impl Default for FirecrackerRestoreLayout {
    fn default() -> Self {
        Self {
            version: 1,
            memory_path: "vm/memory".to_owned(),
            vmstate_path: "vm/vmstate".to_owned(),
            rootfs_backing_path: "vm/rootfs.ext4".to_owned(),
            api_socket_path: "api.sock".to_owned(),
            console_log_path: "console.log".to_owned(),
            vsock_uds_path: Some("vsock/guest.sock".to_owned()),
        }
    }
}

impl FirecrackerRestoreLayout {
    pub fn encode(&self) -> Result<Vec<u8>, VmSnapshotError> {
        self.validate()?;
        Ok(serde_jcs::to_vec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, VmSnapshotError> {
        let layout: Self = serde_json::from_slice(bytes)?;
        if serde_jcs::to_vec(&layout)? != bytes {
            return Err(VmSnapshotError::InvalidDescriptor(
                "Firecracker restore layout is not canonical JCS".to_owned(),
            ));
        }
        layout.validate()?;
        Ok(layout)
    }

    pub fn validate(&self) -> Result<(), VmSnapshotError> {
        if self.version != 1 {
            return Err(VmSnapshotError::InvalidDescriptor(
                "unsupported Firecracker restore layout version".to_owned(),
            ));
        }
        for (name, path) in [
            ("memory", &self.memory_path),
            ("vmstate", &self.vmstate_path),
            ("rootfs", &self.rootfs_backing_path),
            ("API socket", &self.api_socket_path),
            ("console log", &self.console_log_path),
        ] {
            if !normalized_relative_path(path) {
                return Err(VmSnapshotError::InvalidDescriptor(format!(
                    "Firecracker {name} path must be normalized and relative"
                )));
            }
        }
        if self
            .vsock_uds_path
            .as_deref()
            .is_some_and(|path| !normalized_relative_path(path))
        {
            return Err(VmSnapshotError::InvalidDescriptor(
                "Firecracker vsock path must be normalized and relative".to_owned(),
            ));
        }
        let mut paths = std::collections::BTreeSet::new();
        for path in [
            Some(self.memory_path.as_str()),
            Some(self.vmstate_path.as_str()),
            Some(self.rootfs_backing_path.as_str()),
            Some(self.api_socket_path.as_str()),
            Some(self.console_log_path.as_str()),
            self.vsock_uds_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !paths.insert(path) {
                return Err(VmSnapshotError::InvalidDescriptor(
                    "Firecracker restore layout paths must be unique".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn normalized_relative_path(value: &str) -> bool {
    let path = std::path::Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

#[derive(Debug, Clone)]
pub struct FirecrackerBackendConfig {
    pub binary: PathBuf,
    pub ip_binary: PathBuf,
    pub work_root: PathBuf,
    pub slot_id: String,
    pub api_timeout: Duration,
}

impl Default for FirecrackerBackendConfig {
    fn default() -> Self {
        let work_root = std::env::var_os("ATO_FC_WORK")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".capsule")
                    .join("vm-backend")
            });
        Self {
            binary: std::env::var_os("ATO_FC_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("firecracker")),
            ip_binary: PathBuf::from("ip"),
            work_root,
            slot_id: "0".to_owned(),
            api_timeout: Duration::from_secs(15),
        }
    }
}

pub struct FirecrackerBackend {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    config: FirecrackerBackendConfig,
    capture_source: Option<Arc<dyn ActiveVmCaptureSource>>,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerBackendConfig) -> Self {
        Self {
            config,
            capture_source: None,
        }
    }

    pub fn with_capture_source(
        config: FirecrackerBackendConfig,
        capture_source: Arc<dyn ActiveVmCaptureSource>,
    ) -> Self {
        Self {
            config,
            capture_source: Some(capture_source),
        }
    }

    pub fn probe(&self) -> RunnerCapabilities {
        #[allow(unused_mut)]
        let mut capabilities = RunnerCapabilities {
            architecture: std::env::consts::ARCH.to_owned(),
            host_os: std::env::consts::OS.to_owned(),
            ..RunnerCapabilities::default()
        };
        #[cfg(target_os = "linux")]
        {
            let version = Command::new(&self.config.binary)
                .arg("--version")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|output| parse_firecracker_version(&output));
            let network_namespaces = Command::new(&self.config.ip_binary)
                .args(["netns", "list"])
                .output()
                .is_ok_and(|output| output.status.success());
            if Path::new("/dev/kvm").exists()
                && network_namespaces
                && let Some(version) = version
            {
                capabilities.backends.insert("firecracker".to_owned());
                capabilities
                    .backend_versions
                    .insert("firecracker".to_owned(), version.clone());
                capabilities.guest_os.insert("linux".to_owned());
                capabilities
                    .snapshot_formats
                    .insert("fc-full-file-v1".to_owned());
                capabilities.device_features.extend([
                    "kvm".to_owned(),
                    "virtio-blk".to_owned(),
                    "content-addressed-rootfs-path-v1".to_owned(),
                ]);
                capabilities
                    .network_features
                    .extend(["tap".to_owned(), "network-namespace".to_owned()]);
                capabilities.vsock_features.insert("vsock-uds".to_owned());
                if semver::Version::parse(&version)
                    .is_ok_and(|version| version >= semver::Version::new(1, 16, 0))
                {
                    capabilities
                        .vsock_features
                        .insert("vsock-override".to_owned());
                }
                capabilities.cpu_features = linux_cpu_features();
                capabilities.memory_mib = linux_memory_mib();
            }
        }
        capabilities
    }
}

impl Default for FirecrackerBackend {
    fn default() -> Self {
        Self::new(FirecrackerBackendConfig::default())
    }
}

impl VmSnapshotBackend for FirecrackerBackend {
    fn id(&self) -> &str {
        "firecracker"
    }

    fn capture(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError> {
        self.capture_source
            .as_ref()
            .ok_or_else(|| {
                VmSnapshotError::Backend(
                    "no active Firecracker Realization is registered for capture".to_owned(),
                )
            })?
            .capture_active(request)
    }

    fn restore(
        &self,
        request: &VmRestoreRequest<'_>,
    ) -> Result<Box<dyn VmBackendSession>, VmSnapshotError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = request;
            Err(VmSnapshotError::Backend(
                "Firecracker restore requires Linux/KVM".to_owned(),
            ))
        }
        #[cfg(target_os = "linux")]
        {
            self.restore_linux(request)
        }
    }
}

#[cfg(target_os = "linux")]
impl FirecrackerBackend {
    fn restore_linux(
        &self,
        request: &VmRestoreRequest<'_>,
    ) -> Result<Box<dyn VmBackendSession>, VmSnapshotError> {
        fs::create_dir_all(&self.config.work_root)?;
        let mut resources = FirecrackerResources::allocate(&self.config, request)?;
        resources.child = Some(resources.spawn_firecracker()?);
        wait_for_socket(
            &resources.api_socket,
            resources.child.as_mut().expect("child was stored"),
            self.config.api_timeout,
        )?;
        let body = snapshot_load_body(
            &resources.vmstate,
            &resources.memory,
            resources.vsock_path.as_deref(),
            &request.descriptor.firecracker_version,
        )?;
        firecracker_api(
            &resources.api_socket,
            "PUT",
            "/snapshot/load",
            &serde_json::to_vec(&body)?,
            self.config.api_timeout,
        )?;
        Ok(Box::new(FirecrackerSession {
            resources: Some(resources),
            api_timeout: self.config.api_timeout,
        }))
    }
}

#[cfg(target_os = "linux")]
fn required_artifact(
    request: &VmRestoreRequest<'_>,
    role: ArtifactRole,
) -> Result<PathBuf, VmSnapshotError> {
    request
        .artifacts
        .get(&role)
        .cloned()
        .ok_or_else(|| VmSnapshotError::Backend(format!("missing {role:?} artifact")))
}

#[cfg(target_os = "linux")]
struct FirecrackerResources {
    config: FirecrackerBackendConfig,
    session_dir: TempDir,
    api_socket: PathBuf,
    console: File,
    memory: PathBuf,
    vmstate: PathBuf,
    rootfs: PathBuf,
    child: Option<Child>,
    tap_created: Option<String>,
    netns_created: Option<String>,
    vsock_path: Option<PathBuf>,
    slot_path: PathBuf,
    slot_file: Option<File>,
}

#[cfg(target_os = "linux")]
impl FirecrackerResources {
    fn allocate(
        config: &FirecrackerBackendConfig,
        request: &VmRestoreRequest<'_>,
    ) -> Result<Self, VmSnapshotError> {
        if config.slot_id.is_empty()
            || !config
                .slot_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(VmSnapshotError::Backend(
                "invalid runner slot id".to_owned(),
            ));
        }
        let layout = load_restore_layout(request)?;
        if request.descriptor.vsock_contract.uds_path.as_deref() != layout.vsock_uds_path.as_deref()
        {
            return Err(VmSnapshotError::Backend(
                "Firecracker descriptor vsock path does not match restore layout".to_owned(),
            ));
        }
        let slots = config.work_root.join("slots");
        let sessions = config.work_root.join("sessions");
        fs::create_dir_all(&slots)?;
        fs::create_dir_all(&sessions)?;
        let slot_path = slots.join(format!("{}.lock", config.slot_id));
        let slot_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&slot_path)
            .map_err(|error| {
                VmSnapshotError::Backend(format!("runner slot unavailable: {error}"))
            })?;
        let allocated_session = (|| {
            let session_dir = tempfile::Builder::new()
                .prefix("firecracker-")
                .tempdir_in(&sessions)?;
            let api_socket = session_dir.path().join(&layout.api_socket_path);
            let console_path = session_dir.path().join(&layout.console_log_path);
            create_parent(&api_socket)?;
            create_parent(&console_path)?;
            let console = File::create(console_path)?;
            let memory = materialize_layout_artifact(
                required_artifact(request, ArtifactRole::Memory)?,
                session_dir.path().join(&layout.memory_path),
            )?;
            let vmstate = materialize_layout_artifact(
                required_artifact(request, ArtifactRole::Vmstate)?,
                session_dir.path().join(&layout.vmstate_path),
            )?;
            let rootfs = materialize_layout_artifact(
                required_artifact(request, ArtifactRole::Rootfs)?,
                session_dir.path().join(&layout.rootfs_backing_path),
            )?;
            Ok::<_, VmSnapshotError>((session_dir, api_socket, console, memory, vmstate, rootfs))
        })();
        let (session_dir, api_socket, console, memory, vmstate, rootfs) = match allocated_session {
            Ok(allocated) => allocated,
            Err(error) => {
                drop(slot_file);
                let _ = fs::remove_file(&slot_path);
                return Err(error);
            }
        };
        let mut resources = Self {
            config: config.clone(),
            session_dir,
            api_socket,
            console,
            memory,
            vmstate,
            rootfs,
            child: None,
            tap_created: None,
            netns_created: None,
            vsock_path: None,
            slot_path,
            slot_file: Some(slot_file),
        };
        if let Some(tap) = &request.descriptor.network_contract.tap_device {
            let netns = format!("ato-vm-{}", config.slot_id);
            command_ok(
                &config.ip_binary,
                &["netns", "add", &netns],
                "create network namespace",
            )?;
            resources.netns_created = Some(netns.clone());
            command_ok(
                &config.ip_binary,
                &["netns", "exec", &netns, "ip", "link", "set", "lo", "up"],
                "activate network namespace loopback",
            )?;
            command_ok(
                &config.ip_binary,
                &[
                    "netns", "exec", &netns, "ip", "tuntap", "add", "dev", tap, "mode", "tap",
                ],
                "create namespaced TAP",
            )?;
            resources.tap_created = Some(tap.clone());
            command_ok(
                &config.ip_binary,
                &[
                    "netns", "exec", &netns, "ip", "link", "set", "dev", tap, "up",
                ],
                "activate namespaced TAP",
            )?;
        }
        if let Some(relative) = &layout.vsock_uds_path {
            let path = resources.session_dir.path().join(relative);
            if path.exists() {
                return Err(VmSnapshotError::Backend(format!(
                    "vsock path already exists: {}",
                    path.display()
                )));
            }
            let parent = path
                .parent()
                .ok_or_else(|| VmSnapshotError::Backend("vsock path has no parent".to_owned()))?;
            fs::create_dir_all(parent)?;
            resources.vsock_path = Some(path);
        }
        Ok(resources)
    }

    fn spawn_firecracker(&mut self) -> Result<Child, VmSnapshotError> {
        if !self.rootfs.is_file() {
            return Err(VmSnapshotError::Backend(
                "reconstructed rootfs backing path is missing".to_owned(),
            ));
        }
        let mut command = if let Some(netns) = &self.netns_created {
            let mut command = Command::new(&self.config.ip_binary);
            command.args(["netns", "exec", netns]);
            command.arg(&self.config.binary);
            command
        } else {
            Command::new(&self.config.binary)
        };
        command
            .current_dir(self.session_dir.path())
            .arg("--api-sock")
            .arg(&self.api_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(self.console.try_clone()?))
            .stderr(Stdio::from(self.console.try_clone()?))
            .spawn()
            .map_err(|error| VmSnapshotError::Backend(format!("Firecracker spawn: {error}")))
    }

    fn cleanup(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        let _ = fs::remove_file(&self.api_socket);
        if let Some(path) = &self.vsock_path {
            let _ = fs::remove_file(path);
        }
        if let Some(netns) = &self.netns_created {
            let _ = Command::new(&self.config.ip_binary)
                .args(["netns", "del", netns])
                .status();
        }
        self.tap_created = None;
        self.netns_created = None;
        self.slot_file.take();
        let _ = fs::remove_file(&self.slot_path);
        let _ = self.session_dir.path();
    }
}

#[cfg(target_os = "linux")]
impl Drop for FirecrackerResources {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(target_os = "linux")]
fn load_restore_layout(
    request: &VmRestoreRequest<'_>,
) -> Result<FirecrackerRestoreLayout, VmSnapshotError> {
    let path = required_artifact(request, ArtifactRole::Metadata)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)?;
    FirecrackerRestoreLayout::decode(&bytes)
}

fn create_parent(path: &Path) -> Result<(), VmSnapshotError> {
    let parent = path
        .parent()
        .ok_or_else(|| VmSnapshotError::Backend("restore path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn materialize_layout_artifact(
    source: PathBuf,
    destination: PathBuf,
) -> Result<PathBuf, VmSnapshotError> {
    create_parent(&destination)?;
    fs::copy(source, &destination)?;
    File::open(&destination)?.sync_all()?;
    Ok(destination)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn snapshot_load_body(
    vmstate: &Path,
    memory: &Path,
    vsock_path: Option<&Path>,
    firecracker_version: &str,
) -> Result<serde_json::Value, VmSnapshotError> {
    let mut body = serde_json::json!({
        "snapshot_path": vmstate,
        "mem_backend": {
            "backend_type": "File",
            "backend_path": memory,
        },
        "resume_vm": true,
        "enable_diff_snapshots": false,
    });
    if let Some(path) = vsock_path {
        let version = semver::Version::parse(firecracker_version).map_err(|error| {
            VmSnapshotError::Backend(format!("invalid Firecracker version: {error}"))
        })?;
        if version >= semver::Version::new(1, 16, 0) {
            body.as_object_mut()
                .expect("snapshot load body is an object")
                .insert(
                    "vsock_override".to_owned(),
                    serde_json::json!({ "uds_path": path }),
                );
        }
        // Before 1.16 the backend path is not overridable.  The descriptor is
        // restricted to a relative path and the VMM runs with a unique
        // per-session current_dir, reproducing the snapshot's logical path
        // without cross-session collision.
    }
    Ok(body)
}

#[cfg(target_os = "linux")]
struct FirecrackerSession {
    resources: Option<FirecrackerResources>,
    api_timeout: Duration,
}

#[cfg(target_os = "linux")]
impl VmBackendSession for FirecrackerSession {
    fn activate(&mut self) -> Result<(), VmSnapshotError> {
        Ok(())
    }

    fn publish(&mut self) -> Result<(), VmSnapshotError> {
        Ok(())
    }

    fn wait(&mut self) -> Result<(), VmSnapshotError> {
        let resources = self
            .resources
            .as_mut()
            .ok_or_else(|| VmSnapshotError::Backend("session already stopped".to_owned()))?;
        let status = resources
            .child
            .as_mut()
            .ok_or_else(|| VmSnapshotError::Backend("Firecracker process missing".to_owned()))?
            .wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(VmSnapshotError::Backend(format!(
                "Firecracker exited with {status}"
            )))
        }
    }

    fn quiesce(&mut self) -> Result<(), VmSnapshotError> {
        if let Some(mut resources) = self.resources.take() {
            let pause = firecracker_api(
                &resources.api_socket,
                "PATCH",
                "/vm",
                br#"{"state":"Paused"}"#,
                self.api_timeout,
            );
            resources.cleanup();
            pause
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn command_ok(binary: &Path, args: &[&str], operation: &str) -> Result<(), VmSnapshotError> {
    let status = Command::new(binary).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(VmSnapshotError::Backend(format!(
            "{operation} failed with {status}"
        )))
    }
}

#[cfg(target_os = "linux")]
fn wait_for_socket(
    path: &Path,
    child: &mut Child,
    timeout: Duration,
) -> Result<(), VmSnapshotError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(VmSnapshotError::Backend(format!(
                "Firecracker exited before API readiness: {status}"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(VmSnapshotError::Backend(
        "Firecracker API socket readiness timed out".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn firecracker_api(
    socket: &Path,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<(), VmSnapshotError> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.take(1024 * 1024).read_to_end(&mut response)?;
    let status = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| VmSnapshotError::Backend("invalid Firecracker API response".to_owned()))?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(VmSnapshotError::Backend(format!(
            "Firecracker API {method} {path} returned {status}"
        )))
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_firecracker_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find_map(|word| word.strip_prefix('v'))
        .filter(|version| semver::Version::parse(version).is_ok())
        .map(ToOwned::to_owned)
}

#[cfg(target_os = "linux")]
fn linux_cpu_features() -> std::collections::BTreeSet<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .find(|line| line.starts_with("flags") || line.starts_with("Features"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, flags)| flags.split_whitespace().map(ToOwned::to_owned).collect())
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn linux_memory_mib() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                let value = line.strip_prefix("MemTotal:")?.split_whitespace().next()?;
                value.parse::<u64>().ok().map(|kib| kib / 1024)
            })
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_computation::{ComputationRef, ContentRef};
    use std::sync::Mutex;

    const TARGET: &str = "blake3:b18ad849d301ad6b009e4e6c8ab413667050c87b3514d08ccfb9d9bca8baf291";
    const FRONTIER: &str =
        "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct TestLease {
        frontier: ContentRef,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FirecrackerRecordCaptureLease for TestLease {
        fn frontier_ref(&self) -> &ContentRef {
            &self.frontier
        }
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.events
                .lock()
                .unwrap()
                .push("barrier.release".to_owned());
        }
    }

    struct TestBarrier {
        frontier: ContentRef,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FirecrackerRecordCaptureBarrier for TestBarrier {
        fn pause_and_seal(
            &self,
        ) -> Result<Box<dyn FirecrackerRecordCaptureLease>, VmSnapshotError> {
            self.events.lock().unwrap().push("barrier.seal".to_owned());
            Ok(Box::new(TestLease {
                frontier: self.frontier.clone(),
                events: Arc::clone(&self.events),
            }))
        }
    }

    struct TestActiveVm {
        target: ComputationRef,
        events: Arc<Mutex<Vec<String>>>,
        fail_snapshot: bool,
    }

    impl ActiveFirecrackerRealization for TestActiveVm {
        fn target(&self) -> &ComputationRef {
            &self.target
        }

        fn realization_id(&self) -> &str {
            "realization.test-2048"
        }

        fn capture_spec(&self) -> ActiveFirecrackerCaptureSpec {
            ActiveFirecrackerCaptureSpec {
                captured_at: "2030-01-01T00:00:00Z".to_owned(),
                snapshot_format: "fc-full-file-v1".to_owned(),
                architecture: "x86_64".to_owned(),
                guest_os: "linux".to_owned(),
                host_backend_contract: HostBackendContract {
                    backend_id: "firecracker".to_owned(),
                    host_os: "linux".to_owned(),
                    required_features: std::collections::BTreeSet::from(["kvm".to_owned()]),
                },
                cpu_contract: CpuContract {
                    vcpu_count: 1,
                    required_features: std::collections::BTreeSet::new(),
                },
                firecracker_version: "1.16.0".to_owned(),
                device_contract: DeviceContract {
                    required_features: std::collections::BTreeSet::from(["virtio-blk".to_owned()]),
                },
                network_contract: NetworkContract {
                    required_features: std::collections::BTreeSet::from(["tap".to_owned()]),
                    tap_device: Some("tap0".to_owned()),
                },
                vsock_contract: VsockContract {
                    required_features: std::collections::BTreeSet::from(["vsock-uds".to_owned()]),
                    uds_path: Some("vsock/guest.sock".to_owned()),
                },
                memory_contract: MemoryContract {
                    guest_memory_mib: 128,
                    minimum_host_memory_mib: 256,
                },
                state_contract_refs: Vec::new(),
                placement_hint: Some("test-linux".to_owned()),
                restore_layout: FirecrackerRestoreLayout::default(),
            }
        }

        fn freeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
            self.events
                .lock()
                .unwrap()
                .push("ingress.freeze".to_owned());
            Ok(())
        }

        fn quiesce_interactions(&mut self) -> Result<(), VmSnapshotError> {
            self.events
                .lock()
                .unwrap()
                .push("interactions.quiesce".to_owned());
            Ok(())
        }

        fn pause_vm(&mut self) -> Result<(), VmSnapshotError> {
            self.events.lock().unwrap().push("vm.pause".to_owned());
            Ok(())
        }

        fn create_full_snapshot(
            &mut self,
            memory_path: &Path,
            vmstate_path: &Path,
        ) -> Result<(), VmSnapshotError> {
            self.events
                .lock()
                .unwrap()
                .push("snapshot.create".to_owned());
            if self.fail_snapshot {
                return Err(VmSnapshotError::Backend("snapshot failure".to_owned()));
            }
            fs::write(memory_path, b"memory")?;
            fs::write(vmstate_path, b"vmstate")?;
            Ok(())
        }

        fn copy_rootfs_backing(&mut self, destination: &Path) -> Result<(), VmSnapshotError> {
            self.events.lock().unwrap().push("rootfs.copy".to_owned());
            fs::write(destination, b"rootfs")?;
            Ok(())
        }

        fn resume_vm(&mut self) -> Result<(), VmSnapshotError> {
            self.events.lock().unwrap().push("vm.resume".to_owned());
            Ok(())
        }

        fn unfreeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
            self.events
                .lock()
                .unwrap()
                .push("ingress.unfreeze".to_owned());
            Ok(())
        }
    }

    fn capture_harness(
        fail_snapshot: bool,
    ) -> (
        FirecrackerActiveVmCaptureSource,
        VmCaptureRequest,
        Arc<Mutex<Vec<String>>>,
        tempfile::TempDir,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let target = ComputationRef::parse(TARGET).unwrap();
        let frontier = ContentRef::parse(FRONTIER).unwrap();
        let root = tempfile::Builder::new()
            .prefix("active-vm-capture-")
            .tempdir_in(std::env::current_dir().unwrap().join(".tmp"))
            .unwrap();
        let source = FirecrackerActiveVmCaptureSource::new(
            Box::new(TestActiveVm {
                target: target.clone(),
                events: Arc::clone(&events),
                fail_snapshot,
            }),
            Arc::new(TestBarrier {
                frontier: frontier.clone(),
                events: Arc::clone(&events),
            }),
            root.path().to_path_buf(),
        );
        (source, VmCaptureRequest { target }, events, root)
    }

    #[test]
    fn parses_firecracker_version_without_accepting_noise() {
        assert_eq!(
            parse_firecracker_version("Firecracker v1.7.0\n"),
            Some("1.7.0".to_owned())
        );
        assert_eq!(parse_firecracker_version("Firecracker unknown"), None);
    }

    #[test]
    fn unsupported_host_probe_never_claims_compatibility_by_default() {
        let backend = FirecrackerBackend::new(FirecrackerBackendConfig {
            binary: PathBuf::from("definitely-not-a-firecracker-binary"),
            ..FirecrackerBackendConfig::default()
        });
        let probe = backend.probe();
        assert!(!probe.backends.contains("firecracker"));
    }

    #[test]
    fn restore_layout_roundtrips_and_rejects_host_paths() {
        let layout = FirecrackerRestoreLayout::default();
        assert_eq!(
            FirecrackerRestoreLayout::decode(&layout.encode().unwrap()).unwrap(),
            layout
        );

        let mut absolute = layout.clone();
        absolute.rootfs_backing_path = "/var/lib/ato/rootfs.ext4".to_owned();
        assert!(
            absolute
                .encode()
                .unwrap_err()
                .to_string()
                .contains("relative")
        );

        let mut duplicate = layout;
        duplicate.vmstate_path = duplicate.memory_path.clone();
        assert!(
            duplicate
                .encode()
                .unwrap_err()
                .to_string()
                .contains("unique")
        );
    }

    #[test]
    fn vsock_override_is_version_gated_and_session_unique() {
        let path = Path::new("/run/ato/session-2/vsock/guest.sock");
        let old = snapshot_load_body(
            Path::new("vm/vmstate"),
            Path::new("vm/memory"),
            Some(path),
            "1.15.1",
        )
        .unwrap();
        assert!(old.get("vsock_override").is_none());

        let current = snapshot_load_body(
            Path::new("vm/vmstate"),
            Path::new("vm/memory"),
            Some(path),
            "1.16.0",
        )
        .unwrap();
        assert_eq!(
            current["vsock_override"]["uds_path"],
            serde_json::json!(path)
        );
    }

    #[test]
    fn active_capture_orders_pause_barrier_snapshot_and_resume() {
        let (source, request, events, _root) = capture_harness(false);
        let captured = source.capture_active(&request).unwrap();
        assert_eq!(captured.target, request.target);
        assert_eq!(
            captured.record_frontier_ref,
            ContentRef::parse(FRONTIER).unwrap()
        );
        assert!(
            captured
                .artifacts
                .iter()
                .all(|artifact| artifact.path.is_file())
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "ingress.freeze",
                "interactions.quiesce",
                "vm.pause",
                "barrier.seal",
                "snapshot.create",
                "rootfs.copy",
                "barrier.release",
                "vm.resume",
                "ingress.unfreeze",
            ]
        );
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.as_str() == "barrier.seal")
                .count(),
            1
        );
    }

    #[test]
    fn active_capture_failure_resumes_vm_unfreezes_ingress_and_removes_files() {
        let (source, request, events, root) = capture_harness(true);
        let error = match source.capture_active(&request) {
            Ok(_) => panic!("capture unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("snapshot failure"));
        assert_eq!(
            *events.lock().unwrap(),
            [
                "ingress.freeze",
                "interactions.quiesce",
                "vm.pause",
                "barrier.seal",
                "snapshot.create",
                "barrier.release",
                "vm.resume",
                "ingress.unfreeze",
            ]
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
