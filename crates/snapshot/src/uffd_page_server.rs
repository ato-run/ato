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

/// U1 measurement receipt (written to `<overlay>/.uffd-receipt.json` by `restore()`
/// so the KVM smoke can read it back). Shape is a subset of the documented
/// `UffdRestoreReceipt` (`docs/ready-state/uffd-mem-backend.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct U1Receipt {
    /// Userfault fd extracted from the `SCM_RIGHTS` handshake message.
    pub fd_received: bool,
    /// Guest memory regions parsed from the handshake JSON body.
    pub region_count: u32,
    /// `UFFD_EVENT_PAGEFAULT` events served.
    pub page_fault_count: u64,
    /// Sum of `page_size` per served fault (`UFFDIO_COPY`/`UFFDIO_ZEROPAGE` len).
    pub bytes_copied: u64,
    /// Latency from event-loop entry to the first fault served (µs).
    pub first_fault_us: Option<u128>,
    /// Median per-fault ioctl service time (µs).
    pub p50_fault_service_us: Option<u128>,
    /// p95 per-fault ioctl service time (µs).
    pub p95_fault_service_us: Option<u128>,
    /// True only for U1b (real pages); false for U1a (zero pages).
    pub vm_reaches_health: bool,
    /// `wait_health` elapsed (ms); `None` for U1a.
    pub time_to_health_ms: Option<u128>,
    /// `Some(pid)` — the U1 page-server is local/in-process (a thread).
    pub page_server_pid: Option<i32>,
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::{PageServer, PageServerHandle, PageSource};

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{file_offset_for, page_align_down, region_for, Region, U1Receipt};
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
    }

    impl PageSource {
        pub(crate) fn mem_file(mem_path: &Path) -> io::Result<PageSource> {
            Ok(PageSource::MemFile(MemMap::open(mem_path)?))
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
                        range: UffdioRange { start: fault_page, len: page_size as u64 },
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
            Ok(PageServer { listener, source, page_size: page_size() })
        }

        /// Spawn the serving thread (accept the FC connection → `SCM_RIGHTS`
        /// handshake → uffd event loop) and return a handle.
        pub(crate) fn serve(self) -> PageServerHandle {
            let shared = Arc::new(Shared::default());
            let t_shared = Arc::clone(&shared);
            let PageServer { listener, source, page_size } = self;
            let join = std::thread::spawn(move || {
                if let Err(e) = serve_loop(&listener, &source, page_size, &t_shared) {
                    eprintln!("UFFD page-server: serve loop ended: {e}");
                }
            });
            PageServerHandle { shared, join: Some(join), pid: std::process::id() as i32 }
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
        let (level, ctype, clen) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type, (*cmsg).cmsg_len) };
        let want = unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) } as usize;
        if level != libc::SOL_SOCKET || ctype != libc::SCM_RIGHTS || clen != want {
            return Err(io::Error::other("unexpected control message (not a single SCM_RIGHTS fd)"));
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
    ) -> io::Result<()> {
        let (conn, _) = listener.accept()?;
        let (uffd, regions) = recv_handshake(conn.as_raw_fd())?;
        shared.fd_received.store(true, Ordering::SeqCst);
        shared.region_count.store(regions.len() as u32, Ordering::SeqCst);
        let uffd_fd = uffd.as_raw_fd();
        let loop_start = Instant::now();

        loop {
            if shared.stop.load(Ordering::SeqCst) {
                break;
            }
            let mut pfd = libc::pollfd { fd: uffd_fd, events: libc::POLLIN, revents: 0 };
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
                    shared.page_fault_count.fetch_add(1, Ordering::SeqCst);
                    shared.bytes_copied.fetch_add(bytes, Ordering::SeqCst);
                    let _ = shared.first_fault_us.compare_exchange(
                        0,
                        loop_start.elapsed().as_micros().max(1) as u64,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    if let Ok(mut v) = shared.service_us.lock() {
                        v.push(t0.elapsed().as_micros() as u32);
                    }
                    crate::bench::count("uffd.fault_events", 1);
                }
                Err(e) => {
                    // A missed fault stalls the guest but never corrupts the host.
                    eprintln!("UFFD page-server: serve_fault failed at {fault_page:#x}: {e}");
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

        /// Snapshot the counters into a receipt WITHOUT stopping the thread.
        pub(crate) fn receipt(&self, vm_reaches_health: bool, time_to_health_ms: Option<u128>) -> U1Receipt {
            let faults = self.shared.page_fault_count.load(Ordering::SeqCst);
            let mut svc = self.shared.service_us.lock().map(|v| v.clone()).unwrap_or_default();
            svc.sort_unstable();
            let pct = |p: f64| -> Option<u128> {
                if svc.is_empty() {
                    return None;
                }
                let idx = (((svc.len() as f64) * p).ceil() as usize).saturating_sub(1).min(svc.len() - 1);
                Some(svc[idx] as u128)
            };
            let first = self.shared.first_fault_us.load(Ordering::SeqCst);
            U1Receipt {
                fd_received: self.shared.fd_received.load(Ordering::SeqCst),
                region_count: self.shared.region_count.load(Ordering::SeqCst),
                page_fault_count: faults,
                bytes_copied: self.shared.bytes_copied.load(Ordering::SeqCst),
                first_fault_us: if first == 0 { None } else { Some(first as u128) },
                p50_fault_service_us: pct(0.50),
                p95_fault_service_us: pct(0.95),
                vm_reaches_health,
                time_to_health_ms,
                page_server_pid: Some(self.pid),
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
    use super::U1Receipt;
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
    }
    pub(crate) struct PageServer;
    impl PageServer {
        pub(crate) fn bind(_sock_path: &Path, _source: PageSource) -> io::Result<PageServer> {
            Err(io::Error::other("UFFD page-server is Linux-only"))
        }
        pub(crate) fn serve(self) -> PageServerHandle {
            PageServerHandle
        }
    }
    #[derive(Debug)]
    pub(crate) struct PageServerHandle;
    impl PageServerHandle {
        pub(crate) fn wait_for_first_fault(&self, _timeout: Duration) -> bool {
            false
        }
        pub(crate) fn receipt(&self, _h: bool, _t: Option<u128>) -> U1Receipt {
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
            Region { base_host_virt_addr: 0x1_0000, size: 0x2000, offset: 0 },
            Region { base_host_virt_addr: 0x10_0000, size: 0x1000, offset: 0x2000 },
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
}
