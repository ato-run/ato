//! U1 (#854): a **local, in-process Firecracker UFFD page-server** (spike).
//!
//! When `LoadSnapshot` is issued with `mem_backend.backend_type = "Uffd"`,
//! Firecracker creates the `userfaultfd`, registers guest memory, and — over the
//! page-server's UDS — sends ONE `SCM_RIGHTS` message: the userfault fd (ancillary
//! data) + a JSON body of guest memory region mappings. The page-server then drains
//! the uffd event loop and serves each page fault via `UFFDIO_ZEROPAGE`
//! (`PageSource::Zero`, U1a — proves fd handoff + loop + ioctl plumbing; the VM does
//! NOT reach health) or `UFFDIO_COPY` from an `mmap` of the materialized `.mem`
//! (`PageSource::MemFile`, U1b — the VM reaches `/health`).
//!
//! **Scope (U1):** plumbing + handshake only. NO CAS / `MemoryPageIndex` / hotset /
//! `BindingLease` / remote / CRIU (those are U2+). The single fault-serving dispatch
//! ([`PageSource::serve_fault`]) is the U2 seam: a CAS-backed source drops in there
//! without touching the handshake, the event loop, or the receipt.
//!
//! This is exercised only by `#[ignore]`d KVM-gated smokes behind the `ATO_FC_UFFD`
//! env gate in `firecracker.rs`; the default restore path (File backend) is
//! unchanged. The userfaultfd ioctls are Linux-only, so the live body is
//! `#[cfg(target_os = "linux")]` with a non-Linux stub that fails closed.

use serde::{Deserialize, Serialize};

/// One guest memory region from the Firecracker UFFD handshake JSON body
/// (`GuestRegionUffdMapping`). Extra fields Firecracker may add are ignored.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Region {
    pub base_host_virt_addr: u64,
    pub size: u64,
    pub offset: u64,
}

impl Region {
    fn contains(&self, page: u64) -> bool {
        page >= self.base_host_virt_addr && page < self.base_host_virt_addr.wrapping_add(self.size)
    }
}

/// Round a fault address DOWN to the page boundary (`page_size` must be a power of 2).
pub(crate) fn page_align_down(addr: u64, page_size: u64) -> u64 {
    addr & !(page_size - 1)
}

/// Offset into the backing memory for a (page-aligned) fault inside `region`.
pub(crate) fn file_offset_for(region: &Region, fault_page: u64) -> u64 {
    region.offset + (fault_page - region.base_host_virt_addr)
}

/// Find the region containing a page-aligned fault address.
pub(crate) fn region_for(regions: &[Region], fault_page: u64) -> Option<&Region> {
    regions.iter().find(|r| r.contains(fault_page))
}

/// Current `UffdRestoreReceipt` schema version. Bump on any breaking field change.
pub(crate) const UFFD_RECEIPT_SCHEMA_VERSION: u32 = 1;

/// U8 (#875): the **stable, versioned Ready-State restore receipt** — the single
/// schema a File-vs-UFFD benchmark compares across, written to
/// `<overlay>/.uffd-receipt.json`. Page-server-measured fields (fault counts,
/// latencies) are set by [`PageServerHandle::receipt`]; restore-level context
/// (backend, source, hashes, sizes, timing) is filled by `restore()`. All
/// context/measurement fields are `#[serde(default)]` so older receipts still
/// deserialize (legacy receipts default `schema_version = 0`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct UffdRestoreReceipt {
    /// Schema version (see [`UFFD_RECEIPT_SCHEMA_VERSION`]). `0` = a legacy receipt.
    #[serde(default)]
    pub schema_version: u32,
    // ── restore-level context (filled by restore(); File backend fills the shared
    // subset so the two are comparable) ──────────────────────────────────────────
    /// Snapshot backend id (`firecracker`).
    #[serde(default)]
    pub backend: String,
    /// `mem_backend` used: `file` | `uffd`.
    #[serde(default)]
    pub mem_backend: String,
    /// Where memory pages came from: `file` | `local_cas` | `remote_cas`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub capsule_manifest_hash: String,
    #[serde(default)]
    pub runner_class_id: Option<String>,
    /// Content hash of the memory image (the memory blob id) — a hotset profile is
    /// only valid for a matching image (U12).
    #[serde(default)]
    pub memory_image_hash: String,
    #[serde(default)]
    pub memory_bytes_total: u64,
    /// Bytes of the memory image materialized to disk before `LoadSnapshot` (File:
    /// the whole image; UFFD: 0 — served on demand).
    #[serde(default)]
    pub memory_bytes_materialized: u64,
    #[serde(default)]
    pub pages_total: u64,
    // ── page-server measurement (set by PageServerHandle::receipt) ────────────────
    /// Userfault fd extracted from the `SCM_RIGHTS` handshake message.
    pub fd_received: bool,
    /// Guest memory regions parsed from the handshake JSON body.
    pub region_count: u32,
    /// `UFFD_EVENT_PAGEFAULT` events served on demand (a.k.a. pages faulted).
    pub page_fault_count: u64,
    /// Sum of `page_size` per served fault (`UFFDIO_COPY`/`UFFDIO_ZEROPAGE` len).
    pub bytes_copied: u64,
    /// Latency from event-loop entry to the first fault served (µs).
    pub first_fault_us: Option<u128>,
    /// Median per-fault ioctl service time (µs).
    pub p50_fault_service_us: Option<u128>,
    /// p95 per-fault ioctl service time (µs).
    pub p95_fault_service_us: Option<u128>,
    /// True only when real pages were served (U1b/U2/U6); false for U1a zero pages.
    pub vm_reaches_health: bool,
    /// `wait_health` elapsed (ms); `None` when health is not reached.
    pub time_to_health_ms: Option<u128>,
    /// `Some(pid)` — the page-server is local/in-process (a thread).
    pub page_server_pid: Option<i32>,
    /// U3 (#856): distinct guest pages faulted BEFORE `/health` (the hotset).
    #[serde(default)]
    pub pre_health_pages: Option<u64>,
    /// U4 (#857): pages prefetched proactively from the hotset profile (0 = demand-only).
    #[serde(default)]
    pub prefetch_pages: u64,
    /// U6 (#859): memory chunks fetched from the remote CAS via read-through.
    #[serde(default)]
    pub remote_chunks_fetched: u64,
    // ── outcome (filled by restore()) ────────────────────────────────────────────
    /// Total restore wall time (ms), rehydrate + LoadSnapshot + health.
    #[serde(default)]
    pub restore_total_ms: Option<u128>,
    /// U5 (#858): set when the restore failed closed (CAS miss/corrupt, page-server
    /// crash); `None` on success.
    #[serde(default)]
    pub fail_closed_reason: Option<String>,
    /// Whether teardown left no orphan VM/tap/overlay/socket (set by the caller when
    /// known).
    #[serde(default)]
    pub teardown_clean: Option<bool>,
}

/// U3 (#856): one recorded page fault — the raw signal a [`HotsetProfile`] is built
/// from in U4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TraceEntry {
    /// Page-aligned host virtual address that faulted (run-specific — Firecracker
    /// mmaps guest memory at a different host address each restore).
    pub page_gpa: u64,
    /// Offset into the memory image for this page (STABLE across restores — this is
    /// the portable key a `HotsetProfile` is built from).
    pub file_offset: u64,
    /// Latency from event-loop entry to this fault (µs).
    pub first_fault_at_us: u128,
    /// Time to service this fault (the ioctl) (µs).
    pub fault_service_us: u128,
    /// `demand` (U3) or `prefetch` (U4).
    pub source: String,
    /// `pre_health` or `post_health` (load-snapshot faults fold into pre_health).
    pub phase: String,
}

/// U3 (#856): the per-restore fault trace (the input to U4's `HotsetProfile`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct HotsetTrace {
    pub entries: Vec<TraceEntry>,
}

impl HotsetTrace {
    /// Distinct page GPAs faulted before `/health` — the hotset.
    pub(crate) fn pre_health_pages(&self) -> u64 {
        let mut seen = std::collections::BTreeSet::new();
        for e in &self.entries {
            if e.phase == "pre_health" {
                seen.insert(e.page_gpa);
            }
        }
        seen.len() as u64
    }
}

/// U4 (#857): the predictive prefetch model derived from a prior restore's
/// [`HotsetTrace`] — the distinct page GPAs touched before `/health`, in
/// first-touch order. On the next restore the page-server `UFFDIO_COPY`s these
/// proactively (before the guest demand-faults them) to cut faults in the
/// latency-critical pre-health window.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct HotsetProfile {
    /// Hot memory-image **file offsets** (stable across restores), in first-touch
    /// order. The page-server maps each back to this run's host address.
    pub offsets: Vec<u64>,
}

impl HotsetProfile {
    /// Build a profile from a trace: the pre-health page **file offsets**,
    /// de-duplicated, in first-touch order (earliest fault first). File offsets are
    /// portable across restores; host addresses are not. (U4 derives the next
    /// restore's prefetch list from a prior restore's trace; U12 persists it per
    /// identity via [`HotsetProfileStore`].)
    pub(crate) fn from_trace(trace: &HotsetTrace) -> HotsetProfile {
        let mut seen = std::collections::HashSet::new();
        let mut offsets = Vec::new();
        let mut pre: Vec<&TraceEntry> = trace
            .entries
            .iter()
            .filter(|e| e.phase == "pre_health")
            .collect();
        pre.sort_by_key(|e| e.first_fault_at_us);
        for e in pre {
            if seen.insert(e.file_offset) {
                offsets.push(e.file_offset);
            }
        }
        HotsetProfile { offsets }
    }
}

/// U12 (#879): the identity a hotset profile is valid for. A profile prefetches
/// file-offsets into ONE specific memory image on ONE runner/backend; applying it to
/// any other identity could prefetch offsets that do not exist in this image.
/// Persistence is keyed by ALL of these, so a mismatch is a cache **miss** (the
/// profile is never loaded), never a wrong-image prefetch.
#[derive(Debug, Clone)]
pub(crate) struct HotsetKey {
    pub capsule_manifest_hash: String,
    /// `""` when the manifest pins no runner class.
    pub runner_class_id: String,
    /// Content hash of the memory image (the memory blob id) — the field that makes a
    /// profile image-specific.
    pub memory_image_hash: String,
    pub backend_id: String,
    pub page_size: u64,
    pub memory_size: u64,
}

impl HotsetKey {
    /// Stable, filesystem-safe id: blake3 over every field (any field differing ⇒ a
    /// different id ⇒ a different file ⇒ no cross-identity reuse).
    pub(crate) fn id(&self) -> String {
        let mut h = blake3::Hasher::new();
        for f in [
            self.capsule_manifest_hash.as_str(),
            self.runner_class_id.as_str(),
            self.memory_image_hash.as_str(),
            self.backend_id.as_str(),
        ] {
            h.update(f.as_bytes());
            h.update(b"\0");
        }
        h.update(&self.page_size.to_le_bytes());
        h.update(&self.memory_size.to_le_bytes());
        h.finalize().to_hex().to_string()
    }
}

/// U12 (#879): a keyed, persistent hotset-profile store. `load` returns a profile
/// ONLY for an exact identity match; a different memory image / runner / backend /
/// page-size / memory-size is a miss, so a stale profile is never applied to the
/// wrong image (the U12 safety invariant). `save` is atomic (tmp + rename).
pub(crate) struct HotsetProfileStore {
    root: std::path::PathBuf,
}

impl HotsetProfileStore {
    pub(crate) fn open(root: impl Into<std::path::PathBuf>) -> HotsetProfileStore {
        HotsetProfileStore { root: root.into() }
    }

    fn path(&self, key: &HotsetKey) -> std::path::PathBuf {
        self.root.join(format!("{}.json", key.id()))
    }

    pub(crate) fn save(&self, key: &HotsetKey, profile: &HotsetProfile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let final_path = self.path(key);
        let tmp = final_path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(profile).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, final_path)
    }

    /// Load the profile for `key`, or `None` on absence / mismatch / parse error.
    pub(crate) fn load(&self, key: &HotsetKey) -> Option<HotsetProfile> {
        let text = std::fs::read(self.path(key)).ok()?;
        serde_json::from_slice(&text).ok()
    }
}

/// Find the region whose **file-offset** range contains `file_offset`, and the host
/// page address that offset maps to in this run.
pub(crate) fn host_page_for_offset(regions: &[Region], file_offset: u64) -> Option<u64> {
    regions
        .iter()
        .find(|r| file_offset >= r.offset && file_offset < r.offset.wrapping_add(r.size))
        .map(|r| r.base_host_virt_addr + (file_offset - r.offset))
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::{PageServer, PageServerHandle, PageSource};

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{
        HotsetProfile, HotsetTrace, Region, TraceEntry, UffdRestoreReceipt, file_offset_for,
        host_page_for_offset, page_align_down, region_for,
    };
    use capsulefs::{BlobManifest, CasStore, LazyBlobReader, MEMORY_PAGE_CHUNK_SIZE};
    use std::collections::HashMap;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    // ── userfaultfd FFI (absent from libc 0.2; defined here) ───────────────────
    const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
    const UFFDIO: u32 = 0xAA;

    /// `struct uffd_msg` (32 bytes; pagefault arg). repr(C) is naturally aligned.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UffdMsg {
        event: u8,
        reserved1: u8,
        reserved2: u16,
        reserved3: u32,
        pf_flags: u64,
        pf_address: u64,
        pf_feat: u64,
    }

    #[repr(C)]
    struct UffdioCopy {
        dst: u64,
        src: u64,
        len: u64,
        mode: u64,
        copy: i64,
    }

    #[repr(C)]
    struct UffdioRange {
        start: u64,
        len: u64,
    }

    #[repr(C)]
    struct UffdioZeropage {
        range: UffdioRange,
        mode: u64,
        zeropage: i64,
    }

    fn uffdio_copy_req() -> libc::Ioctl {
        libc::_IOWR::<UffdioCopy>(UFFDIO, 0x03)
    }
    fn uffdio_zeropage_req() -> libc::Ioctl {
        libc::_IOWR::<UffdioZeropage>(UFFDIO, 0x04)
    }

    fn page_size() -> usize {
        // SAFETY: sysconf with a constant name is always safe.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v > 0 { v as usize } else { 4096 }
    }

    /// A read-only `mmap` of the materialized `.mem` file; `munmap` on drop.
    pub(crate) struct MemMap {
        base: *mut libc::c_void,
        len: usize,
    }
    // SAFETY: the mapping is read-only and stable for the lifetime of MemMap; the
    // serving thread only reads from it.
    unsafe impl Send for MemMap {}
    unsafe impl Sync for MemMap {}

    impl MemMap {
        fn open(path: &Path) -> io::Result<Self> {
            let file = std::fs::File::open(path)?;
            let len = file.metadata()?.len() as usize;
            if len == 0 {
                return Err(io::Error::other("empty .mem file"));
            }
            // SAFETY: fd is valid, len > 0; PROT_READ + MAP_PRIVATE of a regular file.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(MemMap { base, len })
        }
    }

    impl Drop for MemMap {
        fn drop(&mut self) {
            // SAFETY: base/len came from a successful mmap and are unmapped once.
            unsafe { libc::munmap(self.base, self.len) };
        }
    }

    /// Where served pages come from. `serve_fault` is the single dispatch point —
    /// the U2 seam (a CAS-backed source drops in here).
    pub(crate) enum PageSource {
        /// U1a: kernel-zeroed pages via `UFFDIO_ZEROPAGE` (plumbing proof).
        Zero,
        /// U1b: real pages copied from the `.mem` mmap via `UFFDIO_COPY`.
        MemFile(MemMap),
        /// U2 (#855): serve pages lazily from local CAS — read the 2 MiB chunk
        /// containing the fault (cached = fault-around), copy the requested page.
        Cas(CasSource),
    }

    /// U2 CAS-backed page source: on a fault, read the containing 2 MiB memory chunk
    /// from local CAS via `read_range` (one chunk covers many 4 KB faults — the
    /// fault-around-2-MiB policy of #852) and cache it, then `UFFDIO_COPY` the page.
    /// No full `.mem` materialization. `read_range` re-verifies chunk hashes
    /// (fail-closed). The serve loop is single-threaded, so the cache mutex is
    /// uncontended.
    pub(crate) struct CasSource {
        store: CasStore,
        /// U6 (#859): optional remote CAS the page-server reads through on a local
        /// miss (fetch the chunk from remote, cache it local, then serve). Demand-
        /// only — only the working set crosses the "network".
        remote: Option<CasStore>,
        /// U8 (#875): chunks fetched from remote (shared with restore() for the
        /// receipt's `remote_chunks_fetched`).
        remote_fetches: Arc<AtomicU64>,
        mem_blob: BlobManifest,
        chunk_size: u64,
        total_len: u64,
        cache: std::sync::Mutex<HashMap<u64, Arc<Vec<u8>>>>,
    }

    impl CasSource {
        fn chunk_start(&self, file_offset: u64) -> u64 {
            (file_offset / self.chunk_size) * self.chunk_size
        }
        /// U6: ensure the CAS chunks overlapping `[start, start+len)` are present in
        /// the local store, fetching any missing ones from the remote read-through.
        fn ensure_local(&self, start: u64, len: u64) -> io::Result<()> {
            let Some(remote) = &self.remote else {
                return Ok(());
            };
            let end = start + len;
            for c in &self.mem_blob.chunks {
                let c_end = c.offset.wrapping_add(c.length);
                if c_end <= start || c.offset >= end {
                    continue;
                }
                if !self.store.has_chunk(&c.hash) {
                    let bytes = remote.get_chunk(&c.hash).map_err(|e| {
                        io::Error::other(format!("remote read-through {}: {e}", c.hash.hex()))
                    })?;
                    self.store
                        .put_chunk(&bytes)
                        .map_err(|e| io::Error::other(format!("cache read-through chunk: {e}")))?;
                    self.remote_fetches.fetch_add(1, Ordering::SeqCst);
                }
            }
            Ok(())
        }
        /// Get the (cached) 2 MiB chunk containing `file_offset`, reading it from CAS
        /// on a miss (read-through from remote first, if configured).
        fn chunk_for(&self, file_offset: u64) -> io::Result<Arc<Vec<u8>>> {
            let start = self.chunk_start(file_offset);
            let mut cache = self.cache.lock().unwrap();
            if let Some(c) = cache.get(&start) {
                return Ok(Arc::clone(c));
            }
            let len = self.chunk_size.min(self.total_len.saturating_sub(start));
            self.ensure_local(start, len)?;
            let bytes = LazyBlobReader::new(&self.store, &self.mem_blob)
                .read_range(start, len)
                .map_err(|e| io::Error::other(format!("cas read_range @{start}+{len}: {e}")))?;
            let arc = Arc::new(bytes);
            cache.insert(start, Arc::clone(&arc));
            Ok(arc)
        }
    }

    impl PageSource {
        pub(crate) fn mem_file(mem_path: &Path) -> io::Result<PageSource> {
            Ok(PageSource::MemFile(MemMap::open(mem_path)?))
        }

        pub(crate) fn cas(
            store: CasStore,
            mem_blob: BlobManifest,
            remote: Option<CasStore>,
            remote_fetches: Arc<AtomicU64>,
        ) -> PageSource {
            let total_len = mem_blob.total_len;
            PageSource::Cas(CasSource {
                store,
                remote,
                remote_fetches,
                mem_blob,
                chunk_size: MEMORY_PAGE_CHUNK_SIZE as u64,
                total_len,
                cache: std::sync::Mutex::new(HashMap::new()),
            })
        }

        /// Serve one page fault; returns bytes served. `mode = 0` so the kernel
        /// wakes the faulting vCPU on completion. `EEXIST` (already-present page from
        /// a concurrent-vCPU double fault) is benign.
        fn serve_fault(
            &self,
            uffd_fd: RawFd,
            fault_page: u64,
            file_offset: u64,
            page_size: usize,
        ) -> io::Result<u64> {
            let rc = match self {
                PageSource::Zero => {
                    let mut z = UffdioZeropage {
                        range: UffdioRange {
                            start: fault_page,
                            len: page_size as u64,
                        },
                        mode: 0,
                        zeropage: 0,
                    };
                    // SAFETY: uffd_fd is the kernel userfault fd; z is a valid
                    // UFFDIO_ZEROPAGE arg for a page-aligned, page-sized range.
                    unsafe { libc::ioctl(uffd_fd, uffdio_zeropage_req(), &mut z) }
                }
                PageSource::MemFile(mm) => {
                    if file_offset as usize + page_size > mm.len {
                        return Err(io::Error::other("fault past end of .mem mmap"));
                    }
                    let mut c = UffdioCopy {
                        dst: fault_page,
                        src: mm.base as u64 + file_offset,
                        len: page_size as u64,
                        mode: 0,
                        copy: 0,
                    };
                    // SAFETY: dst is a registered guest page; src..src+len lies within
                    // the read-only mmap (bounds-checked above).
                    unsafe { libc::ioctl(uffd_fd, uffdio_copy_req(), &mut c) }
                }
                PageSource::Cas(cas) => {
                    // Read (or reuse) the 2 MiB chunk containing the fault, then copy
                    // the page from it. `chunk` (Arc) is held alive across the ioctl.
                    let chunk = cas.chunk_for(file_offset)?;
                    let page_in_chunk = (file_offset - cas.chunk_start(file_offset)) as usize;
                    if page_in_chunk + page_size > chunk.len() {
                        return Err(io::Error::other("fault past end of CAS memory chunk"));
                    }
                    let mut c = UffdioCopy {
                        dst: fault_page,
                        src: chunk.as_ptr() as u64 + page_in_chunk as u64,
                        len: page_size as u64,
                        mode: 0,
                        copy: 0,
                    };
                    // SAFETY: dst is a registered guest page; src..src+len lies within
                    // `chunk`, which outlives this ioctl call (bounds-checked above).
                    unsafe { libc::ioctl(uffd_fd, uffdio_copy_req(), &mut c) }
                }
            };
            if rc == -1 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EEXIST) {
                    // Page already present (double fault / vCPU race) — benign.
                    return Ok(page_size as u64);
                }
                return Err(err);
            }
            Ok(page_size as u64)
        }
    }

    /// Counters written by the serving thread, snapshot-able at any time.
    #[derive(Debug, Default)]
    struct Shared {
        fd_received: AtomicBool,
        region_count: AtomicU32,
        page_fault_count: AtomicU64,
        bytes_copied: AtomicU64,
        first_fault_us: AtomicU64, // 0 = unset
        service_us: Mutex<Vec<u32>>,
        stop: AtomicBool,
        // U3 (#856): set by restore() when /health passes, so faults are tagged
        // pre_health vs post_health; the per-fault trace feeds U4's hotset profile.
        health_reached: AtomicBool,
        trace: Mutex<Vec<TraceEntry>>,
        // U4 (#857): pages prefetched proactively from the hotset profile.
        prefetch_count: AtomicU64,
        // U5 (#858): a fatal serve error (CAS miss/corrupt) — Some(reason) means the
        // page-server can no longer serve, so restore() fails closed fast.
        failed: Mutex<Option<String>>,
    }

    pub(crate) struct PageServer {
        listener: UnixListener,
        source: PageSource,
        page_size: usize,
    }

    impl PageServer {
        /// Bind the UDS (the listen-before-`LoadSnapshot` guarantee). Removes a stale
        /// socket first.
        pub(crate) fn bind(sock_path: &Path, source: PageSource) -> io::Result<PageServer> {
            let _ = std::fs::remove_file(sock_path);
            let listener = UnixListener::bind(sock_path)?;
            Ok(PageServer {
                listener,
                source,
                page_size: page_size(),
            })
        }

        /// Spawn the serving thread (accept the FC connection → `SCM_RIGHTS`
        /// handshake → optional hotset prefetch → uffd event loop) and return a
        /// handle. `hotset` (U4): pages to `UFFDIO_COPY` proactively before demand.
        pub(crate) fn serve(self, hotset: Option<HotsetProfile>) -> PageServerHandle {
            let shared = Arc::new(Shared::default());
            let t_shared = Arc::clone(&shared);
            let PageServer {
                listener,
                source,
                page_size,
            } = self;
            let join = std::thread::spawn(move || {
                if let Err(e) =
                    serve_loop(&listener, &source, page_size, &t_shared, hotset.as_ref())
                {
                    eprintln!("UFFD page-server: serve loop ended: {e}");
                }
            });
            PageServerHandle {
                shared,
                join: Some(join),
                pid: std::process::id() as i32,
            }
        }
    }

    /// Receive the fd + region body in one `SCM_RIGHTS` `recvmsg`.
    fn recv_handshake(conn_fd: RawFd) -> io::Result<(OwnedFd, Vec<Region>)> {
        let mut body = [0u8; 16384];
        let mut iov = libc::iovec {
            iov_base: body.as_mut_ptr().cast(),
            iov_len: body.len(),
        };
        // SAFETY: CMSG_SPACE is a const fn; sizing for exactly one fd.
        let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
        let mut cbuf = vec![0u8; space];
        // SAFETY: zeroed msghdr is valid; we set the pointers below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = space;

        // SAFETY: msg is fully initialized; conn_fd is the accepted UnixStream fd.
        let n = unsafe { libc::recvmsg(conn_fd, &mut msg, 0) };
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::other("SCM_RIGHTS truncated (fd dropped)"));
        }
        if msg.msg_flags & libc::MSG_TRUNC != 0 {
            return Err(io::Error::other("handshake body truncated"));
        }
        // Extract the fd from the first SCM_RIGHTS control message.
        // SAFETY: msg has a control buffer; CMSG_FIRSTHDR returns null if none.
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cmsg.is_null() {
            return Err(io::Error::other("no control message (no fd)"));
        }
        // SAFETY: cmsg is non-null and points into cbuf.
        let (level, ctype, clen) =
            unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type, (*cmsg).cmsg_len) };
        let want = unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) } as usize;
        if level != libc::SOL_SOCKET || ctype != libc::SCM_RIGHTS || clen != want {
            return Err(io::Error::other(
                "unexpected control message (not a single SCM_RIGHTS fd)",
            ));
        }
        let mut raw: libc::c_int = -1;
        // SAFETY: CMSG_DATA points to clen-prefixed payload >= size_of::<c_int>().
        unsafe {
            std::ptr::copy_nonoverlapping(
                libc::CMSG_DATA(cmsg),
                (&mut raw as *mut libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>(),
            );
        }
        if raw < 0 {
            return Err(io::Error::other("invalid fd in SCM_RIGHTS"));
        }
        // SAFETY: raw is a valid fd just received; own it so it closes exactly once.
        let uffd = unsafe { OwnedFd::from_raw_fd(raw) };
        let regions: Vec<Region> = serde_json::from_slice(&body[..n as usize])
            .map_err(|e| io::Error::other(format!("parse region body: {e}")))?;
        Ok((uffd, regions))
    }

    fn serve_loop(
        listener: &UnixListener,
        source: &PageSource,
        page_size: usize,
        shared: &Arc<Shared>,
        hotset: Option<&HotsetProfile>,
    ) -> io::Result<()> {
        let (conn, _) = listener.accept()?;
        let (uffd, regions) = recv_handshake(conn.as_raw_fd())?;
        shared.fd_received.store(true, Ordering::SeqCst);
        shared
            .region_count
            .store(regions.len() as u32, Ordering::SeqCst);
        let uffd_fd = uffd.as_raw_fd();
        let loop_start = Instant::now();

        // U4 (#857): prefetch the hotset BEFORE entering the demand loop — proactively
        // UFFDIO_COPY each profiled page (which also wakes any guest thread already
        // blocked on it), so the latency-critical pre-health working set is resident
        // and the guest demand-faults far fewer pages. EEXIST (guest beat us to it) is
        // benign. Non-hotset faults queue in the uffd and drain in the demand loop.
        if let Some(profile) = hotset {
            for &file_offset in &profile.offsets {
                if shared.stop.load(Ordering::SeqCst) {
                    break;
                }
                let aligned = page_align_down(file_offset, page_size as u64);
                // Map the stable file offset to THIS run's host page address.
                let Some(fault_page) = host_page_for_offset(&regions, aligned) else {
                    continue;
                };
                let t0 = Instant::now();
                if let Ok(bytes) = source.serve_fault(uffd_fd, fault_page, aligned, page_size) {
                    shared.prefetch_count.fetch_add(1, Ordering::SeqCst);
                    shared.bytes_copied.fetch_add(bytes, Ordering::SeqCst);
                    if let Ok(mut t) = shared.trace.lock() {
                        t.push(TraceEntry {
                            page_gpa: fault_page,
                            file_offset: aligned,
                            first_fault_at_us: loop_start.elapsed().as_micros(),
                            fault_service_us: t0.elapsed().as_micros(),
                            source: "prefetch".to_string(),
                            phase: "pre_health".to_string(),
                        });
                    }
                }
            }
        }

        loop {
            if shared.stop.load(Ordering::SeqCst) {
                break;
            }
            let mut pfd = libc::pollfd {
                fd: uffd_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: single valid pollfd.
            let r = unsafe { libc::poll(&mut pfd, 1, 200) };
            if r == 0 {
                continue; // timeout — re-check stop
            }
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(e);
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                break; // Firecracker gone
            }
            if pfd.revents & libc::POLLIN == 0 {
                continue;
            }
            let mut m: UffdMsg = unsafe { std::mem::zeroed() };
            // SAFETY: reading exactly one uffd_msg-sized record from the uffd.
            let got = unsafe {
                libc::read(
                    uffd_fd,
                    (&mut m as *mut UffdMsg).cast(),
                    std::mem::size_of::<UffdMsg>(),
                )
            };
            if got == 0 {
                break; // EOF — uffd closed (FC exited)
            }
            if got < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EAGAIN) {
                    continue;
                }
                return Err(e);
            }
            if got as usize != std::mem::size_of::<UffdMsg>() || m.event != UFFD_EVENT_PAGEFAULT {
                continue;
            }
            let t0 = Instant::now();
            let fault_page = page_align_down(m.pf_address, page_size as u64);
            let Some(region) = region_for(&regions, fault_page) else {
                // Out-of-range fault: zero it so the guest does not wedge.
                let _ = PageSource::Zero.serve_fault(uffd_fd, fault_page, 0, page_size);
                continue;
            };
            let file_offset = file_offset_for(region, fault_page);
            match source.serve_fault(uffd_fd, fault_page, file_offset, page_size) {
                Ok(bytes) => {
                    let at_us = loop_start.elapsed().as_micros();
                    let service_us = t0.elapsed().as_micros();
                    shared.page_fault_count.fetch_add(1, Ordering::SeqCst);
                    shared.bytes_copied.fetch_add(bytes, Ordering::SeqCst);
                    let _ = shared.first_fault_us.compare_exchange(
                        0,
                        at_us.max(1) as u64,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    if let Ok(mut v) = shared.service_us.lock() {
                        v.push(service_us as u32);
                    }
                    // U3: record the fault for the hotset trace.
                    if let Ok(mut t) = shared.trace.lock() {
                        t.push(TraceEntry {
                            page_gpa: fault_page,
                            file_offset,
                            first_fault_at_us: at_us,
                            fault_service_us: service_us,
                            source: "demand".to_string(),
                            phase: if shared.health_reached.load(Ordering::SeqCst) {
                                "post_health".to_string()
                            } else {
                                "pre_health".to_string()
                            },
                        });
                    }
                    crate::bench::count("uffd.fault_events", 1);
                }
                Err(e) => {
                    // U5 (#858): a fatal serve error (CAS miss / corrupt chunk —
                    // read_range fails closed on a hash mismatch) means this page can
                    // NEVER be served. Record the failure and STOP — the guest hangs
                    // on the unserved page, so restore()'s health wait fails fast
                    // (instead of silently booting a VM on missing/corrupt memory).
                    eprintln!("UFFD page-server: fatal serve_fault at {fault_page:#x}: {e}");
                    if let Ok(mut f) = shared.failed.lock() {
                        *f = Some(format!("serve_fault @ {fault_page:#x}: {e}"));
                    }
                    break;
                }
            }
        }
        Ok(()) // uffd OwnedFd + conn drop here (closed once)
    }

    #[derive(Debug)]
    pub(crate) struct PageServerHandle {
        shared: Arc<Shared>,
        join: Option<JoinHandle<()>>,
        pid: i32,
    }

    impl PageServerHandle {
        /// Wait until the first fault is served (U1a, where the VM never reaches health).
        pub(crate) fn wait_for_first_fault(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.shared.page_fault_count.load(Ordering::SeqCst) > 0 {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            self.shared.page_fault_count.load(Ordering::SeqCst) > 0
        }

        /// U3: tag subsequent faults as post-health (call when `/health` passes).
        pub(crate) fn mark_health_reached(&self) {
            self.shared.health_reached.store(true, Ordering::SeqCst);
        }

        /// U5: `Some(reason)` if the page-server hit a fatal serve error (CAS
        /// miss/corrupt) and can no longer serve — restore() must fail closed.
        pub(crate) fn failed(&self) -> Option<String> {
            self.shared.failed.lock().ok().and_then(|f| f.clone())
        }

        /// U3: snapshot the per-restore fault trace (for the `.hotset-trace.json`).
        pub(crate) fn trace(&self) -> HotsetTrace {
            let entries = self
                .shared
                .trace
                .lock()
                .map(|t| t.clone())
                .unwrap_or_default();
            HotsetTrace { entries }
        }

        /// Snapshot the counters into a receipt WITHOUT stopping the thread.
        pub(crate) fn receipt(
            &self,
            vm_reaches_health: bool,
            time_to_health_ms: Option<u128>,
        ) -> UffdRestoreReceipt {
            let faults = self.shared.page_fault_count.load(Ordering::SeqCst);
            let mut svc = self
                .shared
                .service_us
                .lock()
                .map(|v| v.clone())
                .unwrap_or_default();
            svc.sort_unstable();
            let pct = |p: f64| -> Option<u128> {
                if svc.is_empty() {
                    return None;
                }
                let idx = (((svc.len() as f64) * p).ceil() as usize)
                    .saturating_sub(1)
                    .min(svc.len() - 1);
                Some(svc[idx] as u128)
            };
            let first = self.shared.first_fault_us.load(Ordering::SeqCst);
            UffdRestoreReceipt {
                schema_version: super::UFFD_RECEIPT_SCHEMA_VERSION,
                fd_received: self.shared.fd_received.load(Ordering::SeqCst),
                region_count: self.shared.region_count.load(Ordering::SeqCst),
                page_fault_count: faults,
                bytes_copied: self.shared.bytes_copied.load(Ordering::SeqCst),
                first_fault_us: if first == 0 {
                    None
                } else {
                    Some(first as u128)
                },
                p50_fault_service_us: pct(0.50),
                p95_fault_service_us: pct(0.95),
                vm_reaches_health,
                time_to_health_ms,
                page_server_pid: Some(self.pid),
                pre_health_pages: if vm_reaches_health {
                    Some(self.trace().pre_health_pages())
                } else {
                    None
                },
                prefetch_pages: self.shared.prefetch_count.load(Ordering::SeqCst),
                // restore() fills the context (backend/source/hashes/sizes/timing).
                ..Default::default()
            }
        }

        /// Signal stop and join the serving thread (it also self-exits on uffd EOF
        /// once Firecracker is killed).
        pub(crate) fn stop_and_join(mut self) -> io::Result<()> {
            self.shared.stop.store(true, Ordering::SeqCst);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
            Ok(())
        }
    }
}

// ── non-Linux stub: keeps the crate compiling on macOS/CI; fails closed ────────
#[cfg(not(target_os = "linux"))]
pub(crate) use stub::{PageServer, PageServerHandle, PageSource};

#[cfg(not(target_os = "linux"))]
mod stub {
    use super::UffdRestoreReceipt;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    pub(crate) struct MemMap;
    pub(crate) enum PageSource {
        Zero,
        MemFile(MemMap),
    }
    impl PageSource {
        pub(crate) fn mem_file(_mem_path: &Path) -> io::Result<PageSource> {
            Err(io::Error::other("UFFD page-server is Linux-only"))
        }
        pub(crate) fn cas(
            _store: capsulefs::CasStore,
            _mem_blob: capsulefs::BlobManifest,
            _remote: Option<capsulefs::CasStore>,
            _remote_fetches: std::sync::Arc<std::sync::atomic::AtomicU64>,
        ) -> PageSource {
            PageSource::Zero // unreachable: bind() fails closed on non-linux
        }
    }
    pub(crate) struct PageServer;
    impl PageServer {
        pub(crate) fn bind(_sock_path: &Path, _source: PageSource) -> io::Result<PageServer> {
            Err(io::Error::other("UFFD page-server is Linux-only"))
        }
        pub(crate) fn serve(self, _hotset: Option<super::HotsetProfile>) -> PageServerHandle {
            PageServerHandle
        }
    }
    #[derive(Debug)]
    pub(crate) struct PageServerHandle;
    impl PageServerHandle {
        pub(crate) fn wait_for_first_fault(&self, _timeout: Duration) -> bool {
            false
        }
        pub(crate) fn mark_health_reached(&self) {}
        pub(crate) fn failed(&self) -> Option<String> {
            None
        }
        pub(crate) fn trace(&self) -> super::HotsetTrace {
            super::HotsetTrace::default()
        }
        pub(crate) fn receipt(&self, _h: bool, _t: Option<u128>) -> UffdRestoreReceipt {
            unreachable!("uffd page-server is linux-only")
        }
        pub(crate) fn stop_and_join(self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_align_down_rounds_to_boundary() {
        assert_eq!(page_align_down(0x1234, 4096), 0x1000);
        assert_eq!(page_align_down(0x1000, 4096), 0x1000);
        assert_eq!(page_align_down(0x1fff, 4096), 0x1000);
        assert_eq!(page_align_down(0, 4096), 0);
    }

    #[test]
    fn region_lookup_and_offset() {
        let regions = vec![
            Region {
                base_host_virt_addr: 0x1_0000,
                size: 0x2000,
                offset: 0,
            },
            Region {
                base_host_virt_addr: 0x10_0000,
                size: 0x1000,
                offset: 0x2000,
            },
        ];
        // a page in the first region
        let r = region_for(&regions, 0x1_1000).expect("region");
        assert_eq!(r.base_host_virt_addr, 0x1_0000);
        assert_eq!(file_offset_for(r, 0x1_1000), 0x1000);
        // a page in the second region (offset carries the prior region's bytes)
        let r2 = region_for(&regions, 0x10_0000).expect("region2");
        assert_eq!(file_offset_for(r2, 0x10_0000), 0x2000);
        // out of range
        assert!(region_for(&regions, 0x99_0000).is_none());
    }

    #[test]
    fn region_body_parses_firecracker_json_ignoring_extra_fields() {
        // Firecracker sends GuestRegionUffdMapping[]; tolerate extra keys.
        let body = r#"[
          {"base_host_virt_addr": 4096, "size": 8192, "offset": 0, "page_size_kib": 4}
        ]"#;
        let regions: Vec<Region> = serde_json::from_str(body).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].base_host_virt_addr, 4096);
        assert_eq!(regions[0].size, 8192);
    }

    /// U8 (#875): the receipt schema round-trips and a legacy receipt (pre-U8, no
    /// context/version) still deserializes with `schema_version = 0`.
    #[test]
    fn uffd_receipt_schema_round_trips_and_legacy_defaults() {
        let r = UffdRestoreReceipt {
            schema_version: UFFD_RECEIPT_SCHEMA_VERSION,
            backend: "firecracker".into(),
            mem_backend: "uffd".into(),
            source: "remote_cas".into(),
            capsule_manifest_hash: "blake3:cap".into(),
            memory_bytes_total: 512 * 1024 * 1024,
            pages_total: 131072,
            page_fault_count: 42,
            prefetch_pages: 40,
            remote_chunks_fetched: 7,
            restore_total_ms: Some(489),
            ..Default::default()
        };
        let back: UffdRestoreReceipt =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.schema_version, UFFD_RECEIPT_SCHEMA_VERSION);
        assert_eq!(back.source, "remote_cas");
        assert_eq!(back.page_fault_count, 42);
        assert_eq!(back.remote_chunks_fetched, 7);

        // A legacy receipt (only the pre-U8 measured fields) still parses.
        let legacy = r#"{"fd_received":true,"region_count":1,"page_fault_count":5,
          "bytes_copied":20480,"first_fault_us":100,"p50_fault_service_us":5,
          "p95_fault_service_us":15,"vm_reaches_health":false,"time_to_health_ms":null,
          "page_server_pid":123}"#;
        let old: UffdRestoreReceipt = serde_json::from_str(legacy).unwrap();
        assert_eq!(old.schema_version, 0, "legacy receipt ⇒ schema_version 0");
        assert_eq!(old.page_fault_count, 5);
        assert!(
            old.backend.is_empty() && old.source.is_empty(),
            "legacy has no context"
        );
    }

    /// U12 (#879): a persisted hotset profile round-trips for an EXACT identity, and a
    /// key that differs in ANY field (here the memory image hash) is a miss — a stale
    /// profile is never applied to a different image.
    #[test]
    fn hotset_profile_store_keys_by_identity_and_never_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        let store = HotsetProfileStore::open(dir.path().join("hotset"));
        let key = HotsetKey {
            capsule_manifest_hash: "blake3:cap".into(),
            runner_class_id: "blake3:runner".into(),
            memory_image_hash: "blake3:mem-A".into(),
            backend_id: "firecracker".into(),
            page_size: 4096,
            memory_size: 536870912,
        };
        let profile = HotsetProfile {
            offsets: vec![0, 2 << 20, 4 << 20],
        };
        store.save(&key, &profile).unwrap();

        // exact identity ⇒ hit.
        assert_eq!(store.load(&key).unwrap().offsets, profile.offsets);

        // different memory image ⇒ MISS (never the wrong-image profile).
        let mut other = key.clone();
        other.memory_image_hash = "blake3:mem-B".into();
        assert!(
            store.load(&other).is_none(),
            "different image must not load the profile"
        );

        // different runner / backend / page-size / mem-size ⇒ all misses.
        for mutate in [
            |k: &mut HotsetKey| k.runner_class_id = "blake3:other".into(),
            |k: &mut HotsetKey| k.backend_id = "qemu".into(),
            |k: &mut HotsetKey| k.page_size = 2 << 20,
            |k: &mut HotsetKey| k.memory_size = 1 << 30,
        ] {
            let mut k = key.clone();
            mutate(&mut k);
            assert!(store.load(&k).is_none(), "any identity change ⇒ miss");
        }
    }

    /// U13 (#880): a corrupt profile file is IGNORED (load ⇒ None), never a bad
    /// profile applied — a garbage prefetch list must not reach the page-server.
    #[test]
    fn hotset_profile_store_ignores_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("h");
        let store = HotsetProfileStore::open(&root);
        let key = HotsetKey {
            capsule_manifest_hash: "blake3:cap".into(),
            runner_class_id: String::new(),
            memory_image_hash: "blake3:mem".into(),
            backend_id: "firecracker".into(),
            page_size: 4096,
            memory_size: 1 << 20,
        };
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("{}.json", key.id())),
            b"}{ not json at all",
        )
        .unwrap();
        assert!(
            store.load(&key).is_none(),
            "corrupt profile file ⇒ None (never a bad profile)"
        );
    }
}
