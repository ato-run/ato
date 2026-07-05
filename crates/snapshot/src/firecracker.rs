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
    BlobManifest, CasStore, ChunkingKind, HotsetRecorder, LayerKind, LazyBlobReader, store_blob,
};
use serde_json::json;

use crate::agent_channel::{AgentChannel, FirecrackerAgentChannel, GUEST_AGENT_VSOCK_PORT};
use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    SupervisorBindings, TeardownReceipt,
};
use crate::manifest::{
    NoSecretProof, ReadyStateManifest, RestoreContract, SnapshotBackendInfo, SupervisorBuildReceipt,
    READY_STATE_SCHEMA,
};
use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};
use crate::bench;
use crate::scanner;
#[cfg(test)]
use crate::backend::BuildLayers;
#[cfg(test)]
use crate::manifest::ReadyStateLayers;

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
    /// Per-slot network isolation (#948 N-slot). When `Some`, this restore runs
    /// inside network namespace `netns`: the tap + guest keep the frozen
    /// snapshot addressing (`tap_dev` / `host_ip` / `guest_ip`) but live inside
    /// the namespace, and the guest is reached from the root namespace at
    /// `ingress_ip` via a veth pair (`veth_root` ↔ `veth_ns`) + in-ns DNAT.
    /// `None` = the legacy single-slot path in the root namespace (unchanged).
    pub netns: Option<String>,
    pub ingress_ip: Option<String>,
    pub veth_root: Option<String>,
    pub veth_root_ip: Option<String>,
    pub veth_ns: Option<String>,
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
            // Legacy single-slot by default; `for_slot` fills these when netns-on.
            netns: None,
            ingress_ip: None,
            veth_root: None,
            veth_root_ip: None,
            veth_ns: None,
        }
    }
}

impl FirecrackerConfig {
    /// Derive a per-slot config for network-namespace isolation (#948 N-slot).
    ///
    /// `netns_enabled` = `ATO_FC_NETNS=1 || max_slots > 1` (decided by the
    /// caller). When off, the returned config is `base` unchanged — the legacy
    /// single-slot root-namespace path, byte-identical to today. When on, EVERY
    /// slot (including slot 0) runs in its own namespace `ato-slot-{index}`,
    /// reached from the root namespace at `{prefix}.{index}.2` via a veth `/30`.
    /// The tap name and guest IP stay the frozen snapshot values — only the
    /// namespace and the host-side ingress differ per slot.
    pub fn for_slot(index: usize, netns_enabled: bool, base: &FirecrackerConfig) -> FirecrackerConfig {
        let mut c = base.clone();
        if !netns_enabled {
            return c;
        }
        // Integer-only names/addresses (no shell interpolation risk). `/30`
        // subnet per slot: .1 = root veth end, .2 = in-ns ingress.
        let prefix = env_or("ATO_FC_NETNS_CIDR_PREFIX", "10.201");
        c.netns = Some(format!("ato-slot-{index}"));
        c.veth_root = Some(format!("vsl{index}h"));
        c.veth_ns = Some(format!("vsl{index}n"));
        c.veth_root_ip = Some(format!("{prefix}.{index}.1"));
        c.ingress_ip = Some(format!("{prefix}.{index}.2"));
        c
    }
}

/// Firecracker microVM snapshot backend.
#[derive(Debug, Clone, Default)]
pub struct FirecrackerBackend {
    config: FirecrackerConfig,
    /// Live restored sessions (session_id → VMM child), so `stop()` can
    /// kill **and reap** the process it spawned (not just `kill -9` a pid).
    sessions: Arc<Mutex<HashMap<String, Child>>>,
    /// U1 (#854): live UFFD page-server threads keyed by session_id, kept alive
    /// for the session (faults arrive lazily) and joined on `stop()`. Empty unless
    /// `ATO_FC_UFFD` selected the Uffd `mem_backend` for that restore.
    page_servers: Arc<Mutex<HashMap<String, crate::uffd_page_server::PageServerHandle>>>,
}

impl FirecrackerBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: FirecrackerConfig) -> Self {
        Self { config, sessions: Arc::default(), page_servers: Arc::default() }
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

    /// The build-boot kernel cmdline. `page_hygiene` (v1.2 PR 3d, supervisor builds
    /// only) appends `init_on_free=1 init_on_alloc=1 page_poison=1` so freed guest
    /// pages — including the revoked placeholder binding — are zeroed before the
    /// snapshot. Restore replays the baked-in cmdline, so this needs setting only
    /// here. No-binding builds keep the exact historical string.
    fn boot_args(&self, page_hygiene: bool) -> String {
        let hygiene = if page_hygiene { " init_on_free=1 init_on_alloc=1 page_poison=1" } else { "" };
        format!(
            "console=ttyS0 reboot=k panic=1 pci=off{hygiene} ip={}::{}:{}::eth0:off",
            self.config.guest_ip, self.config.host_ip, self.config.guest_mask
        )
    }

    /// The address the RESTORE process (root namespace) and any fronting proxy
    /// must dial to reach this session's workload: the guest IP directly in the
    /// legacy path, or the per-slot ingress (`10.201.{i}.2`) in netns mode,
    /// which DNATs into the namespace to the same frozen guest IP.
    fn reachable_host(&self) -> &str {
        self.config.ingress_ip.as_deref().unwrap_or(&self.config.guest_ip)
    }

    /// Stable cache path keyed on a layer's content id (no content read needed),
    /// so build and restore agree and large immutable layers are rehydrated from
    /// CapsuleFS at most once, then reused across restores.
    fn cache_path(&self, kind: &str, blob: &BlobManifest, ext: &str) -> PathBuf {
        self.config.work_root.join(kind).join(format!("{}.{ext}", blob_id_hex(blob)))
    }
    /// Rehydrate a layer to `path`. `always` forces a fresh write (rw rootfs);
    /// otherwise it is a no-op when the file is already cached. Materialization is
    /// ATOMIC (write a temp file, then rename) so Firecracker never sees a partial
    /// memory/rootfs file — required for the parallel prefetch path (Phase 6A).
    fn rehydrate_atomic(&self, path: &Path, store: &CasStore, blob: &BlobManifest, always: bool) -> Result<(), SnapshotError> {
        if !always && path.exists() {
            // Validate the cached file before trusting it (Phase 7.5c). The
            // rehydrate path is already integrity-checked (read_all re-hashes every
            // CAS chunk, fail-closed); a cache HIT is the only unvalidated path.
            // A `total_len` size check catches truncation / interrupted writes /
            // disk-full partials — the realistic corruption of an immutable
            // content-addressed file. On mismatch (or in opt-in deep mode) we drop
            // the file and re-rehydrate from CAS, which re-verifies all chunks and
            // fails closed BEFORE LoadSnapshot if CAS cannot supply valid bytes.
            let actual = std::fs::metadata(path).map(|m| m.len()).ok();
            let size_ok = actual == Some(blob.total_len);
            if size_ok && !verify_hash_enabled() {
                return Ok(());
            }
            if !size_ok {
                eprintln!(
                    "READY-STATE: cached layer {} failed size validation (expected {}, got {:?}) — discarding + re-rehydrating from CAS",
                    blob_id_hex(blob),
                    blob.total_len,
                    actual
                );
                let _ = std::fs::remove_file(path);
            }
            // (deep mode with size_ok: fall through to re-read + re-verify chunks.)
        }
        let bytes = LazyBlobReader::new(store, blob).read_all()?;
        self.write_atomic(path, &bytes)
    }
    /// Rehydrate a layer to `path` only if it is not already on disk (atomic).
    fn ensure_cached(&self, path: &Path, store: &CasStore, blob: &BlobManifest) -> Result<(), SnapshotError> {
        self.rehydrate_atomic(path, store, blob, false)
    }
    /// Atomic file write: write a sibling temp file, then rename over `path`, so a
    /// concurrent/aborted writer never exposes a partial file. Cleans the temp on
    /// failure.
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| self.backend_err(e.to_string()))?;
        }
        let tmp = path.with_extension(capsulefs::unique_tmp_suffix());
        std::fs::write(&tmp, bytes).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            self.backend_err(format!("write {}: {e}", tmp.display()))
        })?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            self.backend_err(format!("rename {} -> {}: {e}", tmp.display(), path.display()))
        })
    }
    fn lock_path(&self) -> PathBuf {
        // Per-slot lock: in netns mode the tap name (`fctap0`) is identical in
        // every namespace, so keying the lock on the tap alone would re-serialize
        // all slots on one shared host file. Key on the namespace instead so each
        // slot has its own lock; legacy (no netns) keeps the tap-keyed path.
        let key = self.config.netns.as_deref().unwrap_or(&self.config.tap_dev);
        self.config.work_root.join(format!("{key}.lock"))
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
    /// `ip netns exec <ns> <argv…>` — run a host command inside a namespace.
    fn run_in_netns(&self, ns: &str, argv: &[&str]) -> Result<(), SnapshotError> {
        let mut a = vec!["netns", "exec", ns];
        a.extend_from_slice(argv);
        let status = Command::new("ip").args(&a).status()
            .map_err(|e| self.backend_err(format!("spawn `ip netns exec {ns} {}`: {e}", argv.join(" "))))?;
        if status.success() { Ok(()) } else { Err(self.backend_err(format!("`ip netns exec {ns} {}` failed", argv.join(" ")))) }
    }

    fn net_up(&self, guest_port: u16) -> Result<(), SnapshotError> {
        match self.config.netns.clone() {
            None => self.net_up_root(),
            Some(ns) => self.net_up_netns(&ns, guest_port),
        }
    }

    /// Legacy single-slot networking in the ROOT namespace (unchanged).
    fn net_up_root(&self) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let _ = Command::new("ip").args(["link", "del", tap]).status();
        self.run_ip(&["tuntap", "add", "dev", tap, "mode", "tap"])?;
        self.run_ip(&["addr", "add", &format!("{}/24", self.config.host_ip), "dev", tap])?;
        self.run_ip(&["link", "set", tap, "up"])?;
        Ok(())
    }

    /// Per-slot networking (#948 N-slot): the frozen tap (`fctap0`) + guest
    /// (`172.16.0.2`) live inside namespace `ns`, reached from the root namespace
    /// at `ingress_ip` via a veth `/30` + in-ns DNAT to the guest. All addresses
    /// are integer-derived and passed as argv (no shell). Idempotent: a stale
    /// namespace from a crashed prior run is torn down first.
    fn net_up_netns(&self, ns: &str, guest_port: u16) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let host_ip = &self.config.host_ip;
        let guest_ip = &self.config.guest_ip;
        let veth_root = self.config.veth_root.as_deref().ok_or_else(|| self.backend_err("netns config missing veth_root"))?;
        let veth_ns = self.config.veth_ns.as_deref().ok_or_else(|| self.backend_err("netns config missing veth_ns"))?;
        let veth_root_ip = self.config.veth_root_ip.as_deref().ok_or_else(|| self.backend_err("netns config missing veth_root_ip"))?;
        let ingress_ip = self.config.ingress_ip.as_deref().ok_or_else(|| self.backend_err("netns config missing ingress_ip"))?;
        let port = guest_port.to_string();
        let dnat = format!("{guest_ip}:{port}");
        let veth_root_cidr = format!("{veth_root_ip}/30");
        let ingress_cidr = format!("{ingress_ip}/30");
        let host_cidr = format!("{host_ip}/24");

        // Clean any stale state from a crashed prior run (best-effort).
        self.net_down();
        // Namespace + loopback + in-ns tap with the frozen guest addressing.
        self.run_ip(&["netns", "add", ns])?;
        self.run_in_netns(ns, &["ip", "link", "set", "lo", "up"])?;
        self.run_in_netns(ns, &["ip", "tuntap", "add", "dev", tap, "mode", "tap"])?;
        self.run_in_netns(ns, &["ip", "addr", "add", &host_cidr, "dev", tap])?;
        self.run_in_netns(ns, &["ip", "link", "set", tap, "up"])?;
        // veth pair: root end stays in root ns, the other end moves into `ns`.
        self.run_ip(&["link", "add", veth_root, "type", "veth", "peer", "name", veth_ns])?;
        self.run_ip(&["link", "set", veth_ns, "netns", ns])?;
        self.run_ip(&["addr", "add", &veth_root_cidr, "dev", veth_root])?;
        self.run_ip(&["link", "set", veth_root, "up"])?;
        self.run_in_netns(ns, &["ip", "addr", "add", &ingress_cidr, "dev", veth_ns])?;
        self.run_in_netns(ns, &["ip", "link", "set", veth_ns, "up"])?;
        // Forward + DNAT the ingress to the guest, MASQUERADE toward the tap so
        // the guest replies to a same-subnet source. All rules stay inside `ns`
        // (root namespace is left untouched → teardown is just `ip netns del`).
        self.run_in_netns(ns, &["sysctl", "-q", "-w", "net.ipv4.ip_forward=1"])?;
        self.run_in_netns(ns, &["iptables", "-t", "nat", "-A", "PREROUTING", "-d", ingress_ip, "-p", "tcp", "--dport", &port, "-j", "DNAT", "--to-destination", &dnat])?;
        self.run_in_netns(ns, &["iptables", "-t", "nat", "-A", "POSTROUTING", "-o", tap, "-j", "MASQUERADE"])?;
        Ok(())
    }

    fn net_down(&self) {
        match &self.config.netns {
            // Deleting the namespace atomically removes the in-ns tap, the
            // in-ns veth end, and all in-ns iptables rules. The root veth end is
            // auto-removed with its peer, but delete it explicitly too.
            Some(ns) => {
                let _ = Command::new("ip").args(["netns", "del", ns]).status();
                if let Some(v) = &self.config.veth_root {
                    let _ = Command::new("ip").args(["link", "del", v]).status();
                }
            }
            None => {
                let _ = Command::new("ip").args(["link", "del", &self.config.tap_dev]).status();
            }
        }
    }

    /// The base command to launch firecracker — wrapped in `ip netns exec <ns>`
    /// when this slot is namespaced so the VMM (and its tap) live inside `ns`.
    fn fc_command(&self) -> Command {
        match &self.config.netns {
            Some(ns) => {
                let mut c = Command::new("ip");
                c.args(["netns", "exec", ns, &self.config.firecracker_bin]);
                c
            }
            None => Command::new(&self.config.firecracker_bin),
        }
    }

    fn start_fc(&self, sock: &Path, console_log: &Path) -> Result<FcProcess, SnapshotError> {
        let _ = std::fs::remove_file(sock);
        let log = std::fs::File::create(console_log).map_err(|e| self.backend_err(format!("create console log: {e}")))?;
        let child = self.fc_command()
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

    fn configure_boot(&self, fc: &FcProcess, kernel: &Path, rootfs: &Path, read_only: bool, page_hygiene: bool) -> Result<(), SnapshotError> {
        let mc = if let Some(t) = &self.config.cpu_template {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib, "cpu_template": t})
        } else {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib})
        };
        fc.api(self, "PUT", "/machine-config", Some(&mc.to_string()))?;
        fc.api(self, "PUT", "/boot-source", Some(&json!({
            "kernel_image_path": kernel.to_string_lossy(), "boot_args": self.boot_args(page_hygiene)
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
        self.wait_health_until(port, path, || None)
    }

    /// `wait_health` with a fail-fast `abort` check polled each iteration (U5
    /// #858): when it returns `Some(reason)` the wait stops with an error instead of
    /// burning the full `boot_timeout` — used so a UFFD page-server failure (CAS
    /// miss/corrupt → the guest can never fault its memory in) fails closed fast.
    fn wait_health_until(
        &self,
        port: u16,
        path: &str,
        abort: impl Fn() -> Option<String>,
    ) -> Result<u128, SnapshotError> {
        // Dial the ROOT-reachable address: the guest IP directly (legacy), or
        // the per-slot ingress (netns mode) which DNATs into the namespace.
        let reachable = self.reachable_host();
        let addr: std::net::SocketAddr = format!("{reachable}:{port}")
            .parse().map_err(|e| self.backend_err(format!("bad guest addr: {e}")))?;
        let start = Instant::now();
        while start.elapsed() < self.config.boot_timeout {
            if let Some(reason) = abort() {
                return Err(self.backend_err(format!("restore failed closed: {reason}")));
            }
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

    /// v1.2 PR 3d step 1 of the supervisor build drive: connect the guest-agent
    /// (retrying while the guest boots) and deliver every placeholder lease, then
    /// poll bound-ready. The agent starts the workload at bound-ready, so the
    /// caller's `wait_health` right after this is the placeholder health-verify.
    fn supervisor_deliver_placeholders(&self, uds: &Path, drive: &SupervisorDrive) -> Result<(), SnapshotError> {
        let mut ch = FirecrackerAgentChannel::connect_with_retry(
            uds,
            GUEST_AGENT_VSOCK_PORT,
            self.config.boot_timeout,
        )
        .map_err(|e| self.backend_err(format!("supervisor build: {e:#}")))?;
        for lease in &drive.leases {
            match ch.request(HostToAgent::Deliver(lease.to_delivery())) {
                Ok(AgentToHost::Ack { .. }) => {}
                Ok(AgentToHost::Error { message }) => {
                    return Err(self.backend_err(format!("supervisor build: placeholder delivery refused: {message}")));
                }
                Ok(other) => {
                    return Err(self.backend_err(format!("supervisor build: unexpected Deliver response: {other:?}")));
                }
                Err(e) => return Err(self.backend_err(format!("supervisor build: deliver: {e:#}"))),
            }
        }
        for _ in 0..10 {
            match ch.request(HostToAgent::QueryBoundReady) {
                Ok(AgentToHost::BoundReady { ready: true, .. }) => return Ok(()),
                Ok(AgentToHost::BoundReady { ready: false, .. }) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Ok(other) => {
                    return Err(self.backend_err(format!("supervisor build: unexpected BoundReady response: {other:?}")));
                }
                Err(e) => return Err(self.backend_err(format!("supervisor build: bound-ready poll: {e:#}"))),
            }
        }
        Err(self.backend_err("supervisor build: agent never reached bound-ready after placeholder delivery"))
    }

    /// v1.2 PR 3d step 2, run AFTER health passed and BEFORE the pause/snapshot:
    /// `StopWorkload` (the agent SIGTERM→SIGKILLs the app; bounded, ack'd) then
    /// `Revoke` every placeholder lease (tmpfs scrub, ack'd) — so the snapshot is
    /// taken with the workload down and no binding material in guest tmpfs. Order
    /// is contract-fixed: StopWorkload FIRST, then Revoke (binding_control §v1.2).
    fn supervisor_stop_and_revoke(&self, uds: &Path, drive: &SupervisorDrive) -> Result<(), SnapshotError> {
        let mut ch = FirecrackerAgentChannel::connect(uds, GUEST_AGENT_VSOCK_PORT, Duration::from_secs(10))
            .map_err(|e| self.backend_err(format!("supervisor build: reconnect for stop: {e:#}")))?;
        match ch.request(HostToAgent::StopWorkload) {
            Ok(AgentToHost::WorkloadStopped { was_running }) => {
                if !was_running {
                    // Health just passed, so the workload MUST have been running; a
                    // false here means the drive raced or the agent lost it — refuse
                    // to seal an unexplained state.
                    return Err(self.backend_err("supervisor build: StopWorkload reported was_running=false after health passed"));
                }
            }
            Ok(AgentToHost::Error { message }) => {
                return Err(self.backend_err(format!("supervisor build: StopWorkload refused: {message}")));
            }
            Ok(other) => {
                return Err(self.backend_err(format!("supervisor build: unexpected StopWorkload response: {other:?}")));
            }
            Err(e) => return Err(self.backend_err(format!("supervisor build: StopWorkload: {e:#}"))),
        }
        for name in &drive.binding_names {
            match ch.request(HostToAgent::Revoke { id: BindingLeaseId::new(format!("lease-build-{name}")) }) {
                Ok(AgentToHost::Scrubbed { .. }) => {}
                Ok(AgentToHost::Error { message }) => {
                    return Err(self.backend_err(format!("supervisor build: revoke refused: {message}")));
                }
                Ok(other) => {
                    return Err(self.backend_err(format!("supervisor build: unexpected Revoke response: {other:?}")));
                }
                Err(e) => return Err(self.backend_err(format!("supervisor build: revoke: {e:#}"))),
            }
        }
        Ok(())
    }

    /// v1.2 PR 3d: after StopWorkload+Revoke, verify the workload is actually DOWN
    /// (its listener gone) before sealing. This caught a real bug: the agent acked
    /// WorkloadStopped after killing only the wrapper shell while the orphaned app
    /// kept serving — the acks alone are not proof. "Down" = TCP connect refused;
    /// a still-accepting listener within the window fails the build closed.
    fn wait_workload_down(&self, port: u16, timeout: Duration) -> Result<(), SnapshotError> {
        let reachable = self.reachable_host();
        let addr: std::net::SocketAddr = format!("{reachable}:{port}")
            .parse().map_err(|e| self.backend_err(format!("bad guest addr: {e}")))?;
        let start = Instant::now();
        while start.elapsed() < timeout {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(self.backend_err(
            "supervisor build: workload still accepting connections after StopWorkload — \
             refusing to seal a snapshot with the app running",
        ))
    }

    /// v1.2 PR 3d: restore-readiness probe for a SUPERVISOR artifact. The workload is
    /// down by design (StopWorkload+Revoke ran before the seal), so readiness =
    /// "VM resumed + guest-agent reachable" — and, fail-closed, the agent must
    /// report NOT bound-ready: a bound-ready session straight out of restore means
    /// binding state survived the seal (a pre-bind-seal violation), never expose it.
    fn probe_restored_agent_unbound(&self, uds: &Path) -> Result<(), SnapshotError> {
        let mut ch = FirecrackerAgentChannel::connect_with_retry(
            uds,
            GUEST_AGENT_VSOCK_PORT,
            self.config.boot_timeout,
        )
        .map_err(|e| self.backend_err(format!("supervisor restore: agent unreachable: {e:#}")))?;
        match ch.request(HostToAgent::QueryBoundReady) {
            Ok(AgentToHost::BoundReady { ready: false, .. }) => Ok(()),
            Ok(AgentToHost::BoundReady { ready: true, .. }) => Err(self.backend_err(
                "supervisor restore: session is ALREADY bound-ready after restore — \
                 binding state survived the seal (pre-bind-seal violation); refusing to expose",
            )),
            Ok(other) => Err(self.backend_err(format!(
                "supervisor restore: unexpected BoundReady response: {other:?}"
            ))),
            Err(e) => Err(self.backend_err(format!("supervisor restore: bound-ready probe: {e:#}"))),
        }
    }

    /// v1.2 PR 3d: build-failure forensics. The guest console (`console.log`) was
    /// being captured and then silently deleted with the build dir on EVERY outcome —
    /// exactly why earlier guest failures were undiagnosable. On failure, always emit
    /// the console tail; with ATO_FC_KEEP_BUILD_DIR=1 also announce the preserved dir
    /// (the caller then skips the cleanup).
    fn emit_build_failure_diagnostics(&self, build_dir: &Path) {
        let console = build_dir.join("console.log");
        if let Ok(bytes) = std::fs::read(&console) {
            let tail = &bytes[bytes.len().saturating_sub(4096)..];
            eprintln!(
                "READY-STATE build failed — guest console tail ({} of {} bytes):\n{}",
                tail.len(),
                bytes.len(),
                String::from_utf8_lossy(tail)
            );
        }
        if keep_build_dir_enabled() {
            eprintln!("READY-STATE: ATO_FC_KEEP_BUILD_DIR set — preserving {}", build_dir.display());
        }
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

fn blob_id_hex(blob: &BlobManifest) -> String {
    blob.id().hex().to_string()
}

/// Phase 6A opt-in: memory-first parallel restore prefetch. Off → the default
/// sequential rehydrate (unchanged restore semantics). This is **restore I/O
/// scheduling**, NOT lazy memory / UFFD — File memory still needs a complete file
/// before LoadSnapshot. Enabled by `ATO_READY_STATE_HOTSET=1` or
/// `ATO_READY_STATE_PREFETCH=memory`.
/// Opt-in deep cache mode (Phase 7.5c): `ATO_READY_STATE_VERIFY_HASH=1` makes a
/// restore **not trust the cache hit** — it re-rehydrates every cached layer from
/// CAS, which re-verifies all chunk hashes (it does NOT hash the existing cached
/// file in place; there is no aggregate cached-file hash). Off by default — for
/// paranoid/CI runs that want to catch a same-size in-place bit-flip in a cached
/// file. Default validation is size-only (`total_len`).
fn verify_hash_enabled() -> bool {
    std::env::var("ATO_READY_STATE_VERIFY_HASH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// U1 (#854)/U2 (#855) test-only gate: `ATO_FC_UFFD` selects the Uffd `mem_backend`
/// for a restore instead of the default File backend. `zero` → serve kernel-zeroed
/// pages (U1a plumbing); `mem` → serve real pages from the materialized `.mem`
/// (U1b); `cas`/`1` → serve pages lazily from local CAS WITHOUT materializing `.mem`
/// (U2, fault-around 2 MiB). Unset / `0` / `file` → File backend (default,
/// unchanged). Exercised only by the `#[ignore]`d KVM smokes; never a product
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UffdMode {
    Zero,
    Mem,
    Cas,
}
fn uffd_mode() -> Option<UffdMode> {
    match std::env::var("ATO_FC_UFFD").ok().as_deref() {
        Some("zero") => Some(UffdMode::Zero),
        Some("mem") => Some(UffdMode::Mem),
        Some("cas") | Some("1") => Some(UffdMode::Cas),
        _ => None,
    }
}

/// L2 (#912): whether the host has an AF_VSOCK transport (`/dev/vhost-vsock`, i.e. the
/// `vhost_vsock` module is loaded). Cheap + side-effect-free.
fn host_vhost_vsock_present() -> bool {
    std::path::Path::new("/dev/vhost-vsock").exists()
}

/// Whether to attach a Firecracker vsock device (for the guest-agent binding channel).
/// Off by default → the restore path is unchanged.
fn vsock_enabled() -> bool {
    matches!(std::env::var("ATO_FC_VSOCK").ok().as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// The vsock host UDS for a capsule — **deterministic** so build (which bakes it into
/// the snapshot) and restore (which re-creates it) agree on the same-host developer
/// preview. Firecracker does not allow overriding the vsock uds at load, so the path is
/// derived from the capsule hash, not the ephemeral overlay.
fn vsock_uds_path(capsule_manifest_hash: &str) -> PathBuf {
    let safe: String = capsule_manifest_hash
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join("ato-vsock").join(format!("{safe}.sock"))
}

/// v1.2 PR 3d: keep the transient build dir (incl. `console.log`) on disk instead of
/// removing it — the failure-forensics escape hatch. Off by default.
fn keep_build_dir_enabled() -> bool {
    matches!(std::env::var("ATO_FC_KEEP_BUILD_DIR").ok().as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// v1.2 PR 3d: a unique, never-stored placeholder value for one build-time binding.
/// Not a secret — it exists only to let the supervisor start the workload once
/// (health-verify) and to serve as an advisory memory-hygiene canary after revoke.
/// Sourced from /dev/urandom (the FC backend is Linux-only).
fn generate_build_placeholder() -> Result<String, std::io::Error> {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("ATO-BUILD-PLACEHOLDER-{hex}"))
}

/// v1.2 PR 3d: the in-flight state of one supervisor build drive — the placeholder
/// leases to deliver and (for the post-snapshot advisory scan) their raw values.
/// Lives only for the duration of `build_ready_state`; nothing here is stored.
struct SupervisorDrive {
    binding_names: Vec<String>,
    leases: Vec<BindingLease>,
    placeholder_values: Vec<String>,
}

impl SupervisorDrive {
    /// Parse + validate every binding name and mint one placeholder lease per name.
    /// Fail-closed on an invalid name (the #961 emission gate should make this
    /// unreachable, but the backend revalidates rather than trusting its caller).
    fn prepare(sup: &SupervisorBindings) -> Result<Self, String> {
        if sup.binding_names.is_empty() {
            return Err("supervisor build requires at least one binding name".into());
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut leases = Vec::with_capacity(sup.binding_names.len());
        let mut values = Vec::with_capacity(sup.binding_names.len());
        for name in &sup.binding_names {
            let bname = BindingName::parse(name.as_str())
                .map_err(|e| format!("supervisor binding name '{name}': {e}"))?;
            let value = generate_build_placeholder()
                .map_err(|e| format!("generate build placeholder: {e}"))?;
            leases.push(BindingLease::issue(
                BindingLeaseId::new(format!("lease-build-{name}")),
                bname,
                SecretValue::new(value.clone()),
                now_ms,
                3_600_000, // 1h — outlives any sane build; revoked before the snapshot anyway
            ));
            values.push(value);
        }
        Ok(SupervisorDrive {
            binding_names: sup.binding_names.clone(),
            leases,
            placeholder_values: values,
        })
    }
}

fn hotset_enabled() -> bool {
    std::env::var("ATO_READY_STATE_HOTSET")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("ATO_READY_STATE_PREFETCH")
            .map(|v| v.eq_ignore_ascii_case("memory"))
            .unwrap_or(false)
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
        // U0: truthfully report whether this host could drive a `Uffd` mem_backend
        // (probe only — no restore path uses it yet). See crate::uffd.
        let (supports_uffd_mem_backend, uffd_reason) = crate::uffd::evaluate(
            std::env::consts::ARCH,
            kvm,
            version.as_deref(),
            crate::uffd::host_userfaultfd_present(),
        );
        // L2 (#912): binding-lease placement capabilities. The full flow needs the
        // Firecracker backend, host vsock, a guest-agent, and x86_64 (the guest-agent +
        // vsock plumbing are x86_64). stop-scrub + the no-secret scanner ship with it.
        let supports_vsock = host_vhost_vsock_present();
        let supports_binding_lease = available && supports_vsock && std::env::consts::ARCH == "x86_64";
        let binding = crate::backend::BindingCapabilities {
            supports_firecracker: true,
            supports_vsock,
            supports_guest_agent: true,
            supports_binding_lease,
            supports_stop_scrub: supports_binding_lease,
            supports_no_secret_scan: true,
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
            supports_uffd_mem_backend,
            uffd_reason,
            binding,
        }
    }

    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        // PREFLIGHT GATE (fail closed BEFORE any store / stable-rootfs write / boot):
        // a declared marker in rootfs/runtime/dependency/app, or a provider/env
        // secret in app/dependency, rejects the build before secret-bearing rootfs
        // bytes are ever written to CAS. Runs even without /dev/kvm. The full
        // six-layer gate (incl. vmstate/memory) still runs post-snapshot in
        // seal_and_scan, before THOSE layers are stored.
        crate::seal::preflight_gate(
            &input.layers.rootfs,
            input.layers.runtime.as_deref(),
            input.layers.dependency.as_deref(),
            input.layers.app.as_deref(),
            &input.declared_secret_markers,
        )?;
        // v1.2 PR 3d: supervisor prerequisites — fail closed BEFORE any boot. A
        // supervisor rootfs starts its workload only at bound-ready, so building one
        // without the vsock channel would just burn the boot timeout.
        let supervisor_drive = match &input.supervisor {
            Some(sup) => {
                if !vsock_enabled() {
                    return Err(self.backend_err(
                        "supervisor build requires the vsock binding channel (set ATO_FC_VSOCK=1): \
                         the guest-agent gates workload start on placeholder delivery",
                    ));
                }
                Some(SupervisorDrive::prepare(sup).map_err(|e| self.backend_err(e))?)
            }
            None => None,
        };
        self.ensure_available()?;
        self.acquire_lock("build")?;
        let _lock = BuildLock { path: self.lock_path() };
        std::fs::create_dir_all(&self.config.work_root).map_err(|e| self.backend_err(e.to_string()))?;
        let build_dir = self.config.work_root.join(format!("build-{}", std::process::id()));
        std::fs::create_dir_all(&build_dir).map_err(|e| self.backend_err(e.to_string()))?;

        // Store the rootfs blob first so its content id keys the stable drive
        // path the snapshot records (restore reuses the same path without
        // re-reading 300MB to recompute it).
        let cd = ChunkingKind::ContentDefined;
        let rootfs_blob = bench::time("build.store_rootfs", || {
            store_blob(input.store, LayerKind::Rootfs, &input.layers.rootfs, cd)
        })?;
        let rootfs_path = self.cache_path("rootfs", &rootfs_blob, "ext4");
        if !rootfs_path.exists() {
            self.write_file(&rootfs_path, &input.layers.rootfs)?;
        }
        let vmstate_path = build_dir.join("vmstate");
        let mem_path = build_dir.join("mem");
        let port = hc_port(&input.restore_contract, self.config.healthcheck_port);
        let path = hc_path(&input.restore_contract, &self.config.healthcheck_path);

        // Build always runs in the root namespace (default config, netns=None).
        self.net_up(port)?;
        let snap = (|| -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
            let fc = bench::time("build.start_fc", || {
                self.start_fc(&build_dir.join("api.sock"), &build_dir.join("console.log"))
            })?;
            self.configure_boot(
                &fc,
                &self.config.kernel_path,
                &rootfs_path,
                self.config.rootfs_read_only,
                // v1.2 PR 3d: supervisor builds get the page-hygiene cmdline so freed
                // guest pages (incl. the revoked placeholder) are zeroed pre-snapshot.
                supervisor_drive.is_some(),
            )?;
            // Phase 8a-HW (#912): attach a vsock device BEFORE boot/snapshot so the
            // guest-agent binding channel is captured in the snapshot. The uds_path is
            // baked into the snapshot (FC forbids overriding it at load), so it is a
            // deterministic per-capsule path both build and restore compute.
            let vsock_uds = if vsock_enabled() {
                let uds = vsock_uds_path(&input.capsule_manifest_hash);
                if let Some(d) = uds.parent() {
                    std::fs::create_dir_all(d).map_err(|e| self.backend_err(format!("vsock dir: {e}")))?;
                }
                let _ = std::fs::remove_file(&uds);
                fc.api(self, "PUT", "/vsock", Some(&json!({
                    "guest_cid": 3, "uds_path": uds.to_string_lossy()
                }).to_string()))?;
                Some(uds)
            } else {
                None
            };
            bench::time("build.boot_to_health", || -> Result<(), SnapshotError> {
                fc.api(self, "PUT", "/actions", Some(&json!({"action_type":"InstanceStart"}).to_string()))?;
                // v1.2 PR 3d: a supervisor guest starts its workload only at
                // bound-ready — deliver the placeholder leases first, THEN health.
                if let Some(drive) = &supervisor_drive {
                    let uds = vsock_uds.as_ref().ok_or_else(|| {
                        self.backend_err("supervisor build: vsock uds missing (unreachable: gated above)")
                    })?;
                    self.supervisor_deliver_placeholders(uds, drive)?;
                }
                self.wait_health(port, &path)?; // secret-free seal point (placeholder-only for supervisor builds)
                // v1.2 PR 3d: StopWorkload → Revoke all placeholders BEFORE the
                // pause/snapshot, so the seal carries no running workload and no
                // binding material in guest tmpfs (contract order: stop, then revoke).
                // Then VERIFY the listener is gone — acks alone are not proof (a
                // wrapper-shell kill once left the orphaned app serving).
                if let Some(drive) = &supervisor_drive {
                    let uds = vsock_uds.as_ref().ok_or_else(|| {
                        self.backend_err("supervisor build: vsock uds missing (unreachable: gated above)")
                    })?;
                    self.supervisor_stop_and_revoke(uds, drive)?;
                    self.wait_workload_down(port, Duration::from_secs(10))?;
                }
                Ok(())
            })?;
            bench::time("build.snapshot_create", || -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
                fc.api(self, "PATCH", "/vm", Some(&json!({"state":"Paused"}).to_string()))?;
                fc.api(self, "PUT", "/snapshot/create", Some(&json!({
                    "snapshot_type":"Full",
                    "snapshot_path": vmstate_path.to_string_lossy(),
                    "mem_file_path": mem_path.to_string_lossy()
                }).to_string()))?;
                let vmstate = std::fs::read(&vmstate_path).map_err(|e| self.backend_err(format!("read vmstate: {e}")))?;
                let mem = std::fs::read(&mem_path).map_err(|e| self.backend_err(format!("read mem: {e}")))?;
                Ok((vmstate, mem))
            })
            // fc drops here → killed+reaped
        })();
        self.net_down();
        let (vmstate, mem) = match snap {
            Ok(v) => v,
            Err(e) => {
                // v1.2 PR 3d: surface the guest console before (conditionally)
                // discarding the build dir — a silent delete made guest-side
                // failures undiagnosable.
                self.emit_build_failure_diagnostics(&build_dir);
                if !keep_build_dir_enabled() {
                    let _ = std::fs::remove_dir_all(&build_dir);
                }
                return Err(e);
            }
        };

        // v1.2 PR 3d: ADVISORY placeholder-hygiene scan (kernel init_on_free-
        // dependent, #947 finding) — the revoked placeholder SHOULD be gone from the
        // snapshot bytes on a hygiene-enabled kernel, but its residue is NOT a
        // secret leak (the value is a build-scoped random token, discarded below),
        // so this records honestly instead of gating.
        let supervisor_receipt = supervisor_drive.as_ref().map(|drive| {
            let secrets: Vec<&[u8]> = drive.placeholder_values.iter().map(|v| v.as_bytes()).collect();
            let absent = crate::no_secret_scan::blob_is_clean(&mem, &secrets)
                && crate::no_secret_scan::blob_is_clean(&vmstate, &secrets);
            eprintln!(
                "READY-STATE supervisor build: placeholder absent from sealed mem/vmstate = {absent} \
                 (advisory; requires kernel init_on_free support)"
            );
            SupervisorBuildReceipt {
                binding_names: drive.binding_names.clone(),
                page_hygiene_boot_args: true,
                placeholder_absent_from_seal: Some(absent),
            }
        });

        // ── seal + no-secret scan via the shared orchestration ───────────────
        // rootfs was already stored above (for the stable drive path) → pass it
        // as prestored. vmstate/mem are scanned by REFERENCE — no clone of the
        // ~100s-of-MB images. Declared markers fail closed on every layer;
        // provider/env block on app/dependency; the large opaque layers are
        // advisory + content-cached + budgeted.
        let cache = crate::scan_cache::ScanCache::open(input.store.root());
        let out = bench::time("build.seal_and_scan", || {
            crate::seal::seal_and_scan(
                input.store,
                crate::seal::SealLayersRef {
                    rootfs: &input.layers.rootfs,
                    runtime: input.layers.runtime.as_deref(),
                    dependency: input.layers.dependency.as_deref(),
                    app: input.layers.app.as_deref(),
                    vmstate: &vmstate,
                    memory: &mem,
                },
                &input.declared_secret_markers,
                &cache,
                crate::seal::advisory_budget_from_env(),
                Some(rootfs_blob),
            )
        });
        // seal_and_scan fails closed (nothing stored) on declared/blocking hits.
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.emit_build_failure_diagnostics(&build_dir);
                if !keep_build_dir_enabled() {
                    let _ = std::fs::remove_dir_all(&build_dir);
                }
                return Err(e);
            }
        };
        let advisories = scanner::advisory_summaries_capped(&out.report, 50);
        let coverage = out.coverage;
        let sealed_bytes = out.sealed_bytes;
        let layers = out.layers;

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
            coverage,
        };
        let runner_class_id = Some(input.runner_class.unwrap_or_else(|| self.runner_facts().id()));
        let manifest = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: input.capsule_manifest_hash,
            has_vsock: vsock_enabled(),
            runner_class_id,
            execution_id: input.execution_id.clone(),
            layers,
            hotset_profile,
            snapshot_backend: self.backend_info(),
            restore_contract: input.restore_contract,
            sanitizer_contract: input.sanitizer_contract,
            no_secret_proof: Some(no_secret_proof.clone()),
            build_receipt_id: None,
            supervisor_build: supervisor_receipt,
        };
        if !keep_build_dir_enabled() {
            let _ = std::fs::remove_dir_all(&build_dir);
        }
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

        // N-slot fail-closed guards (#948, Phase -1 audit). Netns isolates the
        // NETWORK, not host filesystem paths, so two concurrent restores of the
        // SAME snapshot still collide on any shared host path:
        //  * rw-rootfs is rehydrated to a content-addressed SHARED cache path →
        //    two writers corrupt it; require read-only rootfs under netns.
        //  * a vsock UDS path is BAKED into the snapshot (`/tmp/ato-vsock/{hash}`)
        //    and recreated on load → identical for every instance; refuse until
        //    it is mount-namespace isolated. (Showcase apps have no vsock.)
        if self.config.netns.is_some() {
            if !self.config.rootfs_read_only {
                return Err(self.unsupported(
                    "N-slot (netns) restore requires read-only rootfs; rw-rootfs writes a shared cache path and would corrupt under concurrency",
                ));
            }
            if vsock_enabled() || input.manifest.has_vsock {
                return Err(self.unsupported(
                    "N-slot (netns) restore does not yet support vsock snapshots; the baked vsock UDS path collides across concurrent instances",
                ));
            }
        }

        self.acquire_lock("restore")?;
        // From here, on any error we must release the lock + net before returning.
        let result = (|| -> Result<(RestoredSession, Child, Option<crate::uffd_page_server::PageServerHandle>), SnapshotError> {
            std::fs::create_dir_all(&input.overlay_root).map_err(|e| self.backend_err(e.to_string()))?;
            // restored_bytes = the logical bytes the session is restored from
            // (independent of whether a cached layer was reused on disk).
            let restored_bytes = rootfs.total_len + vmstate.total_len + memory.total_len;
            let restore_start = Instant::now(); // U8: restore_total_ms

            // mem + vmstate are immutable snapshot outputs → content-addressed
            // shared cache, rehydrated from CapsuleFS at most ONCE then reused
            // across restores (Firecracker maps the mem file private/CoW, so
            // sharing is leak-safe — proven by the state-leak test). This avoids
            // re-reading + rewriting ~512MB of memory image every restore.
            // rootfs at the SAME content-id path the snapshot recorded. Read-only:
            // reuse the shared immutable copy (leak-safe + fast). Read-write:
            // rewrite a fresh copy per restore (single session ⇒ no overlap;
            // fresh ⇒ leak-safe), at the cost of a per-restore copy.
            let mem_path = self.cache_path("mem", memory, "mem");
            let vmstate_path = self.cache_path("vmstate", vmstate, "vmstate");
            let rootfs_path = self.cache_path("rootfs", rootfs, "ext4");
            let rw_rootfs = !self.config.rootfs_read_only;
            // U11 (#878): the product preview drives UFFD local-CAS demand via the
            // input flag; the env gate (uffd_mode) remains for the test-only KVM
            // smokes and takes effect only when the input flag is off.
            let uffd = if input.uffd_preview { Some(UffdMode::Cas) } else { uffd_mode() };

            if uffd == Some(UffdMode::Cas) {
                // U2 (#855): memory is served lazily from local CAS by the page-server
                // (NO full .mem materialization) — materialize only vmstate + rootfs.
                bench::time("restore.cache_vmstate", || self.ensure_cached(&vmstate_path, input.store, vmstate))?;
                bench::time("restore.cache_rootfs", || {
                    self.rehydrate_atomic(&rootfs_path, input.store, rootfs, rw_rootfs)
                })?;
            } else if hotset_enabled() {
                // ── Phase 6A: memory-FIRST parallel rehydrate ───────────────────
                // Overlap memory/rootfs/vmstate materialization so cold-cache
                // restore approaches max(rootfs, memory) instead of their sum. All
                // files are ATOMICALLY materialized and JOINED before LoadSnapshot.
                // NOT lazy memory / UFFD — File memory still needs a complete file;
                // this is restore I/O scheduling, not guest page hotset. A task
                // error fails closed (Firecracker is never started).
                bench::record("restore.prefetch.plan", Duration::from_micros(0));
                let join_start = Instant::now();
                let (md, rd, vd) = std::thread::scope(
                    |s| -> Result<(Duration, Duration, Duration), SnapshotError> {
                        // memory is spawned first (priority — the per-capsule cold cost).
                        let mem_t = s.spawn(|| {
                            let t = Instant::now();
                            self.rehydrate_atomic(&mem_path, input.store, memory, false).map(|_| t.elapsed())
                        });
                        let rootfs_t = s.spawn(|| {
                            let t = Instant::now();
                            self.rehydrate_atomic(&rootfs_path, input.store, rootfs, rw_rootfs).map(|_| t.elapsed())
                        });
                        let vmstate_t = s.spawn(|| {
                            let t = Instant::now();
                            self.rehydrate_atomic(&vmstate_path, input.store, vmstate, false).map(|_| t.elapsed())
                        });
                        let md = mem_t.join().map_err(|_| self.backend_err("memory prefetch task panicked"))??;
                        let rd = rootfs_t.join().map_err(|_| self.backend_err("rootfs prefetch task panicked"))??;
                        let vd = vmstate_t.join().map_err(|_| self.backend_err("vmstate prefetch task panicked"))??;
                        Ok((md, rd, vd))
                    },
                )?;
                // Record the parallel per-layer durations + the wall-clock join on
                // the main thread (sub-thread thread-locals don't reach the drain).
                bench::record("restore.prefetch.memory", md);
                bench::record("restore.prefetch.rootfs", rd);
                bench::record("restore.prefetch.vmstate", vd);
                bench::record("restore.prefetch.join", join_start.elapsed());
            } else {
                // Sequential rehydrate (default — unchanged restore semantics).
                bench::time("restore.cache_mem", || self.ensure_cached(&mem_path, input.store, memory))?;
                bench::time("restore.cache_vmstate", || self.ensure_cached(&vmstate_path, input.store, vmstate))?;
                bench::time("restore.cache_rootfs", || {
                    self.rehydrate_atomic(&rootfs_path, input.store, rootfs, rw_rootfs)
                })?;
            }

            let port = hc_port(&input.manifest.restore_contract, self.config.healthcheck_port);
            let path = hc_path(&input.manifest.restore_contract, &self.config.healthcheck_path);

            // Per-slot DNAT targets this guest port (see net_up_netns).
            self.net_up(port)?;

            // U1 (#854)/U2 (#855): when ATO_FC_UFFD is set, start the local page-server
            // on a UDS BEFORE LoadSnapshot (Firecracker connects to it during load) and
            // switch the mem_backend from File to Uffd. Default (unset) keeps the File
            // path byte-for-byte. `zero`/`mem` serve from the materialized .mem (U1);
            // `cas` serves lazily from local CAS WITHOUT materializing .mem (U2).
            let sock = input.overlay_root.join(".page-server.sock");
            let mut page_handle: Option<crate::uffd_page_server::PageServerHandle> = None;
            // U8 (#875): shared with the cas source so the receipt can report
            // remote_chunks_fetched.
            let remote_fetches = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            // U12 (#879): identity + keyed store for hotset-profile persistence.
            // ATO_FC_UFFD_HOTSET_STORE=<dir> enables load-before-serve + save-after-
            // health, keyed so a mismatched image/runner/backend is a miss.
            let hotset_key = crate::uffd_page_server::HotsetKey {
                capsule_manifest_hash: input.manifest.capsule_manifest_hash.clone(),
                runner_class_id: input.manifest.runner_class_id.as_ref().map(|r| r.to_string()).unwrap_or_default(),
                memory_image_hash: memory.id().hex().to_string(),
                backend_id: FIRECRACKER_BACKEND_ID.to_string(),
                page_size: 4096,
                memory_size: memory.total_len,
            };
            let hotset_store = std::env::var("ATO_FC_UFFD_HOTSET_STORE")
                .ok()
                .filter(|v| !v.is_empty())
                .map(crate::uffd_page_server::HotsetProfileStore::open);
            if let Some(mode) = uffd {
                let source = match mode {
                    UffdMode::Zero => crate::uffd_page_server::PageSource::Zero,
                    UffdMode::Mem => crate::uffd_page_server::PageSource::mem_file(&mem_path)
                        .map_err(|e| self.backend_err(format!("uffd mem source: {e}")))?,
                    UffdMode::Cas => {
                        let store = capsulefs::CasStore::open(input.store.root())
                            .map_err(|e| self.backend_err(format!("uffd cas store: {e}")))?;
                        // U6 (#859): ATO_FC_UFFD_REMOTE=<root> → read through a remote
                        // CAS on a local miss (fetch + cache local, then serve).
                        let remote = std::env::var("ATO_FC_UFFD_REMOTE")
                            .ok()
                            .filter(|v| !v.is_empty())
                            .and_then(|root| capsulefs::CasStore::open(root).ok());
                        crate::uffd_page_server::PageSource::cas(store, memory.clone(), remote, std::sync::Arc::clone(&remote_fetches))
                    }
                };
                let server = crate::uffd_page_server::PageServer::bind(&sock, source)
                    .map_err(|e| self.backend_err(format!("uffd page-server bind: {e}")))?;
                // U12 (#879): load a persisted hotset profile from the keyed store
                // first (product path; a mismatched identity is a miss, never a
                // wrong-image prefetch), falling back to the explicit
                // ATO_FC_UFFD_HOTSET file (U4 smokes).
                let hotset = hotset_store
                    .as_ref()
                    .and_then(|s| s.load(&hotset_key))
                    .or_else(|| {
                        std::env::var("ATO_FC_UFFD_HOTSET").ok().and_then(|p| {
                            std::fs::read_to_string(&p)
                                .ok()
                                .and_then(|t| serde_json::from_str::<crate::uffd_page_server::HotsetProfile>(&t).ok())
                        })
                    });
                page_handle = Some(server.serve(hotset));
            }

            let fc = bench::time("restore.start_fc", || {
                self.start_fc(&input.overlay_root.join("api.sock"), &input.overlay_root.join("console.log"))
            })?;
            // Phase 8a-HW (#912): the snapshot carries the vsock device with its baked
            // uds_path; FC re-creates that socket on load, so its directory must exist.
            // The artifact self-describes vsock (manifest.has_vsock) so restore preps it
            // without an env flag; ATO_FC_VSOCK still forces it for the smokes.
            let vsock_uds = if vsock_enabled() || input.manifest.has_vsock {
                let uds = vsock_uds_path(&input.manifest.capsule_manifest_hash);
                if let Some(d) = uds.parent() {
                    std::fs::create_dir_all(d).map_err(|e| self.backend_err(format!("vsock dir: {e}")))?;
                }
                let _ = std::fs::remove_file(&uds);
                Some(uds)
            } else {
                None
            };
            bench::time("restore.load_snapshot", || {
                let mem_backend = if uffd.is_some() {
                    json!({"backend_type":"Uffd","backend_path": sock.to_string_lossy()})
                } else {
                    json!({"backend_type":"File","backend_path": mem_path.to_string_lossy()})
                };
                fc.api(self, "PUT", "/snapshot/load", Some(&json!({
                    "snapshot_path": vmstate_path.to_string_lossy(),
                    "mem_backend": mem_backend,
                    "resume_vm": true
                }).to_string()))
            })?;

            // Readiness: U1a (Zero) serves garbage pages, so the guest never reaches
            // health — confirm the fault loop fired instead. Everything else (File,
            // U1b Mem) waits for health as usual.
            //
            // v1.2 PR 3d: a SUPERVISOR artifact wakes with the workload down BY
            // DESIGN (StopWorkload+Revoke ran before the seal), so a TCP health-wait
            // can never pass until the caller delivers the REAL bindings. Its
            // readiness gate is instead: guest-agent reachable over vsock AND not
            // bound-ready (bound-ready out of restore = binding state survived the
            // seal → fail closed).
            let time_to_health_ms: Option<u128> = if input.manifest.supervisor_build.is_some() {
                let uds = vsock_uds.as_ref().ok_or_else(|| {
                    self.backend_err(
                        "supervisor artifact restored without a vsock uds \
                         (manifest.has_vsock must be true for a supervisor build)",
                    )
                })?;
                let probe_start = Instant::now();
                bench::time("restore.probe_agent", || self.probe_restored_agent_unbound(uds))?;
                Some(probe_start.elapsed().as_millis())
            } else {
                match (uffd, &page_handle) {
                    // U1a: zero pages → never reaches health; confirm the loop fired.
                    (Some(UffdMode::Zero), Some(h)) => {
                        h.wait_for_first_fault(Duration::from_secs(10));
                        None
                    }
                    // U1b/U2 (uffd mem/cas): wait for health but FAIL CLOSED FAST (U5) if
                    // the page-server hits a fatal CAS miss/corrupt — don't burn the full
                    // timeout booting a VM on memory that can never be served.
                    (Some(_), Some(h)) => {
                        let ms = self.wait_health_until(port, &path, || h.failed())?;
                        h.mark_health_reached();
                        Some(ms)
                    }
                    // File backend (default): unchanged.
                    _ => Some(bench::time("restore.wait_health", || self.wait_health(port, &path))?),
                }
            };

            // U1: snapshot a receipt + (U3) the per-restore fault trace for the smoke.
            if let Some(h) = &page_handle {
                let mut r = h.receipt(time_to_health_ms.is_some(), time_to_health_ms);
                // U8 (#875): fill the restore-level context so the receipt is the
                // stable, File-comparable schema.
                r.backend = FIRECRACKER_BACKEND_ID.to_string();
                r.mem_backend = "uffd".to_string();
                r.source = match uffd {
                    Some(UffdMode::Zero) => "zero".to_string(),
                    Some(UffdMode::Mem) => "file".to_string(),
                    Some(UffdMode::Cas) if remote_fetches.load(std::sync::atomic::Ordering::SeqCst) > 0 => "remote_cas".to_string(),
                    _ => "local_cas".to_string(),
                };
                r.capsule_manifest_hash = input.manifest.capsule_manifest_hash.clone();
                r.runner_class_id = input.manifest.runner_class_id.as_ref().map(|id| id.to_string());
                r.memory_image_hash = memory.id().hex().to_string();
                r.memory_bytes_total = memory.total_len;
                // Mem mode mmaps the fully materialized .mem; cas/zero materialize nothing.
                r.memory_bytes_materialized = if uffd == Some(UffdMode::Mem) { memory.total_len } else { 0 };
                r.pages_total = memory.total_len.div_ceil(4096);
                r.remote_chunks_fetched = remote_fetches.load(std::sync::atomic::Ordering::SeqCst);
                r.restore_total_ms = Some(restore_start.elapsed().as_millis());
                let _ = std::fs::write(
                    input.overlay_root.join(".uffd-receipt.json"),
                    serde_json::to_string_pretty(&r).unwrap_or_default(),
                );
                let _ = std::fs::write(
                    input.overlay_root.join(".hotset-trace.json"),
                    serde_json::to_string(&h.trace()).unwrap_or_default(),
                );
                // U12 (#879): persist this run's pre-health hotset as the profile for
                // this exact identity, so the NEXT restore prefetches it. Only when a
                // profile store is configured and the VM actually reached health.
                if let Some(store) = &hotset_store
                    && r.vm_reaches_health
                {
                    let profile = crate::uffd_page_server::HotsetProfile::from_trace(&h.trace());
                    if !profile.offsets.is_empty() {
                        let _ = store.save(&hotset_key, &profile);
                    }
                }
            }

            let session_id = format!("fc-{}-{}", manifest_short(&input.manifest), std::process::id());
            let child = fc.detach().ok_or_else(|| self.backend_err("lost firecracker child after restore"))?;
            let _ = std::fs::write(input.overlay_root.join(".fc-session.json"), json!({
                "pid": child.id(), "tap": self.config.tap_dev, "session_id": session_id,
                // L5 (#912): record the vsock UDS so a cross-process `ato stop` can unlink it.
                "vsock_uds": vsock_uds.as_ref().map(|p| p.to_string_lossy().to_string()),
                // #948 N-slot: record the namespace + root veth so a cross-process
                // `ato stop` (fresh backend, empty config) tears down the exact
                // per-slot network state this restore created.
                "netns": self.config.netns,
                "veth_root": self.config.veth_root,
            }).to_string());
            let session = RestoredSession {
                session_id,
                backend_id: FIRECRACKER_BACKEND_ID.to_string(),
                guest_port: Some(port),
                overlay_root: input.overlay_root.clone(),
                restored_bytes,
                vmm_pid: Some(child.id() as i32),
                vsock_uds,
                // Where the restored workload is reachable from the root namespace:
                // the guest IP directly (legacy) or the per-slot ingress (netns).
                // Any fronting proxy dials this exact address.
                workload_addr: Some(format!("{}:{}", self.reachable_host(), port)),
            };
            Ok((session, child, page_handle))
        })();

        match result {
            Ok((session, child, page_handle)) => {
                self.sessions.lock().unwrap().insert(session.session_id.clone(), child);
                if let Some(h) = page_handle {
                    self.page_servers.lock().unwrap().insert(session.session_id.clone(), h);
                }
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
        // Read the session record FIRST: a cross-process `ato stop` has a fresh
        // backend (empty in-memory registry) and possibly a different ATO_FC_TAP
        // env than the run process, so the authoritative pid + tap come from
        // .fc-session.json (written at restore), not self.config / self.sessions.
        let meta = std::fs::read_to_string(session.overlay_root.join(".fc-session.json")).unwrap_or_default();
        let recorded_tap = json_str(&meta, "tap");
        let tap = recorded_tap.as_deref().unwrap_or(&self.config.tap_dev);
        // #948 N-slot: the recorded namespace (if any) is authoritative for a
        // cross-process `ato stop` whose fresh backend has an empty config.
        let recorded_netns = json_str(&meta, "netns").filter(|s| !s.is_empty());
        let netns = recorded_netns.as_deref().or(self.config.netns.as_deref());
        let recorded_veth = json_str(&meta, "veth_root").filter(|s| !s.is_empty());
        let veth_root = recorded_veth.as_deref().or(self.config.veth_root.as_deref());

        // FIXED TEARDOWN ORDER (#948): (1) kill+reap the VMM, (2) wait for exit,
        // BEFORE removing the namespace — `ip netns del` while firecracker is
        // still attached would drop the named-ns bind mount but leave the live
        // namespace held by the process, leaking it invisibly.
        if let Some(mut child) = self.sessions.lock().unwrap().remove(&session.session_id) {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = session.vmm_pid.map(|p| p as u32).or_else(|| json_u32(&meta, "pid")) {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
        // U1 (#854): stop + join the page-server thread (if any) AFTER killing the
        // child, so the guest stops faulting and the uffd read hits EOF. The
        // .page-server.sock is removed by the overlay teardown below.
        if let Some(h) = self.page_servers.lock().unwrap().remove(&session.session_id) {
            let _ = h.stop_and_join();
        }
        // (3) Tear down the network + (5) the per-slot lockfile. In netns mode
        // `ip netns del` atomically removes the in-ns tap, the in-ns veth end,
        // and all in-ns iptables rules; (4) the root veth end is deleted too.
        // The lock is keyed on the namespace (netns) else the tap (legacy).
        if let Some(ns) = netns {
            let _ = Command::new("ip").args(["netns", "del", ns]).status();
            if let Some(v) = veth_root {
                let _ = Command::new("ip").args(["link", "del", v]).status();
            }
            let _ = std::fs::remove_file(self.config.work_root.join(format!("{ns}.lock")));
        } else {
            let _ = Command::new("ip").args(["link", "del", tap]).status();
            let _ = std::fs::remove_file(self.config.work_root.join(format!("{tap}.lock")));
        }
        // L5 (#912): remove the Firecracker vsock host UDS so it does not linger after
        // teardown (Firecracker does not unlink it on exit). The session carries it
        // when this stop() came from restore; a cross-process `ato stop` recomputes the
        // deterministic path from the recorded manifest hash.
        if let Some(uds) = session
            .vsock_uds
            .clone()
            .or_else(|| json_str(&meta, "vsock_uds").map(std::path::PathBuf::from))
        {
            let _ = std::fs::remove_file(&uds);
        }
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
/// Extract a quoted string value for `key` from a flat JSON object (no deps).
fn json_str(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let i = s.find(&needle)? + needle.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── #948 N-slot: per-slot netns config derivation + host-path isolation ──

    #[test]
    fn for_slot_netns_off_is_legacy_identity() {
        let base = FirecrackerConfig::default();
        let c = FirecrackerConfig::for_slot(0, false, &base);
        assert!(c.netns.is_none() && c.ingress_ip.is_none() && c.veth_root.is_none());
        // legacy reachable host is the guest IP; lock is tap-keyed.
        assert_eq!(FirecrackerBackend::with_config(c.clone()).reachable_host(), c.guest_ip);
        assert!(FirecrackerBackend::with_config(c).lock_path().to_string_lossy().contains("fctap0.lock"));
    }

    #[test]
    fn for_slot_netns_on_derives_distinct_per_slot_addressing() {
        let base = FirecrackerConfig::default();
        let s0 = FirecrackerConfig::for_slot(0, true, &base);
        let s1 = FirecrackerConfig::for_slot(1, true, &base);
        // slot 0 is ALSO namespaced when netns is on (all-or-nothing).
        assert_eq!(s0.netns.as_deref(), Some("ato-slot-0"));
        assert_eq!(s1.netns.as_deref(), Some("ato-slot-1"));
        // frozen snapshot addressing is IDENTICAL across slots (isolated by ns)…
        assert_eq!(s0.tap_dev, s1.tap_dev);
        assert_eq!(s0.guest_ip, s1.guest_ip);
        // …while every host-visible handle is distinct per slot. (Prefix-
        // agnostic — the exact CIDR is covered by the override test, which
        // mutates the shared env and so can't assert exact values in parallel.)
        assert_ne!(s0.ingress_ip, s1.ingress_ip);
        assert_ne!(s0.veth_root, s1.veth_root);
        assert_ne!(s0.veth_ns, s1.veth_ns);
        assert!(s0.ingress_ip.as_deref().unwrap().ends_with(".0.2"));
        assert!(s1.ingress_ip.as_deref().unwrap().ends_with(".1.2"));
        // reachable host is the per-slot ingress, not the shared guest IP.
        let s1_ingress = s1.ingress_ip.clone().unwrap();
        assert_eq!(FirecrackerBackend::with_config(s1).reachable_host(), s1_ingress);
    }

    #[test]
    fn per_slot_lock_paths_are_distinct_for_same_snapshot() {
        // The bug this guards: two slots share tap `fctap0`, so a tap-keyed lock
        // would re-serialize them. Namespaced slots get namespace-keyed locks.
        let base = FirecrackerConfig::default();
        let l0 = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(0, true, &base)).lock_path();
        let l1 = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(1, true, &base)).lock_path();
        assert_ne!(l0, l1);
        assert!(l0.to_string_lossy().contains("ato-slot-0.lock"));
        assert!(l1.to_string_lossy().contains("ato-slot-1.lock"));
    }

    #[test]
    fn for_slot_honors_cidr_prefix_override() {
        // SAFETY: single-threaded test; restores the var before returning.
        let prev = std::env::var("ATO_FC_NETNS_CIDR_PREFIX").ok();
        unsafe { std::env::set_var("ATO_FC_NETNS_CIDR_PREFIX", "10.99") };
        let c = FirecrackerConfig::for_slot(2, true, &FirecrackerConfig::default());
        assert_eq!(c.ingress_ip.as_deref(), Some("10.99.2.2"));
        assert_eq!(c.veth_root_ip.as_deref(), Some("10.99.2.1"));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ATO_FC_NETNS_CIDR_PREFIX", v),
                None => std::env::remove_var("ATO_FC_NETNS_CIDR_PREFIX"),
            }
        }
    }

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
        // U0 UFFD facet invariant: false ⇒ a concrete reason; true ⇒ no reason.
        // (On this test host — non-x86_64 or no /dev/kvm — it is false with a reason.)
        if p.supports_uffd_mem_backend {
            assert!(p.uffd_reason.is_none());
        } else {
            assert!(p.uffd_reason.is_some(), "unsupported UFFD must carry a reason");
        }
    }

    #[test]
    fn restore_is_unsupported_without_kvm() {
        if FirecrackerBackend::kvm_present() { return; }
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = err_manifest();
        assert!(FirecrackerBackend::new().inspect(&store, &m).is_ok()); // inspect needs no KVM
        let backend = FirecrackerBackend::new();
        let input = RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov"), host_runner_class: None, uffd_preview: false };
        assert!(matches!(backend.restore(input), Err(SnapshotError::Unsupported { .. })));
    }

    #[test]
    fn build_preflight_rejects_declared_marker_before_kvm_and_store() {
        // Even on a KVM-less host, a declared marker in rootfs is rejected by the
        // preflight gate BEFORE ensure_available()/any CAS store — proving the
        // gate-before-store invariant on the Firecracker path.
        use crate::manifest::{RestoreContract, SanitizerContract};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let input = BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: "blake3:preflight".to_string(),
            runner_class: None,
            layers: BuildLayers {
                rootfs: b"....PREFLIGHT_MARKER_XYZ....".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract { ports: vec![8080], healthcheck: Some("/health".to_string()), expected_ready_ms: Some(3000) },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec!["PREFLIGHT_MARKER_XYZ".to_string()],
            execution_id: None,
            supervisor: None,
        };
        let err = FirecrackerBackend::new().build_ready_state(input).unwrap_err();
        assert!(matches!(err, SnapshotError::SecretFoundInSnapshot(_)), "preflight must reject before KVM/store: {err:?}");
        assert!(store.list_chunks().unwrap().is_empty(), "rejected build must persist no rootfs in CAS");
    }

    // ── v1.2 PR 3d: supervisor build drive ────────────────────────────────────

    #[test]
    fn boot_args_add_page_hygiene_only_for_supervisor_builds() {
        let b = FirecrackerBackend::new();
        let plain = b.boot_args(false);
        // The no-binding cmdline is byte-identical to the historical string.
        assert!(plain.starts_with("console=ttyS0 reboot=k panic=1 pci=off ip="), "{plain}");
        assert!(!plain.contains("init_on_free"), "no hygiene args on the no-binding path: {plain}");
        let hardened = b.boot_args(true);
        assert!(hardened.contains("init_on_free=1 init_on_alloc=1 page_poison=1"), "{hardened}");
        // Hygiene args are inserted, nothing else changes.
        assert_eq!(hardened.replace(" init_on_free=1 init_on_alloc=1 page_poison=1", ""), plain);
    }

    #[test]
    fn build_placeholders_are_unique_and_prefixed() {
        let a = generate_build_placeholder().unwrap();
        let b = generate_build_placeholder().unwrap();
        assert!(a.starts_with("ATO-BUILD-PLACEHOLDER-"), "{a}");
        assert_ne!(a, b, "placeholders must be unique per generation");
    }

    #[test]
    fn supervisor_drive_validates_names_and_never_logs_values() {
        // Valid lowercase binding names prepare fine; each gets a distinct placeholder.
        let drive = SupervisorDrive::prepare(&SupervisorBindings {
            binding_names: vec!["openai_api_key".into(), "db_url".into()],
        })
        .expect("prepare");
        assert_eq!(drive.leases.len(), 2);
        assert_eq!(drive.placeholder_values.len(), 2);
        assert_ne!(drive.placeholder_values[0], drive.placeholder_values[1]);
        // An invalid (uppercase) name fails closed — the backend revalidates
        // rather than trusting the emission layer. (match, not unwrap_err: the
        // drive deliberately has no Debug impl — it holds placeholder values.)
        let err = match SupervisorDrive::prepare(&SupervisorBindings {
            binding_names: vec!["OPENAI_API_KEY".into()],
        }) {
            Err(e) => e,
            Ok(_) => panic!("uppercase binding name must fail closed"),
        };
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
        // Empty set is not a supervisor build.
        assert!(SupervisorDrive::prepare(&SupervisorBindings { binding_names: vec![] }).is_err());
    }

    #[test]
    fn supervisor_build_without_vsock_fails_closed_before_boot() {
        // SAFETY: test-local var, removed before returning.
        unsafe { std::env::remove_var("ATO_FC_VSOCK") };
        use crate::manifest::{RestoreContract, SanitizerContract};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let input = BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: "blake3:supervisor-no-vsock".to_string(),
            runner_class: None,
            layers: BuildLayers {
                rootfs: b"rootfs".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract { ports: vec![8080], healthcheck: Some("/health".to_string()), expected_ready_ms: Some(3000) },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: None,
            supervisor: Some(SupervisorBindings { binding_names: vec!["openai_api_key".into()] }),
        };
        let err = FirecrackerBackend::new().build_ready_state(input).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ATO_FC_VSOCK"), "must name the missing flag: {msg}");
        assert!(store.list_chunks().unwrap().is_empty(), "no bytes stored on the fail-closed path");
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
    fn hotset_flag_detection() {
        // SAFETY: serial within this fn; vars restored at the end.
        let p1 = std::env::var("ATO_READY_STATE_HOTSET").ok();
        let p2 = std::env::var("ATO_READY_STATE_PREFETCH").ok();
        let set = |k: &str, v: Option<&str>| unsafe {
            match v { Some(v) => std::env::set_var(k, v), None => std::env::remove_var(k) }
        };
        set("ATO_READY_STATE_HOTSET", None);
        set("ATO_READY_STATE_PREFETCH", None);
        assert!(!hotset_enabled(), "off by default");
        set("ATO_READY_STATE_HOTSET", Some("1"));
        assert!(hotset_enabled());
        set("ATO_READY_STATE_HOTSET", Some("0"));
        set("ATO_READY_STATE_PREFETCH", Some("memory"));
        assert!(hotset_enabled(), "PREFETCH=memory enables");
        set("ATO_READY_STATE_PREFETCH", Some("rootfs"));
        assert!(!hotset_enabled(), "PREFETCH=rootfs does not enable memory-first");
        set("ATO_READY_STATE_HOTSET", p1.as_deref());
        set("ATO_READY_STATE_PREFETCH", p2.as_deref());
    }

    /// U7 (#874): the UFFD gate is env-only and defaults to the File backend. With
    /// `ATO_FC_UFFD` unset, `uffd_mode()` is `None` → restore() uses the File
    /// `mem_backend` (the default path invariant). This is the guard that keeps the
    /// spike from leaking into the product path.
    #[test]
    fn uffd_mode_is_env_only_and_defaults_to_file() {
        // SAFETY: serial within this fn; var restored at the end.
        let prev = std::env::var("ATO_FC_UFFD").ok();
        let set = |v: Option<&str>| unsafe {
            match v { Some(v) => std::env::set_var("ATO_FC_UFFD", v), None => std::env::remove_var("ATO_FC_UFFD") }
        };
        set(None);
        assert_eq!(uffd_mode(), None, "unset ⇒ File backend (default path)");
        set(Some("0"));
        assert_eq!(uffd_mode(), None, "0 ⇒ File");
        set(Some("file"));
        assert_eq!(uffd_mode(), None, "file ⇒ File");
        set(Some("zero"));
        assert_eq!(uffd_mode(), Some(UffdMode::Zero));
        set(Some("mem"));
        assert_eq!(uffd_mode(), Some(UffdMode::Mem));
        set(Some("cas"));
        assert_eq!(uffd_mode(), Some(UffdMode::Cas));
        set(Some("1"));
        assert_eq!(uffd_mode(), Some(UffdMode::Cas));
        set(Some("garbage"));
        assert_eq!(uffd_mode(), None, "unknown token ⇒ File (fail safe to default)");
        set(prev.as_deref());
    }

    #[test]
    fn rehydrate_atomic_materializes_caches_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let blob = store_blob(&store, LayerKind::Memory, b"mem-bytes-xyz", ChunkingKind::ContentDefined).unwrap();
        let b = FirecrackerBackend::new();
        let path = dir.path().join("layers").join("mem.bin");

        // materializes the full content (atomic).
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"mem-bytes-xyz");

        // cache hit: second non-forced call does not rewrite.
        let m1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), m1, "cached layer must not be rewritten");

        // always=true forces a fresh write (rw rootfs semantics).
        b.rehydrate_atomic(&path, &store, &blob, true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"mem-bytes-xyz");

        // no leftover temp files (atomic temp+rename cleaned up).
        let temps = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(temps, 0, "atomic write must leave no temp files");
    }

    #[test]
    fn rehydrate_atomic_discards_wrong_size_cache_and_rehydrates() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let blob = store_blob(&store, LayerKind::Memory, b"good-memory-bytes", ChunkingKind::ContentDefined).unwrap();
        let b = FirecrackerBackend::new();
        let path = dir.path().join("layers").join("mem.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A corrupt (truncated) cached file must be detected by the size check and
        // re-rehydrated from CAS — never trusted into LoadSnapshot.
        std::fs::write(&path, b"truncated").unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"good-memory-bytes", "corrupt cache repaired from CAS");

        // A correct cache (size matches) is a no-op hit (not rewritten).
        let m1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), m1, "valid cache must not be rewritten");
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
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            layers: ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: SnapshotBackendInfo { kind: FIRECRACKER_BACKEND_ID.to_string(), version: "0".to_string(), snapshot_format_version: SNAPSHOT_FORMAT.to_string(), cpu_template: None },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        }
    }
}

#[cfg(test)]
mod kvm_tests;
