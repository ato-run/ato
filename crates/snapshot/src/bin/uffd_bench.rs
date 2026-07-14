//! U9 (#876): Ready-State File-vs-UFFD benchmark harness.
//!
//! Builds one sealed capsule from a rootfs, then restores it N times in each
//! `mem_backend` mode — **file-cold**, **file-warm**, **uffd-local**,
//! **uffd-hotset**, **uffd-remote** — measuring restore wall time and (for UFFD
//! modes) the U8 `.uffd-receipt.json` metrics. Emits JSON + a markdown table.
//!
//! ```sh
//! sudo -E env ATO_READY_STATE_BENCH=1 ATO_FC_BIN=$PWD/firecracker \
//!   ATO_FC_KERNEL=$PWD/vmlinux ATO_FC_ROOTFS_READONLY=1 \
//!   ATO_FC_WORK=/tmp/ato-fc-bench ATO_FC_BOOT_TIMEOUT_S=60 \
//!   uffd_bench --rootfs app.ext4 --app tiny-http --iterations 5 --out results
//! ```
//! Only the bench sets the UFFD env; `ato run` is never affected. No-binding
//! capsules only.

use std::path::{Path, PathBuf};
use std::time::Instant;

use capsulefs::CasStore;
use serde_json::{Value, json};
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract, RestoreReadyStateInput,
    SanitizerContract, SnapshotBackend, bench,
};

struct Args {
    rootfs: PathBuf,
    app: String,
    iterations: usize,
    out: PathBuf,
}

fn parse_args() -> Args {
    let mut a = Args {
        rootfs: PathBuf::new(),
        app: "app".into(),
        iterations: 5,
        out: PathBuf::from("uffd-bench-out"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().expect("missing value");
        match k.as_str() {
            "--rootfs" => a.rootfs = PathBuf::from(v()),
            "--app" => a.app = v(),
            "--iterations" => a.iterations = v().parse().unwrap_or(5),
            "--out" => a.out = PathBuf::from(v()),
            _ => {}
        }
    }
    a
}

fn build_input<'a>(store: &'a CasStore, rootfs: &[u8]) -> BuildReadyStateInput<'a> {
    BuildReadyStateInput {
        store,
        capsule_manifest_hash: "blake3:uffd-bench".to_string(),
        runner_class: None,
        surface_requirement: None,
        layers: BuildLayers {
            rootfs: rootfs.to_vec(),
            runtime: None,
            dependency: None,
            app: None,
            vmstate: Vec::new(),
            memory: Vec::new(),
        },
        restore_contract: RestoreContract {
            ports: vec![8080],
            healthcheck: Some("/health".into()),
            expected_ready_ms: Some(5000),
            ..Default::default()
        },
        sanitizer_contract: SanitizerContract::default(),
        declared_secret_markers: vec![],
        execution_id: None,
        supervisor: None,
    }
}

/// Clear ONLY the materialized memory cache so a File restore re-rehydrates the
/// image (the mem_backend cost we compare); the ro-shared rootfs + small vmstate
/// stay warm so the comparison isolates the memory path, not rootfs I/O. UFFD modes
/// never materialize `.mem`, so this is a no-op for them.
fn clear_mem_cache() {
    let work = std::env::var("ATO_FC_WORK").unwrap_or_else(|_| "/tmp/ato-fc".into());
    let _ = std::fs::remove_dir_all(Path::new(&work).join("mem"));
}

fn stats(v: &[f64]) -> Value {
    if v.is_empty() {
        return json!({"n": 0});
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| {
        s[((s.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(s.len() - 1)]
    };
    json!({"n": s.len(), "min": s[0], "median": pct(0.5), "p95": pct(0.95), "max": s[s.len()-1]})
}

/// Read `.uffd-receipt.json` fields (U8 schema) generically.
fn receipt_num(overlay: &Path, key: &str) -> Option<f64> {
    let text = std::fs::read_to_string(overlay.join(".uffd-receipt.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get(key).and_then(|x| x.as_f64())
}

/// Build a hotset profile from a prior restore's `.hotset-trace.json`: the distinct
/// pre-health file offsets in first-touch order. Written as `{"offsets":[...]}`.
fn build_hotset_profile(overlay: &Path, out: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(overlay.join(".hotset-trace.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(entries) = v.get("entries").and_then(|e| e.as_array()) else {
        return false;
    };
    let mut pre: Vec<(&Value, f64)> = entries
        .iter()
        .filter(|e| e.get("phase").and_then(|p| p.as_str()) == Some("pre_health"))
        .map(|e| {
            (
                e,
                e.get("first_fault_at_us")
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
            )
        })
        .collect();
    pre.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let mut seen = std::collections::HashSet::new();
    let mut offsets = Vec::new();
    for (e, _) in pre {
        if let Some(off) = e.get("file_offset").and_then(|x| x.as_u64())
            && seen.insert(off)
        {
            offsets.push(off);
        }
    }
    if offsets.is_empty() {
        return false;
    }
    std::fs::write(
        out,
        serde_json::to_string(&json!({"offsets": offsets})).unwrap(),
    )
    .is_ok()
}

/// Restore `iterations` times in `mode` from `store`, cold cache each time. Returns
/// the per-mode summary + raw rows.
fn run_mode(
    backend: &FirecrackerBackend,
    store: &CasStore,
    manifest: &snapshot::ReadyStateManifest,
    mode: &str,
    iterations: usize,
    dir: &Path,
    hotset_profile: Option<&Path>,
) -> (Value, Vec<Value>) {
    // SAFETY: single-threaded bench; the bench sets UFFD env, never product code.
    unsafe {
        std::env::remove_var("ATO_FC_UFFD");
        std::env::remove_var("ATO_FC_UFFD_HOTSET");
        // NOTE: ATO_FC_UFFD_REMOTE is owned by main() (set only around uffd-remote);
        // run_mode must NOT clear it or the remote read-through never engages.
        match mode {
            "file-cold" | "file-warm" => {}
            "uffd-local" => std::env::set_var("ATO_FC_UFFD", "cas"),
            "uffd-hotset" => {
                std::env::set_var("ATO_FC_UFFD", "cas");
                if let Some(p) = hotset_profile {
                    std::env::set_var("ATO_FC_UFFD_HOTSET", p);
                }
            }
            "uffd-remote" => std::env::set_var("ATO_FC_UFFD", "cas"),
            _ => {}
        }
    }
    let mut totals = Vec::new();
    let mut health = Vec::new();
    let mut faults = Vec::new();
    let mut prefetched = Vec::new();
    let mut bytes = Vec::new();
    let mut remote = Vec::new();
    let mut raw = Vec::new();
    for i in 0..iterations {
        if mode != "file-warm" {
            clear_mem_cache();
        }
        let ov = dir.join(format!("ov-{mode}-{i}"));
        let t = Instant::now();
        let r = backend.restore(RestoreReadyStateInput {
            store,
            manifest: manifest.clone(),
            overlay_root: ov.clone(),
            host_runner_class: None,
            uffd_preview: false,
        });
        let total = t.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(r) => {
                totals.push(total);
                if let Some(h) = receipt_num(&ov, "time_to_health_ms") {
                    health.push(h);
                }
                if let Some(f) = receipt_num(&ov, "page_fault_count") {
                    faults.push(f);
                }
                if let Some(p) = receipt_num(&ov, "prefetch_pages") {
                    prefetched.push(p);
                }
                if let Some(b) = receipt_num(&ov, "bytes_copied") {
                    bytes.push(b);
                }
                if let Some(rc) = receipt_num(&ov, "remote_chunks_fetched") {
                    remote.push(rc);
                }
                raw.push(json!({"mode": mode, "run": i, "restore_ms": total}));
                let _ = backend.stop(r.session);
            }
            Err(e) => raw.push(json!({"mode": mode, "run": i, "error": e.to_string()})),
        }
        eprintln!("[{mode} {i}] {total:.0}ms");
    }
    let summ = json!({
        "restore_ms": stats(&totals),
        "time_to_health_ms": stats(&health),
        "page_faults": stats(&faults),
        "prefetched_pages": stats(&prefetched),
        "bytes_copied": stats(&bytes),
        "remote_chunks_fetched": stats(&remote),
    });
    (summ, raw)
}

fn main() {
    let args = parse_args();
    if !bench::is_enabled() {
        eprintln!("ERROR: set ATO_READY_STATE_BENCH=1");
        std::process::exit(2);
    }
    if !FirecrackerBackend::kvm_present() {
        eprintln!("SKIP: /dev/kvm absent");
        std::process::exit(0);
    }
    let rootfs = std::fs::read(&args.rootfs).expect("read --rootfs");
    let backend = FirecrackerBackend::new();
    let dir = std::env::temp_dir().join(format!("uffd-bench-{}", args.app));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = CasStore::open(dir.join("cas")).unwrap();

    // Build once (retry — occasional VMM hiccup).
    let mut manifest = None;
    for attempt in 0..3 {
        match backend.build_ready_state(build_input(&store, &rootfs)) {
            Ok(r) => {
                manifest = Some(r.manifest);
                break;
            }
            Err(e) => eprintln!("[build attempt {attempt}] failed: {e}"),
        }
    }
    let manifest = manifest.expect("build failed 3x");
    let mem = manifest.layers.memory.as_ref().expect("memory layer");
    eprintln!(
        "[bench] app={} mem_bytes={} iterations={}",
        args.app, mem.total_len, args.iterations
    );

    let mut summary = serde_json::Map::new();
    let mut all_raw = Vec::new();

    // Build a hotset profile from one demand run.
    let profile_path = dir.join("hotset.json");
    unsafe {
        std::env::set_var("ATO_FC_UFFD", "cas");
    }
    clear_mem_cache();
    let prof_ov = dir.join("ov-profile");
    let mut has_profile = false;
    if let Ok(r) = backend.restore(RestoreReadyStateInput {
        store: &store,
        manifest: manifest.clone(),
        overlay_root: prof_ov.clone(),
        host_runner_class: None,
        uffd_preview: false,
    }) {
        has_profile = build_hotset_profile(&prof_ov, &profile_path);
        let _ = backend.stop(r.session);
    }
    unsafe {
        std::env::remove_var("ATO_FC_UFFD");
    }

    for mode in [
        "file-cold",
        "file-warm",
        "uffd-local",
        "uffd-hotset",
        "uffd-remote",
    ] {
        // uffd-remote needs a local store WITHOUT memory + a remote WITH memory.
        let (s, raw) = if mode == "uffd-remote" {
            let remote = CasStore::open(dir.join("remote")).unwrap();
            let local = CasStore::open(dir.join("local-no-mem")).unwrap();
            for c in &mem.chunks {
                if let Ok(b) = store.get_chunk(&c.hash) {
                    let _ = remote.put_chunk(&b);
                }
            }
            for layer in [
                manifest.layers.rootfs.as_ref(),
                manifest.layers.vmstate.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                for c in &layer.chunks {
                    if let Ok(b) = store.get_chunk(&c.hash) {
                        let _ = local.put_chunk(&b);
                    }
                }
            }
            unsafe {
                std::env::set_var("ATO_FC_UFFD_REMOTE", dir.join("remote"));
            }
            let r = run_mode(
                &backend,
                &local,
                &manifest,
                mode,
                args.iterations,
                &dir,
                None,
            );
            unsafe {
                std::env::remove_var("ATO_FC_UFFD_REMOTE");
            }
            r
        } else if mode == "file-warm" {
            // prime the cache once, then warm runs.
            clear_mem_cache();
            if let Ok(r) = backend.restore(RestoreReadyStateInput {
                store: &store,
                manifest: manifest.clone(),
                overlay_root: dir.join("ov-warm-prime"),
                host_runner_class: None,
                uffd_preview: false,
            }) {
                let _ = backend.stop(r.session);
            }
            run_mode(
                &backend,
                &store,
                &manifest,
                mode,
                args.iterations,
                &dir,
                None,
            )
        } else {
            run_mode(
                &backend,
                &store,
                &manifest,
                mode,
                args.iterations,
                &dir,
                if has_profile {
                    Some(profile_path.as_path())
                } else {
                    None
                },
            )
        };
        summary.insert(mode.to_string(), s);
        all_raw.extend(raw);
    }

    let report = json!({
        "app": args.app,
        "mem_bytes_total": mem.total_len,
        "iterations": args.iterations,
        "modes": Value::Object(summary.clone()),
    });
    std::fs::create_dir_all(&args.out).unwrap();
    std::fs::write(
        args.out.join(format!("{}.json", args.app)),
        serde_json::to_string_pretty(&json!({"report": report, "raw": all_raw})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        args.out.join(format!("{}.md", args.app)),
        markdown(&args.app, mem.total_len, &summary),
    )
    .unwrap();
    println!("{}", markdown(&args.app, mem.total_len, &summary));
}

fn markdown(app: &str, mem_bytes: u64, summary: &serde_json::Map<String, Value>) -> String {
    let g = |m: &str, metric: &str, stat: &str| -> String {
        summary
            .get(m)
            .and_then(|s| s.get(metric))
            .and_then(|s| s.get(stat))
            .and_then(|v| v.as_f64())
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into())
    };
    let mut s = format!(
        "### {app} ({} MiB memory image)\n\n",
        mem_bytes / 1024 / 1024
    );
    s.push_str("| mode | restore median ms | restore p95 ms | time→health median ms | page faults | prefetched | remote chunks |\n");
    s.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for m in [
        "file-cold",
        "file-warm",
        "uffd-local",
        "uffd-hotset",
        "uffd-remote",
    ] {
        s.push_str(&format!(
            "| {m} | {} | {} | {} | {} | {} | {} |\n",
            g(m, "restore_ms", "median"),
            g(m, "restore_ms", "p95"),
            g(m, "time_to_health_ms", "median"),
            g(m, "page_faults", "median"),
            g(m, "prefetched_pages", "median"),
            g(m, "remote_chunks_fetched", "median"),
        ));
    }
    s
}
