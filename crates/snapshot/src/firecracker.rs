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
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use capsule::foundation::install_lifecycle::RunnerClassFacts;
use capsule::snapshot_manifest::{
    PortabilityTier, SNAPSHOT_COMPATIBILITY_V1_SCHEMA, SnapshotBackendKind,
    SnapshotCompatibilityContractV1,
};
use capsulefs::{
    BlobManifest, CasStore, ChunkingKind, HotsetRecorder, LayerKind, LazyBlobReader, store_blob,
};
use serde_json::json;

#[cfg(unix)]
use crate::agent_channel::{AgentChannel, FirecrackerAgentChannel, GUEST_AGENT_VSOCK_PORT};
#[cfg(test)]
use crate::backend::BuildLayers;
use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    SupervisorBindings, TeardownReceipt, compatibility_class_identity,
};
use crate::bench;
#[cfg(test)]
use crate::manifest::ReadyStateLayers;
use crate::manifest::{
    NoSecretProof, READY_STATE_SCHEMA, ReadyStateManifest, RestoreContract, SnapshotBackendInfo,
    SupervisorBuildReceipt,
};
use crate::scanner;
#[cfg(unix)]
use protocol::binding_control::{AgentToHost, HostToAgent};
use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

pub const FIRECRACKER_BACKEND_ID: &str = "firecracker";
const KVM_DEVICE: &str = "/dev/kvm";
const SNAPSHOT_FORMAT: &str = "fc-full-file-v1";
/// Numeric generation counterpart to [`SNAPSHOT_FORMAT`] (whose name embeds
/// the same generation as its `-v1` suffix), for
/// `SnapshotCompatibilityContractV1::format_version` (a `u32`, not the
/// descriptive format string).
const SNAPSHOT_FORMAT_VERSION: u32 = 1;
const DEVICE_PROFILE: &str = "virtio-blk+virtio-net+vsock";
const NETWORK_MODEL: &str = "tap";
/// Ceiling for a per-job `boot_timeout` override (`with_boot_timeout`). A build
/// that hasn't reached readiness in 10 min is wedged; the cap keeps a single job
/// from pinning the builder forever regardless of what the job requests.
const MAX_JOB_BOOT_TIMEOUT_S: u64 = 600;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
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
    /// v1.4 (ato#970): per-slot vsock UDS isolation. The vsock host UDS path is
    /// BAKED into the snapshot (deterministic per capsule hash — Firecracker
    /// cannot override it at load), so two concurrent restores of the same
    /// capsule collide on it. When `Some`, the VMM runs in a PRIVATE MOUNT
    /// NAMESPACE with this directory bind-mounted over the baked path's parent
    /// (`$TMPDIR/ato-vsock`): the guest-facing socket lands in this per-slot
    /// directory, where the host dials it. `None` = legacy shared path.
    pub vsock_slot_dir: Option<PathBuf>,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: env_or("ATO_FC_BIN", "firecracker"),
            kernel_path: PathBuf::from(env_or("ATO_FC_KERNEL", "vmlinux")),
            base_rootfs_path: std::env::var("ATO_FC_BASE_ROOTFS")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
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
            cpu_template: std::env::var("ATO_FC_CPU_TEMPLATE")
                .ok()
                .filter(|v| !v.is_empty()),
            boot_timeout: Duration::from_secs(
                env_or("ATO_FC_BOOT_TIMEOUT_S", "30").parse().unwrap_or(30),
            ),
            // Legacy single-slot by default; `for_slot` fills these when netns-on.
            netns: None,
            ingress_ip: None,
            veth_root: None,
            veth_root_ip: None,
            veth_ns: None,
            vsock_slot_dir: None,
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
    pub fn for_slot(
        index: usize,
        netns_enabled: bool,
        base: &FirecrackerConfig,
    ) -> FirecrackerConfig {
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
        // v1.4 (ato#970): each slot gets a private view of the baked vsock UDS
        // parent dir, so concurrent supervisor restores never share a socket path.
        // Under `/run` (root-owned, NOT sticky world-writable like /tmp) so an
        // unprivileged local user cannot pre-plant the path or a symlink — the
        // bind-mount SOURCE must never be attacker-influencable (restore also
        // verifies it at use, see `ensure_private_dir`). The dir name is
        // deliberately TERSE: the host-side dial path is
        // `<dir>/blake3_<64-hex>.sock` (76 bytes of file name) and AF_UNIX caps
        // sun_path at ~108 bytes — `/tmp/ato-vsock-slots/ato-slot-0/…` was
        // exactly 108 and failed SUN_LEN on the first live restore.
        // This is a HOST-side dial path convention (AF_UNIX, always Linux —
        // Firecracker itself is Linux-only), never a native filesystem path
        // of the platform running this code — build it with an explicit
        // forward slash rather than `PathBuf::join`, which would emit `\` on
        // a non-Linux *build/test* host and corrupt the socket path string.
        c.vsock_slot_dir = Some(PathBuf::from(format!("/run/ato/vsk/{index}")));
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
        Self {
            config,
            sessions: Arc::default(),
            page_servers: Arc::default(),
        }
    }

    /// Return a clone of this backend with a per-job readiness `boot_timeout`
    /// override (compose_import: heavy images — Java/Spring, big JVMs — need a
    /// larger budget than the env default; a plain 2-service app should not be
    /// forced to wait the whole ceiling before failing). `None` keeps the
    /// env/default. The override is CLAMPED to `[1, MAX_JOB_BOOT_TIMEOUT_S]` so a
    /// job can never pin the builder on a hung guest indefinitely. Cloning is
    /// cheap: `config` is small and the session/page-server maps are `Arc`s.
    pub fn with_boot_timeout(&self, secs: Option<u64>) -> Self {
        let mut b = self.clone();
        if let Some(s) = secs {
            b.config.boot_timeout = Duration::from_secs(s.clamp(1, MAX_JOB_BOOT_TIMEOUT_S));
        }
        b
    }

    pub fn kvm_present() -> bool {
        Path::new(KVM_DEVICE).exists()
    }

    fn detect_version(&self) -> Option<String> {
        let out = Command::new(&self.config.firecracker_bin)
            .arg("--version")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for tok in text.split_whitespace() {
            if let Some(v) = tok.strip_prefix('v')
                && v.split('.').count() >= 2
            {
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
            return Err(
                self.unsupported(format!("{KVM_DEVICE} not present; Firecracker needs KVM"))
            );
        }
        if self.detect_version().is_none() {
            return Err(self.unsupported(format!(
                "firecracker binary '{}' not found or not runnable",
                self.config.firecracker_bin
            )));
        }
        Ok(())
    }

    fn runner_facts(&self) -> RunnerClassFacts {
        let mut f = RunnerClassFacts::from_host();
        f.vmm = FIRECRACKER_BACKEND_ID.to_string();
        f.vmm_version = self
            .detect_version()
            .unwrap_or_else(|| "unknown".to_string());
        f.snapshot_format = SNAPSHOT_FORMAT.to_string();
        f.cpu_template = self.config.cpu_template.clone();
        f.guest_kernel_id =
            blake3_file(&self.config.kernel_path).unwrap_or_else(|| "unset".to_string());
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
            version: self
                .detect_version()
                .unwrap_or_else(|| "unknown".to_string()),
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
        let hygiene = if page_hygiene {
            " init_on_free=1 init_on_alloc=1 page_poison=1"
        } else {
            ""
        };
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
        self.config
            .ingress_ip
            .as_deref()
            .unwrap_or(&self.config.guest_ip)
    }

    /// Stable cache path keyed on a layer's content id (no content read needed),
    /// so build and restore agree and large immutable layers are rehydrated from
    /// CapsuleFS at most once, then reused across restores.
    fn cache_path(&self, kind: &str, blob: &BlobManifest, ext: &str) -> PathBuf {
        self.config
            .work_root
            .join(kind)
            .join(format!("{}.{ext}", blob_id_hex(blob)))
    }
    /// Rehydrate a layer to `path`. `always` forces a fresh write (rw rootfs);
    /// otherwise it is a no-op when the file is already cached. Materialization is
    /// ATOMIC (write a temp file, then rename) so Firecracker never sees a partial
    /// memory/rootfs file — required for the parallel prefetch path (Phase 6A).
    fn rehydrate_atomic(
        &self,
        path: &Path,
        store: &CasStore,
        blob: &BlobManifest,
        always: bool,
    ) -> Result<(), SnapshotError> {
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
    fn ensure_cached(
        &self,
        path: &Path,
        store: &CasStore,
        blob: &BlobManifest,
    ) -> Result<(), SnapshotError> {
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
            self.backend_err(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
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
        std::fs::create_dir_all(&self.config.work_root)
            .map_err(|e| self.backend_err(e.to_string()))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.lock_path())
        {
            Ok(mut f) => {
                let _ = f.write_all(owner.as_bytes());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(self.backend_err(format!(
                    "single-session backend busy: tap '{}' is held by another session (lock {})",
                    self.config.tap_dev,
                    self.lock_path().display()
                )))
            }
            Err(e) => Err(self.backend_err(format!("acquire lock: {e}"))),
        }
    }
    fn release_lock(&self) {
        let _ = std::fs::remove_file(self.lock_path());
    }

    fn run_ip(&self, args: &[&str]) -> Result<(), SnapshotError> {
        let status = Command::new("ip")
            .args(args)
            .status()
            .map_err(|e| self.backend_err(format!("spawn `ip {}`: {e}", args.join(" "))))?;
        if status.success() {
            Ok(())
        } else {
            Err(self.backend_err(format!("`ip {}` failed", args.join(" "))))
        }
    }
    /// `ip netns exec <ns> <argv…>` — run a host command inside a namespace.
    fn run_in_netns(&self, ns: &str, argv: &[&str]) -> Result<(), SnapshotError> {
        let mut a = vec!["netns", "exec", ns];
        a.extend_from_slice(argv);
        let status = Command::new("ip").args(&a).status().map_err(|e| {
            self.backend_err(format!(
                "spawn `ip netns exec {ns} {}`: {e}",
                argv.join(" ")
            ))
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(self.backend_err(format!("`ip netns exec {ns} {}` failed", argv.join(" "))))
        }
    }

    fn net_up(&self, guest_ports: &[u16]) -> Result<(), SnapshotError> {
        match self.config.netns.clone() {
            None => self.net_up_root(),
            Some(ns) => self.net_up_netns(&ns, guest_ports),
        }
    }

    /// Legacy single-slot networking in the ROOT namespace (unchanged).
    fn net_up_root(&self) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let _ = Command::new("ip").args(["link", "del", tap]).status();
        self.run_ip(&["tuntap", "add", "dev", tap, "mode", "tap"])?;
        self.run_ip(&[
            "addr",
            "add",
            &format!("{}/24", self.config.host_ip),
            "dev",
            tap,
        ])?;
        self.run_ip(&["link", "set", tap, "up"])?;
        Ok(())
    }

    /// Per-slot networking (#948 N-slot): the frozen tap (`fctap0`) + guest
    /// (`172.16.0.2`) live inside namespace `ns`, reached from the root namespace
    /// at `ingress_ip` via a veth `/30` + in-ns DNAT to the guest. All addresses
    /// are integer-derived and passed as argv (no shell). Idempotent: a stale
    /// namespace from a crashed prior run is torn down first.
    fn net_up_netns(&self, ns: &str, guest_ports: &[u16]) -> Result<(), SnapshotError> {
        let tap = &self.config.tap_dev;
        let host_ip = &self.config.host_ip;
        let guest_ip = &self.config.guest_ip;
        let veth_root = self
            .config
            .veth_root
            .as_deref()
            .ok_or_else(|| self.backend_err("netns config missing veth_root"))?;
        let veth_ns = self
            .config
            .veth_ns
            .as_deref()
            .ok_or_else(|| self.backend_err("netns config missing veth_ns"))?;
        let veth_root_ip = self
            .config
            .veth_root_ip
            .as_deref()
            .ok_or_else(|| self.backend_err("netns config missing veth_root_ip"))?;
        let ingress_ip = self
            .config
            .ingress_ip
            .as_deref()
            .ok_or_else(|| self.backend_err("netns config missing ingress_ip"))?;
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
        self.run_ip(&[
            "link", "add", veth_root, "type", "veth", "peer", "name", veth_ns,
        ])?;
        self.run_ip(&["link", "set", veth_ns, "netns", ns])?;
        self.run_ip(&["addr", "add", &veth_root_cidr, "dev", veth_root])?;
        self.run_ip(&["link", "set", veth_root, "up"])?;
        self.run_in_netns(ns, &["ip", "addr", "add", &ingress_cidr, "dev", veth_ns])?;
        self.run_in_netns(ns, &["ip", "link", "set", veth_ns, "up"])?;
        // Forward + DNAT the ingress to the guest, MASQUERADE toward the tap so
        // the guest replies to a same-subnet source. All rules stay inside `ns`
        // (root namespace is left untouched → teardown is just `ip netns del`).
        self.run_in_netns(ns, &["sysctl", "-q", "-w", "net.ipv4.ip_forward=1"])?;
        for guest_port in guest_ports {
            let port = guest_port.to_string();
            let dnat = format!("{guest_ip}:{port}");
            self.run_in_netns(
                ns,
                &[
                    "iptables",
                    "-t",
                    "nat",
                    "-A",
                    "PREROUTING",
                    "-d",
                    ingress_ip,
                    "-p",
                    "tcp",
                    "--dport",
                    &port,
                    "-j",
                    "DNAT",
                    "--to-destination",
                    &dnat,
                ],
            )?;
        }
        self.run_in_netns(
            ns,
            &[
                "iptables",
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-o",
                tap,
                "-j",
                "MASQUERADE",
            ],
        )?;
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
                let _ = Command::new("ip")
                    .args(["link", "del", &self.config.tap_dev])
                    .status();
            }
        }
    }

    /// The base command to launch firecracker — wrapped in `ip netns exec <ns>`
    /// when this slot is namespaced so the VMM (and its tap) live inside `ns`.
    ///
    /// v1.4 (ato#970): when the restore also needs vsock isolation
    /// (`vsock_isolation` = a supervisor artifact under netns), the VMM is
    /// additionally wrapped in a PRIVATE MOUNT NAMESPACE (`unshare -m`, which
    /// makes mounts rprivate) with the slot's own directory bind-mounted over
    /// the BAKED vsock UDS parent — the socket FC re-creates at the baked path
    /// then lands in the per-slot directory, where the host dials it. The bind
    /// mount is invisible outside the VMM's namespace and dies with it. Both
    /// directories are created by the caller before spawn (same underlying fs).
    fn fc_command(&self, vsock_isolation: bool) -> Command {
        match &self.config.netns {
            Some(ns) => match self
                .config
                .vsock_slot_dir
                .as_ref()
                .filter(|_| vsock_isolation)
            {
                Some(slot_dir) => {
                    let mut c = Command::new("ip");
                    c.args([
                        "netns",
                        "exec",
                        ns,
                        "unshare",
                        "--mount",
                        "sh",
                        "-c",
                        // $1 = per-slot dir, $2 = baked vsock parent; the rest is
                        // the VMM argv (start_fc appends --api-sock etc. after
                        // the firecracker binary below).
                        r#"mount --bind "$1" "$2" && shift 2 && exec "$@""#,
                        "sh",
                    ]);
                    c.arg(slot_dir);
                    c.arg(vsock_uds_parent_dir());
                    c.arg(&self.config.firecracker_bin);
                    c
                }
                None => {
                    let mut c = Command::new("ip");
                    c.args(["netns", "exec", ns, &self.config.firecracker_bin]);
                    c
                }
            },
            None => Command::new(&self.config.firecracker_bin),
        }
    }

    fn start_fc(&self, sock: &Path, console_log: &Path) -> Result<FcProcess, SnapshotError> {
        self.start_fc_with(sock, console_log, false)
    }

    fn start_fc_with(
        &self,
        sock: &Path,
        console_log: &Path,
        vsock_isolation: bool,
    ) -> Result<FcProcess, SnapshotError> {
        let _ = std::fs::remove_file(sock);
        let log = std::fs::File::create(console_log)
            .map_err(|e| self.backend_err(format!("create console log: {e}")))?;
        let child = self
            .fc_command(vsock_isolation)
            .arg("--api-sock")
            .arg(sock)
            .stdout(Stdio::from(
                log.try_clone()
                    .map_err(|e| self.backend_err(e.to_string()))?,
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| self.backend_err(format!("spawn firecracker: {e}")))?;
        let mut fc = FcProcess {
            child: Some(child),
            sock: sock.to_path_buf(),
        };
        for _ in 0..100 {
            if sock.exists() {
                return Ok(fc);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        fc.kill_now();
        Err(self.backend_err("firecracker api socket never appeared"))
    }

    fn configure_boot(
        &self,
        fc: &FcProcess,
        kernel: &Path,
        rootfs: &Path,
        read_only: bool,
        page_hygiene: bool,
    ) -> Result<(), SnapshotError> {
        let mc = if let Some(t) = &self.config.cpu_template {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib, "cpu_template": t})
        } else {
            json!({"vcpu_count": self.config.vcpu_count, "mem_size_mib": self.config.mem_size_mib})
        };
        fc.api(self, "PUT", "/machine-config", Some(&mc.to_string()))?;
        fc.api(self, "PUT", "/boot-source", Some(&json!({
            "kernel_image_path": kernel.to_string_lossy(), "boot_args": self.boot_args(page_hygiene)
        }).to_string()))?;
        fc.api(
            self,
            "PUT",
            "/drives/rootfs",
            Some(
                &json!({
                    "drive_id": "rootfs", "path_on_host": rootfs.to_string_lossy(),
                    "is_root_device": true, "is_read_only": read_only
                })
                .to_string(),
            ),
        )?;
        fc.api(
            self,
            "PUT",
            "/network-interfaces/eth0",
            Some(
                &json!({
                    "iface_id": "eth0", "host_dev_name": self.config.tap_dev
                })
                .to_string(),
            ),
        )?;
        Ok(())
    }

    /// v1.6 (ato#983) Slice 2: attach each durable state volume as a writable,
    /// non-root drive, in the SAME deterministic order (`state0`, `state1`, ...)
    /// as [`crate::state_volume::state_drive_configs`]. A no-op when `paths` is
    /// empty — `configure_boot` above is untouched, so a no-state-volume build
    /// PUTs the exact same drives as before this slice (byte-identical).
    fn configure_state_drives(
        &self,
        fc: &FcProcess,
        paths: &[PathBuf],
    ) -> Result<(), SnapshotError> {
        for cfg in crate::state_volume::state_drive_configs(paths) {
            let drive_id = cfg["drive_id"]
                .as_str()
                .expect("state_drive_configs always sets drive_id");
            fc.api(
                self,
                "PUT",
                &format!("/drives/{drive_id}"),
                Some(&cfg.to_string()),
            )?;
        }
        Ok(())
    }

    /// True when the buffered HTTP status line reports a 2xx or 3xx code — the
    /// app answered. Many apps redirect `/` (302 → login/dashboard) as their
    /// first live response, so a redirect is a valid readiness signal, not a
    /// failure; only a 4xx/5xx (or non-HTTP bytes) keeps the poll waiting.
    /// Parses `HTTP/1.x NNN ...`: first token starts with `HTTP/`, second is
    /// the numeric status.
    fn http_status_ready(buf: &[u8]) -> bool {
        let line = String::from_utf8_lossy(buf);
        let mut tok = line.split_whitespace();
        let is_http = tok.next().is_some_and(|v| v.starts_with("HTTP/"));
        let code = tok.next().and_then(|c| c.parse::<u16>().ok());
        is_http && code.is_some_and(|c| (200..400).contains(&c))
    }

    /// Poll the guest healthcheck (contract-driven port/path) until ready.
    fn wait_health(&self, port: u16, path: &str) -> Result<u128, SnapshotError> {
        self.wait_health_until(port, path, || None)
    }

    /// Warm `warmup_paths` into guest memory BEFORE the Pause+Snapshot, so the
    /// sealed memory image already carries the user's first-screen work
    /// (template generation, JIT, DB init, First Frame prep). All paths are
    /// hit on each round; the round succeeds only when every path answers
    /// ready. `stable_successes` consecutive stable rounds are required, polled
    /// `stable_interval_ms` apart — settle any in-guest retry that reloads
    /// routes / recompiles on the first hit before freezing the state.
    /// Idempotent: empty `warmup_paths` ⇒ no work.
    ///
    /// `boot_timeout` is the ONLY bound: the whole point of warmup is to absorb
    /// slow post-health first-screen work (the measured ~3s of template/JIT/DB
    /// init), so a shorter private budget here would fail exactly the builds
    /// this is meant to speed up. A genuinely broken path still fails the build
    /// closed — it just takes the full boot budget to prove it.
    fn warmup_paths(&self, port: u16, contract: &RestoreContract) -> Result<(), SnapshotError> {
        if contract.warmup_paths.is_empty() {
            return Ok(());
        }
        contract
            .validate_probe_paths()
            .map_err(|e| self.backend_err(format!("warmup: {e}")))?;
        let addr = self.probe_addr(port)?;
        let successes = contract.effective_stable_successes();
        let interval = contract.effective_stable_interval();
        let started = Instant::now();
        let timeout = self.config.boot_timeout;
        let mut streak = 0u32;
        while streak < successes {
            if started.elapsed() >= timeout {
                return Err(self.backend_err(format!(
                    "warmup timeout: needed {successes} stable round(s) of {:?} within {:?}",
                    contract.warmup_paths, timeout
                )));
            }
            let all_ok = contract
                .warmup_paths
                .iter()
                .all(|p| self.probe_ready(addr, p));
            streak = if all_ok { streak + 1 } else { 0 };
            if streak < successes {
                std::thread::sleep(interval);
            }
        }
        Ok(())
    }

    /// P1: resolve the operator's `ATO_RUNNER_UFFD_PREVIEW` opt-in into an
    /// actual mode, or `None` to stay on the eager File path.
    ///
    /// This is the capability gate the preview flag promises. Without it,
    /// opting in on a host that cannot serve page faults does NOT degrade to
    /// File — every restore on that runner fails (the page-server bind or the
    /// fault loop errors, and the lease dies). A canary whose blast radius is
    /// "all restores on this box" is not a canary, so an unsupported host falls
    /// back and says why.
    ///
    /// The capability decision is [`crate::uffd::evaluate`] via [`Self::probe`]
    /// — the same arch/KVM/Firecracker-version/userfaultfd rule this backend
    /// already reports as `supports_uffd_mem_backend`, so the flag can never
    /// disagree with what the runner advertises. (U0 built that probe and noted
    /// "no restore path uses it yet"; this is that path.)
    fn uffd_preview_mode(
        &self,
        store: &CasStore,
        memory: &BlobManifest,
        supervisor_build: Option<&crate::manifest::SupervisorBuildReceipt>,
    ) -> Option<UffdMode> {
        let caps = self.probe();
        Self::uffd_preview_mode_for(
            caps.supports_uffd_mem_backend,
            caps.uffd_reason.as_deref(),
            store,
            memory,
            supervisor_build,
        )
    }

    /// The preview gate as data-in / decision-out, so the rule is testable
    /// without a UFFD-capable host (the KVM smokes that exercise the real thing
    /// are all `#[ignore]`d).
    ///
    /// Two preconditions, both failing toward File — File is the safe backend
    /// and UFFD is only ever the optimization:
    ///
    /// 1. the host can serve page faults at all, and
    /// 2. **this snapshot's memory image is actually resident in the local CAS.**
    ///
    /// (2) is the placement contract [`crate::mem_backend_selector::decide_mem_backend`]
    /// states as "memory image not in local CAS → File", and that
    /// [`RestoreReadyStateInput::uffd_preview`] documents. It is the same rule
    /// this gate had been asserting without checking.
    ///
    /// Local residency is a **precondition** for choosing UFFD, never
    /// a disqualifier: `PageSource::Cas` resolves every guest fault out of the
    /// local CAS, and in production it is built with `remote: None` (remote
    /// read-through needs an explicit `ATO_FC_UFFD_REMOTE`), so a chunk that is
    /// not on disk when the guest touches that page has nowhere to come from.
    ///
    /// Checking the CAS is *openable* does not establish (2) — `CasStore::open`
    /// `create_dir_all`s the layout, so it succeeds on an empty store and fails
    /// only on permissions/ENOSPC. The residency question has to be asked of the
    /// memory blob itself.
    ///
    /// Why the failure modes are not symmetric, and why this is worth a gate:
    /// under File a missing chunk fails in `rehydrate_atomic` BEFORE
    /// `PUT /snapshot/load`, so the lease dies with a clean `MissingChunk` and
    /// nothing boots. Under UFFD the same missing chunk is not observed until the
    /// guest faults on that page — after the VM is running and the session has
    /// been handed out — where it surfaces as a page-server serve error and a
    /// fail-closed abort. Same root cause, far worse blast radius, so the cheap
    /// pre-boot stat() sweep buys a strictly better failure.
    fn uffd_preview_mode_for(
        host_supports_uffd: bool,
        uffd_reason: Option<&str>,
        store: &CasStore,
        memory: &BlobManifest,
        supervisor_build: Option<&crate::manifest::SupervisorBuildReceipt>,
    ) -> Option<UffdMode> {
        let refuse = |reason: String| {
            eprintln!(
                "UFFD preview: ATO_RUNNER_UFFD_PREVIEW is set but this restore cannot be \
                 demand-paged ({reason}); restoring via the eager File path instead."
            );
            None
        };
        // Precondition (0), and the selector's HIGHEST-precedence rule: a
        // binding-required artifact is never UFFD until Phase 8 BindingLease
        // (`decide_mem_backend`: "capsule requires bindings → File"). The runner
        // lane never evaluates that selector — `ATO_RUNNER_UFFD_PREVIEW` flows
        // straight from the env var into `RestoreReadyStateInput` — so without
        // this check the flag alone was enough to demand-page a supervisor
        // artifact, which is exactly what the selector forbids. Enforcing it
        // here rather than at the call site means no lane can bypass it.
        if declares_required_bindings(supervisor_build) {
            return refuse(
                "capsule requires bindings; UFFD is no-binding-only until Phase 8 BindingLease"
                    .to_string(),
            );
        }
        if !host_supports_uffd {
            return refuse(
                uffd_reason
                    .unwrap_or("uffd mem_backend unsupported")
                    .to_string(),
            );
        }
        // `PageSource::Cas` serves every guest fault straight out of the local
        // CAS; if it cannot be opened the guest faults on memory nobody can supply.
        let local = match CasStore::open(store.root()) {
            Ok(local) => local,
            Err(e) => return refuse(format!("local CAS unavailable: {e}")),
        };
        // ...and openable is not the same as populated: demand paging has no
        // fetch path once the guest is live, so require the bytes up front.
        if !local.has_all_chunks(memory) {
            return refuse(format!(
                "memory image {} is not fully resident in the local CAS at {}",
                memory.id().hex(),
                local.root().display()
            ));
        }
        Some(UffdMode::Cas)
    }

    /// The root-reachable address of a guest port: the guest IP directly
    /// (legacy) or the per-slot ingress (netns mode) which DNATs into the ns.
    fn probe_addr(&self, port: u16) -> Result<std::net::SocketAddr, SnapshotError> {
        let reachable = self.reachable_host();
        format!("{reachable}:{port}")
            .parse()
            .map_err(|e| self.backend_err(format!("bad guest addr: {e}")))
    }

    /// One HTTP/1.0 GET against the guest; true when the app answered ready
    /// (2xx/3xx, see [`Self::http_status_ready`]). This is the single probe
    /// shared by the warmup rounds and the health/content-ready wait, so a path
    /// that warms at build cannot be judged by a different rule at restore.
    fn probe_ready(&self, addr: std::net::SocketAddr, path: &str) -> bool {
        let io = Duration::from_millis(500);
        let Ok(mut s) = TcpStream::connect_timeout(&addr, io) else {
            return false;
        };
        let _ = s.set_read_timeout(Some(io));
        let req = format!(
            "GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n",
            self.config.guest_ip
        );
        let mut buf = [0u8; 32];
        s.write_all(req.as_bytes()).is_ok()
            && matches!(s.read(&mut buf), Ok(n) if n > 0 && Self::http_status_ready(&buf[..n]))
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
        let addr = self.probe_addr(port)?;
        let start = Instant::now();
        while start.elapsed() < self.config.boot_timeout {
            if let Some(reason) = abort() {
                return Err(self.backend_err(format!("restore failed closed: {reason}")));
            }
            if self.probe_ready(addr, path) {
                return Ok(start.elapsed().as_millis());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(self.backend_err("guest never became healthy within timeout"))
    }

    /// v1.2 PR 3d step 1 of the supervisor build drive: connect the guest-agent
    /// (retrying while the guest boots) and deliver every placeholder lease, then
    /// poll bound-ready. The agent starts the workload at bound-ready, so the
    /// caller's `wait_health` right after this is the placeholder health-verify.
    ///
    /// Unix-only (the guest-agent channel is a Firecracker vsock UDS, which does
    /// not exist on Windows); the non-unix stub fails closed like `fc_request`.
    #[cfg(not(unix))]
    fn supervisor_deliver_placeholders(
        &self,
        _uds: &Path,
        _drive: &SupervisorDrive,
    ) -> Result<(), SnapshotError> {
        Err(self.backend_err("Firecracker supervisor drive is only supported on Unix hosts"))
    }

    #[cfg(unix)]
    fn supervisor_deliver_placeholders(
        &self,
        uds: &Path,
        drive: &SupervisorDrive,
    ) -> Result<(), SnapshotError> {
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
                    return Err(self.backend_err(format!(
                        "supervisor build: placeholder delivery refused: {message}"
                    )));
                }
                Ok(other) => {
                    return Err(self.backend_err(format!(
                        "supervisor build: unexpected Deliver response: {other:?}"
                    )));
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
                    return Err(self.backend_err(format!(
                        "supervisor build: unexpected BoundReady response: {other:?}"
                    )));
                }
                Err(e) => {
                    return Err(
                        self.backend_err(format!("supervisor build: bound-ready poll: {e:#}"))
                    );
                }
            }
        }
        Err(self.backend_err(
            "supervisor build: agent never reached bound-ready after placeholder delivery",
        ))
    }

    /// v1.2 PR 3d step 2, run AFTER health passed and BEFORE the pause/snapshot:
    /// `StopWorkload` (the agent SIGTERM→SIGKILLs the app; bounded, ack'd) then
    /// `Revoke` every placeholder lease (tmpfs scrub, ack'd) — so the snapshot is
    /// taken with the workload down and no binding material in guest tmpfs. Order
    /// is contract-fixed: StopWorkload FIRST, then Revoke (binding_control §v1.2).
    ///
    /// Unix-only (vsock UDS); non-unix stub fails closed as above.
    #[cfg(not(unix))]
    fn supervisor_stop_and_revoke(
        &self,
        _uds: &Path,
        _drive: &SupervisorDrive,
    ) -> Result<(), SnapshotError> {
        Err(self.backend_err("Firecracker supervisor drive is only supported on Unix hosts"))
    }

    #[cfg(unix)]
    fn supervisor_stop_and_revoke(
        &self,
        uds: &Path,
        drive: &SupervisorDrive,
    ) -> Result<(), SnapshotError> {
        let mut ch =
            FirecrackerAgentChannel::connect(uds, GUEST_AGENT_VSOCK_PORT, Duration::from_secs(10))
                .map_err(|e| {
                    self.backend_err(format!("supervisor build: reconnect for stop: {e:#}"))
                })?;
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
                return Err(
                    self.backend_err(format!("supervisor build: StopWorkload refused: {message}"))
                );
            }
            Ok(other) => {
                return Err(self.backend_err(format!(
                    "supervisor build: unexpected StopWorkload response: {other:?}"
                )));
            }
            Err(e) => {
                return Err(self.backend_err(format!("supervisor build: StopWorkload: {e:#}")));
            }
        }
        for name in &drive.binding_names {
            match ch.request(HostToAgent::Revoke {
                id: BindingLeaseId::new(format!("lease-build-{name}")),
            }) {
                Ok(AgentToHost::Scrubbed { .. }) => {}
                Ok(AgentToHost::Error { message }) => {
                    return Err(
                        self.backend_err(format!("supervisor build: revoke refused: {message}"))
                    );
                }
                Ok(other) => {
                    return Err(self.backend_err(format!(
                        "supervisor build: unexpected Revoke response: {other:?}"
                    )));
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
            .parse()
            .map_err(|e| self.backend_err(format!("bad guest addr: {e}")))?;
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

    /// v1.2 PR 3d: restore-readiness probe for a SUPERVISOR artifact with REQUIRED
    /// bindings (zero-binding supervisor artifacts health-wait instead — see
    /// [`restore_uses_agent_probe`]). The workload is down by design
    /// (StopWorkload+Revoke ran before the seal), so readiness = "VM resumed +
    /// guest-agent reachable" — and, fail-closed, the agent must report NOT
    /// bound-ready: a bound-ready session straight out of restore means binding
    /// state survived the seal (a pre-bind-seal violation), never expose it.
    ///
    /// Unix-only (vsock UDS); non-unix stub fails closed as above.
    #[cfg(not(unix))]
    fn probe_restored_agent_unbound(&self, _uds: &Path) -> Result<(), SnapshotError> {
        Err(self.backend_err("Firecracker supervisor restore is only supported on Unix hosts"))
    }

    #[cfg(unix)]
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
            Err(e) => {
                Err(self.backend_err(format!("supervisor restore: bound-ready probe: {e:#}")))
            }
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
            eprintln!(
                "READY-STATE: ATO_FC_KEEP_BUILD_DIR set — preserving {}",
                build_dir.display()
            );
        }
    }

    fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| self.backend_err(e.to_string()))?;
        }
        std::fs::write(path, bytes)
            .map_err(|e| self.backend_err(format!("write {}: {e}", path.display())))
    }
}

fn hc_port(c: &RestoreContract, fallback: u16) -> u16 {
    c.ports.first().copied().unwrap_or(fallback)
}

fn network_ports(c: &RestoreContract, health_port: u16) -> Result<Vec<u16>, String> {
    use protocol::session_surface::EndpointProtocol;
    let mut ports = vec![health_port];
    if c.endpoints.is_empty() {
        ports.extend(c.ports.iter().copied());
    } else {
        for endpoint in &c.endpoints {
            // Only TCP/HTTP endpoints ride the slot ingress DNAT. vsock
            // endpoints (e.g. guest_control) never touch the TCP ingress, and
            // their port space is u32 — a legitimate vsock port above u16
            // must not fail the restore closed.
            if !matches!(
                endpoint.protocol,
                EndpointProtocol::Tcp | EndpointProtocol::Http
            ) {
                continue;
            }
            ports.push(
                u16::try_from(endpoint.port)
                    .map_err(|_| format!("endpoint port {} is outside u16", endpoint.port))?,
            );
        }
    }
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn hc_path(c: &RestoreContract, fallback: &str) -> String {
    c.healthcheck
        .clone()
        .unwrap_or_else(|| fallback.to_string())
}

/// Effective path used to judge RESTORE readiness — the user's first-screen, not
/// only a health endpoint. `content_ready_path` wins; otherwise the healthcheck;
/// otherwise the fallback (e.g. `/`).
fn content_ready_path(c: &RestoreContract, fallback: &str) -> String {
    c.content_ready_path_or(fallback)
}

fn blake3_file(path: &Path) -> Option<String> {
    Some(format!(
        "blake3:{}",
        blake3::hash(&std::fs::read(path).ok()?).to_hex()
    ))
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
    matches!(
        std::env::var("ATO_FC_VSOCK").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// The parent directory of every baked vsock host UDS (`$TMPDIR/ato-vsock`). Also the
/// bind-mount TARGET for v1.4 per-slot vsock isolation (see `fc_command`).
fn vsock_uds_parent_dir() -> PathBuf {
    std::env::temp_dir().join("ato-vsock")
}

/// v1.4 (ato#970): create-or-verify a directory that participates in the vsock
/// bind mount, refusing symlinks. Both mount endpoints matter: a symlinked
/// SOURCE would hand the socket to an attacker-chosen directory, a symlinked
/// TARGET would redirect the mount inside the VMM's namespace. `/run/ato/vsk`
/// is root-owned so unprivileged users cannot pre-plant it, but the baked
/// TARGET lives under the sticky world-writable `$TMPDIR` — verify, never
/// assume. `symlink_metadata` does not follow links, so a planted symlink is
/// seen as such (fail-closed), and 0o700 keeps the slot dirs root-private.
#[cfg(unix)]
fn ensure_private_dir(dir: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder.create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(std::io::Error::other(format!(
            "{} exists but is not a directory (symlink planted?)",
            dir.display()
        )));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_dir(dir: &Path, _mode: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
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
    vsock_uds_parent_dir().join(format!("{safe}.sock"))
}

/// v1.2 PR 3d: keep the transient build dir (incl. `console.log`) on disk instead of
/// removing it — the failure-forensics escape hatch. Off by default.
fn keep_build_dir_enabled() -> bool {
    matches!(
        std::env::var("ATO_FC_KEEP_BUILD_DIR").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
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
    ///
    /// An EMPTY set is a valid supervisor build (ato#1002 D4: a zero-binding
    /// dockerfile import still runs guest-agent + supervisor in the rootfs): it
    /// prepares zero leases and the build drive skips the placeholder protocol
    /// entirely — see [`SupervisorDrive::has_placeholders`] and `build_ready_state`.
    fn prepare(sup: &SupervisorBindings) -> Result<Self, String> {
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

    /// Whether this drive has placeholders to deliver/revoke. Empty (a
    /// zero-binding import, ato#1002 D4) ⇒ the guest started its workload at
    /// boot (vacuously bound-ready, ato#1001), so the build runs NO vsock
    /// protocol step and the artifact seals with the workload RUNNING under
    /// the v1.0 no-binding contract ("boot, healthcheck answers").
    fn has_placeholders(&self) -> bool {
        !self.binding_names.is_empty()
    }
}

/// Which restore-readiness lane a sealed artifact uses (v1.2 PR 3d, revised by
/// ato#1002 D4). The agent probe applies ONLY to a supervisor artifact WITH
/// required bindings — sealed workload-down by design, so readiness = agent
/// reachable + NOT bound-ready. A ZERO-binding supervisor artifact (dockerfile
/// import) sealed with the workload RUNNING, and its agent is VACUOUSLY
/// bound-ready (empty required set) — the probe's "not bound-ready" gate would
/// fail closed on a state that is not a pre-bind-seal violation (nothing was
/// ever bound), so it takes the ordinary health wait, exactly like a no-binding
/// artifact.
fn restore_uses_agent_probe(
    supervisor_build: Option<&crate::manifest::SupervisorBuildReceipt>,
) -> bool {
    declares_required_bindings(supervisor_build)
}

/// Does this artifact require bindings? Shared by the agent-probe selection
/// above and the UFFD refusal in [`FirecrackerBackend::uffd_preview_mode_for`]
/// so the two cannot drift apart — they are the same question about the same
/// receipt, asked for different reasons.
fn declares_required_bindings(
    supervisor_build: Option<&crate::manifest::SupervisorBuildReceipt>,
) -> bool {
    supervisor_build.is_some_and(|s| !s.binding_names.is_empty())
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

// Firecracker's own API is a Unix socket (it does not run on Windows at all);
// the non-unix stub exists only so the crate compiles cross-platform, matching
// `ensure_private_dir`'s established fail-closed-at-runtime pattern above.
#[cfg(not(unix))]
fn fc_request(
    _sock: &Path,
    _method: &str,
    _path: &str,
    _body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    Err(std::io::Error::other(
        "Firecracker is only supported on Unix hosts",
    ))
}

#[cfg(unix)]
fn fc_request(
    sock: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
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
    let status = headers
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0u16);
    let content_length = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().ok())
        })
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
    fn api(
        &self,
        b: &FirecrackerBackend,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(), SnapshotError> {
        let (status, text) = fc_request(&self.sock, method, path, body)
            .map_err(|e| b.backend_err(format!("api {method} {path}: {e}")))?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(b.backend_err(format!(
                "api {method} {path} -> HTTP {status}: {}",
                text.lines().last().unwrap_or("")
            )))
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

/// Firecracker-concrete capture primitives (ato-wizard PR-2). These decompose the
/// inline pause→snapshot/create→resume that `build_ready_state` performs into
/// callable pieces so the interactive submission-wizard HOLD path can drive the
/// SAME concrete IO against a *live, held* guest without a new `SnapshotBackend`
/// trait method (USER DECISION: Firecracker-concrete hold path).
/// A build guest that has been booted to its seal point and is **still running**.
///
/// This is the interactive HOLD's counterpart to [`FirecrackerBackend::build_ready_state`]:
/// the auto-seal build pauses its guest once and throws it away, while a hold keeps
/// the workload live so a human can operate it and pick the moment to capture. The
/// two share one boot path ([`FirecrackerBackend::boot_to_seal_point`]) so a held
/// capture cannot drift from a build capture.
///
/// **Capture policy.** This type implements RFC §8.3 `running` ONLY: the workload
/// stays up across a capture. A capsule whose supervisor declares placeholder
/// bindings needs `workload_idle` (stop the workload and revoke placeholders before
/// capture) and is refused at [`FirecrackerBackend::boot_and_hold`] — never
/// downgraded to a secret-bearing running capture.
///
/// **Lifetime.** The guest is owned here and killed on `Drop`, so a dropped or
/// forgotten hold can never leak a running VM. Prefer [`HeldGuest::release`], which
/// also tears the network down and cleans scratch, and surfaces errors instead of
/// swallowing them.
pub struct HeldGuest<'a> {
    backend: &'a FirecrackerBackend,
    /// Live guest. Private on purpose — `FcProcess` and the raw Firecracker API
    /// are not part of this crate's public surface.
    fc: Option<FcProcess>,
    input: BuildReadyStateInput<'a>,
    build_dir: PathBuf,
    rootfs_path: PathBuf,
    rootfs_blob: BlobManifest,
    vmstate_path: PathBuf,
    mem_path: PathBuf,
    port: u16,
    /// Held for the whole hold: the build lock is per-slot, and a hold occupies
    /// its slot for as long as the guest is up.
    _lock: BuildLock,
    _state_volume_locks: Option<crate::state_volume::VolumeLockGuard>,
    /// Set by `teardown` so `release` + `Drop` do not tear down twice.
    torn_down: bool,
}

impl<'a> HeldGuest<'a> {
    /// The `ip:port` a host-side proxy can dial to reach the live workload.
    ///
    /// This is the same address the health probe just succeeded against, so a
    /// caller that fronts it with a proxy is fronting a workload that answered.
    pub fn workload_addr(&self) -> String {
        format!("{}:{}", self.backend.reachable_host(), self.port)
    }

    /// Capture an immutable candidate from the LIVE guest and **keep it running**.
    ///
    /// Pause → `snapshot/create` → resume, then the identical seal + scan +
    /// manifest assembly the auto-seal build performs, so a candidate is a
    /// [`BuildReadyStateReceipt`] exactly like a built one — no second artifact
    /// shape, and no new sealing surface on this crate.
    ///
    /// Per RFC §8.2 this may be called MORE THAN ONCE: a candidate that fails its
    /// disposable-restore validation does not end the hold, and the author can
    /// bring the app to a different state and capture again.
    ///
    /// `source_lost` reports the ADR-012 distinction on BOTH outcomes: the guest
    /// either came back and this hold can capture again, or it did not and the
    /// hold is finished. That one bit is what decides "return to holding" vs "end
    /// the attempt" downstream, so the error carries it too — losing it on a
    /// failure would offer the author a retry against a dead VM.
    pub fn capture_candidate(&mut self) -> Result<HeldCandidate, HeldCaptureFailure> {
        let fc = self.fc.as_ref().ok_or_else(|| HeldCaptureFailure {
            error: self
                .backend
                .backend_err("capture on a released hold: the guest is already gone"),
            // Nothing to resume — the guest is already gone for good.
            source_lost: true,
        })?;
        let captured = bench::time("hold.capture_running_candidate", || {
            self.backend
                .capture_running_candidate(fc, &self.vmstate_path, &self.mem_path)
        })
        .map_err(|f| HeldCaptureFailure {
            error: f.error,
            source_lost: f.source_lost,
        })?;
        // The bytes are already taken and the guest already resumed (or not), so
        // a sealing failure must NOT swallow `source_lost`.
        let source_lost = captured.source_lost;
        let receipt = self
            .backend
            .seal_ready_state(
                &self.input,
                &self.build_dir,
                self.rootfs_blob.clone(),
                &captured.vmstate,
                &captured.mem,
                // A `running` hold never delivers placeholders (a supervisor
                // capsule is refused at `boot_and_hold`), so there is no
                // supervisor drive and no placeholder-hygiene receipt to record.
                None,
            )
            .map_err(|error| HeldCaptureFailure { error, source_lost })?;
        Ok(HeldCandidate {
            receipt,
            source_lost,
        })
    }

    /// Tear the hold down: kill the guest, bring the network down, clean scratch.
    ///
    /// Teardown is best-effort by construction — killing an already-dead process,
    /// tearing down a network that is already gone, and removing scratch that may
    /// not exist are all expected to be no-ops rather than failures — so this
    /// reports nothing. `Drop` runs exactly the same teardown if it is never
    /// called; `release` exists to make the moment explicit and to stop the guest
    /// before the caller does anything else.
    pub fn release(mut self) {
        // `teardown` is idempotent, so the `Drop` that follows this call is a
        // no-op. (`mem::forget` would be wrong here: it would also skip
        // `BuildLock`'s drop and wedge the slot lock forever.)
        self.teardown();
    }

    /// Idempotent: `release` calls it explicitly and `Drop` calls it again, and
    /// tearing a network down twice would just be dead `ip link del` work.
    fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        // Dropping `FcProcess` kills and reaps the guest.
        self.fc = None;
        self.backend.net_down();
        if !keep_build_dir_enabled() {
            let _ = std::fs::remove_dir_all(&self.build_dir);
            let _ = std::fs::remove_file(&self.rootfs_path);
        }
    }
}

impl Drop for HeldGuest<'_> {
    fn drop(&mut self) {
        // A forgotten hold must never leave a VM (and its slot lock) behind.
        self.teardown();
    }
}

/// One immutable candidate captured from a live [`HeldGuest`].
pub struct HeldCandidate {
    /// The sealed candidate — the same shape a built Ready-State has.
    pub receipt: BuildReadyStateReceipt,
    /// ADR-012: false when the source guest resumed and the hold can capture
    /// again; true when it could not, which is terminal for this hold.
    pub source_lost: bool,
}

/// A capture that did not produce a candidate.
///
/// Carries `source_lost` for the same reason [`HeldCandidate`] does: whether the
/// hold can be retried is decided by whether the GUEST survived, which is
/// independent of why the capture failed. A failure that dropped this bit would
/// let a caller offer "save again" against a guest that is gone.
pub struct HeldCaptureFailure {
    /// What went wrong — snapshotting, or sealing the bytes it produced.
    pub error: SnapshotError,
    /// ADR-012: true when the source guest could not be resumed.
    pub source_lost: bool,
}

impl std::fmt::Debug for HeldCaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldCaptureFailure")
            .field("error", &self.error)
            .field("source_lost", &self.source_lost)
            .finish()
    }
}

impl std::fmt::Display for HeldCaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (source_lost={})", self.error, self.source_lost)
    }
}

impl std::error::Error for HeldCaptureFailure {}

impl FirecrackerBackend {
    /// Boot a prepared build to its SEAL POINT and hand the guest back **alive**.
    ///
    /// This is the boot half of [`Self::build_ready_state`], extracted verbatim so
    /// the auto-seal build and the interactive HOLD (PR-2) cannot drift: both reach
    /// the seal point through this one path. It configures the machine, attaches the
    /// rootfs / durable-state / vsock devices, starts the instance, delivers
    /// supervisor placeholders when the capsule has any, waits for health, and warms
    /// the first-screen paths.
    ///
    /// It deliberately stops THERE. It does not stop/revoke the workload, does not
    /// pause, does not snapshot, and does not tear anything down — the returned
    /// [`FcProcess`] is live, and whoever holds it owns the guest's lifetime
    /// (dropping it kills and reaps the guest). The returned UDS path is the vsock
    /// channel, needed by the caller only when the capsule has placeholders.
    ///
    /// Callers must have already run the preflight gate, acquired the build lock,
    /// stored the rootfs, prepared the state volumes, and brought the network up;
    /// this method does none of that.
    #[allow(clippy::too_many_arguments)]
    fn boot_to_seal_point(
        &self,
        input: &BuildReadyStateInput<'_>,
        build_dir: &Path,
        rootfs_path: &Path,
        state_drive_paths: &[PathBuf],
        supervisor_drive: Option<&SupervisorDrive>,
        port: u16,
        path: &str,
    ) -> Result<(FcProcess, Option<PathBuf>), SnapshotError> {
        let fc = bench::time("build.start_fc", || {
            self.start_fc(&build_dir.join("api.sock"), &build_dir.join("console.log"))
        })?;
        self.configure_boot(
            &fc,
            &self.config.kernel_path,
            rootfs_path,
            self.config.rootfs_read_only,
            // v1.2 PR 3d: supervisor builds get the page-hygiene cmdline so freed
            // guest pages (incl. the revoked placeholder) are zeroed pre-snapshot.
            supervisor_drive.is_some(),
        )?;
        // v1.6 (ato#983) Slice 2: attach each durable state volume as a
        // writable, non-root drive BEFORE boot/snapshot — Firecracker records
        // the whole device set (incl. these) into the snapshot it takes below,
        // so restore's `PUT /snapshot/load` recreates them without any new
        // `PUT /drives` call (mirrors how the rootfs drive itself works).
        self.configure_state_drives(&fc, state_drive_paths)?;
        // Phase 8a-HW (#912): attach a vsock device BEFORE boot/snapshot so the
        // guest-agent binding channel is captured in the snapshot. The uds_path is
        // baked into the snapshot (FC forbids overriding it at load), so it is a
        // deterministic per-capsule path both build and restore compute.
        let vsock_uds = if vsock_enabled() {
            let uds = vsock_uds_path(&input.capsule_manifest_hash);
            if let Some(d) = uds.parent() {
                std::fs::create_dir_all(d)
                    .map_err(|e| self.backend_err(format!("vsock dir: {e}")))?;
            }
            let _ = std::fs::remove_file(&uds);
            fc.api(
                self,
                "PUT",
                "/vsock",
                Some(&json!({ "guest_cid": 3, "uds_path": uds.to_string_lossy() }).to_string()),
            )?;
            Some(uds)
        } else {
            None
        };
        bench::time("build.boot_to_health", || -> Result<(), SnapshotError> {
            fc.api(
                self,
                "PUT",
                "/actions",
                Some(&json!({"action_type":"InstanceStart"}).to_string()),
            )?;
            // v1.2 PR 3d: a supervisor guest with REQUIRED bindings starts its
            // workload only at bound-ready — deliver the placeholder leases
            // first, THEN health. A ZERO-binding supervisor build (dockerfile
            // import, ato#1002 D4) skips delivery entirely: the agent is
            // vacuously bound-ready and started the workload at boot (ato#1001),
            // so the health wait below is reached directly.
            if let Some(drive) = supervisor_drive.filter(|d| d.has_placeholders()) {
                let uds = vsock_uds.as_ref().ok_or_else(|| {
                    self.backend_err(
                        "supervisor build: vsock uds missing (unreachable: gated above)",
                    )
                })?;
                self.supervisor_deliver_placeholders(uds, drive)?;
            }
            self.wait_health(port, path)?; // secret-free seal point (placeholder-only for supervisor builds)
            // Warm the user-facing first-screen paths into guest memory BEFORE
            // the Pause+Snapshot, so the sealed image already carries template
            // rendering / JIT / DB-init / First-Frame-prep — the user's first
            // request then hits warm pages instead of redoing that work after
            // resume. Skipped for the required-binding supervisor carve-out:
            // its workload is stopped + revoked before the seal (in the caller)
            // and the equivalent first-screen work is driven via the guest-agent
            // bound-ready transition at restore time, so warming here would be
            // wasted I/O against a workload about to be torn down.
            let warmup_cap = !supervisor_drive.is_some_and(|d| d.has_placeholders());
            if warmup_cap {
                self.warmup_paths(port, &input.restore_contract)?;
            }
            Ok(())
        })?;
        Ok((fc, vsock_uds))
    }

    /// Boot `input` and HOLD the guest live for interactive capture (RFC §8.3
    /// `running`).
    ///
    /// Same preparation and same boot path as [`Self::build_ready_state`] — one
    /// difference, deliberately: nothing pauses or tears the guest down, so the
    /// workload keeps serving and the caller decides when to capture.
    ///
    /// **Refuses a `workload_idle` capsule, fail-closed.** A supervisor that
    /// declares placeholder bindings needs the workload stopped and the
    /// placeholders revoked before capture; capturing it live would seal binding
    /// material into shared bytes. RFC §8.3 forbids downgrading that case to a
    /// running capture, so it is rejected here rather than quietly weakened.
    pub fn boot_and_hold<'a>(
        &'a self,
        input: BuildReadyStateInput<'a>,
    ) -> Result<HeldGuest<'a>, SnapshotError> {
        // RFC §8.3, fail closed BEFORE any work: a live capture of a capsule that
        // needs External State or restore-time bindings is exactly what
        // `workload_idle` exists to prevent. `workload_idle` is a separate
        // lifecycle (#1093), so until it lands this is a refusal, never a fallback.
        //
        // BOTH halves of `SupervisorBindings` gate this, because they are
        // independent: `binding_names` is the placeholder/secret channel, while
        // `state_volumes` is durable per-owner storage attached as drives. A
        // ZERO-binding supervisor WITH state volumes is a real, supported build
        // (ato#1002 D4), and admitting it here would boot the hold with `&[]`
        // state drives — the workload would come up against storage that simply
        // is not attached. Durable state is restore-time state by v1.6's rule, so
        // §8.3 puts it on the `workload_idle` side too.
        // The predicate is `supervisor.is_some()`, not a field enumeration. A
        // supervisor capsule with neither bindings nor volumes still diverges
        // from `build_ready_state` in ways this path does not reproduce: the
        // build hard-refuses any supervisor when `!vsock_enabled()`, and it
        // passes `Some(drive)` into the boot so the page-hygiene kernel cmdline
        // is ON and the manifest carries a `SupervisorBuildReceipt`. A hold that
        // admitted it would seal a candidate of the SAME capsule under a
        // different cmdline and with that receipt missing. Enumerating fields
        // here would keep re-opening that gap every time a field is added —
        // which is exactly how the `state_volumes` hole got in.
        if input.supervisor.is_some() {
            return Err(self.backend_err(
                "interactive hold requires the `running` capture policy, but this capsule \
                 has a supervisor (bindings and/or durable state volumes) and therefore \
                 needs `workload_idle`: the workload is stopped and its placeholders \
                 revoked before capture. Refusing rather than capturing a workload whose \
                 restore-time state is not attached.",
            ));
        }
        crate::seal::preflight_gate(
            &input.layers.rootfs,
            input.layers.runtime.as_deref(),
            input.layers.dependency.as_deref(),
            input.layers.app.as_deref(),
            &input.declared_secret_markers,
        )?;
        self.ensure_available()?;
        self.acquire_lock("hold")?;
        let lock = BuildLock {
            path: self.lock_path(),
        };
        std::fs::create_dir_all(&self.config.work_root)
            .map_err(|e| self.backend_err(e.to_string()))?;
        let build_dir = self
            .config
            .work_root
            .join(format!("hold-{}", std::process::id()));
        std::fs::create_dir_all(&build_dir).map_err(|e| self.backend_err(e.to_string()))?;

        let rootfs_blob = bench::time("hold.store_rootfs", || {
            store_blob(
                input.store,
                LayerKind::Rootfs,
                &input.layers.rootfs,
                ChunkingKind::ContentDefined,
            )
        })?;
        let rootfs_path = self.cache_path("rootfs", &rootfs_blob, "ext4");
        if !rootfs_path.exists() {
            self.write_file(&rootfs_path, &input.layers.rootfs)?;
        }
        let port = hc_port(&input.restore_contract, self.config.healthcheck_port);
        let health_path = hc_path(&input.restore_contract, &self.config.healthcheck_path);

        // No supervisor ⇒ no durable state volumes to prepare (the refusal above
        // is what guarantees that), so the hold has no volume locks to hold.
        let network_ports = network_ports(&input.restore_contract, port)
            .map_err(|error| self.backend_err(error))?;
        self.net_up(&network_ports)?;

        let boot = self.boot_to_seal_point(
            &input,
            &build_dir,
            &rootfs_path,
            &[],
            None,
            port,
            &health_path,
        );
        let (fc, _vsock_uds) = match boot {
            Ok(v) => v,
            Err(e) => {
                self.emit_build_failure_diagnostics(&build_dir);
                self.net_down();
                if !keep_build_dir_enabled() {
                    let _ = std::fs::remove_dir_all(&build_dir);
                    let _ = std::fs::remove_file(&rootfs_path);
                }
                return Err(e);
            }
        };
        Ok(HeldGuest {
            backend: self,
            fc: Some(fc),
            vmstate_path: build_dir.join("vmstate"),
            mem_path: build_dir.join("mem"),
            input,
            build_dir,
            rootfs_path,
            rootfs_blob,
            port,
            _lock: lock,
            _state_volume_locks: None,
            torn_down: false,
        })
    }

    /// Seal a captured `(vmstate, mem)` pair into a [`BuildReadyStateReceipt`].
    ///
    /// Shared verbatim by the auto-seal build and by a held capture
    /// ([`HeldGuest::capture_candidate`]), so a candidate taken from a live guest
    /// is sealed, scanned and described by exactly the same code as a built one.
    /// Scratch cleanup is deliberately NOT done here: a build discards its scratch,
    /// a hold keeps it for the next capture.
    #[allow(clippy::too_many_arguments)]
    fn seal_ready_state(
        &self,
        input: &BuildReadyStateInput<'_>,
        build_dir: &Path,
        rootfs_blob: BlobManifest,
        vmstate: &[u8],
        mem: &[u8],
        supervisor_drive: Option<&SupervisorDrive>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        // v1.2 PR 3d: ADVISORY placeholder-hygiene scan (kernel init_on_free-
        // dependent, #947 finding) — the revoked placeholder SHOULD be gone from the
        // snapshot bytes on a hygiene-enabled kernel, but its residue is NOT a
        // secret leak (the value is a build-scoped random token, discarded below),
        // so this records honestly instead of gating.
        let supervisor_receipt = supervisor_drive.map(|drive| {
            let secrets: Vec<&[u8]> = drive.placeholder_values.iter().map(|v| v.as_bytes()).collect();
            let absent = crate::no_secret_scan::blob_is_clean(mem, &secrets)
                && crate::no_secret_scan::blob_is_clean(vmstate, &secrets);
            eprintln!(
                "READY-STATE supervisor build: placeholder absent from sealed mem/vmstate = {absent} \
                 (advisory; requires kernel init_on_free support)"
            );
            SupervisorBuildReceipt {
                binding_names: drive.binding_names.clone(),
                page_hygiene_boot_args: true,
                placeholder_absent_from_seal: Some(absent),
                // v1.6 (ato#983) Slice 2: persist so restore recomputes the SAME
                // backing-file/lock paths without the caller resupplying them.
                state_volumes: input.supervisor.as_ref().map(|s| s.state_volumes.clone()).unwrap_or_default(),
                state_owner_scope: input.supervisor.as_ref().and_then(|s| s.state_owner_scope.clone()),
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
                    vmstate,
                    memory: mem,
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
                // Scratch cleanup belongs to the CALLER: a build discards it here,
                // while a hold keeps the guest (and its scratch) alive for a retry.
                self.emit_build_failure_diagnostics(build_dir);
                return Err(e);
            }
        };
        let advisories = scanner::advisory_summaries_capped(&out.report, 50);
        let coverage = out.coverage;
        let sealed_bytes = out.sealed_bytes;
        let layers = out.layers;

        let mut rec = HotsetRecorder::new();
        if let Some(m) = &layers.memory {
            rec.extend_from_manifest(m);
        }
        if let Some(r) = &layers.rootfs {
            rec.extend_from_manifest(r);
        }
        let hotset_profile = rec.finish();

        let no_secret_proof = NoSecretProof {
            scanner_version: scanner::SCANNER_VERSION.to_string(),
            scanned_layers: layers.iter().map(|(n, _)| n.to_string()).collect(),
            findings: Vec::new(),
            advisories,
            verdict: "clean".to_string(),
            coverage,
        };
        let runner_class_id = Some(
            input
                .runner_class
                .clone()
                .unwrap_or_else(|| self.runner_facts().id()),
        );
        let manifest = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: input.capsule_manifest_hash.clone(),
            has_vsock: vsock_enabled(),
            runner_class_id,
            execution_id: input.execution_id.clone(),
            // `BuildReadyStateInput` does not yet carry a schema tag for the
            // declared execution id — that wiring is later, separate work.
            // Until then every sealed manifest is honestly legacy.
            execution_identity_schema: None,
            surface_requirement: input.surface_requirement.clone(),
            layers,
            hotset_profile,
            snapshot_backend: self.backend_info(),
            restore_contract: input.restore_contract.clone(),
            sanitizer_contract: input.sanitizer_contract.clone(),
            no_secret_proof: Some(no_secret_proof.clone()),
            build_receipt_id: None,
            supervisor_build: supervisor_receipt,
        };
        Ok(BuildReadyStateReceipt {
            manifest,
            sealed_bytes,
            no_secret_proof,
        })
    }

    /// Pause → `PUT /snapshot/create` → read of a live guest, factored out of
    /// [`Self::build_ready_state`] so the identical primitive is reusable by the
    /// interactive HOLD path. Pauses the guest, takes a `Full` snapshot to
    /// `vmstate_path` / `mem_path`, and returns the sealed `(vmstate, mem)` bytes.
    ///
    /// It does **not** resume or tear down `fc`: the caller owns the guest
    /// lifecycle. `build_ready_state` drops `fc` (→ killed+reaped) right after;
    /// the HOLD path resumes it via [`Self::resume_vm`], keeping the source alive
    /// (see [`Self::capture_running_candidate`]).
    fn pause_snapshot_create(
        &self,
        fc: &FcProcess,
        vmstate_path: &Path,
        mem_path: &Path,
    ) -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
        fc.api(
            self,
            "PATCH",
            "/vm",
            Some(&json!({"state":"Paused"}).to_string()),
        )?;
        fc.api(
            self,
            "PUT",
            "/snapshot/create",
            Some(
                &json!({
                    "snapshot_type":"Full",
                    "snapshot_path": vmstate_path.to_string_lossy(),
                    "mem_file_path": mem_path.to_string_lossy()
                })
                .to_string(),
            ),
        )?;
        let vmstate = std::fs::read(vmstate_path)
            .map_err(|e| self.backend_err(format!("read vmstate: {e}")))?;
        let mem =
            std::fs::read(mem_path).map_err(|e| self.backend_err(format!("read mem: {e}")))?;
        Ok((vmstate, mem))
    }

    /// Resume a paused guest (`PATCH /vm {"state":"Resumed"}`), keeping `fc`
    /// alive. Used only by the interactive HOLD path
    /// ([`Self::capture_running_candidate`]) to bring the *source* VM back after a
    /// running capture; the auto-seal build path never resumes (it drops `fc`).
    // Wired into build_ready_state's interactive HOLD branch in a later PR-2 slice
    // (live-VM IO, verified on real hardware).
    #[allow(dead_code)]
    fn resume_vm(&self, fc: &FcProcess) -> Result<(), SnapshotError> {
        fc.api(
            self,
            "PATCH",
            "/vm",
            Some(&json!({"state":"Resumed"}).to_string()),
        )
    }

    /// Firecracker-concrete RUNNING capture for the submission-wizard HOLD phase
    /// (ADR-001/007/012): with the live held guest already `pause_permitted` by the
    /// quiesce handshake, take an immutable candidate ([`Self::pause_snapshot_create`])
    /// then **resume the source guest** ([`Self::resume_vm`]), leaving `fc` ALIVE so
    /// the held session keeps serving and can be re-captured. This is the concrete
    /// capture-action the pure `HoldPhase` orchestration
    /// (`snapshot-builder::hold_phase`) drives on the real path — honoring the
    /// Firecracker-concrete hold path WITHOUT a new backend trait method.
    ///
    /// On resume failure the candidate bytes are still returned with
    /// `source_lost = true` (ADR-012 `accepting_source_lost`): the capture
    /// succeeded, only the live source could not be brought back.
    ///
    /// This is live-VM IO and is **not** KVM-free-testable; it is verified on real
    /// hardware in a follow-up. The KVM-free coverage lives in the pure HoldPhase
    /// orchestration tests, which drive an equivalent fake capture-action seam.
    // Real HOLD-path consumer (boot-to-health → hold session) lands in a later
    // PR-2 slice.
    fn capture_running_candidate(
        &self,
        fc: &FcProcess,
        vmstate_path: &Path,
        mem_path: &Path,
    ) -> Result<RunningCaptureBytes, RunningCaptureFailure> {
        // `pause_snapshot_create` pauses FIRST and only then does its fallible
        // work (snapshot/create, then two file reads). A bare `?` here would
        // short-circuit past the resume and strand the guest PAUSED forever —
        // and a full builder disk making `PUT /snapshot/create` return EAGAIN is
        // a failure this file has actually observed. RFC §8.2 says a failed
        // capture is not terminal: the author adjusts the app and saves again.
        // That is only true if the guest is running again, so resume on BOTH
        // paths and report whether it worked.
        match self.pause_snapshot_create(fc, vmstate_path, mem_path) {
            Ok((vmstate, mem)) => {
                let source_lost = self.resume_vm(fc).is_err();
                Ok(RunningCaptureBytes {
                    vmstate,
                    mem,
                    source_lost,
                })
            }
            Err(error) => {
                let source_lost = self.resume_vm(fc).is_err();
                Err(RunningCaptureFailure { error, source_lost })
            }
        }
    }
}

/// Bytes of one running capture plus whether the source guest was lost on resume
/// (ADR-012). Produced by [`FirecrackerBackend::capture_running_candidate`] on the
/// interactive submission-wizard HOLD path; consumed by the later-slice HOLD
/// wiring (real-VM verified).
struct RunningCaptureFailure {
    error: SnapshotError,
    source_lost: bool,
}

#[allow(dead_code)]
struct RunningCaptureBytes {
    vmstate: Vec<u8>,
    mem: Vec<u8>,
    source_lost: bool,
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
            Some(format!(
                "firecracker binary '{}' not found",
                self.config.firecracker_bin
            ))
        } else {
            None
        };
        // U0: truthfully report whether this host could drive a `Uffd` mem_backend.
        // P1 (#1082) consumes this as the gate for `uffd_preview`, so what the
        // runner advertises and what the preview flag does cannot diverge.
        // See crate::uffd.
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
        let supports_binding_lease =
            available && supports_vsock && std::env::consts::ARCH == "x86_64";
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

    fn snapshot_compatibility_contract(
        &self,
    ) -> Result<SnapshotCompatibilityContractV1, SnapshotError> {
        self.ensure_available()?;
        let facts = self.runner_facts();
        let vmm_identity = facts.vmm_version.clone();
        let state_codec = "raw".to_string();
        let guest_kernel_identity = facts.guest_kernel_id.clone();
        // `cpu_template` is Required+non-empty on the v1 contract (unlike the
        // legacy `Option<String>`): an unpinned template is real information
        // ("no template selected"), not an omission, so it gets an explicit
        // sentinel rather than staying absent.
        let cpu_template = facts
            .cpu_template
            .clone()
            .unwrap_or_else(|| "none".to_string());
        let runner_restore_contract = facts.id().to_string();
        let compatibility_class_identity = compatibility_class_identity(
            SnapshotBackendKind::Firecracker,
            SNAPSHOT_FORMAT_VERSION,
            &vmm_identity,
            &state_codec,
            &guest_kernel_identity,
            &cpu_template,
            &runner_restore_contract,
        )?;
        Ok(SnapshotCompatibilityContractV1 {
            schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
            backend: SnapshotBackendKind::Firecracker,
            format_version: SNAPSHOT_FORMAT_VERSION,
            vmm_identity,
            state_codec,
            guest_kernel_identity,
            cpu_template,
            runner_restore_contract,
            portability_tier: PortabilityTier::ClassPortable,
            compatibility_class_identity,
        })
    }

    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        // NOTE: the boot half of this lives in `boot_to_seal_point` (an inherent
        // method), which hands the guest back ALIVE. The auto-seal path below
        // pauses and drops it; the interactive HOLD path (PR-2) keeps it running.
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
        let _lock = BuildLock {
            path: self.lock_path(),
        };
        std::fs::create_dir_all(&self.config.work_root)
            .map_err(|e| self.backend_err(e.to_string()))?;
        let build_dir = self
            .config
            .work_root
            .join(format!("build-{}", std::process::id()));
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
        // Build-scoped scratch cleanup (skipped when ATO_KEEP_BUILD_DIR is set, so a
        // failed build is still inspectable). Removes BOTH the build dir
        // (vmstate/mem/console) AND the rootfs cache file. The rootfs is a
        // content-addressed `<work>/rootfs/<hash>.ext4` that a BUILD uses exactly
        // once (the boot-to-seal here); a runner rehydrates it from CAS on demand,
        // so leaving it behind just accumulates one ~rootfs-sized (multi-GB) file
        // per build until the builder disk fills — which surfaces downstream as the
        // firecracker `PUT /snapshot/create: Resource temporarily unavailable`
        // (EAGAIN). Called on EVERY exit path below. A concurrent build has a
        // DIFFERENT rootfs hash ⇒ a different path, so this never races it.
        let cleanup_scratch = || {
            if !keep_build_dir_enabled() {
                let _ = std::fs::remove_dir_all(&build_dir);
                let _ = std::fs::remove_file(&rootfs_path);
            }
        };
        let port = hc_port(&input.restore_contract, self.config.healthcheck_port);
        let path = hc_path(&input.restore_contract, &self.config.healthcheck_path);

        // v1.6 (ato#983) Slice 2: ensure + lock every durable state volume BEFORE
        // boot, so the backing file exists when Firecracker attaches it as a
        // drive. `state_volume::prepare_volumes` is shared with `restore()` and
        // is the tested fix for ato#990's review finding — it builds its lock
        // guard INCREMENTALLY (pushing each lock the instant it's acquired), so
        // a later volume's acquire/ensure failure still releases every earlier
        // lock via Drop, rather than leaking them (which a "collect into a
        // plain Vec, wrap in a guard only after the loop" shape would have).
        // `_state_volume_locks`'s guard is dropped when this CALL returns
        // (success or error) — a build here is a temporary boot-to-snapshot,
        // not the long-lived session; `restore()` acquires its OWN lock
        // lifetime, held until `stop()`.
        let mut state_drive_paths: Vec<PathBuf> = Vec::new();
        let mut _state_volume_locks: Option<crate::state_volume::VolumeLockGuard> = None;
        if let Some(sup) = &input.supervisor
            && !sup.state_volumes.is_empty()
        {
            let owner_scope = sup.state_owner_scope.as_deref().ok_or_else(|| {
                self.backend_err(
                    "durable state volumes require state_owner_scope (cannot compute a stable \
                     backing-file path without it)",
                )
            })?;
            let (paths, guard) = crate::state_volume::prepare_volumes(
                &crate::state_volume::Mkfsext4Formatter,
                &self.config.work_root,
                owner_scope,
                &sup.state_volumes,
            )
            .map_err(|e| self.backend_err(e))?;
            state_drive_paths = paths;
            _state_volume_locks = Some(guard);
        }

        // Build always runs in the root namespace (default config, netns=None).
        let network_ports = network_ports(&input.restore_contract, port)
            .map_err(|error| self.backend_err(error))?;
        self.net_up(&network_ports)?;
        let snap = (|| -> Result<(Vec<u8>, Vec<u8>), SnapshotError> {
            // The guest is booted to its seal point by the shared primitive and
            // handed back ALIVE; this path then stops+revokes (supervisor only)
            // and pauses it, and `fc` drops at the end of this closure.
            let (fc, vsock_uds) = self.boot_to_seal_point(
                &input,
                &build_dir,
                &rootfs_path,
                &state_drive_paths,
                supervisor_drive.as_ref(),
                port,
                &path,
            )?;
            // v1.2 PR 3d: StopWorkload → Revoke all placeholders BEFORE the
            // pause/snapshot, so the seal carries no running workload and no
            // binding material in guest tmpfs (contract order: stop, then revoke).
            // Then VERIFY the listener is gone — acks alone are not proof (a
            // wrapper-shell kill once left the orphaned app serving).
            //
            // ato#1002 D4: the ZERO-binding supervisor build SKIPS stop+revoke
            // and seals with the workload RUNNING — there is no placeholder to
            // scrub (nothing was delivered, so the seal is secret-free by
            // construction), and a workload-down seal could never start again
            // after restore: nothing is ever delivered to a zero-binding
            // session, so no bound-ready transition would relaunch it. Its
            // seal contract is exactly v1.0 no-binding: boot, healthcheck
            // answers — see `restore_uses_agent_probe` for the restore side.
            //
            // This step is what makes the AUTO-SEAL build differ from the
            // interactive HOLD (RFC §8.3 `running`): a hold keeps the workload
            // up, so it never runs this and instead refuses a placeholder-
            // bearing capsule outright.
            if let Some(drive) = supervisor_drive.as_ref().filter(|d| d.has_placeholders()) {
                let uds = vsock_uds.as_ref().ok_or_else(|| {
                    self.backend_err(
                        "supervisor build: vsock uds missing (unreachable: gated above)",
                    )
                })?;
                self.supervisor_stop_and_revoke(uds, drive)?;
                self.wait_workload_down(port, Duration::from_secs(10))?;
            }
            bench::time("build.snapshot_create", || {
                // Firecracker-concrete pause → snapshot/create → read, factored into
                // a callable primitive reused by the interactive HOLD path (PR-2).
                // The auto-seal build path never resumes: `fc` drops below.
                self.pause_snapshot_create(&fc, &vmstate_path, &mem_path)
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
                cleanup_scratch();
                return Err(e);
            }
        };

        // Seal + scan + manifest assembly is shared with the interactive HOLD
        // (`HeldGuest::capture_candidate`) so a held candidate and a built one are
        // sealed by exactly the same code and come back in the same shape.
        let receipt = self.seal_ready_state(
            &input,
            &build_dir,
            rootfs_blob,
            &vmstate,
            &mem,
            supervisor_drive.as_ref(),
        );
        cleanup_scratch();
        receipt
    }

    fn inspect(
        &self,
        store: &CasStore,
        manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError> {
        let all = manifest
            .layers
            .iter()
            .all(|(_, blob)| store.has_all_chunks(blob));
        Ok(SnapshotInspection {
            manifest_id: manifest.id(),
            backend_kind: manifest.snapshot_backend.kind.clone(),
            layers: manifest.layers.iter().map(|(n, _)| n.to_string()).collect(),
            total_bytes: manifest.total_layer_bytes(),
            all_chunks_present: all,
        })
    }

    fn restore(&self, input: RestoreReadyStateInput<'_>) -> Result<RestoreReceipt, SnapshotError> {
        self.ensure_available()?;
        // ── runner-class gate (fail-closed) ──────────────────────────────────
        let host_class = input
            .host_runner_class
            .unwrap_or_else(|| self.runner_facts().id());
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

        let rootfs = input
            .manifest
            .layers
            .rootfs
            .as_ref()
            .ok_or_else(|| self.backend_err("manifest has no rootfs layer"))?;
        let vmstate = input
            .manifest
            .layers
            .vmstate
            .as_ref()
            .ok_or_else(|| self.backend_err("manifest has no vmstate layer"))?;
        let memory = input
            .manifest
            .layers
            .memory
            .as_ref()
            .ok_or_else(|| self.backend_err("manifest has no memory layer"))?;

        // N-slot fail-closed guards (#948, Phase -1 audit). Netns isolates the
        // NETWORK, not host filesystem paths, so two concurrent restores of the
        // SAME snapshot still collide on any shared host path:
        //  * rw-rootfs is rehydrated to a content-addressed SHARED cache path →
        //    two writers corrupt it; require read-only rootfs under netns.
        //  * a vsock UDS path is BAKED into the snapshot (`/tmp/ato-vsock/{hash}`)
        //    and recreated on load → identical for every instance; refuse until
        //    it is mount-namespace isolated (v1.4: per-slot vsock UDS pathing).
        //
        // v1.3 (ato#968): the vsock gate is MANIFEST-driven, not env-driven.
        // Restore devices come from the snapshot, so `ATO_FC_VSOCK=1` on the
        // host says nothing about whether THIS artifact collides — gating on it
        // blocked every N-slot restore on a supervisor-capable host (runner
        // profiles: public N-slot = non-vsock artifacts; supervisor = vsock
        // artifacts). The env flag's forced UDS prep is correspondingly skipped
        // under netns below.
        //
        // v1.4 (ato#970): a vsock artifact under netns is no longer refused when
        // the slot provides `vsock_slot_dir` — the VMM then runs in a private
        // mount namespace with that directory bind-mounted over the baked UDS
        // parent, so concurrent instances get distinct sockets (see
        // `fc_command`). A netns slot WITHOUT a slot dir stays fail-closed.
        if self.config.netns.is_some() {
            if !self.config.rootfs_read_only {
                return Err(self.unsupported(
                    "N-slot (netns) restore requires read-only rootfs; rw-rootfs writes a shared cache path and would corrupt under concurrency",
                ));
            }
            if input.manifest.has_vsock && self.config.vsock_slot_dir.is_none() {
                return Err(self.unsupported(
                    "N-slot (netns) restore of a vsock snapshot needs a per-slot vsock dir (vsock_slot_dir); without one the baked vsock UDS path collides across concurrent instances",
                ));
            }
        }

        self.acquire_lock("restore")?;

        // v1.6 (ato#983) Slice 2: ensure + lock every durable state volume the
        // sealed manifest recorded, BEFORE `PUT /snapshot/load` — Firecracker
        // restores the WHOLE device set from the snapshot (no new `PUT /drives`
        // call, mirrors rootfs), so the backing files must simply exist at the
        // same paths the build attached. Locks are held for the LIVE SESSION
        // (unlike build's, which release when that call returns) — released in
        // `stop()`, via paths recorded into `.fc-session.json` below so a
        // cross-process `ato stop` (fresh backend) can find and release them too.
        // `prepare_volumes` (shared with `build_ready_state` above) releases
        // every lock it acquired so far, via its guard's `Drop`, the moment
        // ANY volume's acquire/ensure fails — the ato#990 review fix. On
        // SUCCESS the guard is `mem::forget`-ed: unlike build's temporary
        // boot-to-snapshot, these locks must outlive this function call, for
        // the whole live session — released later by `stop()`, from the paths
        // recorded into `.fc-session.json` below (same cross-process pattern
        // already used for pid/tap/vsock), not by Drop.
        let mut state_volume_lock_paths: Vec<PathBuf> = Vec::new();
        // v1.6 (ato#983) Slice 4 fix: recorded so `stop()` can fsync each
        // backing file from the HOST side before this session's lock is
        // released (see the fsync note on `stop()` below) — found live on
        // real KVM hardware as ext4 `EBADMSG` (metadata checksum mismatch) on
        // a second restore of the same capsule, i.e. the guest's own clean
        // `sync`+`umount` was not sufficient by itself for durability across
        // a hard-killed VMM.
        let mut state_volume_paths: Vec<PathBuf> = Vec::new();
        if let Some(sup) = &input.manifest.supervisor_build
            && !sup.state_volumes.is_empty()
        {
            let prep = (|| -> Result<(Vec<PathBuf>, crate::state_volume::VolumeLockGuard), SnapshotError> {
                let owner_scope = sup.state_owner_scope.as_deref().ok_or_else(|| {
                    self.backend_err("sealed manifest has durable state volumes but no state_owner_scope")
                })?;
                crate::state_volume::prepare_volumes(
                    &crate::state_volume::Mkfsext4Formatter,
                    &self.config.work_root,
                    owner_scope,
                    &sup.state_volumes,
                )
                .map_err(|e| self.backend_err(e))
            })();
            match prep {
                Ok((paths, guard)) => {
                    state_volume_paths = paths;
                    state_volume_lock_paths = guard.0.clone();
                    std::mem::forget(guard);
                }
                Err(e) => {
                    self.release_lock();
                    return Err(e);
                }
            }
        }

        // From here, on any error we must release the lock + net before returning.
        let result = (|| -> Result<(RestoredSession, Child, Option<crate::uffd_page_server::PageServerHandle>, Option<u128>), SnapshotError> {
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
            // input flag, behind a host-capability gate that degrades to the eager
            // File path (P1 — an operator opt-in must never take a runner's leases
            // down with it). The env gate (uffd_mode) remains for the test-only KVM
            // smokes: it stays UNGATED and hard-fails, because a smoke that silently
            // fell back to File would assert nothing. It takes effect only when the
            // product flag is off.
            let uffd = if input.uffd_preview {
                self.uffd_preview_mode(
                    input.store,
                    memory,
                    input.manifest.supervisor_build.as_ref(),
                )
            } else {
                uffd_mode()
            };

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
            let path = content_ready_path(
                &input.manifest.restore_contract,
                &self.config.healthcheck_path,
            );

            // Per-slot DNAT targets every declared endpoint, not only the HTTP
            // health port. Pixel RFB and other guest-private surfaces share the
            // same namespace ingress and must remain reachable after restore.
            let network_ports = network_ports(&input.manifest.restore_contract, port)
                .map_err(|error| self.backend_err(error))?;
            self.net_up(&network_ports)?;

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

            // v1.4 (ato#970): vsock isolation is decided BEFORE the VMM spawns —
            // the mount-ns wrapper bind-mounts the per-slot dir over the baked
            // UDS parent at exec, so both directories must already exist.
            let vsock_isolation = input.manifest.has_vsock && self.config.netns.is_some();
            if vsock_isolation {
                let slot_dir = self.config.vsock_slot_dir.as_ref().ok_or_else(|| {
                    self.backend_err("vsock isolation without a vsock_slot_dir (gate should have refused)")
                })?;
                // Symlink-refusing creation for BOTH mount endpoints (see
                // `ensure_private_dir`): the source is root-private, the baked
                // target under $TMPDIR keeps legacy world-readable perms.
                ensure_private_dir(slot_dir, 0o700)
                    .map_err(|e| self.backend_err(format!("vsock slot dir: {e}")))?;
                ensure_private_dir(&vsock_uds_parent_dir(), 0o755)
                    .map_err(|e| self.backend_err(format!("vsock dir: {e}")))?;
            }
            let fc = bench::time("restore.start_fc", || {
                self.start_fc_with(&input.overlay_root.join("api.sock"), &input.overlay_root.join("console.log"), vsock_isolation)
            })?;
            // Phase 8a-HW (#912): the snapshot carries the vsock device with its baked
            // uds_path; FC re-creates that socket on load, so its directory must exist.
            // The artifact self-describes vsock (manifest.has_vsock) so restore preps it
            // without an env flag; ATO_FC_VSOCK still forces it for the smokes —
            // EXCEPT under netns (v1.3, ato#968): the UDS path is deterministic per
            // capsule hash, so a forced prep (`remove_file`) from one slot could rip
            // out another slot's live socket.
            // v1.4: under isolation the HOST-side dial path is the per-slot dir —
            // the baked path stays what FC (in its private mount ns) re-creates.
            let vsock_uds = if input.manifest.has_vsock
                || (vsock_enabled() && self.config.netns.is_none())
            {
                let baked = vsock_uds_path(&input.manifest.capsule_manifest_hash);
                if vsock_isolation {
                    let slot_dir = self.config.vsock_slot_dir.as_ref().ok_or_else(|| {
                        self.backend_err("vsock isolation without a vsock_slot_dir (gate should have refused)")
                    })?;
                    let file = baked.file_name().ok_or_else(|| {
                        self.backend_err("baked vsock uds path has no file name")
                    })?;
                    let host_uds = slot_dir.join(file);
                    let _ = std::fs::remove_file(&host_uds);
                    Some(host_uds)
                } else {
                    if let Some(d) = baked.parent() {
                        std::fs::create_dir_all(d).map_err(|e| self.backend_err(format!("vsock dir: {e}")))?;
                    }
                    let _ = std::fs::remove_file(&baked);
                    Some(baked)
                }
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
            // v1.2 PR 3d: a SUPERVISOR artifact with REQUIRED bindings wakes with
            // the workload down BY DESIGN (StopWorkload+Revoke ran before the
            // seal), so a TCP health-wait can never pass until the caller delivers
            // the REAL bindings. Its readiness gate is instead: guest-agent
            // reachable over vsock AND not bound-ready (bound-ready out of restore
            // = binding state survived the seal → fail closed). ato#1002 D4: a
            // ZERO-binding supervisor artifact (dockerfile import) sealed RUNNING
            // and wakes vacuously bound-ready — it health-waits like a no-binding
            // artifact (see `restore_uses_agent_probe`).
            let agent_probe = restore_uses_agent_probe(input.manifest.supervisor_build.as_ref());
            let time_to_health_ms: Option<u128> = if agent_probe {
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
            // P3: the wait above IS the content-ready wait — `path` is the
            // artifact's content_ready_path (the first screen the browser loads),
            // not just `/health`. The supervisor's agent probe is not an HTTP
            // probe of that path, so it reports no content-ready time.
            let content_ready_ms = if agent_probe { None } else { time_to_health_ms };

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
                // v1.6 (ato#983) Slice 2: so a cross-process `ato stop` (fresh
                // backend, empty in-memory registry) can release these too.
                "state_volume_locks": state_volume_lock_paths.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
                // v1.6 (ato#983) Slice 4 fix: so `stop()` can fsync each backing
                // file from the host side (see `stop()`'s fsync note).
                "state_volume_paths": state_volume_paths.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
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
            Ok((session, child, page_handle, content_ready_ms))
        })();

        match result {
            Ok((session, child, page_handle, content_ready_ms)) => {
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(session.session_id.clone(), child);
                if let Some(h) = page_handle {
                    self.page_servers
                        .lock()
                        .unwrap()
                        .insert(session.session_id.clone(), h);
                }
                // lock + tap intentionally held for the live session (released by stop()).
                Ok(RestoreReceipt {
                    ready_state_manifest_id: input.manifest.id(),
                    session,
                    content_ready_ms,
                })
            }
            Err(e) => {
                self.net_down();
                self.release_lock();
                for l in &state_volume_lock_paths {
                    crate::state_volume::release_volume_lock(l);
                }
                Err(e)
            }
        }
    }

    fn stop(&self, session: RestoredSession) -> Result<TeardownReceipt, SnapshotError> {
        // Read the session record FIRST: a cross-process `ato stop` has a fresh
        // backend (empty in-memory registry) and possibly a different ATO_FC_TAP
        // env than the run process, so the authoritative pid + tap come from
        // .fc-session.json (written at restore), not self.config / self.sessions.
        let meta = std::fs::read_to_string(session.overlay_root.join(".fc-session.json"))
            .unwrap_or_default();
        let recorded_tap = json_str(&meta, "tap");
        let tap = recorded_tap.as_deref().unwrap_or(&self.config.tap_dev);
        // #948 N-slot: the recorded namespace (if any) is authoritative for a
        // cross-process `ato stop` whose fresh backend has an empty config.
        let recorded_netns = json_str(&meta, "netns").filter(|s| !s.is_empty());
        let netns = recorded_netns.as_deref().or(self.config.netns.as_deref());
        let recorded_veth = json_str(&meta, "veth_root").filter(|s| !s.is_empty());
        let veth_root = recorded_veth
            .as_deref()
            .or(self.config.veth_root.as_deref());

        // FIXED TEARDOWN ORDER (#948): (1) kill+reap the VMM, (2) wait for exit,
        // BEFORE removing the namespace — `ip netns del` while firecracker is
        // still attached would drop the named-ns bind mount but leave the live
        // namespace held by the process, leaking it invisibly.
        if let Some(mut child) = self.sessions.lock().unwrap().remove(&session.session_id) {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = session
            .vmm_pid
            .map(|p| p as u32)
            .or_else(|| json_u32(&meta, "pid"))
        {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
        // v1.6 (ato#983) Slice 4 fix: fsync every durable-state backing file
        // from the HOST side now that the VMM is confirmed dead (`wait()`
        // above returned) — found live on real KVM hardware: a second
        // restore of the same capsule read back ext4 EBADMSG (metadata
        // checksum mismatch) on the file the FIRST run wrote through, i.e.
        // the guest's own clean `sync`+`umount` (done over vsock before this
        // `stop()` call, see `binding_host::stop_scrub`) was not sufficient
        // by itself — nothing guaranteed Firecracker's own virtio-blk
        // backing-file writes were flushed past the HOST's page cache before
        // the VMM process was killed. This is a plain `File::sync_all()` on
        // the same path `prepare_volumes` formatted/attached; failure is
        // logged, not fatal (stop must never fail because a diagnostic-only
        // durability belt-and-suspenders step couldn't run).
        for p in json_str_array(&meta, "state_volume_paths") {
            match std::fs::OpenOptions::new()
                .write(true)
                .open(&p)
                .and_then(|f| f.sync_all())
            {
                Ok(()) => {}
                Err(e) => eprintln!("stop(): fsync durable-state backing file {p}: {e}"),
            }
        }
        // U1 (#854): stop + join the page-server thread (if any) AFTER killing the
        // child, so the guest stops faulting and the uffd read hits EOF. The
        // .page-server.sock is removed by the overlay teardown below.
        if let Some(h) = self
            .page_servers
            .lock()
            .unwrap()
            .remove(&session.session_id)
        {
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
        // v1.6 (ato#983) Slice 2: release every durable-state-volume lock this
        // session held (recorded at restore, read back the same cross-process
        // way as pid/tap/vsock above). The BACKING FILE is deliberately never
        // touched here — it lives under `<work_root>/state/...`, a sibling of
        // (never inside) `overlay_root`, so the `remove_dir_all` below cannot
        // reach it even by accident. Durable state survives stop() by
        // construction, not by a conditional check.
        for lock in json_str_array(&meta, "state_volume_locks") {
            crate::state_volume::release_volume_lock(Path::new(&lock));
        }
        let overlay_removed =
            session.overlay_root.exists() && std::fs::remove_dir_all(&session.overlay_root).is_ok();
        Ok(TeardownReceipt {
            session_id: session.session_id,
            overlay_removed,
        })
    }
}

fn manifest_short(m: &ReadyStateManifest) -> String {
    m.id()
        .strip_prefix("blake3:")
        .unwrap_or("000000")
        .chars()
        .take(12)
        .collect()
}
fn json_u32(s: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\":");
    let i = s.find(&needle)? + needle.len();
    let rest = s[i..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
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
/// v1.6 (ato#983) Slice 2: extract a JSON array-of-strings value for `key`
/// from a flat JSON object (no deps, same minimal style as `json_str` above).
/// Returns an empty vec (not an error) when `key` is absent — an artifact
/// sealed before this slice has no `state_volume_locks` field at all.
fn json_str_array(s: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":[");
    let Some(i) = s.find(&needle) else {
        return Vec::new();
    };
    let rest = &s[i + needle.len()..];
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .filter_map(|piece| {
            let piece = piece.trim().strip_prefix('"')?.strip_suffix('"')?;
            (!piece.is_empty()).then(|| piece.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── readiness status classification: 2xx/3xx = the app answered ──

    // ── P0 warmup loop: seal only after the first screen is stably serveable ──

    /// A guest stand-in: answers `status_for(path)` per request and counts hits.
    /// Returns (port, hits) — the listener thread lives for the test.
    fn spawn_probe_server(
        status_for: impl Fn(&str) -> &'static str + Send + 'static,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t_hits = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { break };
                let mut line = String::new();
                if BufReader::new(s.try_clone().unwrap())
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                // "GET /path HTTP/1.0"
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                t_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = s.write_all(status_for(&path).as_bytes());
            }
        });
        (port, hits)
    }

    fn warmup_backend(boot_timeout: Duration) -> FirecrackerBackend {
        FirecrackerBackend::with_config(FirecrackerConfig {
            guest_ip: "127.0.0.1".to_string(),
            boot_timeout,
            ..Default::default()
        })
    }

    #[test]
    fn warmup_is_skipped_when_no_paths_are_declared() {
        // v1 default: an artifact with no [snapshot].warmup_paths seals exactly as
        // before — the loop must not dial the guest at all.
        let (port, hits) = spawn_probe_server(|_| "HTTP/1.1 200 OK\r\n\r\n");
        let b = warmup_backend(Duration::from_secs(5));
        b.warmup_paths(port, &RestoreContract::default())
            .expect("empty warmup is a no-op");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn warmup_requires_consecutive_stable_rounds_and_accepts_redirects() {
        // Every declared path is hit each round, and a 302 (`/` → login) counts as
        // the app answering — the same rule the restore-side wait applies.
        let (port, hits) = spawn_probe_server(|p| match p {
            "/" => "HTTP/1.1 302 Found\r\nLocation: /app\r\n\r\n",
            _ => "HTTP/1.1 200 OK\r\n\r\n",
        });
        let b = warmup_backend(Duration::from_secs(5));
        let contract = RestoreContract {
            warmup_paths: vec!["/".to_string(), "/api/health".to_string()],
            stable_successes: Some(3),
            stable_interval_ms: Some(1),
            ..Default::default()
        };
        b.warmup_paths(port, &contract)
            .expect("warmup should settle");
        // 3 stable rounds x 2 paths — proves the streak is counted, not just one hit.
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 6);
    }

    #[test]
    fn warmup_keeps_retrying_a_slow_path_until_the_boot_timeout() {
        // The regression this guards: a private round cap (10 rounds x 250ms ≈ 2.75s)
        // would fail the build on exactly the ~3s post-health first-screen work this
        // feature exists to absorb. `boot_timeout` is the only budget, and an
        // intervening failure must not poison a later success.
        let flips = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t_flips = std::sync::Arc::clone(&flips);
        let (port, _hits) = spawn_probe_server(move |_| {
            // 404 for the first 20 rounds — well past any 10-round cap — then ready.
            if t_flips.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 20 {
                "HTTP/1.1 404 Not Found\r\n\r\n"
            } else {
                "HTTP/1.1 200 OK\r\n\r\n"
            }
        });
        let b = warmup_backend(Duration::from_secs(10));
        let contract = RestoreContract {
            warmup_paths: vec!["/".to_string()],
            stable_successes: Some(2),
            stable_interval_ms: Some(1),
            ..Default::default()
        };
        b.warmup_paths(port, &contract)
            .expect("a slow first screen must warm, not fail the build");
    }

    #[test]
    fn warmup_fails_the_build_when_a_path_never_becomes_ready() {
        // Fail closed: a genuinely broken path must not seal a cold first screen.
        let (port, _hits) = spawn_probe_server(|_| "HTTP/1.1 500 Internal Server Error\r\n\r\n");
        let b = warmup_backend(Duration::from_millis(300));
        let contract = RestoreContract {
            warmup_paths: vec!["/broken".to_string()],
            stable_interval_ms: Some(1),
            ..Default::default()
        };
        let err = b
            .warmup_paths(port, &contract)
            .expect_err("a never-ready path must fail the build");
        assert!(format!("{err}").contains("warmup timeout"), "{err}");
    }

    #[test]
    fn warmup_rejects_a_path_that_would_break_the_probe_request_line() {
        // An authoring typo fails with a pointed error instead of an opaque
        // timeout, and a CR/LF can never be smuggled into the guest request.
        let (port, hits) = spawn_probe_server(|_| "HTTP/1.1 200 OK\r\n\r\n");
        let b = warmup_backend(Duration::from_secs(5));
        for bad in ["health", "/a\r\nX-Injected: 1"] {
            let contract = RestoreContract {
                warmup_paths: vec![bad.to_string()],
                ..Default::default()
            };
            let err = b.warmup_paths(port, &contract).expect_err("must reject");
            assert!(format!("{err}").contains("not a valid probe path"), "{err}");
        }
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an invalid path must be rejected before any guest dial"
        );
    }

    #[test]
    fn http_status_ready_accepts_2xx_and_3xx_rejects_errors_and_garbage() {
        // 2xx and 3xx (a `/` → login redirect is a valid live signal).
        for line in [
            "HTTP/1.0 200 OK\r\n",
            "HTTP/1.1 204 No Content\r\n",
            "HTTP/1.1 301 Moved Permanently\r\n",
            "HTTP/1.1 302 Found\r\nLocation: /lo",
            "HTTP/1.1 307 Temporary Redirect\r\n",
            "HTTP/1.1 399 Weird\r\n",
        ] {
            assert!(
                FirecrackerBackend::http_status_ready(line.as_bytes()),
                "{line:?} should be ready"
            );
        }
        // 4xx/5xx (app up but the probe path errors) keeps waiting, and
        // non-HTTP / truncated bytes never falsely pass.
        for line in [
            "HTTP/1.1 404 Not Found\r\n",
            "HTTP/1.1 500 Internal Server Error\r\n",
            "HTTP/1.1 100 Continue\r\n",
            "SSH-2.0-OpenSSH\r\n",
            "garbage",
            "HTTP/1.",
            "",
        ] {
            assert!(
                !FirecrackerBackend::http_status_ready(line.as_bytes()),
                "{line:?} should NOT be ready"
            );
        }
    }

    #[test]
    fn with_boot_timeout_overrides_and_clamps() {
        let b = FirecrackerBackend::new();
        let base = b.config.boot_timeout;
        // None inherits the env/default unchanged.
        assert_eq!(b.with_boot_timeout(None).config.boot_timeout, base);
        // A per-job value overrides.
        assert_eq!(
            b.with_boot_timeout(Some(300)).config.boot_timeout,
            Duration::from_secs(300)
        );
        // Clamped fail-closed to [1, MAX_JOB_BOOT_TIMEOUT_S].
        assert_eq!(
            b.with_boot_timeout(Some(9999)).config.boot_timeout,
            Duration::from_secs(MAX_JOB_BOOT_TIMEOUT_S)
        );
        assert_eq!(
            b.with_boot_timeout(Some(0)).config.boot_timeout,
            Duration::from_secs(1)
        );
    }

    // ── #948 N-slot: per-slot netns config derivation + host-path isolation ──

    #[test]
    fn for_slot_netns_off_is_legacy_identity() {
        let base = FirecrackerConfig::default();
        let c = FirecrackerConfig::for_slot(0, false, &base);
        assert!(c.netns.is_none() && c.ingress_ip.is_none() && c.veth_root.is_none());
        // legacy reachable host is the guest IP; lock is tap-keyed.
        assert_eq!(
            FirecrackerBackend::with_config(c.clone()).reachable_host(),
            c.guest_ip
        );
        assert!(
            FirecrackerBackend::with_config(c)
                .lock_path()
                .to_string_lossy()
                .contains("fctap0.lock")
        );
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
        assert_eq!(
            FirecrackerBackend::with_config(s1).reachable_host(),
            s1_ingress
        );
    }

    #[test]
    fn network_ports_include_every_declared_endpoint() {
        use protocol::session_surface::{
            EndpointContract, EndpointExposure, EndpointProtocol, EndpointReadiness, EndpointRole,
        };

        let contract = RestoreContract {
            ports: vec![3000],
            endpoints: vec![
                EndpointContract {
                    role: EndpointRole::AppHttp,
                    protocol: EndpointProtocol::Http,
                    exposure: EndpointExposure::HostInternal,
                    port: 3000,
                    readiness: EndpointReadiness::HttpGet {
                        path: "/healthz".to_string(),
                    },
                },
                EndpointContract {
                    role: EndpointRole::PixelRfb,
                    protocol: EndpointProtocol::Tcp,
                    exposure: EndpointExposure::GuestPrivate,
                    port: 5901,
                    readiness: EndpointReadiness::FirstFrame,
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            network_ports(&contract, 3000).expect("valid endpoint ports"),
            vec![3000, 5901]
        );
    }

    #[test]
    fn network_ports_skip_vsock_endpoints_and_their_u32_port_space() {
        use protocol::session_surface::{
            EndpointContract, EndpointExposure, EndpointProtocol, EndpointReadiness, EndpointRole,
        };

        // guest_control rides vsock, never the TCP ingress: no DNAT rule for
        // it, and its u32 port space (here above u16::MAX) must not fail the
        // restore closed.
        let contract = RestoreContract {
            ports: vec![3000],
            endpoints: vec![
                EndpointContract {
                    role: EndpointRole::AppHttp,
                    protocol: EndpointProtocol::Http,
                    exposure: EndpointExposure::HostInternal,
                    port: 3000,
                    readiness: EndpointReadiness::HttpGet {
                        path: "/healthz".to_string(),
                    },
                },
                EndpointContract {
                    role: EndpointRole::GuestControl,
                    protocol: EndpointProtocol::Vsock,
                    exposure: EndpointExposure::GuestPrivate,
                    port: 70_000,
                    readiness: EndpointReadiness::VsockConnect,
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            network_ports(&contract, 3000).expect("vsock port must not fail the derivation"),
            vec![3000]
        );
    }

    #[test]
    fn network_ports_preserve_legacy_port_projection() {
        let contract = RestoreContract {
            ports: vec![8080, 9090, 8080],
            ..Default::default()
        };

        assert_eq!(
            network_ports(&contract, 8080).expect("valid legacy ports"),
            vec![8080, 9090]
        );
    }

    #[test]
    fn network_ports_fail_closed_on_out_of_range_endpoint() {
        use protocol::session_surface::{
            EndpointContract, EndpointExposure, EndpointProtocol, EndpointReadiness, EndpointRole,
        };

        let contract = RestoreContract {
            endpoints: vec![EndpointContract {
                role: EndpointRole::PixelRfb,
                protocol: EndpointProtocol::Tcp,
                exposure: EndpointExposure::GuestPrivate,
                port: 70_000,
                readiness: EndpointReadiness::FirstFrame,
            }],
            ..Default::default()
        };

        assert!(network_ports(&contract, 3000).is_err());
    }

    // ── v1.4 (ato#970): per-slot vsock UDS isolation ──

    #[test]
    fn for_slot_netns_on_derives_distinct_vsock_slot_dirs() {
        let base = FirecrackerConfig::default();
        // netns off ⇒ no isolation dir (legacy shared baked path).
        assert!(
            FirecrackerConfig::for_slot(0, false, &base)
                .vsock_slot_dir
                .is_none()
        );
        // netns on ⇒ every slot gets its own dir.
        let s0 = FirecrackerConfig::for_slot(0, true, &base);
        let s1 = FirecrackerConfig::for_slot(1, true, &base);
        let (d0, d1) = (s0.vsock_slot_dir.unwrap(), s1.vsock_slot_dir.unwrap());
        assert_ne!(d0, d1);
        assert_eq!(d0, PathBuf::from("/run/ato/vsk/0"));
        assert_eq!(d1, PathBuf::from("/run/ato/vsk/1"));
        // AF_UNIX sun_path budget: the host-side dial path (slot dir + the
        // 76-byte `blake3_<64-hex>.sock` file name) must stay under SUN_LEN
        // (~108). `/tmp/ato-vsock-slots/ato-slot-0/…` was exactly 108 and
        // failed the first live restore — keep the dir terse. Budgeted against
        // `/tmp` (restore is Linux-only; this test also runs on macOS, where
        // `temp_dir()` is a long per-user path that never restores anything).
        let dial = format!("/run/ato/vsk/99/blake3_{}.sock", "a".repeat(64));
        assert!(
            dial.len() <= 100,
            "host dial path must fit sun_path with margin, got {} bytes: {dial}",
            dial.len(),
        );
    }

    #[test]
    fn fc_command_wraps_vsock_isolation_in_a_private_mount_ns() {
        let base = FirecrackerConfig::default();

        // Legacy: no netns ⇒ plain firecracker, isolation flag irrelevant.
        let plain = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(0, false, &base));
        assert_eq!(
            plain.fc_command(true).get_program(),
            std::ffi::OsStr::new(&base.firecracker_bin)
        );

        // Netns without isolation: ip netns exec <ns> <fc> (unchanged shape).
        let ns = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(1, true, &base));
        let cmd = ns.fc_command(false);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("ip"));
        assert_eq!(
            args,
            vec!["netns", "exec", "ato-slot-1", &base.firecracker_bin]
        );

        // Netns WITH isolation: the VMM is exec'd inside `unshare --mount` with
        // the per-slot dir bind-mounted over the baked vsock parent, and the
        // firecracker binary last so start_fc's appended args reach "$@".
        let cmd = ns.fc_command(true);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("ip"));
        assert_eq!(&args[..3], ["netns", "exec", "ato-slot-1"]);
        assert_eq!(&args[3..5], ["unshare", "--mount"]);
        assert_eq!(args[5], "sh");
        assert_eq!(args[6], "-c");
        assert!(args[7].contains(r#"mount --bind "$1" "$2""#) && args[7].contains(r#"exec "$@""#));
        assert_eq!(args[9], "/run/ato/vsk/1");
        assert_eq!(args[10], vsock_uds_parent_dir().to_string_lossy());
        assert_eq!(args[11], base.firecracker_bin);
        assert_eq!(
            args.len(),
            12,
            "firecracker binary must be LAST so --api-sock appends into \"$@\""
        );
    }

    #[test]
    fn per_slot_lock_paths_are_distinct_for_same_snapshot() {
        // The bug this guards: two slots share tap `fctap0`, so a tap-keyed lock
        // would re-serialize them. Namespaced slots get namespace-keyed locks.
        let base = FirecrackerConfig::default();
        let l0 = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(0, true, &base))
            .lock_path();
        let l1 = FirecrackerBackend::with_config(FirecrackerConfig::for_slot(1, true, &base))
            .lock_path();
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
        let expect = FirecrackerBackend::kvm_present()
            && FirecrackerBackend::new().detect_version().is_some();
        assert_eq!(p.available, expect);
        if !p.available {
            assert!(p.reason.is_some());
        }
        // U0 UFFD facet invariant: false ⇒ a concrete reason; true ⇒ no reason.
        // (On this test host — non-x86_64 or no /dev/kvm — it is false with a reason.)
        if p.supports_uffd_mem_backend {
            assert!(p.uffd_reason.is_none());
        } else {
            assert!(
                p.uffd_reason.is_some(),
                "unsupported UFFD must carry a reason"
            );
        }
    }

    #[test]
    fn restore_is_unsupported_without_kvm() {
        if FirecrackerBackend::kvm_present() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = err_manifest();
        assert!(FirecrackerBackend::new().inspect(&store, &m).is_ok()); // inspect needs no KVM
        let backend = FirecrackerBackend::new();
        let input = RestoreReadyStateInput {
            store: &store,
            manifest: m,
            overlay_root: dir.path().join("ov"),
            host_runner_class: None,
            uffd_preview: false,
        };
        assert!(matches!(
            backend.restore(input),
            Err(SnapshotError::Unsupported { .. })
        ));
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
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: b"....PREFLIGHT_MARKER_XYZ....".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![8080],
                healthcheck: Some("/health".to_string()),
                expected_ready_ms: Some(3000),
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec!["PREFLIGHT_MARKER_XYZ".to_string()],
            execution_id: None,
            supervisor: None,
        };
        let err = FirecrackerBackend::new()
            .build_ready_state(input)
            .unwrap_err();
        assert!(
            matches!(err, SnapshotError::SecretFoundInSnapshot(_)),
            "preflight must reject before KVM/store: {err:?}"
        );
        assert!(
            store.list_chunks().unwrap().is_empty(),
            "rejected build must persist no rootfs in CAS"
        );
    }

    // ── interactive HOLD: the `workload_idle` refusal (RFC §8.3) ──────────────

    /// A capsule that declares supervisor bindings cannot be held live.
    ///
    /// Its bindings are delivered as placeholders and must be revoked with the
    /// workload STOPPED before capture (`workload_idle`, #1093). Capturing it
    /// running would seal binding material into bytes many users restore. The RFC
    /// forbids falling back to a running capture, so this must be a refusal —
    /// and it must happen before any boot, store write, or lock.
    #[test]
    fn hold_refuses_a_capsule_that_needs_workload_idle() {
        use crate::manifest::{RestoreContract, SanitizerContract};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FirecrackerBackend::with_config(FirecrackerConfig {
            work_root: dir.path().join("work"),
            ..FirecrackerConfig::default()
        });
        let input = BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: "blake3:hold-idle".to_string(),
            runner_class: None,
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: b"rootfs".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![8080],
                healthcheck: Some("/health".to_string()),
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: Vec::new(),
            execution_id: None,
            supervisor: Some(SupervisorBindings {
                binding_names: vec!["openai_api_key".into()],
                ..Default::default()
            }),
        };
        // Matched rather than `unwrap_err`'d on purpose: `HeldGuest` deliberately
        // has no `Debug` (it owns the live process handle), so the success arm
        // must be destructured explicitly.
        let Err(err) = backend.boot_and_hold(input) else {
            panic!("a binding-declaring capsule must never reach a live hold");
        };
        let text = format!("{err:?}");
        assert!(
            text.contains("workload_idle"),
            "the refusal must name the policy the capsule actually needs: {text}"
        );
        // Refused BEFORE any work: nothing stored, and the slot lock is free for
        // the next build (a leaked lock would wedge the whole builder).
        assert!(
            store.list_chunks().unwrap().is_empty(),
            "a refused hold must persist nothing in CAS"
        );
        assert!(
            !backend.lock_path().exists(),
            "a refused hold must not leave the slot lock behind"
        );
    }

    /// A capsule with durable state volumes but ZERO bindings is refused too.
    ///
    /// The two `SupervisorBindings` fields are independent, and a zero-binding
    /// supervisor build is a real, supported case (ato#1002 D4). Gating only on
    /// `binding_names` would admit it — and the hold attaches no state drives, so
    /// the workload would come up against storage that is not there. Durable
    /// state is restore-time state, so §8.3 puts this on the `workload_idle` side.
    #[test]
    fn hold_refuses_durable_state_volumes_even_with_zero_bindings() {
        use crate::manifest::{RestoreContract, SanitizerContract};
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FirecrackerBackend::with_config(FirecrackerConfig {
            work_root: dir.path().join("work"),
            ..FirecrackerConfig::default()
        });
        let input = BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: "blake3:hold-volumes".to_string(),
            runner_class: None,
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: b"rootfs".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![8080],
                healthcheck: Some("/health".to_string()),
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: Vec::new(),
            execution_id: None,
            supervisor: Some(SupervisorBindings {
                // ZERO bindings — the field the first refusal keys on is empty.
                binding_names: Vec::new(),
                state_volumes: vec![crate::state_volume::DurableVolumeSpec {
                    state_name: "data".to_string(),
                    size_mb: 64,
                }],
                state_owner_scope: Some("owner/capsule".to_string()),
            }),
        };
        let Err(err) = backend.boot_and_hold(input) else {
            panic!("a capsule with durable state volumes must never reach a live hold");
        };
        let text = format!("{err:?}");
        assert!(
            text.contains("workload_idle"),
            "a supervisor capsule must be refused whatever it declares: {text}"
        );
        assert!(
            store.list_chunks().unwrap().is_empty(),
            "a refused hold must persist nothing in CAS"
        );
        assert!(
            !backend.lock_path().exists(),
            "a refused hold must not leave the slot lock behind"
        );
    }

    // ── v1.2 PR 3d: supervisor build drive ────────────────────────────────────

    #[test]
    fn boot_args_add_page_hygiene_only_for_supervisor_builds() {
        let b = FirecrackerBackend::new();
        let plain = b.boot_args(false);
        // The no-binding cmdline is byte-identical to the historical string.
        assert!(
            plain.starts_with("console=ttyS0 reboot=k panic=1 pci=off ip="),
            "{plain}"
        );
        assert!(
            !plain.contains("init_on_free"),
            "no hygiene args on the no-binding path: {plain}"
        );
        let hardened = b.boot_args(true);
        assert!(
            hardened.contains("init_on_free=1 init_on_alloc=1 page_poison=1"),
            "{hardened}"
        );
        // Hygiene args are inserted, nothing else changes.
        assert_eq!(
            hardened.replace(" init_on_free=1 init_on_alloc=1 page_poison=1", ""),
            plain
        );
    }

    #[test]
    #[cfg(unix)] // netns / SupervisorDrive placeholder gen (/dev/urandom) are Linux/Unix-only
    fn build_placeholders_are_unique_and_prefixed() {
        let a = generate_build_placeholder().unwrap();
        let b = generate_build_placeholder().unwrap();
        assert!(a.starts_with("ATO-BUILD-PLACEHOLDER-"), "{a}");
        assert_ne!(a, b, "placeholders must be unique per generation");
    }

    #[test]
    #[cfg(unix)] // netns / SupervisorDrive placeholder gen (/dev/urandom) are Linux/Unix-only
    fn supervisor_drive_validates_names_and_never_logs_values() {
        // Valid lowercase binding names prepare fine; each gets a distinct placeholder.
        let drive = SupervisorDrive::prepare(&SupervisorBindings {
            binding_names: vec!["openai_api_key".into(), "db_url".into()],
            ..Default::default()
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
            ..Default::default()
        }) {
            Err(e) => e,
            Ok(_) => panic!("uppercase binding name must fail closed"),
        };
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
        // ato#1002 D4: an EMPTY set is an accepted supervisor build (zero-binding
        // dockerfile import) — zero leases, and the drive reports no placeholders
        // so the build skips the delivery/stop protocol entirely.
        let empty = SupervisorDrive::prepare(&SupervisorBindings {
            binding_names: vec![],
            ..Default::default()
        })
        .expect("empty supervisor set must be accepted (ato#1002 D4)");
        assert!(empty.leases.is_empty());
        assert!(empty.placeholder_values.is_empty());
        assert!(!empty.has_placeholders());
        // Non-empty invariance: the prepared drive above still drives the protocol.
        assert!(drive.has_placeholders());
    }

    #[test]
    fn restore_lane_gates_on_required_bindings_not_mere_supervisor_presence() {
        use crate::manifest::SupervisorBuildReceipt;
        // No supervisor_build at all (recipe no-binding artifact) → health wait.
        assert!(!restore_uses_agent_probe(None));
        // ato#1002 D4: a ZERO-binding supervisor artifact (dockerfile import)
        // sealed with the workload RUNNING and wakes vacuously bound-ready — the
        // agent probe's "not bound-ready" gate would fail closed on a state that
        // is NOT a pre-bind-seal violation, so it must health-wait instead.
        let empty = SupervisorBuildReceipt {
            binding_names: vec![],
            page_hygiene_boot_args: true,
            placeholder_absent_from_seal: Some(true),
            state_volumes: vec![],
            state_owner_scope: None,
        };
        assert!(!restore_uses_agent_probe(Some(&empty)));
        // Non-empty invariance: a binding-required artifact keeps the agent probe.
        let bound = SupervisorBuildReceipt {
            binding_names: vec!["openai_api_key".into()],
            page_hygiene_boot_args: true,
            placeholder_absent_from_seal: Some(true),
            state_volumes: vec![],
            state_owner_scope: None,
        };
        assert!(restore_uses_agent_probe(Some(&bound)));
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
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: b"rootfs".to_vec(),
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![8080],
                healthcheck: Some("/health".to_string()),
                expected_ready_ms: Some(3000),
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: None,
            supervisor: Some(SupervisorBindings {
                binding_names: vec!["openai_api_key".into()],
                ..Default::default()
            }),
        };
        let err = FirecrackerBackend::new()
            .build_ready_state(input)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("ATO_FC_VSOCK"),
            "must name the missing flag: {msg}"
        );
        assert!(
            store.list_chunks().unwrap().is_empty(),
            "no bytes stored on the fail-closed path"
        );
    }

    #[test]
    fn config_reads_defaults() {
        let c = FirecrackerConfig::default();
        assert_eq!(c.vcpu_count, 2);
        assert_eq!(c.healthcheck_port, 8080);
        // rootfs_read_only defaults to true, but honors ATO_FC_ROOTFS_READONLY
        // (the KVM integration run sets =0), so assert it matches the env rather
        // than a fixed value.
        let expect_ro = std::env::var("ATO_FC_ROOTFS_READONLY")
            .map(|v| v != "0")
            .unwrap_or(true);
        assert_eq!(c.rootfs_read_only, expect_ro);
    }

    #[test]
    fn hotset_flag_detection() {
        // SAFETY: serial within this fn; vars restored at the end.
        let p1 = std::env::var("ATO_READY_STATE_HOTSET").ok();
        let p2 = std::env::var("ATO_READY_STATE_PREFETCH").ok();
        let set = |k: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
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
        assert!(
            !hotset_enabled(),
            "PREFETCH=rootfs does not enable memory-first"
        );
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
            match v {
                Some(v) => std::env::set_var("ATO_FC_UFFD", v),
                None => std::env::remove_var("ATO_FC_UFFD"),
            }
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
        assert_eq!(
            uffd_mode(),
            None,
            "unknown token ⇒ File (fail safe to default)"
        );
        set(prev.as_deref());
    }

    /// The UFFD preview gate must treat local-CAS residency of THIS snapshot's
    /// memory image as a PRECONDITION, not merely "the CAS directory opens".
    ///
    /// `CasStore::open` `create_dir_all`s the layout, so it succeeds on an empty
    /// store — an openable CAS proves nothing about whether the memory chunks
    /// are actually there. That matters because demand paging has no fetch path
    /// once the guest is live (production builds `PageSource::cas` with
    /// `remote: None`; read-through needs an explicit `ATO_FC_UFFD_REMOTE`), so a
    /// missing chunk that the File path would have caught pre-boot in
    /// `rehydrate_atomic` instead surfaces as a post-boot page-fault abort, after
    /// the session has been handed out. Fail toward File.
    ///
    /// Covers the partial-residency case too: a first-chunk probe reports a
    /// half-fetched image as local, so the gate sweeps the whole chunk list.
    #[test]
    fn uffd_preview_requires_a_resident_memory_image_not_just_an_openable_cas() {
        let dir = tempfile::tempdir().unwrap();
        let cas_root = dir.path().join("cas");
        let store = CasStore::open(&cas_root).unwrap();
        // 8 distinct 8-byte pages ⇒ 8 distinct chunks (distinct bytes ⇒ no dedup).
        let payload: Vec<u8> = (0..64u8).collect();
        let memory = store_blob(
            &store,
            LayerKind::Memory,
            &payload,
            ChunkingKind::PageAligned { page_size: 8 },
        )
        .unwrap();
        assert!(
            memory.chunks.len() > 1,
            "test needs a multi-chunk memory image to cover partial residency"
        );

        let mode =
            |s: &CasStore| FirecrackerBackend::uffd_preview_mode_for(true, None, s, &memory, None);

        // Fully resident on a capable host ⇒ UFFD. The common local-CAS-hit path
        // must NOT regress: this gate only ever subtracts UFFD, never adds it.
        assert_eq!(
            mode(&store),
            Some(UffdMode::Cas),
            "a fully resident memory image on a capable host must still choose UFFD"
        );

        // An incapable host still refuses regardless of residency (unchanged).
        assert_eq!(
            FirecrackerBackend::uffd_preview_mode_for(
                false,
                Some("no userfaultfd"),
                &store,
                &memory,
                None
            ),
            None,
            "an incapable host must fall back to File"
        );

        // A binding-required artifact is refused even when everything else is
        // perfect: capable host, fully resident memory. This is the selector's
        // highest-precedence rule ("capsule requires bindings → File"), which the
        // runner lane never evaluated — `ATO_RUNNER_UFFD_PREVIEW` reaches
        // `RestoreReadyStateInput` straight from the env var, so before this gate
        // the flag alone could demand-page a supervisor artifact.
        let bound = crate::manifest::SupervisorBuildReceipt {
            binding_names: vec!["openai_api_key".into()],
            page_hygiene_boot_args: true,
            placeholder_absent_from_seal: Some(true),
            state_volumes: vec![],
            state_owner_scope: None,
        };
        assert_eq!(
            FirecrackerBackend::uffd_preview_mode_for(true, None, &store, &memory, Some(&bound)),
            None,
            "a binding-required artifact must never be demand-paged before Phase 8"
        );
        // ...while a ZERO-binding supervisor artifact (dockerfile import) is not
        // binding-required, so it keeps UFFD — the gate subtracts only what the
        // selector forbids, it does not blanket-refuse every supervisor build.
        let unbound = crate::manifest::SupervisorBuildReceipt {
            binding_names: vec![],
            ..bound.clone()
        };
        assert_eq!(
            FirecrackerBackend::uffd_preview_mode_for(true, None, &store, &memory, Some(&unbound)),
            Some(UffdMode::Cas),
            "a zero-binding supervisor artifact must still be eligible for UFFD"
        );

        // An empty-but-openable CAS — the exact shape the openability check accepts.
        let empty_root = dir.path().join("empty-cas");
        let empty = CasStore::open(&empty_root).unwrap();
        assert!(
            CasStore::open(&empty_root).is_ok(),
            "an empty CAS still opens — openable must never imply resident"
        );
        assert_eq!(
            mode(&empty),
            None,
            "an openable but empty CAS must fall back to File"
        );

        // PARTIAL residency: chunk 0 present, a later chunk gone. A first-chunk
        // probe would call this local; the whole-blob sweep must not.
        let last = memory.chunks.last().unwrap().hash.clone();
        std::fs::remove_file(cas_root.join("blobs").join("blake3").join(last.hex())).unwrap();
        assert!(
            store.has_chunk(&memory.chunks[0].hash),
            "first chunk is still present — this is the partial case, not an empty one"
        );
        assert_eq!(
            mode(&store),
            None,
            "a partially resident memory image must fall back to File"
        );
    }

    #[test]
    fn rehydrate_atomic_materializes_caches_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let blob = store_blob(
            &store,
            LayerKind::Memory,
            b"mem-bytes-xyz",
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let b = FirecrackerBackend::new();
        let path = dir.path().join("layers").join("mem.bin");

        // materializes the full content (atomic).
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"mem-bytes-xyz");

        // cache hit: second non-forced call does not rewrite.
        let m1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            m1,
            "cached layer must not be rewritten"
        );

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
        let blob = store_blob(
            &store,
            LayerKind::Memory,
            b"good-memory-bytes",
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let b = FirecrackerBackend::new();
        let path = dir.path().join("layers").join("mem.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A corrupt (truncated) cached file must be detected by the size check and
        // re-rehydrated from CAS — never trusted into LoadSnapshot.
        std::fs::write(&path, b"truncated").unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"good-memory-bytes",
            "corrupt cache repaired from CAS"
        );

        // A correct cache (size matches) is a no-op hit (not rewritten).
        let m1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        b.rehydrate_atomic(&path, &store, &blob, false).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            m1,
            "valid cache must not be rewritten"
        );
    }

    #[test]
    fn json_u32_parses() {
        assert_eq!(
            json_u32("{\"pid\":12345,\"tap\":\"x\"}", "pid"),
            Some(12345)
        );
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
            execution_identity_schema: None,
            surface_requirement: None,
            layers: ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: FIRECRACKER_BACKEND_ID.to_string(),
                version: "0".to_string(),
                snapshot_format_version: SNAPSHOT_FORMAT.to_string(),
                cpu_template: None,
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        }
    }

    /// v1.6 (ato#983) Slice 2: `stop()` must release a session's recorded
    /// state-volume locks but must NEVER touch the backing file itself — it
    /// lives outside `overlay_root` (the only thing `stop()` `remove_dir_all`s).
    /// This runs `stop()` for real (no KVM/firecracker binary needed: the pid
    /// doesn't exist so `kill -9` is a no-op, there's no netns/tap to tear
    /// down, and the session record comes entirely from a hand-written
    /// `.fc-session.json`, exactly as a cross-process `ato stop` would read it).
    #[test]
    fn stop_releases_state_volume_locks_but_never_touches_the_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_root = dir.path().join("overlay");
        std::fs::create_dir_all(&overlay_root).unwrap();
        let work_root = dir.path().join("work");

        let vpath = crate::state_volume::volume_path(&work_root, "owner-x", "dbdata");
        let lpath = crate::state_volume::lock_path(&work_root, "owner-x", "dbdata");
        std::fs::create_dir_all(vpath.parent().unwrap()).unwrap();
        std::fs::write(&vpath, b"durable-bytes").unwrap();
        crate::state_volume::acquire_volume_lock(&lpath).unwrap();

        // What restore() would have written — a cross-process `ato stop` reads
        // this, not any in-memory state.
        std::fs::write(
            overlay_root.join(".fc-session.json"),
            json!({
                "pid": 4_100_000_000u64, "tap": "does-not-exist0", "session_id": "fc-test",
                "vsock_uds": serde_json::Value::Null, "netns": serde_json::Value::Null,
                "veth_root": serde_json::Value::Null,
                "state_volume_locks": [lpath.to_string_lossy()],
                "state_volume_paths": [vpath.to_string_lossy()],
            })
            .to_string(),
        )
        .unwrap();

        let backend = FirecrackerBackend {
            config: FirecrackerConfig {
                work_root,
                ..Default::default()
            },
            sessions: Arc::new(Mutex::new(HashMap::new())),
            page_servers: Arc::new(Mutex::new(HashMap::new())),
        };
        let session = RestoredSession {
            session_id: "fc-test".to_string(),
            backend_id: FIRECRACKER_BACKEND_ID.to_string(),
            guest_port: None,
            overlay_root: overlay_root.clone(),
            restored_bytes: 0,
            vmm_pid: None, // absent from the in-memory `sessions` map too — forces the cross-process pid path
            vsock_uds: None,
            workload_addr: None,
        };
        let receipt = backend.stop(session).unwrap();

        assert!(
            receipt.overlay_removed,
            "overlay must still be removed as before this slice"
        );
        assert!(!overlay_root.exists());
        assert!(
            vpath.exists(),
            "the durable state backing file must survive stop()"
        );
        assert_eq!(
            std::fs::read(&vpath).unwrap(),
            b"durable-bytes",
            "content untouched by the fsync"
        );
        assert!(!lpath.exists(), "the volume lock must be released");
    }

    #[test]
    fn stop_tolerates_a_missing_state_volume_path_without_failing() {
        // v1.6 (ato#983) Slice 4 fix: an artifact sealed before this fix has
        // no `state_volume_paths` field at all (json_str_array returns
        // empty); a path that WAS recorded but no longer exists (e.g. an
        // operator manually cleaned it up) must log, not fail `stop()` — the
        // fsync is a best-effort durability belt-and-suspenders step, never
        // a hard requirement for teardown to succeed.
        let dir = tempfile::tempdir().unwrap();
        let overlay_root = dir.path().join("overlay");
        std::fs::create_dir_all(&overlay_root).unwrap();
        let work_root = dir.path().join("work");
        let missing = work_root.join("state").join("does-not-exist.img");

        std::fs::write(
            overlay_root.join(".fc-session.json"),
            json!({
                "pid": 4_100_000_001u64, "tap": "does-not-exist1", "session_id": "fc-test-missing",
                "vsock_uds": serde_json::Value::Null, "netns": serde_json::Value::Null,
                "veth_root": serde_json::Value::Null,
                "state_volume_locks": Vec::<String>::new(),
                "state_volume_paths": [missing.to_string_lossy()],
            })
            .to_string(),
        )
        .unwrap();

        let backend = FirecrackerBackend {
            config: FirecrackerConfig {
                work_root,
                ..Default::default()
            },
            sessions: Arc::new(Mutex::new(HashMap::new())),
            page_servers: Arc::new(Mutex::new(HashMap::new())),
        };
        let session = RestoredSession {
            session_id: "fc-test-missing".to_string(),
            backend_id: FIRECRACKER_BACKEND_ID.to_string(),
            guest_port: None,
            overlay_root: overlay_root.clone(),
            restored_bytes: 0,
            vmm_pid: None,
            vsock_uds: None,
            workload_addr: None,
        };
        let receipt = backend
            .stop(session)
            .expect("stop must not fail on a missing fsync target");
        assert!(receipt.overlay_removed);
    }
}

// KVM/Firecracker is a Unix-only concept (does not exist on Windows at all).
#[cfg(all(test, unix))]
mod kvm_tests;
