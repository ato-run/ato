//! `FirecrackerBackend` — real x86_64 implementation (M0 GO, 2026-06-29).
//!
//! Drives Firecracker over its REST API (a unix socket) to build and restore
//! Ready-State microVM snapshots behind the [`SnapshotBackend`] contract.
//!
//! Scope (deliberate, see the implementation plan §6.1):
//! * **x86_64 only** — M0 validated x86_64; aarch64 is a separate KVM pass.
//! * **File memory backend only** — UFFD is unsupported / fail-closed.
//! * **No GPU** — the GPU fail-closed guard lives in the orchestration seam.
//! * **Single-session** — a Firecracker snapshot bakes in its tap name + guest
//!   IP, so all restores of one snapshot share that network config; this backend
//!   serializes build/restore on a per-tap lockfile. True concurrency needs a
//!   per-session network namespace (future work), not just unique tap names.
//!
//! Disk model (Ready-State: read-only base + disposable overlay):
//! * The rootfs is stored at a **content-addressed, stable path**
//!   (`<work>/rootfs/<blake3>.ext4`) so the path Firecracker records in the
//!   snapshot exists at restore (a snapshot contains memory + device *state*,
//!   never disk bytes). It is mounted **read-only** by default — immutable and
//!   shared across restores, so a disk mutation can never leak between sessions.
//!   With `ATO_FC_ROOTFS_READONLY=0` the rootfs is instead rewritten fresh from
//!   CAS on every restore (still leak-safe, slower). Session-mutable state lives
//!   in guest RAM (private per restore via the File mem mmap); a writable scratch
//!   drive is a documented future increment.
//!
//! Layers: the **rootfs** layer is the bootable disk. `runtime`/`dependency`/
//! `app` are expected to be **baked into the rootfs at capsule build** — here
//! they are content-addressed, no-secret-scanned, and recorded in the manifest
//! for provenance/dedup, but they are **not** mounted as separate drives (the
//! booted VM sees only the rootfs). `vmstate`/`memory` are *produced* by the
//! snapshot, not consumed from the input. (Separate runtime/dep/app drives are a
//! future increment if a capsule needs them mounted independently.)
//!
//! Privilege: Firecracker needs `/dev/kvm` (group `kvm`) and a TAP
//! (`CAP_NET_ADMIN`). This backend shells out to `firecracker`/`ip` directly and
//! does **not** embed `sudo`; the hosting process must hold the caps.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use capsule::foundation::install_lifecycle::RunnerClassFacts;
use capsulefs::{
    BlobManifest, CasStore, ChunkingKind, HotsetRecorder, LayerKind, LazyBlobReader,
    MEMORY_PAGE_CHUNK_SIZE, store_blob,
};
use serde_json::json;

use crate::backend::{
    BackendCapabilities, BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    TeardownReceipt,
};
use crate::manifest::{
    NoSecretProof, ReadyStateLayers, ReadyStateManifest, RestoreContract, SnapshotBackendInfo,
    READY_STATE_SCHEMA,
};
use crate::scanner;

pub const FIRECRACKER_BACKEND_ID: &str = "firecracker";
const KVM_DEVICE: &str = "/dev/kvm";
const SNAPSHOT_FORMAT: &str = "fc-full-file-v1";
const DEVICE_PROFILE: &str = "virtio-blk+virtio-net+vsock";
const NETWORK_MODEL: &str = "tap";

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Backend configuration (env-overridable).
#[derive(Debug, Clone)]
pub struct FirecrackerConfig {
    pub firecracker_bin: String,
    pub kernel_path: PathBuf,
    pub base_rootfs_path: Option<PathBuf>,
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
    pub rootfs_read_only: bool,
    pub tap_dev: String,
    pub host_ip: String,
    pub guest_ip: String,
    pub guest_mask: String,
    pub healthcheck_port: u16,
    pub healthcheck_path: String,
    pub work_root: PathBuf,
    pub cpu_template: Option<String>,
    pub boot_timeout: Duration,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: env_or("ATO_FC_BIN", "firecracker"),
            kernel_path: PathBuf::from(env_or("ATO_FC_KERNEL", "vmlinux")),
            base_rootfs_path: std::env::var("ATO_FC_BASE_ROOTFS").ok().filter(|v| !v.is_empty()).map(PathBuf::from),
            vcpu_count: env_or("ATO_FC_VCPUS", "2").parse().unwrap_or(2),
            mem_size_mib: env_or("ATO_FC_MEM_MIB", "512").parse().unwrap_or(512),
            rootfs_read_only: env_or("ATO_FC_ROOTFS_READONLY", "1") != "0",
            tap_dev: env_or("ATO_FC_TAP", "fctap0"),
            host_ip: env_or("ATO_FC_HOST_IP", "172.16.0.1"),
            guest_ip: env_or("ATO_FC_GUEST_IP", "172.16.0.2"),
            guest_mask: env_or("ATO_FC_GUEST_MASK", "255.255.255.0"),
            healthcheck_port: env_or("ATO_FC_HEALTH_PORT", "8080").parse().unwrap_or(8080),
            healthcheck_path: env_or("ATO_FC_HEALTH_PATH", "/health"),
            work_root: PathBuf::from(env_or("ATO_FC_WORK", "/tmp/ato-fc")),
            cpu_template: std::env::var("ATO_FC_CPU_TEMPLATE").ok().filter(|v| !v.is_empty()),
            boot_timeout: Duration::from_secs(env_or("ATO_FC_BOOT_TIMEOUT_S", "30").parse().unwrap_or(30)),
        }
    }
}

/// Firecracker microVM snapshot backend.
#[derive(Debug, Clone, Default)]
pub struct FirecrackerBackend {
    config: FirecrackerConfig,
    /// Live restored sessions (session_id → VMM child), so `stop()` can
    /// kill **and reap** the process it spawned (not just `kill -9` a pid).
    sessions: Arc<Mutex<HashMap<String, Child>>>,
}

impl FirecrackerBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: FirecrackerConfig) -> Self {
        Self { config, sessions: Arc::default() }
    }

    pub fn kvm_present() -> bool {
        Path::new(KVM_DEVICE).exists()
    }

    fn detect_version(&self) -> Option<String> {
        let out = Command::new(&self.config.firecracker_bin).arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for tok in text.split_whitespace() {
            if let Some(v) = tok.strip_prefix('v') && v.split('.').count() >= 2 {
                return Some(v.to_string());
            }
        }
        None
    }

    fn backend_err(&self, reason: impl Into<String>) -> SnapshotError {
        SnapshotError::Backend { backend: FIRECRACKER_BACKEND_ID.to_string(), reason: reason.into() }
    }
    fn unsupported(&self, reason: impl Into<String>) -> SnapshotError {
        SnapshotError::Unsupported { backend: FIRECRACKER_BACKEND_ID.to_string(), reason: reason.into() }
    }

    fn ensure_available(&self) -> Result<(), SnapshotError> {
        if !Self::kvm_present() {
            return Err(self.unsupported(format!("{KVM_DEVICE} not present; Firecracker needs KVM")));
        }
        if self.detect_version().is_none() {
            return Err(self.unsupported(format!("firecracker binary '{}' not found or not runnable", self.config.firecracker_bin)));
        }
        Ok(())
    }

    fn runner_facts(&self) -> RunnerClassFacts {
        let mut f = RunnerClassFacts::from_host();
        f.vmm = FIRECRACKER_BACKEND_ID.to_string();
        f.vmm_version = self.detect_version().unwrap_or_else(|| "unknown".to_string());
        f.snapshot_format = SNAPSHOT_FORMAT.to_string();
        f.cpu_template = self.config.cpu_template.clone();
        f.guest_kernel_id = blake3_file(&self.config.kernel_path).unwrap_or_else(|| "unset".to_string());
        f.rootfs_base_id = self.config.base_rootfs_path.as_ref().and_then(|p| blake3_file(p)).unwrap_or_else(|| "unset".to_string());
        f.device_profile = DEVICE_PROFILE.to_string();
        f.network_model = NETWORK_MODEL.to_string();
        f
    }

    fn backend_info(&self) -> SnapshotBackendInfo {
        SnapshotBackendInfo {
            kind: FIRECRACKER_BACKEND_ID.to_string(),
            version: self.detect_version().unwrap_or_else(|| "unknown".to_string()),
            snapshot_format_version: SNAPSHOT_FORMAT.to_string(),
            cpu_template: self.config.cpu_template.clone(),
        }
    }

    fn boot_args(&self) -> String {
        format!(
            "console=ttyS0 reboot=k panic=1 pci=off ip={}::{}:{}::eth0:off",
            self.config.guest_ip, self.config.host_ip, self.config.guest_mask
        )
    }

    fn rootfs_dir(&self) -> PathBuf {
        self.config.work_root.join("rootfs")
    }
    /// Stable, content-addressed path the snapshot records for the rootfs drive.
    fn rootfs_path_for(&self, bytes: &[u8]) -> PathBuf {
        self.rootfs_dir().join(format!("{}.ext4", blake3::hash(bytes).to_hex()))
    }
    fn lock_path(&self) -> PathBuf {
        self.config.work_root.join(format!("{}.lock", self.config.tap_dev))
    }
}

// ── single-session lock (the shared tap admits one VMM at a time) ────────────
struct BuildLock {
    path: PathBuf,
}
impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl FirecrackerBackend {
    fn acquire_lock(&self, owner: &str) -> Result<(), SnapshotError> {
        std::fs::create_dir_all(&self.config.work_root).map_err(|e| self.backend_err(e.to_string()))?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(self.lock_path()) {
            Ok(mut f) => { let _ = f.write_all(owner.as_bytes()); Ok(()) }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(self.backend_err(format!(
                "single-session backend busy: tap '{}' is held by another session (lock {})",
                self.config.tap_dev, self.lock_path().display()
            ))),
            Err(e) => Err(self.backend_err(format!("acquire lock: {e}"))),
        }
    }
    fn release_lock(&self) {
        let _ = std::fs::remove_file(self.lock_path());
    }

    fn run_ip(&self, args: &[&str]) -> Result<(), SnapshotError> {
        let status = Command::new("ip").args(args).status()
            .map_err(|e| self.backend_err(format!("spawn `ip {}`: {e}", args.join(" "))))?;
        if status.success() { Ok(()) } else { Err(self.backend_err(format!("`ip {}` failed", args.join(" ")))) }
    }
    fn net_up(&self) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let _ = Command::new("ip").args(["link", "del", tap]).status();
        self.run_ip(&["tuntap", "add", "dev", tap, "mode", "tap"])?;
        self.run_ip(&["addr", "add", &format!("{}/24", self.config.host_ip), "dev", tap])?;
        self.run_ip(&["link", "set", tap, "up"])?;
        Ok(())
    }
    fn net_down(&self) {
        let _ = Command::new("ip").args(["link", "del", &self.config.tap_dev]).status();
    }

    fn start_fc(&self, sock: &Path, console_log: &Path) -> Result<FcProcess, SnapshotError> {
        let _ = std::fs::remove_file(sock);
        let log = std::fs::File::create(console_log).map_err(|e| self.backend_err(format!("create console log: {e}")))?;
        let child = Command::new(&self.config.firecracker_bin)
            .arg("--api-sock").arg(sock)
            .stdout(Stdio::from(log.try_clone().map_err(|e| self.backend_err(e.to_string()))?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| self.backend_err(format!("spawn firecracker: {e}")))?;
        let mut fc = FcProcess { child: Some(child), sock: sock.to_path_buf() };
        for _ in 0..100 {
            if sock.exists() { return Ok(fc); }
            std::thread::sleep(Duration::from_millis(50));
        }
        fc.kill_now();
        Err(self.backend_err("firecracker api socket never appeared"))
    }

    fn configure_boot(&self, fc: &FcProcess, kernel: &Path, rootfs: &Path, read_only: bool) -> Result<(), SnapshotError> {
        let mc = if let Some(t) = &self.config.cpu_template {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib, "cpu_template": t})
        } else {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib})
        };
        fc.api(self, "PUT", "/machine-config", Some(&mc.to_string()))?;
        fc.api(self, "PUT", "/boot-source", Some(&json!({
            "kernel_image_path": kernel.to_string_lossy(), "boot_args": self.boot_args()
        }).to_string()))?;
        fc.api(self, "PUT", "/drives/rootfs", Some(&json!({
            "drive_id": "rootfs", "path_on_host": rootfs.to_string_lossy(),
            "is_root_device": true, "is_read_only": read_only
        }).to_string()))?;
        fc.api(self, "PUT", "/network-interfaces/eth0", Some(&json!({
            "iface_id": "eth0", "host_dev_name": self.config.tap_dev
        }).to_string()))?;
        Ok(())
    }

    /// Poll the guest healthcheck (contract-driven port/path) until ready.
    fn wait_health(&self, port: u16, path: &str) -> Result<u128, SnapshotError> {
        let addr: std::net::SocketAddr = format!("{}:{}", self.config.guest_ip, port)
            .parse().map_err(|e| self.backend_err(format!("bad guest addr: {e}")))?;
        let start = Instant::now();
        while start.elapsed() < self.config.boot_timeout {
            if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let req = format!("GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", self.config.guest_ip);
                let mut buf = [0u8; 32];
                if s.write_all(req.as_bytes()).is_ok()
                    && let Ok(n) = s.read(&mut buf)
                    && n > 0
                    && String::from_utf8_lossy(&buf[..n]).contains(" 200")
                {
                    return Ok(start.elapsed().as_millis());
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(self.backend_err("guest never became healthy within timeout"))
    }

    fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p).map_err(|e| self.backend_err(e.to_string()))?; }
        std::fs::write(path, bytes).map_err(|e| self.backend_err(format!("write {}: {e}", path.display())))
    }
}

fn hc_port(c: &RestoreContract, fallback: u16) -> u16 {
    c.ports.first().copied().unwrap_or(fallback)
}
fn hc_path(c: &RestoreContract, fallback: &str) -> String {
    c.healthcheck.clone().unwrap_or_else(|| fallback.to_string())
}

fn blake3_file(path: &Path) -> Option<String> {
    Some(format!("blake3:{}", blake3::hash(&std::fs::read(path).ok()?).to_hex()))
}

// ── HTTP/1.1 over the Firecracker API unix socket (no extra deps) ────────────
//
// Firecracker's micro-http server keeps the connection alive, so we must NOT
// read to EOF (that blocks until the read timeout). Read exactly the status line
// + headers (until CRLFCRLF), then `Content-Length` body bytes, then stop.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn fc_request(sock: &Path, method: &str, path: &str, body: Option<&str>) -> std::io::Result<(u16, String)> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 2048];
    // 1) read until end of headers.
    let header_end = loop {
        if let Some(p) = find_subslice(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 256 * 1024 {
            break buf.len();
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end.min(buf.len())]).into_owned();
    let status = headers.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0u16);
    let content_length = headers
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().ok()))
        .flatten()
        .unwrap_or(0);
    // 2) read exactly the declared body, then stop (don't wait for close).
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok((status, String::from_utf8_lossy(&buf).into_owned()))
}

/// A spawned Firecracker process. RAII: dropping it kills+reaps the VMM and
/// removes the socket (covers every error path). On success the caller either
/// kills it explicitly (build's transient VM) or `detach()`es the live VM into
/// the session registry (restore).
struct FcProcess {
    child: Option<Child>,
    sock: PathBuf,
}
impl FcProcess {
    fn api(&self, b: &FirecrackerBackend, method: &str, path: &str, body: Option<&str>) -> Result<(), SnapshotError> {
        let (status, text) = fc_request(&self.sock, method, path, body)
            .map_err(|e| b.backend_err(format!("api {method} {path}: {e}")))?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(b.backend_err(format!("api {method} {path} -> HTTP {status}: {}", text.lines().last().unwrap_or(""))))
        }
    }
    fn kill_now(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(&self.sock);
    }
    /// Hand the live VMM child to the caller (registry); the socket stays.
    fn detach(mut self) -> Option<Child> {
        self.child.take()
    }
}
impl Drop for FcProcess {
    fn drop(&mut self) {
        self.kill_now();
    }
}

impl SnapshotBackend for FirecrackerBackend {
    fn id(&self) -> &str {
        FIRECRACKER_BACKEND_ID
    }

    fn probe(&self) -> BackendCapabilities {
        let kvm = Self::kvm_present();
        let version = self.detect_version();
        let available = kvm && version.is_some();
        let reason = if !kvm {
            Some(format!("{KVM_DEVICE} not present; Firecracker needs KVM"))
        } else if version.is_none() {
            Some(format!("firecracker binary '{}' not found", self.config.firecracker_bin))
        } else {
            None
        };
        BackendCapabilities {
            backend_id: FIRECRACKER_BACKEND_ID.to_string(),
            available,
            reason,
            arch: std::env::consts::ARCH.to_string(),
            kvm_present: kvm,
            vmm_version: version,
            snapshot_kind: SnapshotKind::MicroVm,
            memory_snapshot: true,
            filesystem_model: FilesystemModel::Block,
            device_profile: DeviceProfile::Minimal,
            gpu_mode: GpuMode::None,
            oci_native: false,
            isolation_boundary: IsolationBoundary::MicroVm,
            supports_seal_before_bind: true,
            supports_disposable_overlay: true,
        }
    }

    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        self.ensure_available()?;
        self.acquire_lock("build")?;
        let _lock = BuildLock { path: self.lock_path() };
        std::fs::create_dir_all(&self.config.work_root).map_err(|e| self.backend_err(e.to_string()))?;
        let build_dir = self.config.work_root.join(format!("build-{}", std::process::id()));
        std::fs::create_dir_all(&build_dir).map_err(|e| self.backend_err(e.to_string()))?;

        // rootfs at its stable content-addressed path (kept for restore; the
        // snapshot will record THIS path for the block device).
        let rootfs_path = self.rootfs_path_for(&input.layers.rootfs);
        if !rootfs_path.exists() {
            self.write_file(&rootfs_path, &input.layers.rootfs)?;
        }
        let vmstate_path = build_dir.join("vmstate");
        let mem_path = build_dir.join("mem");
        let port = hc_port(&input.restore_contract, self.config.healthcheck_port);
        let path = hc_path(&input.restore_contract, &self.config.healthcheck_path);

        self.net_up()?;
        let snap = (|| -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
            let fc = self.start_fc(&build_dir.join("api.sock"), &build_dir.join("console.log"))?;
            self.configure_boot(&fc, &self.config.kernel_path, &rootfs_path, self.config.rootfs_read_only)?;
            fc.api(self, "PUT", "/actions", Some(&json!({"action_type":"InstanceStart"}).to_string()))?;
            self.wait_health(port, &path)?; // secret-free seal point
            fc.api(self, "PATCH", "/vm", Some(&json!({"state":"Paused"}).to_string()))?;
            fc.api(self, "PUT", "/snapshot/create", Some(&json!({
                "snapshot_type":"Full",
                "snapshot_path": vmstate_path.to_string_lossy(),
                "mem_file_path": mem_path.to_string_lossy()
            }).to_string()))?;
            let vmstate = std::fs::read(&vmstate_path).map_err(|e| self.backend_err(format!("read vmstate: {e}")))?;
            let mem = std::fs::read(&mem_path).map_err(|e| self.backend_err(format!("read mem: {e}")))?;
            Ok((vmstate, mem)) // fc drops here → killed+reaped
        })();
        self.net_down();
        let (vmstate, mem) = match snap {
            Ok(v) => v,
            Err(e) => { let _ = std::fs::remove_dir_all(&build_dir); return Err(e); }
        };

        // ── no-secret gate over all sealed layers (high-entropy advisory) ────
        let sealed = BuildLayers {
            rootfs: input.layers.rootfs.clone(),
            runtime: input.layers.runtime.clone(),
            dependency: input.layers.dependency.clone(),
            app: input.layers.app.clone(),
            vmstate: vmstate.clone(),
            memory: mem.clone(),
        };
        let report = scanner::scan_build_layers(&sealed, &input.declared_secret_markers);
        if !report.declared_hits.is_empty() {
            let _ = std::fs::remove_dir_all(&build_dir);
            return Err(SnapshotError::SecretFoundInSnapshot(report.declared_hits));
        }
        let blocking = report.blocking();
        if !blocking.is_empty() {
            let _ = std::fs::remove_dir_all(&build_dir);
            return Err(SnapshotError::SecretScanFindings(blocking.into_iter().cloned().collect()));
        }
        let advisories: Vec<String> = report.advisory().iter().map(|f| f.summary()).collect();

        let cd = ChunkingKind::ContentDefined;
        let page = ChunkingKind::PageAligned { page_size: MEMORY_PAGE_CHUNK_SIZE as u64 };
        let seal = |kind: LayerKind, bytes: Option<&[u8]>, ch: ChunkingKind| -> Result<Option<BlobManifest>, SnapshotError> {
            match bytes { Some(b) => Ok(Some(store_blob(input.store, kind, b, ch)?)), None => Ok(None) }
        };
        let layers = ReadyStateLayers {
            rootfs: seal(LayerKind::Rootfs, Some(&input.layers.rootfs), cd)?,
            runtime: seal(LayerKind::Runtime, input.layers.runtime.as_deref(), cd)?,
            dependency: seal(LayerKind::Dependency, input.layers.dependency.as_deref(), cd)?,
            app: seal(LayerKind::App, input.layers.app.as_deref(), cd)?,
            vmstate: seal(LayerKind::VmState, Some(&vmstate), cd)?,
            memory: seal(LayerKind::Memory, Some(&mem), page)?,
        };
        let mut rec = HotsetRecorder::new();
        if let Some(m) = &layers.memory { rec.extend_from_manifest(m); }
        if let Some(r) = &layers.rootfs { rec.extend_from_manifest(r); }
        let hotset_profile = rec.finish();

        let no_secret_proof = NoSecretProof {
            scanner_version: scanner::SCANNER_VERSION.to_string(),
            scanned_layers: layers.iter().map(|(n, _)| n.to_string()).collect(),
            findings: Vec::new(),
            advisories,
            verdict: "clean".to_string(),
        };
        let sealed_bytes = layers.iter().map(|(_, m)| m.total_len).sum();
        let runner_class_id = Some(input.runner_class.unwrap_or_else(|| self.runner_facts().id()));
        let manifest = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: input.capsule_manifest_hash,
            runner_class_id,
            execution_id: None,
            layers,
            hotset_profile,
            snapshot_backend: self.backend_info(),
            restore_contract: input.restore_contract,
            sanitizer_contract: input.sanitizer_contract,
            no_secret_proof: Some(no_secret_proof.clone()),
            build_receipt_id: None,
        };
        let _ = std::fs::remove_dir_all(&build_dir);
        Ok(BuildReadyStateReceipt { manifest, sealed_bytes, no_secret_proof })
    }

    fn inspect(
        &self,
        store: &CasStore,
        manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError> {
        let mut all = true;
        for (_, blob) in manifest.layers.iter() {
            for c in &blob.chunks {
                if !store.has_chunk(&c.hash) { all = false; }
            }
        }
        Ok(SnapshotInspection {
            manifest_id: manifest.id(),
            backend_kind: manifest.snapshot_backend.kind.clone(),
            layers: manifest.layers.iter().map(|(n, _)| n.to_string()).collect(),
            total_bytes: manifest.total_layer_bytes(),
            all_chunks_present: all,
        })
    }

    fn restore(
        &self,
        input: RestoreReadyStateInput<'_>,
    ) -> Result<RestoreReceipt, SnapshotError> {
        self.ensure_available()?;
        // ── runner-class gate (fail-closed) ──────────────────────────────────
        let host_class = input.host_runner_class.unwrap_or_else(|| self.runner_facts().id());
        if let Some(expected) = &input.manifest.runner_class_id
            && expected != &host_class
        {
            return Err(SnapshotError::RunnerClassMismatch(
                capsule::foundation::install_lifecycle::RunnerClassMismatch {
                    expected: expected.clone(),
                    actual: host_class,
                    first_divergent_field: "runner_class_id".to_string(),
                },
            ));
        }

        let rootfs = input.manifest.layers.rootfs.as_ref().ok_or_else(|| self.backend_err("manifest has no rootfs layer"))?;
        let vmstate = input.manifest.layers.vmstate.as_ref().ok_or_else(|| self.backend_err("manifest has no vmstate layer"))?;
        let memory = input.manifest.layers.memory.as_ref().ok_or_else(|| self.backend_err("manifest has no memory layer"))?;

        self.acquire_lock("restore")?;
        // From here, on any error we must release the lock + net before returning.
        let result = (|| -> Result<(RestoredSession, Child), SnapshotError> {
            std::fs::create_dir_all(&input.overlay_root).map_err(|e| self.backend_err(e.to_string()))?;
            let mut restored_bytes = 0u64;
            let mut rehydrate = |blob: &BlobManifest| -> Result<Vec<u8>, SnapshotError> {
                let bytes = LazyBlobReader::new(input.store, blob).read_all()?;
                restored_bytes += bytes.len() as u64;
                Ok(bytes)
            };
            let rootfs_bytes = rehydrate(rootfs)?;
            let vmstate_bytes = rehydrate(vmstate)?;
            let mem_bytes = rehydrate(memory)?;

            // rootfs must be at the SAME content-addressed path the snapshot
            // recorded. Read-only: reuse the shared immutable copy (leak-safe by
            // immutability). Read-write: rewrite a fresh copy per restore (single
            // session ⇒ no overlap; fresh ⇒ leak-safe).
            let rootfs_path = self.rootfs_path_for(&rootfs_bytes);
            if !self.config.rootfs_read_only || !rootfs_path.exists() {
                self.write_file(&rootfs_path, &rootfs_bytes)?;
            }
            let vmstate_path = input.overlay_root.join("vmstate");
            let mem_path = input.overlay_root.join("mem");
            self.write_file(&vmstate_path, &vmstate_bytes)?;
            self.write_file(&mem_path, &mem_bytes)?;

            let port = hc_port(&input.manifest.restore_contract, self.config.healthcheck_port);
            let path = hc_path(&input.manifest.restore_contract, &self.config.healthcheck_path);

            self.net_up()?;
            let fc = self.start_fc(&input.overlay_root.join("api.sock"), &input.overlay_root.join("console.log"))?;
            fc.api(self, "PUT", "/snapshot/load", Some(&json!({
                "snapshot_path": vmstate_path.to_string_lossy(),
                "mem_backend": {"backend_type":"File","backend_path": mem_path.to_string_lossy()},
                "resume_vm": true
            }).to_string()))?;
            self.wait_health(port, &path)?;

            let session_id = format!("fc-{}-{}", manifest_short(&input.manifest), std::process::id());
            let child = fc.detach().ok_or_else(|| self.backend_err("lost firecracker child after restore"))?;
            let _ = std::fs::write(input.overlay_root.join(".fc-session.json"), json!({
                "pid": child.id(), "tap": self.config.tap_dev, "session_id": session_id
            }).to_string());
            let session = RestoredSession {
                session_id,
                backend_id: FIRECRACKER_BACKEND_ID.to_string(),
                guest_port: Some(port),
                overlay_root: input.overlay_root.clone(),
                restored_bytes,
            };
            Ok((session, child))
        })();

        match result {
            Ok((session, child)) => {
                self.sessions.lock().unwrap().insert(session.session_id.clone(), child);
                // lock + tap intentionally held for the live session (released by stop()).
                Ok(RestoreReceipt { ready_state_manifest_id: input.manifest.id(), session })
            }
            Err(e) => {
                self.net_down();
                self.release_lock();
                Err(e)
            }
        }
    }

    fn stop(&self, session: RestoredSession) -> Result<TeardownReceipt, SnapshotError> {
        // Primary: kill + reap the child we spawned (no zombie).
        let reaped = if let Some(mut child) = self.sessions.lock().unwrap().remove(&session.session_id) {
            let _ = child.kill();
            let _ = child.wait();
            true
        } else {
            // Fallback (cross-instance stop): kill by recorded pid (best effort).
            let meta = std::fs::read_to_string(session.overlay_root.join(".fc-session.json")).unwrap_or_default();
            if let Some(pid) = json_u32(&meta, "pid") {
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
            false
        };
        let _ = reaped;
        self.net_down();
        self.release_lock();
        let overlay_removed = session.overlay_root.exists() && std::fs::remove_dir_all(&session.overlay_root).is_ok();
        Ok(TeardownReceipt { session_id: session.session_id, overlay_removed })
    }
}

fn manifest_short(m: &ReadyStateManifest) -> String {
    m.id().strip_prefix("blake3:").unwrap_or("000000").chars().take(12).collect()
}
fn json_u32(s: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\":");
    let i = s.find(&needle)? + needle.len();
    let rest = s[i..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_facets_and_availability_matches_host() {
        let p = FirecrackerBackend::new().probe();
        assert_eq!(p.backend_id, FIRECRACKER_BACKEND_ID);
        assert_eq!(p.snapshot_kind, SnapshotKind::MicroVm);
        assert!(p.memory_snapshot);
        assert_eq!(p.filesystem_model, FilesystemModel::Block);
        assert_eq!(p.gpu_mode, GpuMode::None);
        assert!(p.supports_seal_before_bind);
        let expect = FirecrackerBackend::kvm_present() && FirecrackerBackend::new().detect_version().is_some();
        assert_eq!(p.available, expect);
        if !p.available { assert!(p.reason.is_some()); }
    }

    #[test]
    fn restore_is_unsupported_without_kvm() {
        if FirecrackerBackend::kvm_present() { return; }
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = err_manifest();
        assert!(FirecrackerBackend::new().inspect(&store, &m).is_ok()); // inspect needs no KVM
        let backend = FirecrackerBackend::new();
        let input = RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov"), host_runner_class: None };
        assert!(matches!(backend.restore(input), Err(SnapshotError::Unsupported { .. })));
    }

    #[test]
    fn config_reads_defaults() {
        let c = FirecrackerConfig::default();
        assert_eq!(c.vcpu_count, 2);
        assert_eq!(c.healthcheck_port, 8080);
        // rootfs_read_only defaults to true, but honors ATO_FC_ROOTFS_READONLY
        // (the KVM integration run sets =0), so assert it matches the env rather
        // than a fixed value.
        let expect_ro = std::env::var("ATO_FC_ROOTFS_READONLY").map(|v| v != "0").unwrap_or(true);
        assert_eq!(c.rootfs_read_only, expect_ro);
    }

    #[test]
    fn json_u32_parses() {
        assert_eq!(json_u32("{\"pid\":12345,\"tap\":\"x\"}", "pid"), Some(12345));
        assert_eq!(json_u32("{\"pid\": 7 }", "pid"), Some(7));
        assert_eq!(json_u32("{\"tap\":\"x\"}", "pid"), None);
    }

    fn err_manifest() -> ReadyStateManifest {
        use crate::manifest::{RestoreContract, SanitizerContract};
        ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:x".to_string(),
            runner_class_id: None,
            execution_id: None,
            layers: ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: SnapshotBackendInfo { kind: FIRECRACKER_BACKEND_ID.to_string(), version: "0".to_string(), snapshot_format_version: SNAPSHOT_FORMAT.to_string(), cpu_template: None },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
        }
    }
}

#[cfg(test)]
mod kvm_tests;
