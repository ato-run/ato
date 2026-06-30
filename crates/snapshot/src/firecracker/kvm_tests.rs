//! KVM-gated integration tests for the real `FirecrackerBackend`.
//!
//! These are `#[ignore]`d so a normal `cargo test` (and CI without `/dev/kvm`)
//! never runs them. Run them on a KVM host with the M0 stack:
//!
//! ```sh
//! sudo -E env \
//!   ATO_FC_BIN=$PWD/firecracker \
//!   ATO_FC_KERNEL=$PWD/vmlinux \
//!   ATO_FC_TEST_ROOTFS=$PWD/rootfs.ext4 \
//!   cargo test -p snapshot --release -- --ignored --nocapture fc_kvm
//! ```
//!
//! Each test also self-skips (returns early) if `/dev/kvm` is absent or
//! `ATO_FC_TEST_ROOTFS` is unset, so they are safe to invoke anywhere.

use super::*;
use crate::manifest::{RestoreContract, SanitizerContract};

fn skip() -> Option<(FirecrackerBackend, Vec<u8>)> {
    if !FirecrackerBackend::kvm_present() {
        eprintln!("SKIP: /dev/kvm absent");
        return None;
    }
    let rootfs = match std::env::var("ATO_FC_TEST_ROOTFS") {
        Ok(p) if !p.is_empty() => std::fs::read(p).expect("read ATO_FC_TEST_ROOTFS"),
        _ => {
            eprintln!("SKIP: ATO_FC_TEST_ROOTFS not set");
            return None;
        }
    };
    let b = FirecrackerBackend::new();
    if !b.probe().available {
        eprintln!("SKIP: firecracker not available: {:?}", b.probe().reason);
        return None;
    }
    Some((b, rootfs))
}

fn build_input<'a>(store: &'a CasStore, rootfs: Vec<u8>, markers: Vec<String>) -> BuildReadyStateInput<'a> {
    BuildReadyStateInput {
        store,
        capsule_manifest_hash: "blake3:fc-kvm-test".to_string(),
        runner_class: None,
        layers: BuildLayers { rootfs, runtime: None, dependency: None, app: None, vmstate: Vec::new(), memory: Vec::new() },
        restore_contract: RestoreContract { ports: vec![8080], healthcheck: Some("/health".to_string()), expected_ready_ms: Some(3000) },
        sanitizer_contract: SanitizerContract::default(),
        declared_secret_markers: markers,
    }
}

#[test]
#[ignore]
fn fc_kvm_probe_available() {
    if !FirecrackerBackend::kvm_present() { eprintln!("SKIP: no kvm"); return; }
    let p = FirecrackerBackend::new().probe();
    assert!(p.available, "expected available on KVM host: {:?}", p.reason);
    assert_eq!(p.snapshot_kind, SnapshotKind::MicroVm);
    assert!(p.vmm_version.is_some());
}

/// U0: on a real KVM host the UFFD facet must be truthful — `true` (no reason)
/// only when x86_64 + kernel userfaultfd, else `false` with a concrete reason.
#[test]
#[ignore]
fn fc_kvm_probe_uffd() {
    if !FirecrackerBackend::kvm_present() { eprintln!("SKIP: no kvm"); return; }
    let p = FirecrackerBackend::new().probe();
    if std::env::consts::ARCH == "x86_64" && crate::uffd::host_userfaultfd_present() {
        assert!(
            p.supports_uffd_mem_backend,
            "expected UFFD support on x86_64 KVM host with userfaultfd: {:?}",
            p.uffd_reason
        );
        assert!(p.uffd_reason.is_none());
    } else {
        assert!(!p.supports_uffd_mem_backend);
        assert!(p.uffd_reason.is_some());
    }
}

#[test]
#[ignore]
fn fc_kvm_build_restore_roundtrip() {
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let receipt = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build");
    assert!(receipt.no_secret_proof.is_clean());
    assert!(receipt.manifest.layers.memory.is_some());
    assert!(receipt.manifest.runner_class_id.is_some());
    let m = receipt.manifest.clone();
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov"), host_runner_class: None }).expect("restore");
    assert_eq!(r.session.guest_port, Some(8080));
    assert!(r.session.restored_bytes > 0);
    let overlay = r.session.overlay_root.clone();
    let td = b.stop(r.session).expect("stop");
    // Teardown leaves NO resources: overlay gone, tap gone, no firecracker proc.
    assert!(td.overlay_removed);
    assert!(!overlay.exists(), "overlay dir not removed");
    let tap = FirecrackerConfig::default().tap_dev;
    let taps = std::process::Command::new("ip").args(["link", "show", &tap]).output().unwrap();
    assert!(!taps.status.success(), "tap {tap} still present after stop");
    let out = std::process::Command::new("pgrep").args(["-af", "firecracker --api-sock"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).count(),
        0,
        "orphan firecracker left after stop"
    );
}

// ── U1 (#854): UFFD page-server handshake smokes ───────────────────────────────
// These set the process-global ATO_FC_UFFD gate, so run the KVM suite with
// --test-threads=1 (as documented for the fc_kvm_* tests).

fn read_uffd_receipt(overlay: &std::path::Path) -> crate::uffd_page_server::U1Receipt {
    let text = std::fs::read_to_string(overlay.join(".uffd-receipt.json")).expect("uffd receipt written");
    serde_json::from_str(&text).expect("parse uffd receipt")
}

fn assert_clean_teardown(overlay: &std::path::Path) {
    assert!(!overlay.exists(), "overlay dir not removed");
    let tap = FirecrackerConfig::default().tap_dev;
    let taps = std::process::Command::new("ip").args(["link", "show", &tap]).output().unwrap();
    assert!(!taps.status.success(), "tap {tap} still present after stop");
    let out = std::process::Command::new("pgrep").args(["-af", "firecracker --api-sock"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).count(),
        0,
        "orphan firecracker after stop"
    );
    assert!(!overlay.join(".page-server.sock").exists(), "page-server socket not removed");
}

/// U1a: zero/test pages prove the SCM_RIGHTS fd handoff + region parse + uffd event
/// loop + UFFDIO_ZEROPAGE plumbing. The guest gets garbage memory, so it does NOT
/// reach health — we only require the fault loop fired. Teardown must be clean.
#[test]
#[ignore]
fn fc_kvm_uffd_zero_pages_plumbing() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd plumbing needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let overlay = dir.path().join("ov");
    // SAFETY: KVM suite runs --test-threads=1; gate is removed before returning.
    unsafe { std::env::set_var("ATO_FC_UFFD", "zero") };
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: overlay.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };
    let r = r.expect("restore (uffd zero)");

    let rec = read_uffd_receipt(&overlay);
    assert!(rec.fd_received, "userfault fd received via SCM_RIGHTS");
    assert!(rec.region_count > 0, "guest regions parsed: {}", rec.region_count);
    assert!(rec.page_fault_count > 0, "uffd event loop served at least one fault");
    assert!(rec.bytes_copied > 0, "UFFDIO_ZEROPAGE served bytes");
    assert!(!rec.vm_reaches_health, "zero pages must NOT reach health");
    assert!(rec.first_fault_us.is_some(), "first-fault latency recorded");
    assert_eq!(rec.page_server_pid, Some(std::process::id() as i32), "in-process page-server");
    eprintln!("### U1a-RECEIPT {}", serde_json::to_string(&rec).unwrap());

    b.stop(r.session).expect("stop");
    assert_clean_teardown(&overlay);
}

/// U1b: the page-server serves real pages from the materialized .mem file via
/// UFFDIO_COPY, so the VM reaches /health. Proves the full UFFD restore handshake
/// end-to-end (sans CAS, which is U2). Teardown must be clean.
#[test]
#[ignore]
fn fc_kvm_uffd_real_pages_reaches_health() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd serving needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let overlay = dir.path().join("ov");
    // SAFETY: KVM suite runs --test-threads=1; gate is removed before returning.
    unsafe { std::env::set_var("ATO_FC_UFFD", "mem") };
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: overlay.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };
    let r = r.expect("restore (uffd mem)");
    let port = r.session.guest_port.unwrap_or(8080);

    let rec = read_uffd_receipt(&overlay);
    assert!(rec.fd_received && rec.region_count > 0);
    assert!(rec.vm_reaches_health, "UFFDIO_COPY'd real pages must reach health");
    assert!(rec.time_to_health_ms.is_some(), "time-to-health recorded");
    assert!(rec.page_fault_count > 0, "working set faulted in: {}", rec.page_fault_count);
    assert!(rec.bytes_copied > 0 && rec.p50_fault_service_us.is_some() && rec.p95_fault_service_us.is_some());

    // The VM actually served from UFFDIO_COPY'd pages.
    let gip = FirecrackerConfig::default().guest_ip;
    assert!(!http_get(&gip, port, "/health").is_empty(), "guest /health reachable over UFFD restore");
    eprintln!("### U1b-RECEIPT {}", serde_json::to_string(&rec).unwrap());

    b.stop(r.session).expect("stop");
    assert_clean_teardown(&overlay);
}

/// U2 (#855): the page-server serves memory pages **lazily from local CAS** (no full
/// `.mem` materialization) via `read_range` (2 MiB fault-around), and the VM reaches
/// `/health`. Demand-only: only the working set is faulted in, far less than the full
/// memory image.
#[test]
#[ignore]
fn fc_kvm_uffd_cas_demand_serves_from_local_cas() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd cas serving needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let mem_total = m.layers.memory.as_ref().expect("memory layer").total_len;
    let overlay = dir.path().join("ov");
    // SAFETY: KVM suite runs --test-threads=1; gate is removed before returning.
    unsafe { std::env::set_var("ATO_FC_UFFD", "cas") };
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: overlay.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };
    let r = r.expect("restore (uffd cas)");
    let port = r.session.guest_port.unwrap_or(8080);

    let rec = read_uffd_receipt(&overlay);
    assert!(rec.fd_received && rec.region_count > 0);
    assert!(rec.vm_reaches_health, "CAS-served real pages must reach health");
    assert!(rec.page_fault_count > 0 && rec.bytes_copied > 0, "faults served from CAS");
    // Demand-only: the working set faulted in is far smaller than the full memory
    // image — we never materialized the whole .mem.
    assert!(
        rec.bytes_copied < mem_total / 2,
        "demand-only: bytes_copied {} should be << mem_total {}",
        rec.bytes_copied,
        mem_total
    );
    let gip = FirecrackerConfig::default().guest_ip;
    assert!(!http_get(&gip, port, "/health").is_empty(), "guest /health reachable over CAS-UFFD restore");
    eprintln!("### U2-RECEIPT mem_total={mem_total} {}", serde_json::to_string(&rec).unwrap());

    b.stop(r.session).expect("stop");
    assert_clean_teardown(&overlay);
}

fn read_hotset_trace(overlay: &std::path::Path) -> crate::uffd_page_server::HotsetTrace {
    let text = std::fs::read_to_string(overlay.join(".hotset-trace.json")).expect("hotset trace written");
    serde_json::from_str(&text).expect("parse hotset trace")
}

/// U3 (#856): the page-server records a per-restore fault trace (HotsetTrace) — the
/// signal U4's hotset prefetch consumes. Asserts the trace captures the pre-health
/// hotset, page-aligned GPAs, all demand-sourced (no prefetch yet).
#[test]
#[ignore]
fn fc_kvm_uffd_fault_trace_records_hotset() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd trace needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let overlay = dir.path().join("ov");
    // SAFETY: KVM suite runs --test-threads=1; gate removed before returning.
    unsafe { std::env::set_var("ATO_FC_UFFD", "cas") };
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: overlay.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };
    let r = r.expect("restore (uffd cas, trace)");

    let trace = read_hotset_trace(&overlay);
    assert!(!trace.entries.is_empty(), "fault trace recorded");
    let pre = trace.entries.iter().filter(|e| e.phase == "pre_health").count();
    assert!(pre > 0, "pre-health faults recorded (the hotset)");
    assert!(trace.entries.iter().all(|e| e.page_gpa % 4096 == 0), "page-aligned GPAs");
    assert!(trace.entries.iter().all(|e| e.source == "demand"), "U3 is all demand (no prefetch yet)");
    // The receipt's pre_health_pages == distinct pre-health page GPAs in the trace.
    let rec = read_uffd_receipt(&overlay);
    assert_eq!(rec.pre_health_pages, Some(trace.pre_health_pages()), "receipt agrees with trace");
    assert!(rec.pre_health_pages.unwrap() > 0);
    eprintln!(
        "### U3-RECEIPT trace_entries={} pre_health_pages={:?} {}",
        trace.entries.len(),
        rec.pre_health_pages,
        serde_json::to_string(&rec).unwrap()
    );

    b.stop(r.session).expect("stop");
    assert_clean_teardown(&overlay);
}

/// U4 (#857): build a HotsetProfile from a demand-only restore's trace, then prefetch
/// it on a second restore — the prefetched hotset cuts demand faults in the
/// pre-health window. Compares demand-only vs hotset-prefetch.
#[test]
#[ignore]
fn fc_kvm_uffd_hotset_prefetch_cuts_demand_faults() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd hotset needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;

    // Run 1: demand-only (cas) → trace → profile.
    let ov1 = dir.path().join("ov1");
    unsafe { std::env::set_var("ATO_FC_UFFD", "cas") };
    let r1 = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: ov1.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };
    let r1 = r1.expect("restore1 (demand)");
    let rec1 = read_uffd_receipt(&ov1);
    let trace = read_hotset_trace(&ov1);
    let profile = crate::uffd_page_server::HotsetProfile::from_trace(&trace);
    assert!(!profile.offsets.is_empty(), "hotset profile built from trace");
    let profile_path = dir.path().join("hotset.json");
    std::fs::write(&profile_path, serde_json::to_string(&profile).unwrap()).unwrap();
    b.stop(r1.session).expect("stop1");

    // Run 2: cas + hotset prefetch.
    let ov2 = dir.path().join("ov2");
    unsafe {
        std::env::set_var("ATO_FC_UFFD", "cas");
        std::env::set_var("ATO_FC_UFFD_HOTSET", &profile_path);
    }
    let r2 = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: ov2.clone(), host_runner_class: None });
    unsafe {
        std::env::remove_var("ATO_FC_UFFD");
        std::env::remove_var("ATO_FC_UFFD_HOTSET");
    }
    let r2 = r2.expect("restore2 (prefetch)");
    let rec2 = read_uffd_receipt(&ov2);

    assert!(rec2.prefetch_pages > 0, "prefetched the hotset: {}", rec2.prefetch_pages);
    assert!(rec2.vm_reaches_health, "prefetch run reaches health");
    // The prefetched hotset cuts demand faults in the latency window.
    assert!(
        rec2.page_fault_count < rec1.page_fault_count / 2,
        "prefetch cut demand faults: prefetch-run demand={} vs demand-only baseline={}",
        rec2.page_fault_count,
        rec1.page_fault_count
    );
    eprintln!(
        "### U4-COMPARE demand_only={{faults:{},health_ms:{:?}}} prefetch={{prefetch:{},demand:{},health_ms:{:?}}}",
        rec1.page_fault_count, rec1.time_to_health_ms,
        rec2.prefetch_pages, rec2.page_fault_count, rec2.time_to_health_ms
    );

    b.stop(r2.session).expect("stop2");
    assert_clean_teardown(&ov2);
}

/// U5 (#858): a corrupt/missing memory chunk in CAS must FAIL CLOSED — the page-
/// server's read_range fails the hash check, so the guest can never fault its memory
/// in; restore returns Err (fast, not a full-timeout hang) and leaves no orphan
/// firecracker/tap. Never silently boots a VM on corrupt memory.
#[test]
#[ignore]
fn fc_kvm_uffd_corrupt_cas_chunk_fails_closed() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd fail-closed needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    // Corrupt EVERY memory chunk in CAS (wrong bytes → blake3 mismatch on read), so
    // the guest's first memory fault hits a bad chunk and the serve fails closed.
    let mem = m.layers.memory.as_ref().expect("memory layer");
    let blobs = dir.path().join("cas").join("blobs").join("blake3");
    for c in &mem.chunks {
        std::fs::write(blobs.join(c.hash.hex()), b"CORRUPT-NOT-THE-REAL-CHUNK").unwrap();
    }
    let overlay = dir.path().join("ov");
    let started = std::time::Instant::now();
    unsafe { std::env::set_var("ATO_FC_UFFD", "cas") };
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: overlay.clone(), host_runner_class: None });
    unsafe { std::env::remove_var("ATO_FC_UFFD") };

    assert!(r.is_err(), "corrupt CAS memory chunk must fail closed, got Ok");
    eprintln!("### U5 fail-closed in {}ms: {}", started.elapsed().as_millis(), r.unwrap_err());
    // No orphan VM/tap left behind by the failed restore.
    let tap = FirecrackerConfig::default().tap_dev;
    let taps = std::process::Command::new("ip").args(["link", "show", &tap]).output().unwrap();
    assert!(!taps.status.success(), "tap {tap} leaked after failed restore");
    let out = std::process::Command::new("pgrep").args(["-af", "firecracker --api-sock"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).count(),
        0,
        "orphan firecracker after failed restore"
    );
}

/// U6 (#859): the page-server reads memory chunks **through a remote CAS** on a local
/// miss (fetch + cache local, then serve) — demand-only, so only the working set
/// crosses the "network". Local store starts WITHOUT the memory chunks; the VM still
/// reaches /health, and the local store re-gains only the faulted working set.
#[test]
#[ignore]
fn fc_kvm_uffd_remote_readthrough_reaches_health() {
    let Some((b, rootfs)) = skip() else { return };
    if std::env::consts::ARCH != "x86_64" || !crate::uffd::host_userfaultfd_present() {
        eprintln!("SKIP: uffd remote read-through needs x86_64 + userfaultfd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Build into the REMOTE store; the local store gets only rootfs + vmstate, so the
    // guest's memory must be read through from remote on demand.
    let remote = CasStore::open(dir.path().join("remote")).unwrap();
    let m = b.build_ready_state(build_input(&remote, rootfs, vec![])).expect("build").manifest;
    let mem = m.layers.memory.as_ref().expect("memory layer");
    let store = CasStore::open(dir.path().join("local")).unwrap();
    for layer in [m.layers.rootfs.as_ref(), m.layers.vmstate.as_ref()].into_iter().flatten() {
        for c in &layer.chunks {
            let bytes = remote.get_chunk(&c.hash).unwrap();
            store.put_chunk(&bytes).unwrap(); // idempotent (content-addressed)
        }
    }
    let local_mem_before = mem.chunks.iter().filter(|c| store.has_chunk(&c.hash)).count();
    assert!(local_mem_before < mem.chunks.len(), "most memory chunks are NOT local (came from remote)");

    let overlay = dir.path().join("ov");
    unsafe {
        std::env::set_var("ATO_FC_UFFD", "cas");
        std::env::set_var("ATO_FC_UFFD_REMOTE", dir.path().join("remote"));
    }
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: overlay.clone(), host_runner_class: None });
    unsafe {
        std::env::remove_var("ATO_FC_UFFD");
        std::env::remove_var("ATO_FC_UFFD_REMOTE");
    }
    let r = r.expect("restore (uffd cas + remote read-through)");

    let rec = read_uffd_receipt(&overlay);
    assert!(rec.vm_reaches_health, "remote read-through reaches health");
    // Read-through proof: local gained the working-set memory chunks (beyond any
    // shared with rootfs/vmstate), but NOT all of them (demand-only — only the touched
    // chunks crossed the "network").
    let local_mem_after = mem.chunks.iter().filter(|c| store.has_chunk(&c.hash)).count();
    assert!(local_mem_after > local_mem_before, "read-through cached working-set chunks locally");
    assert!(local_mem_after < mem.chunks.len(), "demand-only: not the whole memory image was fetched");
    eprintln!(
        "### U6 read-through: local_mem_chunks {}/{} fetched, health_ms={:?}",
        local_mem_after, mem.chunks.len(), rec.time_to_health_ms
    );

    b.stop(r.session).expect("stop");
    assert_clean_teardown(&overlay);
}

#[test]
#[ignore]
fn fc_kvm_rootfs_is_read_only_shared_across_restores() {
    // Disk-leak safety by construction: the rootfs drive is read-only and at a
    // stable content-addressed path shared by every restore, so no disk mutation
    // can leak between sessions. Assert the config + that the same rootfs path is
    // reused (not rewritten) across two restores.
    let Some((b, rootfs)) = skip() else { return };
    if !FirecrackerConfig::default().rootfs_read_only {
        eprintln!("SKIP: ATO_FC_ROOTFS_READONLY=0 (rw mode rewrites the rootfs per restore — leak-safe by fresh copy, not by sharing)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs.clone(), vec![])).expect("build").manifest;
    let id_hex = m.layers.rootfs.as_ref().unwrap().id().hex().to_string();
    let stable = FirecrackerConfig::default().work_root.join("rootfs").join(format!("{id_hex}.ext4"));
    let r1 = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: dir.path().join("ov1"), host_runner_class: None }).expect("restore1");
    let mtime1 = std::fs::metadata(&stable).unwrap().modified().unwrap();
    b.stop(r1.session).expect("stop1");
    let r2 = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov2"), host_runner_class: None }).expect("restore2");
    let mtime2 = std::fs::metadata(&stable).unwrap().modified().unwrap();
    b.stop(r2.session).expect("stop2");
    assert_eq!(mtime1, mtime2, "read-only rootfs was rewritten between restores (should be shared immutable)");
}

#[test]
#[ignore]
fn fc_kvm_restore_latency_20x() {
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let mut lat = Vec::new();
    for i in 0..20 {
        let ov = dir.path().join(format!("ov{i}"));
        let start = std::time::Instant::now();
        let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: ov, host_runner_class: None }).expect("restore");
        lat.push(start.elapsed().as_millis());
        b.stop(r.session).expect("stop");
    }
    lat.sort_unstable();
    let p95 = lat[(lat.len() * 95 / 100).min(lat.len() - 1)];
    eprintln!("restore latency ms: min={} median={} p95={} max={}", lat[0], lat[lat.len()/2], p95, lat[lat.len()-1]);
    assert!(p95 < 3000, "restore p95 {p95}ms exceeds 3s SLO");
}

#[test]
#[ignore]
fn fc_kvm_runner_class_mismatch_fails_closed() {
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let wrong = capsule::foundation::install_lifecycle::RunnerClassId::from_hash("blake3:deliberately-wrong-class");
    let err = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov"), host_runner_class: Some(wrong) }).unwrap_err();
    assert!(matches!(err, SnapshotError::RunnerClassMismatch(_)), "expected mismatch, got {err:?}");
}

#[test]
#[ignore]
fn fc_kvm_state_leak_regression() {
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let gip = FirecrackerConfig::default().guest_ip;
    let port = FirecrackerConfig::default().healthcheck_port;
    // restore #1: marker empty, then set it
    let r1 = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: dir.path().join("ov1"), host_runner_class: None }).expect("restore1");
    let before = http_get(&gip, port, "/marker");
    http_post(&gip, port, "/marker", "leak-sentinel-12345");
    let after = http_get(&gip, port, "/marker");
    b.stop(r1.session).expect("stop1");
    // restore #2 fresh from same snapshot: marker must be empty again
    let r2 = b.restore(RestoreReadyStateInput { store: &store, manifest: m, overlay_root: dir.path().join("ov2"), host_runner_class: None }).expect("restore2");
    let fresh = http_get(&gip, port, "/marker");
    b.stop(r2.session).expect("stop2");
    assert_eq!(before, "", "fresh restore #1 marker not empty");
    assert_eq!(after, "leak-sentinel-12345", "marker set did not take");
    assert_eq!(fresh, "", "STATE LEAK: marker survived to fresh restore");
}

#[test]
#[ignore]
fn fc_kvm_no_secret_invariant() {
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let receipt = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build");
    let m = receipt.manifest.clone();
    let gip = FirecrackerConfig::default().guest_ip;
    let port = FirecrackerConfig::default().healthcheck_port;
    let sentinel = format!("FC-RUNTIME-SECRET-{}", std::process::id());
    let r = b.restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: dir.path().join("ov"), host_runner_class: None }).expect("restore");
    http_post(&gip, port, "/secret", &sentinel);
    assert_eq!(http_get(&gip, port, "/secret"), sentinel, "post-restore injection did not take");
    b.stop(r.session).expect("stop");
    // The sealed memory/vmstate were captured BEFORE the secret existed → absent.
    let mem = LazyBlobReader::new(&store, m.layers.memory.as_ref().unwrap()).read_all().unwrap();
    let vmstate = LazyBlobReader::new(&store, m.layers.vmstate.as_ref().unwrap()).read_all().unwrap();
    let needle = sentinel.as_bytes();
    assert!(!mem.windows(needle.len()).any(|w| w == needle), "secret leaked into sealed memory");
    assert!(!vmstate.windows(needle.len()).any(|w| w == needle), "secret leaked into sealed vmstate");
}

#[test]
#[ignore]
fn fc_kvm_cross_process_stop_via_record() {
    // Phase 7: a restored session must survive its restoring backend being dropped
    // (the firecracker child is detached → reparents to init), and a FRESH backend
    // (empty in-memory registry, like a separate `ato stop` process) must reap it
    // purely from overlay_root/.fc-session.json (recorded pid + tap).
    let Some((b, rootfs)) = skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path().join("cas")).unwrap();
    let m = b.build_ready_state(build_input(&store, rootfs, vec![])).expect("build").manifest;
    let overlay = dir.path().join("ov");
    let gip = FirecrackerConfig::default().guest_ip;
    let port = FirecrackerConfig::default().healthcheck_port;

    // Process A: restore, then DROP backend A. Its sessions map drops, but a std
    // Child drop does NOT kill the process — the detached VM keeps serving.
    let session = {
        let backend_a = FirecrackerBackend::new();
        let r = backend_a
            .restore(RestoreReadyStateInput { store: &store, manifest: m.clone(), overlay_root: overlay.clone(), host_runner_class: None })
            .expect("restore");
        r.session
    };
    assert!(overlay.join(".fc-session.json").exists(), "session record written");
    assert!(!http_get(&gip, port, "/health").is_empty(), "VM still serving after backend A dropped");

    // Process B: a FRESH backend reaps from the on-disk record (empty registry).
    let backend_b = FirecrackerBackend::new();
    let td = backend_b.stop(session).expect("cross-process stop");
    assert!(td.overlay_removed, "overlay removed");
    assert!(!overlay.exists(), "overlay dir gone");

    let tap = FirecrackerConfig::default().tap_dev;
    let taps = std::process::Command::new("ip").args(["link", "show", &tap]).output().unwrap();
    assert!(!taps.status.success(), "tap {tap} still present after cross-process stop");
    let out = std::process::Command::new("pgrep").args(["-af", "firecracker --api-sock"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).count(),
        0,
        "orphan firecracker after cross-process stop"
    );
}

// minimal HTTP helpers (guest is on the tap network)
fn http_get(ip: &str, port: u16, path: &str) -> String {
    http_req(ip, port, "GET", path, None)
}
fn http_post(ip: &str, port: u16, path: &str, body: &str) -> String {
    http_req(ip, port, "POST", path, Some(body))
}
fn http_req(ip: &str, port: u16, method: &str, path: &str, body: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect((ip, port)).expect("connect guest");
    s.set_read_timeout(Some(std::time::Duration::from_secs(3))).ok();
    let body = body.unwrap_or("");
    let req = format!("{method} {path} HTTP/1.0\r\nHost: {ip}\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}
