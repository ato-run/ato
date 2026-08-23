//! Low-level Firecracker process and snapshot-load backend.
//!
//! The process/API/socket cleanup mechanics are adapted from the historical
//! Ready-State backend; legacy Store identity and launch APIs are not reused.

#[cfg(target_os = "linux")]
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use ato_materializer_api::RunnerCapabilities;
#[cfg(target_os = "linux")]
use ato_objects::blake3_reference;
#[cfg(target_os = "linux")]
use tempfile::TempDir;

#[cfg(target_os = "linux")]
use crate::ArtifactRole;
use crate::{
    CapturedVm, VmBackendSession, VmCaptureRequest, VmRestoreRequest, VmSnapshotBackend,
    VmSnapshotError,
};

pub trait ActiveVmCaptureSource: Send + Sync {
    fn capture_active(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError>;
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
            if Path::new("/dev/kvm").exists()
                && Command::new(&self.config.ip_binary)
                    .arg("-Version")
                    .output()
                    .is_ok()
                && let Some(version) = version
            {
                capabilities.backends.insert("firecracker".to_owned());
                capabilities
                    .backend_versions
                    .insert("firecracker".to_owned(), version);
                capabilities.guest_os.insert("linux".to_owned());
                capabilities
                    .snapshot_formats
                    .insert("fc-full-file-v1".to_owned());
                capabilities.device_features.extend([
                    "kvm".to_owned(),
                    "virtio-blk".to_owned(),
                    "content-addressed-rootfs-path-v1".to_owned(),
                ]);
                capabilities.network_features.insert("tap".to_owned());
                capabilities.vsock_features.insert("vsock-uds".to_owned());
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
        let memory = required_artifact(request, ArtifactRole::Memory)?;
        let vmstate = required_artifact(request, ArtifactRole::Vmstate)?;
        resources.child = Some(
            Command::new(&self.config.binary)
                .arg("--api-sock")
                .arg(&resources.api_socket)
                .stdin(Stdio::null())
                .stdout(Stdio::from(resources.console.try_clone()?))
                .stderr(Stdio::from(resources.console.try_clone()?))
                .spawn()
                .map_err(|error| VmSnapshotError::Backend(format!("Firecracker spawn: {error}")))?,
        );
        wait_for_socket(
            &resources.api_socket,
            resources.child.as_mut().expect("child was stored"),
            self.config.api_timeout,
        )?;
        let body = serde_json::json!({
            "snapshot_path": vmstate,
            "mem_backend": {
                "backend_type": "File",
                "backend_path": memory,
            },
            "resume_vm": true,
            "enable_diff_snapshots": false,
        });
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
    child: Option<Child>,
    tap_created: Option<String>,
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
            let api_socket = session_dir.path().join("api.sock");
            let console = File::create(session_dir.path().join("console.log"))?;
            Ok::<_, VmSnapshotError>((session_dir, api_socket, console))
        })();
        let (session_dir, api_socket, console) = match allocated_session {
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
            child: None,
            tap_created: None,
            vsock_path: None,
            slot_path,
            slot_file: Some(slot_file),
        };
        if let Some(tap) = &request.descriptor.network_contract.tap_device {
            command_ok(
                &config.ip_binary,
                &["tuntap", "add", "dev", tap, "mode", "tap"],
                "create TAP",
            )?;
            resources.tap_created = Some(tap.clone());
            command_ok(
                &config.ip_binary,
                &["link", "set", "dev", tap, "up"],
                "activate TAP",
            )?;
        }
        if let Some(relative) = &request.descriptor.vsock_contract.uds_path {
            let path = config.work_root.join(relative);
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
        materialize_stable_rootfs(config, request)?;
        Ok(resources)
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
        if let Some(tap) = &self.tap_created {
            let _ = Command::new(&self.config.ip_binary)
                .args(["link", "del", tap])
                .status();
        }
        self.tap_created = None;
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
fn materialize_stable_rootfs(
    config: &FirecrackerBackendConfig,
    request: &VmRestoreRequest<'_>,
) -> Result<(), VmSnapshotError> {
    let source = required_artifact(request, ArtifactRole::Rootfs)?;
    let artifact = request
        .descriptor
        .artifacts
        .iter()
        .find(|artifact| artifact.role == ArtifactRole::Rootfs)
        .ok_or_else(|| VmSnapshotError::Backend("rootfs descriptor missing".to_owned()))?;
    let identity = blake3_reference(&serde_jcs::to_vec(artifact)?);
    let root = config.work_root.join("rootfs");
    fs::create_dir_all(&root)?;
    let destination = root.join(format!("{}.ext4", identity.digest()));
    if !destination.exists() {
        let temporary = root.join(format!("{}.partial", identity.digest()));
        fs::copy(&source, &temporary)?;
        File::open(&temporary)?.sync_all()?;
        fs::rename(temporary, destination)?;
    }
    Ok(())
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
}
