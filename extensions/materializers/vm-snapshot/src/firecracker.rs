//! Low-level Firecracker process and snapshot-load backend.
//!
//! The process/API/socket cleanup mechanics are adapted from the historical
//! Ready-State backend; legacy Store identity and launch APIs are not reused.

use std::fs;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
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

/// Runtime-owned ingress gate used by the generic fresh VM bootstrap. It is a
/// physical lifecycle dependency, not part of the VM descriptor or any
/// semantic identity.
pub trait FirecrackerIngressGate: Send {
    fn freeze(&mut self) -> Result<(), VmSnapshotError>;
    fn quiesce(&mut self) -> Result<(), VmSnapshotError>;
    fn unfreeze(&mut self) -> Result<(), VmSnapshotError>;
}

/// Generic inputs for booting a current rootfs into an active Firecracker VM.
/// Source/replay construction remains outside this backend; callers provide a
/// bootable rootfs already associated with the requested ComputationRef.
#[derive(Debug, Clone)]
pub struct FreshFirecrackerConfig {
    pub binary: PathBuf,
    pub ip_binary: PathBuf,
    pub work_root: PathBuf,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub netns_name: String,
    pub tap_device: String,
    pub tap_host_cidr: String,
    pub boot_args: String,
    pub vcpu_count: u32,
    pub memory_mib: u64,
    pub api_timeout: Duration,
}

/// Active VM owner produced by the generic fresh bootstrap. This is the
/// concrete staging bridge consumed by `FirecrackerActiveVmCaptureSource`.
#[cfg(target_os = "linux")]
pub struct FreshFirecrackerRealization {
    target: ato_computation::ComputationRef,
    realization_id: String,
    spec: ActiveFirecrackerCaptureSpec,
    ingress: Box<dyn FirecrackerIngressGate>,
    session: TempDir,
    api_socket: PathBuf,
    rootfs: PathBuf,
    console: File,
    child: Option<Child>,
    ip_binary: PathBuf,
    netns_name: String,
}

#[cfg(target_os = "linux")]
impl FreshFirecrackerRealization {
    /// Physical process identifier exposed for operational receipts only.
    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Per-realization resource root exposed for cleanup verification only.
    pub fn session_root(&self) -> &Path {
        self.session.path()
    }

    /// Isolated physical network namespace used by this realization.
    pub fn network_namespace(&self) -> &str {
        &self.netns_name
    }

    pub fn boot(
        target: ato_computation::ComputationRef,
        realization_id: String,
        config: FreshFirecrackerConfig,
        spec: ActiveFirecrackerCaptureSpec,
        ingress: Box<dyn FirecrackerIngressGate>,
    ) -> Result<Self, VmSnapshotError> {
        spec.restore_layout.validate()?;
        if spec.network_contract.tap_device.as_deref() != Some(config.tap_device.as_str()) {
            return Err(VmSnapshotError::Backend(
                "fresh VM TAP does not match capture contract".to_owned(),
            ));
        }
        if !config.kernel.is_file() || !config.rootfs.is_file() {
            return Err(VmSnapshotError::Backend(
                "fresh VM kernel and rootfs must be regular files".to_owned(),
            ));
        }
        if config.netns_name.is_empty()
            || config.tap_device.is_empty()
            || config.vcpu_count == 0
            || config.memory_mib == 0
        {
            return Err(VmSnapshotError::Backend(
                "fresh VM physical configuration is incomplete".to_owned(),
            ));
        }
        fs::create_dir_all(&config.work_root)?;
        let session = tempfile::Builder::new()
            .prefix("firecracker-fresh-")
            .tempdir_in(&config.work_root)?;
        let api_socket = session.path().join(&spec.restore_layout.api_socket_path);
        let console_path = session.path().join(&spec.restore_layout.console_log_path);
        let rootfs = session
            .path()
            .join(&spec.restore_layout.rootfs_backing_path);
        for path in [&api_socket, &console_path, &rootfs] {
            create_parent(path)?;
        }
        fs::copy(&config.rootfs, &rootfs)?;
        File::open(&rootfs)?.sync_all()?;
        let console = File::create(console_path)?;
        let console_stdout = console.try_clone()?;
        let console_stderr = console.try_clone()?;

        command_ok(
            &config.ip_binary,
            &["netns", "add", &config.netns_name],
            "create fresh VM network namespace",
        )?;
        let setup = (|| {
            command_ok(
                &config.ip_binary,
                &[
                    "netns",
                    "exec",
                    &config.netns_name,
                    "ip",
                    "link",
                    "set",
                    "lo",
                    "up",
                ],
                "activate fresh VM loopback",
            )?;
            command_ok(
                &config.ip_binary,
                &[
                    "netns",
                    "exec",
                    &config.netns_name,
                    "ip",
                    "tuntap",
                    "add",
                    "dev",
                    &config.tap_device,
                    "mode",
                    "tap",
                ],
                "create fresh VM TAP",
            )?;
            command_ok(
                &config.ip_binary,
                &[
                    "netns",
                    "exec",
                    &config.netns_name,
                    "ip",
                    "addr",
                    "add",
                    &config.tap_host_cidr,
                    "dev",
                    &config.tap_device,
                ],
                "address fresh VM TAP",
            )?;
            command_ok(
                &config.ip_binary,
                &[
                    "netns",
                    "exec",
                    &config.netns_name,
                    "ip",
                    "link",
                    "set",
                    "dev",
                    &config.tap_device,
                    "up",
                ],
                "activate fresh VM TAP",
            )?;
            Ok::<(), VmSnapshotError>(())
        })();
        if let Err(error) = setup {
            let _ = Command::new(&config.ip_binary)
                .args(["netns", "del", &config.netns_name])
                .status();
            return Err(error);
        }

        let mut command = Command::new(&config.ip_binary);
        command
            .args(["netns", "exec", &config.netns_name])
            .arg(&config.binary)
            .current_dir(session.path())
            .arg("--api-sock")
            .arg(&api_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(console_stdout))
            .stderr(Stdio::from(console_stderr));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = Command::new(&config.ip_binary)
                    .args(["netns", "del", &config.netns_name])
                    .status();
                return Err(VmSnapshotError::Backend(format!(
                    "fresh Firecracker spawn: {error}"
                )));
            }
        };
        let configured = (|| {
            wait_for_socket(&api_socket, &mut child, config.api_timeout)?;
            firecracker_api(
                &api_socket,
                "PUT",
                "/machine-config",
                &serde_json::to_vec(&serde_json::json!({
                    "vcpu_count": config.vcpu_count,
                    "mem_size_mib": config.memory_mib,
                }))?,
                config.api_timeout,
            )?;
            firecracker_api(
                &api_socket,
                "PUT",
                "/boot-source",
                &serde_json::to_vec(&serde_json::json!({
                    "kernel_image_path": config.kernel,
                    "boot_args": config.boot_args,
                }))?,
                config.api_timeout,
            )?;
            firecracker_api(
                &api_socket,
                "PUT",
                "/drives/rootfs",
                &serde_json::to_vec(&serde_json::json!({
                    "drive_id": "rootfs",
                    "path_on_host": spec.restore_layout.rootfs_backing_path,
                    "is_root_device": true,
                    "is_read_only": false,
                }))?,
                config.api_timeout,
            )?;
            firecracker_api(
                &api_socket,
                "PUT",
                "/network-interfaces/eth0",
                &serde_json::to_vec(&serde_json::json!({
                    "iface_id": "eth0",
                    "host_dev_name": config.tap_device,
                }))?,
                config.api_timeout,
            )?;
            if let Some(path) = &spec.restore_layout.vsock_uds_path {
                let physical_vsock_path = session.path().join(path);
                create_parent(&physical_vsock_path)?;
                firecracker_api(
                    &api_socket,
                    "PUT",
                    "/vsock",
                    &serde_json::to_vec(&serde_json::json!({
                        "guest_cid": 3,
                        "uds_path": physical_vsock_path,
                    }))?,
                    config.api_timeout,
                )?;
            }
            firecracker_api(
                &api_socket,
                "PUT",
                "/actions",
                br#"{"action_type":"InstanceStart"}"#,
                config.api_timeout,
            )
        })();
        if let Err(error) = configured {
            let _ = child.kill();
            let _ = child.wait();
            let _ = Command::new(&config.ip_binary)
                .args(["netns", "del", &config.netns_name])
                .status();
            return Err(error);
        }
        Ok(Self {
            target,
            realization_id,
            spec,
            ingress,
            session,
            api_socket,
            rootfs,
            console,
            child: Some(child),
            ip_binary: config.ip_binary,
            netns_name: config.netns_name,
        })
    }

    fn cleanup(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        let _ = Command::new(&self.ip_binary)
            .args(["netns", "del", &self.netns_name])
            .status();
        let _ = fs::remove_file(&self.api_socket);
        let _ = self.console.sync_all();
        let _ = self.session.path();
    }
}

#[cfg(target_os = "linux")]
impl ActiveFirecrackerRealization for FreshFirecrackerRealization {
    fn target(&self) -> &ato_computation::ComputationRef {
        &self.target
    }

    fn realization_id(&self) -> &str {
        &self.realization_id
    }

    fn capture_spec(&self) -> ActiveFirecrackerCaptureSpec {
        self.spec.clone()
    }

    fn freeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress.freeze()
    }

    fn quiesce_interactions(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress.quiesce()
    }

    fn pause_vm(&mut self) -> Result<(), VmSnapshotError> {
        firecracker_api(
            &self.api_socket,
            "PATCH",
            "/vm",
            br#"{"state":"Paused"}"#,
            Duration::from_secs(15),
        )
    }

    fn create_full_snapshot(
        &mut self,
        memory_path: &Path,
        vmstate_path: &Path,
    ) -> Result<(), VmSnapshotError> {
        firecracker_api(
            &self.api_socket,
            "PUT",
            "/snapshot/create",
            &serde_json::to_vec(&serde_json::json!({
                "snapshot_type": "Full",
                "snapshot_path": vmstate_path,
                "mem_file_path": memory_path,
            }))?,
            Duration::from_secs(15),
        )
    }

    fn copy_rootfs_backing(&mut self, destination: &Path) -> Result<(), VmSnapshotError> {
        fs::copy(&self.rootfs, destination)?;
        File::open(destination)?.sync_all()?;
        Ok(())
    }

    fn resume_vm(&mut self) -> Result<(), VmSnapshotError> {
        firecracker_api(
            &self.api_socket,
            "PATCH",
            "/vm",
            br#"{"state":"Resumed"}"#,
            Duration::from_secs(15),
        )
    }

    fn unfreeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress.unfreeze()
    }
}

#[cfg(target_os = "linux")]
impl Drop for FreshFirecrackerRealization {
    fn drop(&mut self) {
        self.cleanup();
    }
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
    /// Address assigned to the logical TAP inside each per-run namespace.
    /// Identical values are safe because the namespaces are isolated.
    pub tap_host_cidr: Option<String>,
    /// Optional process injected by a hosted runtime to relay a guest TCP
    /// Surface through a unique Unix socket. The relay runs inside the VM's
    /// network namespace, so identical guest addresses remain safe across
    /// concurrent restores of the same snapshot.
    pub surface_relay: Option<FirecrackerSurfaceRelayConfig>,
}

#[derive(Debug, Clone)]
pub struct FirecrackerSurfaceRelayConfig {
    pub binary: PathBuf,
    pub guest_target: std::net::SocketAddr,
    pub uds_path: PathBuf,
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
            tap_host_cidr: None,
            surface_relay: None,
        }
    }
}

pub struct FirecrackerBackend {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    config: FirecrackerBackendConfig,
    capture_source: Option<Arc<dyn ActiveVmCaptureSource>>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    restored_capture: Option<RestoredCaptureRuntime>,
    restored_capture_source: Mutex<Option<Arc<dyn ActiveVmCaptureSource>>>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct RestoredCaptureRuntime {
    barrier: Arc<dyn FirecrackerRecordCaptureBarrier>,
    capture_root: PathBuf,
    ingress: Arc<Mutex<Box<dyn FirecrackerIngressGate>>>,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerBackendConfig) -> Self {
        Self {
            config,
            capture_source: None,
            restored_capture: None,
            restored_capture_source: Mutex::new(None),
        }
    }

    pub fn with_capture_source(
        config: FirecrackerBackendConfig,
        capture_source: Arc<dyn ActiveVmCaptureSource>,
    ) -> Self {
        Self {
            config,
            capture_source: Some(capture_source),
            restored_capture: None,
            restored_capture_source: Mutex::new(None),
        }
    }

    /// Capture-capable restore assembly for a hosted runtime. The active
    /// source is registered only after snapshot/load succeeds, and remains
    /// bound to that exact backend-owned session.
    pub fn with_restored_capture(
        config: FirecrackerBackendConfig,
        barrier: Arc<dyn FirecrackerRecordCaptureBarrier>,
        capture_root: PathBuf,
        ingress: Box<dyn FirecrackerIngressGate>,
    ) -> Self {
        Self {
            config,
            capture_source: None,
            restored_capture: Some(RestoredCaptureRuntime {
                barrier,
                capture_root,
                ingress: Arc::new(Mutex::new(ingress)),
            }),
            restored_capture_source: Mutex::new(None),
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
        if let Some(source) = &self.capture_source {
            return source.capture_active(request);
        }
        let source = self
            .restored_capture_source
            .lock()
            .map_err(|_| VmSnapshotError::Backend("restored capture lock poisoned".to_owned()))?
            .clone()
            .ok_or_else(|| {
                VmSnapshotError::Backend(
                    "no active Firecracker Realization is registered for capture".to_owned(),
                )
            })?;
        source.capture_active(request)
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
        if let Err(error) = wait_for_socket(
            &resources.api_socket,
            resources.child.as_mut().expect("child was stored"),
            self.config.api_timeout,
        ) {
            let diagnostic = bounded_diagnostic_tail(&resources.console_path, 4096)
                .unwrap_or_else(|_| "unavailable".to_owned());
            return Err(VmSnapshotError::Backend(format!(
                "{error}; Firecracker startup diagnostic: {diagnostic}"
            )));
        }
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
        let layout = load_restore_layout(request)?;
        let target =
            ato_computation::ComputationRef::parse(&request.descriptor.target_computation_ref)
                .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
        let shared = Arc::new(Mutex::new(Some(resources)));
        if let Some(capture) = &self.restored_capture {
            let captured_at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| VmSnapshotError::Backend(error.to_string()))?;
            let active = RestoredActiveFirecrackerRealization {
                resources: Arc::clone(&shared),
                target,
                realization_id: format!("firecracker-restored:{}", self.config.slot_id),
                spec: ActiveFirecrackerCaptureSpec {
                    captured_at,
                    snapshot_format: request.descriptor.snapshot_format.clone(),
                    architecture: request.descriptor.architecture.clone(),
                    guest_os: request.descriptor.guest_os.clone(),
                    host_backend_contract: request.descriptor.host_backend_contract.clone(),
                    cpu_contract: request.descriptor.cpu_contract.clone(),
                    firecracker_version: request.descriptor.firecracker_version.clone(),
                    device_contract: request.descriptor.device_contract.clone(),
                    network_contract: request.descriptor.network_contract.clone(),
                    vsock_contract: request.descriptor.vsock_contract.clone(),
                    memory_contract: request.descriptor.memory_contract.clone(),
                    state_contract_refs: request
                        .descriptor
                        .state_contract_refs
                        .iter()
                        .map(|reference| {
                            ato_computation::ContentRef::parse(reference).map_err(|error| {
                                VmSnapshotError::InvalidReference(error.to_string())
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    placement_hint: Some("hosted-restored".to_owned()),
                    restore_layout: layout,
                },
                ingress: Arc::clone(&capture.ingress),
                api_timeout: self.config.api_timeout,
            };
            let source: Arc<dyn ActiveVmCaptureSource> =
                Arc::new(FirecrackerActiveVmCaptureSource::new(
                    Box::new(active),
                    Arc::clone(&capture.barrier),
                    capture.capture_root.clone(),
                ));
            *self.restored_capture_source.lock().map_err(|_| {
                VmSnapshotError::Backend("restored capture lock poisoned".to_owned())
            })? = Some(source);
        }
        Ok(Box::new(FirecrackerSession {
            resources: shared,
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
    console_path: PathBuf,
    memory: PathBuf,
    vmstate: PathBuf,
    rootfs: PathBuf,
    child: Option<Child>,
    surface_relay: Option<Child>,
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
            validate_unix_socket_path(&api_socket, "Firecracker API socket")?;
            create_parent(&api_socket)?;
            create_parent(&console_path)?;
            let console = File::create(&console_path)?;
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
            Ok::<_, VmSnapshotError>((
                session_dir,
                api_socket,
                console,
                console_path,
                memory,
                vmstate,
                rootfs,
            ))
        })();
        let (session_dir, api_socket, console, console_path, memory, vmstate, rootfs) =
            match allocated_session {
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
            console_path,
            memory,
            vmstate,
            rootfs,
            child: None,
            surface_relay: None,
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
            if let Some(cidr) = &config.tap_host_cidr {
                command_ok(
                    &config.ip_binary,
                    &[
                        "netns", "exec", &netns, "ip", "addr", "add", cidr, "dev", tap,
                    ],
                    "address namespaced TAP",
                )?;
            } else if config.surface_relay.is_some() {
                return Err(VmSnapshotError::Backend(
                    "Surface relay requires a namespaced TAP host address".to_owned(),
                ));
            }
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
            validate_unix_socket_path(&path, "Firecracker vsock UDS")?;
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
        if let (Some(netns), Some(relay)) = (&resources.netns_created, &config.surface_relay) {
            validate_unix_socket_path(&relay.uds_path, "Surface relay socket")?;
            if relay.uds_path.exists() {
                return Err(VmSnapshotError::Backend(format!(
                    "Surface relay socket already exists: {}",
                    relay.uds_path.display()
                )));
            }
            let parent = relay.uds_path.parent().ok_or_else(|| {
                VmSnapshotError::Backend("Surface relay socket has no parent".to_owned())
            })?;
            fs::create_dir_all(parent)?;
            let child = Command::new(&config.ip_binary)
                .args(["netns", "exec", netns])
                .arg(&relay.binary)
                .arg("__netns-surface-relay")
                .arg("--listen-unix")
                .arg(&relay.uds_path)
                .arg("--target")
                .arg(relay.guest_target.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(resources.console.try_clone()?))
                .spawn()
                .map_err(|error| {
                    VmSnapshotError::Backend(format!("Surface relay spawn: {error}"))
                })?;
            resources.surface_relay = Some(child);
            wait_for_path(
                &relay.uds_path,
                resources
                    .surface_relay
                    .as_mut()
                    .expect("Surface relay child was stored"),
                config.api_timeout,
                "Surface relay socket",
            )?;
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
        if let Some(child) = &mut self.surface_relay {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.surface_relay = None;
        if let Some(relay) = &self.config.surface_relay {
            let _ = fs::remove_file(&relay.uds_path);
        }
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
    resources: Arc<Mutex<Option<FirecrackerResources>>>,
    api_timeout: Duration,
}

#[cfg(target_os = "linux")]
struct RestoredActiveFirecrackerRealization {
    resources: Arc<Mutex<Option<FirecrackerResources>>>,
    target: ato_computation::ComputationRef,
    realization_id: String,
    spec: ActiveFirecrackerCaptureSpec,
    ingress: Arc<Mutex<Box<dyn FirecrackerIngressGate>>>,
    api_timeout: Duration,
}

#[cfg(target_os = "linux")]
impl RestoredActiveFirecrackerRealization {
    fn with_resources<T>(
        &self,
        operation: impl FnOnce(&mut FirecrackerResources) -> Result<T, VmSnapshotError>,
    ) -> Result<T, VmSnapshotError> {
        let mut resources = self.resources.lock().map_err(|_| {
            VmSnapshotError::Backend("Firecracker session lock poisoned".to_owned())
        })?;
        operation(resources.as_mut().ok_or_else(|| {
            VmSnapshotError::Backend("Firecracker session already stopped".to_owned())
        })?)
    }
}

#[cfg(target_os = "linux")]
impl ActiveFirecrackerRealization for RestoredActiveFirecrackerRealization {
    fn target(&self) -> &ato_computation::ComputationRef {
        &self.target
    }

    fn realization_id(&self) -> &str {
        &self.realization_id
    }

    fn capture_spec(&self) -> ActiveFirecrackerCaptureSpec {
        let mut spec = self.spec.clone();
        if let Ok(captured_at) =
            time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)
        {
            spec.captured_at = captured_at;
        }
        spec
    }

    fn freeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress
            .lock()
            .map_err(|_| VmSnapshotError::Backend("ingress gate lock poisoned".to_owned()))?
            .freeze()
    }

    fn quiesce_interactions(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress
            .lock()
            .map_err(|_| VmSnapshotError::Backend("ingress gate lock poisoned".to_owned()))?
            .quiesce()
    }

    fn pause_vm(&mut self) -> Result<(), VmSnapshotError> {
        self.with_resources(|resources| {
            firecracker_api(
                &resources.api_socket,
                "PATCH",
                "/vm",
                br#"{"state":"Paused"}"#,
                self.api_timeout,
            )
        })
    }

    fn create_full_snapshot(
        &mut self,
        memory_path: &Path,
        vmstate_path: &Path,
    ) -> Result<(), VmSnapshotError> {
        self.with_resources(|resources| {
            firecracker_api(
                &resources.api_socket,
                "PUT",
                "/snapshot/create",
                &serde_json::to_vec(&serde_json::json!({
                    "snapshot_type": "Full",
                    "snapshot_path": vmstate_path,
                    "mem_file_path": memory_path,
                }))?,
                self.api_timeout,
            )
        })
    }

    fn copy_rootfs_backing(&mut self, destination: &Path) -> Result<(), VmSnapshotError> {
        self.with_resources(|resources| {
            fs::copy(&resources.rootfs, destination)?;
            File::open(destination)?.sync_all()?;
            Ok(())
        })
    }

    fn resume_vm(&mut self) -> Result<(), VmSnapshotError> {
        self.with_resources(|resources| {
            firecracker_api(
                &resources.api_socket,
                "PATCH",
                "/vm",
                br#"{"state":"Resumed"}"#,
                self.api_timeout,
            )
        })
    }

    fn unfreeze_ingress(&mut self) -> Result<(), VmSnapshotError> {
        self.ingress
            .lock()
            .map_err(|_| VmSnapshotError::Backend("ingress gate lock poisoned".to_owned()))?
            .unfreeze()
    }
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
        let mut resources = self.resources.lock().map_err(|_| {
            VmSnapshotError::Backend("Firecracker session lock poisoned".to_owned())
        })?;
        let resources = resources
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
        let mut shared = self.resources.lock().map_err(|_| {
            VmSnapshotError::Backend("Firecracker session lock poisoned".to_owned())
        })?;
        if let Some(mut resources) = shared.take() {
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
    wait_for_path(path, child, timeout, "Firecracker API socket")
}

#[cfg(target_os = "linux")]
fn wait_for_path(
    path: &Path,
    child: &mut Child,
    timeout: Duration,
    label: &str,
) -> Result<(), VmSnapshotError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(VmSnapshotError::Backend(format!(
                "{label} owner exited before readiness: {status}"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(VmSnapshotError::Backend(format!(
        "{label} readiness timed out"
    )))
}

#[cfg(target_os = "linux")]
fn validate_unix_socket_path(path: &Path, label: &str) -> Result<(), VmSnapshotError> {
    const SUN_PATH_BYTES: usize = 108;
    if path.as_os_str().as_bytes().len() >= SUN_PATH_BYTES {
        return Err(VmSnapshotError::Backend(format!(
            "{label} path exceeds SUN_LEN"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bounded_diagnostic_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(max_bytes)))?;
    let mut bytes = Vec::with_capacity(length.min(max_bytes) as usize);
    file.take(max_bytes).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => character,
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>()
        .trim()
        .to_owned())
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
    let mut buffer = [0_u8; 8192];
    while response.len() < 1024 * 1024 {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if firecracker_http_response_complete(&response) {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && response.windows(4).any(|window| window == b"\r\n\r\n") =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
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
fn firecracker_http_response_complete(response: &[u8]) -> bool {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header_end = header_end + 4;
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    content_length.is_none_or(|length| response.len() >= header_end + length)
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
    fn detects_firecracker_http_response_without_waiting_for_socket_close() {
        assert!(firecracker_http_response_complete(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
        ));
        assert!(!firecracker_http_response_complete(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 4\r\n\r\nerr"
        ));
        assert!(firecracker_http_response_complete(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 4\r\n\r\nerr!"
        ));
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
