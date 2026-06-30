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
