//! `FirecrackerBackend` — real x86_64 implementation (M0 GO, 2026-06-29).
//!
//! Drives Firecracker over its REST API (a unix socket) to build and restore
//! Ready-State microVM snapshots behind the [`SnapshotBackend`] contract. Scope
//! (deliberate, see the implementation plan §6.1):
//!
//! * **x86_64 only** — M0 validated x86_64; aarch64 is a separate KVM pass.
//! * **File memory backend only** — UFFD is unsupported / fail-closed.
//! * **No GPU** — the GPU fail-closed guard lives in the orchestration seam.
//! * Additive: when `/dev/kvm` or the `firecracker` binary is missing, `probe()`
//!   reports unavailable and every op fails closed with [`SnapshotError`]; the
//!   legacy cold path is unaffected.
//!
//! Privilege: Firecracker needs `/dev/kvm` (group `kvm`) and a TAP device
//! (`CAP_NET_ADMIN`). This backend shells out to `firecracker` and `ip` directly
//! — it does **not** embed `sudo`; the hosting process must have the caps (the
//! KVM-gated tests are run as root). Config is env-overridable so a runner /
//! integration test can point at its kernel, tap, and IPs.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use capsule::foundation::install_lifecycle::RunnerClassFacts;
use capsulefs::{
    BlobManifest, CasStore, ChunkingKind, HotsetRecorder, LayerKind, LazyBlobReader,
    MEMORY_PAGE_CHUNK_SIZE, store_blob,
};

use crate::backend::{
    BackendCapabilities, BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    TeardownReceipt,
};
use crate::manifest::{
    NoSecretProof, ReadyStateLayers, ReadyStateManifest, SnapshotBackendInfo, READY_STATE_SCHEMA,
};
use crate::scanner;

/// Backend id reported by [`FirecrackerBackend`].
pub const FIRECRACKER_BACKEND_ID: &str = "firecracker";
const KVM_DEVICE: &str = "/dev/kvm";
/// Our label for the snapshot wire format this backend produces/consumes
/// (Firecracker full snapshot + File memory backend). Folds into runner_class.
const SNAPSHOT_FORMAT: &str = "fc-full-file-v1";
const DEVICE_PROFILE: &str = "virtio-blk+virtio-net+vsock";
const NETWORK_MODEL: &str = "tap";

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Backend configuration (env-overridable). build/restore need the kernel +
/// network config; `probe()` needs only the binary.
#[derive(Debug, Clone)]
pub struct FirecrackerConfig {
    pub firecracker_bin: String,
    pub kernel_path: PathBuf,
    pub base_rootfs_path: Option<PathBuf>,
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
    pub tap_dev: String,
    pub host_ip: String,
    pub guest_ip: String,
    pub guest_mask: String,
    pub healthcheck_port: u16,
    pub healthcheck_path: String,
    pub host_iface: String,
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
            tap_dev: env_or("ATO_FC_TAP", "fctap0"),
            host_ip: env_or("ATO_FC_HOST_IP", "172.16.0.1"),
            guest_ip: env_or("ATO_FC_GUEST_IP", "172.16.0.2"),
            guest_mask: env_or("ATO_FC_GUEST_MASK", "255.255.255.0"),
            healthcheck_port: env_or("ATO_FC_HEALTH_PORT", "8080").parse().unwrap_or(8080),
            healthcheck_path: env_or("ATO_FC_HEALTH_PATH", "/health"),
            host_iface: env_or("ATO_FC_HOST_IFACE", "ens4"),
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
}

impl FirecrackerBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: FirecrackerConfig) -> Self {
        Self { config }
    }

    pub fn kvm_present() -> bool {
        Path::new(KVM_DEVICE).exists()
    }

    /// `firecracker --version` → `Some("1.16.0")`, or `None` if the binary is
    /// absent / unparseable.
    fn detect_version(&self) -> Option<String> {
        let out = Command::new(&self.config.firecracker_bin)
            .arg("--version")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for tok in text.split_whitespace() {
            if let Some(v) = tok.strip_prefix('v') && v.split('.').count() >= 2 {
                return Some(v.to_string());
            }
        }
        None
    }

    fn backend_err(&self, reason: impl Into<String>) -> SnapshotError {
        SnapshotError::Backend {
            backend: FIRECRACKER_BACKEND_ID.to_string(),
            reason: reason.into(),
        }
    }

    fn unsupported(&self, reason: impl Into<String>) -> SnapshotError {
        SnapshotError::Unsupported {
            backend: FIRECRACKER_BACKEND_ID.to_string(),
            reason: reason.into(),
        }
    }

    fn ensure_available(&self) -> Result<(), SnapshotError> {
        if !Self::kvm_present() {
            return Err(self.unsupported(format!("{KVM_DEVICE} not present; Firecracker needs KVM")));
        }
        if self.detect_version().is_none() {
            return Err(self.unsupported(format!(
                "firecracker binary '{}' not found or not runnable",
                self.config.firecracker_bin
            )));
        }
        Ok(())
    }

    /// Materialize the runner class for this host+backend (plan §5). Starts from
    /// the KVM-free host facts and overrides the backend-supplied facets with
    /// real values so build and restore on the same host agree.
    fn runner_facts(&self) -> RunnerClassFacts {
        let mut f = RunnerClassFacts::from_host();
        f.vmm = FIRECRACKER_BACKEND_ID.to_string();
        f.vmm_version = self.detect_version().unwrap_or_else(|| "unknown".to_string());
        f.snapshot_format = SNAPSHOT_FORMAT.to_string();
        f.cpu_template = self.config.cpu_template.clone();
        f.guest_kernel_id = blake3_file(&self.config.kernel_path).unwrap_or_else(|| "unset".to_string());
        f.rootfs_base_id = self
            .config
            .base_rootfs_path
            .as_ref()
            .and_then(|p| blake3_file(p))
            .unwrap_or_else(|| "unset".to_string());
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
}

// ── tiny HTTP/1.1 client over the Firecracker API unix socket (no deps) ──────
fn fc_request(sock: &Path, method: &str, path: &str, body: Option<&str>) -> std::io::Result<(u16, String)> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0u16);
    Ok((status, text))
}

fn blake3_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

/// A booted/restored Firecracker process + its api socket (RAII-ish; caller
/// kills explicitly via `kill`).
struct FcProcess {
    child: Child,
    sock: PathBuf,
}

impl FcProcess {
    fn api(&self, method: &str, path: &str, body: Option<&str>) -> Result<(), SnapshotError> {
        let (status, text) = fc_request(&self.sock, method, path, body)
            .map_err(|e| SnapshotError::Backend { backend: FIRECRACKER_BACKEND_ID.to_string(), reason: format!("api {method} {path}: {e}") })?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(SnapshotError::Backend {
                backend: FIRECRACKER_BACKEND_ID.to_string(),
                reason: format!("api {method} {path} -> HTTP {status}: {}", text.lines().last().unwrap_or("")),
            })
        }
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.sock);
    }
}

impl FirecrackerBackend {
    fn run_ip(&self, args: &[&str]) -> Result<(), SnapshotError> {
        let status = Command::new("ip").args(args).status()
            .map_err(|e| self.backend_err(format!("spawn `ip {}`: {e}", args.join(" "))))?;
        if status.success() { Ok(()) } else { Err(self.backend_err(format!("`ip {}` failed", args.join(" ")))) }
    }

    fn net_up(&self) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let _ = Command::new("ip").args(["link", "del", tap]).status(); // ignore if absent
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
        let log = std::fs::File::create(console_log)
            .map_err(|e| self.backend_err(format!("create console log: {e}")))?;
        let child = Command::new(&self.config.firecracker_bin)
            .arg("--api-sock").arg(sock)
            .stdout(Stdio::from(log.try_clone().map_err(|e| self.backend_err(e.to_string()))?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| self.backend_err(format!("spawn firecracker: {e}")))?;
        // Wait for the api socket to appear.
        for _ in 0..100 {
            if sock.exists() {
                return Ok(FcProcess { child, sock: sock.to_path_buf() });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = Command::new("kill").arg(child.id().to_string()).status();
        Err(self.backend_err("firecracker api socket never appeared"))
    }

    fn configure_boot(&self, fc: &FcProcess, kernel: &Path, rootfs: &Path) -> Result<(), SnapshotError> {
        fc.api("PUT", "/machine-config", Some(&format!(
            "{{\"vcpu_count\":{},\"mem_size_mib\":{}{}}}",
            self.config.vcpu_count, self.config.mem_size_mib,
            self.config.cpu_template.as_ref().map(|t| format!(",\"cpu_template\":\"{t}\"")).unwrap_or_default()
        )))?;
        fc.api("PUT", "/boot-source", Some(&format!(
            "{{\"kernel_image_path\":\"{}\",\"boot_args\":\"{}\"}}",
            kernel.display(), self.boot_args()
        )))?;
        fc.api("PUT", "/drives/rootfs", Some(&format!(
            "{{\"drive_id\":\"rootfs\",\"path_on_host\":\"{}\",\"is_root_device\":true,\"is_read_only\":false}}",
            rootfs.display()
        )))?;
        fc.api("PUT", "/network-interfaces/eth0", Some(&format!(
            "{{\"iface_id\":\"eth0\",\"host_dev_name\":\"{}\"}}", self.config.tap_dev
        )))?;
        Ok(())
    }

    /// Poll the guest healthcheck (TCP connect, then a minimal HTTP GET) until
    /// it answers or the timeout elapses. Returns ms-to-ready.
    fn wait_health(&self) -> Result<u128, SnapshotError> {
        let addr = format!("{}:{}", self.config.guest_ip, self.config.healthcheck_port);
        let start = Instant::now();
        while start.elapsed() < self.config.boot_timeout {
            if let Ok(mut s) = TcpStream::connect_timeout(
                &addr.parse().map_err(|e| self.backend_err(format!("bad guest addr {addr}: {e}")))?,
                Duration::from_millis(500),
            ) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let req = format!("GET {} HTTP/1.0\r\nHost: {}\r\n\r\n", self.config.healthcheck_path, self.config.guest_ip);
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

    fn write_tmp(&self, dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, SnapshotError> {
        let p = dir.join(name);
        std::fs::write(&p, bytes).map_err(|e| self.backend_err(format!("write {name}: {e}")))?;
        Ok(p)
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
        std::fs::create_dir_all(&self.config.work_root).map_err(|e| self.backend_err(e.to_string()))?;
        let build_dir = self.config.work_root.join(format!("build-{}", std::process::id()));
        std::fs::create_dir_all(&build_dir).map_err(|e| self.backend_err(e.to_string()))?;

        // The rootfs bytes are the bootable ext4 disk; vmstate/memory inputs are
        // ignored (they are PRODUCED by the snapshot below).
        let rootfs_path = self.write_tmp(&build_dir, "rootfs.ext4", &input.layers.rootfs)?;
        let mem_path = build_dir.join("mem");
        let vmstate_path = build_dir.join("vmstate");

        self.net_up()?;
        let result = (|| -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
            let fc = self.start_fc(&build_dir.join("api.sock"), &build_dir.join("console.log"))?;
            self.configure_boot(&fc, &self.config.kernel_path, &rootfs_path)?;
            fc.api("PUT", "/actions", Some("{\"action_type\":\"InstanceStart\"}"))?;
            self.wait_health()?; // boot to readiness (secret-free seal point)
            fc.api("PATCH", "/vm", Some("{\"state\":\"Paused\"}"))?;
            fc.api("PUT", "/snapshot/create", Some(&format!(
                "{{\"snapshot_type\":\"Full\",\"snapshot_path\":\"{}\",\"mem_file_path\":\"{}\"}}",
                vmstate_path.display(), mem_path.display()
            )))?;
            fc.kill();
            let vmstate = std::fs::read(&vmstate_path).map_err(|e| self.backend_err(format!("read vmstate: {e}")))?;
            let mem = std::fs::read(&mem_path).map_err(|e| self.backend_err(format!("read mem: {e}")))?;
            Ok((vmstate, mem))
        })();
        self.net_down();
        let (vmstate, mem) = result?;

        // ── no-secret gate over ALL sealed layers (fs + vmstate + memory) ────
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

        // ── store all layers through CapsuleFS ───────────────────────────────
        let cd = ChunkingKind::ContentDefined;
        let page = ChunkingKind::PageAligned { page_size: MEMORY_PAGE_CHUNK_SIZE as u64 };
        let seal = |kind: LayerKind, bytes: Option<&[u8]>, chunking: ChunkingKind| -> Result<Option<BlobManifest>, SnapshotError> {
            match bytes { Some(b) => Ok(Some(store_blob(input.store, kind, b, chunking)?)), None => Ok(None) }
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
                    actual: host_class.clone(),
                    first_divergent_field: "runner_class_id".to_string(),
                },
            ));
        }

        // ── disposable overlay: fresh per-session dir + writable rootfs copy ──
        std::fs::create_dir_all(&input.overlay_root).map_err(|e| self.backend_err(e.to_string()))?;
        let restored_bytes = std::cell::Cell::new(0u64);
        let rehydrate = |blob: &BlobManifest, name: &str| -> Result<PathBuf, SnapshotError> {
            let bytes = LazyBlobReader::new(input.store, blob).read_all()?;
            restored_bytes.set(restored_bytes.get() + bytes.len() as u64);
            self.write_tmp(&input.overlay_root, name, &bytes)
        };
        let rootfs = input.manifest.layers.rootfs.as_ref()
            .ok_or_else(|| self.backend_err("manifest has no rootfs layer"))?;
        let vmstate = input.manifest.layers.vmstate.as_ref()
            .ok_or_else(|| self.backend_err("manifest has no vmstate layer"))?;
        let memory = input.manifest.layers.memory.as_ref()
            .ok_or_else(|| self.backend_err("manifest has no memory layer"))?;
        let rootfs_path = rehydrate(rootfs, "rootfs.ext4")?;
        let vmstate_path = rehydrate(vmstate, "vmstate")?;
        let mem_path = rehydrate(memory, "mem")?;
        let _ = rootfs_path; // disk is referenced by the snapshot's block device state

        self.net_up()?;
        let sock = input.overlay_root.join("api.sock");
        let start = Instant::now();
        let fc = match self.start_fc(&sock, &input.overlay_root.join("console.log")) {
            Ok(fc) => fc,
            Err(e) => { self.net_down(); return Err(e); }
        };
        let load = fc.api("PUT", "/snapshot/load", Some(&format!(
            "{{\"snapshot_path\":\"{}\",\"mem_backend\":{{\"backend_type\":\"File\",\"backend_path\":\"{}\"}},\"resume_vm\":true}}",
            vmstate_path.display(), mem_path.display()
        )));
        if let Err(e) = load.and_then(|_| self.wait_health().map(|_| ())) {
            // tear down on failure (no orphan)
            let pid = fc.child.id();
            fc.kill();
            let _ = pid;
            self.net_down();
            return Err(e);
        }
        let restore_ms = start.elapsed().as_millis();

        // Persist session metadata so stop() can find the process + tap.
        let session_id = format!("fc-{}-{}", manifest_short(&input.manifest), std::process::id());
        let meta = format!(
            "{{\"pid\":{},\"sock\":\"{}\",\"tap\":\"{}\",\"restore_ms\":{}}}",
            fc.child.id(), sock.display(), self.config.tap_dev, restore_ms
        );
        let _ = std::fs::write(input.overlay_root.join(".fc-session.json"), meta);
        // Detach: leave firecracker running; stop() kills by pid.
        std::mem::forget(fc); // do not kill on drop; session is live

        let session = RestoredSession {
            session_id,
            backend_id: FIRECRACKER_BACKEND_ID.to_string(),
            guest_port: input.manifest.restore_contract.ports.first().copied(),
            overlay_root: input.overlay_root,
            restored_bytes: restored_bytes.get(),
        };
        Ok(RestoreReceipt { session, ready_state_manifest_id: input.manifest.id() })
    }

    fn stop(&self, session: RestoredSession) -> Result<TeardownReceipt, SnapshotError> {
        let meta_path = session.overlay_root.join(".fc-session.json");
        if let Ok(meta) = std::fs::read_to_string(&meta_path)
            && let Some(pid) = json_u32(&meta, "pid")
        {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
        // Remove the tap (best effort) and the disposable overlay.
        self.net_down();
        let overlay_removed = if session.overlay_root.exists() {
            std::fs::remove_dir_all(&session.overlay_root).is_ok()
        } else {
            false
        };
        Ok(TeardownReceipt { session_id: session.session_id, overlay_removed })
    }
}

fn manifest_short(m: &ReadyStateManifest) -> String {
    m.id().strip_prefix("blake3:").unwrap_or("000000").chars().take(12).collect()
}

fn json_u32(s: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\":");
    let i = s.find(&needle)? + needle.len();
    let rest = &s[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── KVM-free unit tests (run everywhere, including CI without /dev/kvm) ──

    #[test]
    fn probe_reports_facets_and_availability_matches_host() {
        let p = FirecrackerBackend::new().probe();
        assert_eq!(p.backend_id, FIRECRACKER_BACKEND_ID);
        assert_eq!(p.snapshot_kind, SnapshotKind::MicroVm);
        assert!(p.memory_snapshot);
        assert_eq!(p.filesystem_model, FilesystemModel::Block);
        assert_eq!(p.gpu_mode, GpuMode::None);
        assert!(p.supports_seal_before_bind);
        // available iff /dev/kvm present AND firecracker runnable.
        let expect = FirecrackerBackend::kvm_present() && FirecrackerBackend::new().detect_version().is_some();
        assert_eq!(p.available, expect);
        if !p.available {
            assert!(p.reason.is_some());
        }
    }

    #[test]
    fn build_is_unsupported_without_kvm() {
        if FirecrackerBackend::kvm_present() {
            return; // covered by the KVM-gated integration test instead
        }
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = err_manifest();
        // inspect doesn't need KVM.
        assert!(FirecrackerBackend::new().inspect(&store, &m).is_ok());
        let backend = FirecrackerBackend::new();
        let input = RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov"), host_runner_class: None };
        assert!(matches!(backend.restore(input), Err(SnapshotError::Unsupported { .. })));
    }

    #[test]
    fn config_reads_defaults() {
        let c = FirecrackerConfig::default();
        assert_eq!(c.vcpu_count, 2);
        assert_eq!(c.healthcheck_port, 8080);
        assert_eq!(c.guest_ip, "172.16.0.2");
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
