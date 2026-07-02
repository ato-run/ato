//! Ready-State Snapshot latency benchmark (dev/KVM-host tool).
//!
//! Decomposes build-time and run-time latency for one rootfs image, separating
//! raw Firecracker time (boot/snapshot/load/health) from Ato overhead
//! (store/scan/seal/rehydrate/cache), across cache modes. Emits raw JSONL, a
//! Markdown summary, and a receipt with host facts under `--out/<target>/`.
//!
//! Requires `/dev/kvm` + the M0 stack. One command, e.g.:
//! ```sh
//! sudo -E env ATO_READY_STATE_BENCH=1 ATO_FC_BIN=$PWD/firecracker \
//!   ATO_FC_KERNEL=$PWD/vmlinux ATO_FC_ROOTFS_READONLY=0 ATO_FC_WORK=/tmp/ato-fc-bench \
//!   cargo run -p snapshot --release --bin ready-state-bench -- \
//!     --rootfs $PWD/rootfs.ext4 --target tiny-http --build-runs 5 --restore-runs 30 \
//!     --out benchmarks/ready-state
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use capsulefs::CasStore;
use serde_json::{json, Value};
use snapshot::{
    bench, BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract,
    RestoreReadyStateInput, SanitizerContract, SnapshotBackend,
};

struct Args {
    rootfs: PathBuf,
    target: String,
    build_runs: usize,
    restore_runs: usize,
    out: PathBuf,
}

fn parse_args() -> Args {
    let mut a = Args {
        rootfs: PathBuf::new(),
        target: "unnamed".to_string(),
        build_runs: 5,
        restore_runs: 30,
        out: PathBuf::from("benchmarks/ready-state"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_default();
        match flag.as_str() {
            "--rootfs" => a.rootfs = PathBuf::from(val()),
            "--target" => a.target = val(),
            "--build-runs" => a.build_runs = val().parse().unwrap_or(5),
            "--restore-runs" => a.restore_runs = val().parse().unwrap_or(30),
            "--out" => a.out = PathBuf::from(val()),
            other => eprintln!("warning: ignoring unknown arg {other}"),
        }
    }
    a
}

fn work_root() -> PathBuf {
    std::env::var("ATO_FC_WORK").ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/ato-fc"))
}

/// Clear the content-addressed layer caches so the next restore is a cold-cache run.
fn clear_layer_cache() {
    let wr = work_root();
    for d in ["mem", "vmstate", "rootfs"] {
        let _ = std::fs::remove_dir_all(wr.join(d));
    }
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn host_facts() -> Value {
    let cpu = sh("sh", &["-c", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2"]);
    let gcp = |k: &str| sh("sh", &["-c", &format!("curl -s -m1 -H 'Metadata-Flavor: Google' http://metadata.google.internal/computeMetadata/v1/instance/{k} 2>/dev/null")]);
    let machine = gcp("machine-type");
    let cgroup_v2 = Path::new("/sys/fs/cgroup/cgroup.controllers").exists();
    json!({
        "cpu_model": cpu.trim(),
        "cpu_platform": gcp("cpu-platform"),
        "machine_type": machine.rsplit('/').next().unwrap_or("").to_string(),
        "arch": std::env::consts::ARCH,
        "kernel": sh("uname", &["-r"]),
        "dev_kvm": Path::new("/dev/kvm").exists(),
        "firecracker_version": std::env::var("ATO_FC_BIN").ok().map(|b| sh(&b, &["--version"]).lines().next().unwrap_or("").to_string()),
        "disk_root_rota": sh("sh", &["-c", "lsblk -ndo ROTA $(findmnt -no SOURCE / 2>/dev/null) 2>/dev/null | head -1"]),
        "cgroup_version": if cgroup_v2 { 2 } else { 1 },
        "rootfs_read_only": std::env::var("ATO_FC_ROOTFS_READONLY").map(|v| v != "0").unwrap_or(true),
    })
}

/// min / median / p90 / p95 / max over a slice of millis.
fn stats(samples: &[f64]) -> Value {
    if samples.is_empty() {
        return json!({"n": 0});
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| s[((s.len() as f64 * p).ceil() as usize).clamp(1, s.len()) - 1];
    json!({
        "n": s.len(),
        "min": s[0],
        "median": pct(0.50),
        "p90": pct(0.90),
        "p95": pct(0.95),
        "max": s[s.len() - 1],
    })
}

fn build_input<'a>(store: &'a CasStore, rootfs: &[u8]) -> BuildReadyStateInput<'a> {
    BuildReadyStateInput {
        store,
        capsule_manifest_hash: "blake3:bench".to_string(),
        runner_class: None,
        layers: BuildLayers {
            rootfs: rootfs.to_vec(),
            runtime: None,
            dependency: None,
            app: None,
            vmstate: Vec::new(),
            memory: Vec::new(),
        },
        restore_contract: RestoreContract { ports: vec![8080], healthcheck: Some("/health".into()), expected_ready_ms: Some(3000) },
        sanitizer_contract: SanitizerContract::default(),
        declared_secret_markers: vec![],
        execution_id: None,
    }
}

/// Record one (phase, mode, run) row: total ms + per-span ms.
fn row(phase: &str, mode: &str, idx: usize, total_ms: f64, spans: &[bench::Span], extra: Value) -> Value {
    let mut span_obj = serde_json::Map::new();
    for s in spans {
        *span_obj.entry(s.name).or_insert(json!(0.0)) = json!(span_obj.get(s.name).and_then(|v| v.as_f64()).unwrap_or(0.0) + s.micros as f64 / 1000.0);
    }
    let mut o = json!({ "phase": phase, "mode": mode, "run": idx, "total_ms": total_ms, "spans": Value::Object(span_obj) });
    if let (Value::Object(m), Value::Object(e)) = (&mut o, &extra) {
        for (k, v) in e { m.insert(k.clone(), v.clone()); }
    }
    o
}

fn main() {
    let args = parse_args();
    if !bench::is_enabled() {
        eprintln!("ERROR: set ATO_READY_STATE_BENCH=1 so latency spans are recorded.");
        std::process::exit(2);
    }
    if !FirecrackerBackend::kvm_present() {
        eprintln!("SKIP: /dev/kvm absent — benchmark requires a KVM host.");
        std::process::exit(0);
    }
    let rootfs = std::fs::read(&args.rootfs).expect("read --rootfs");
    let backend = FirecrackerBackend::new();
    let probe = backend.probe();
    if !probe.available {
        eprintln!("SKIP: firecracker unavailable: {:?}", probe.reason);
        std::process::exit(0);
    }

    let out_dir = args.out.join(&args.target);
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let mut raw: Vec<Value> = Vec::new();

    eprintln!("[bench] target={} rootfs={} build_runs={} restore_runs={}", args.target, rootfs.len(), args.build_runs, args.restore_runs);

    // ── BUILD phase: N seals, keep the last store+manifest for restore. ──────
    let scratch = std::env::temp_dir().join(format!("ato-rs-bench-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let mut last: Option<(PathBuf, CasStore, snapshot::ReadyStateManifest)> = None;
    let mut build_totals = Vec::new();
    let mut sealed_bytes = json!({});
    'builds: for i in 0..args.build_runs {
        let dir = scratch.join(format!("build-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store = CasStore::open(dir.join("cas")).unwrap();
        // Builds boot+snapshot a real VM; the VMM occasionally hiccups
        // (e.g. snapshot/create returning HTTP 0). Retry rather than aborting the
        // whole benchmark, and skip the run if it never succeeds.
        let mut attempt = 0;
        let (total, receipt) = loop {
            let _ = bench::drain();
            let t = Instant::now();
            match backend.build_ready_state(build_input(&store, &rootfs)) {
                Ok(r) => break (t.elapsed().as_secs_f64() * 1000.0, r),
                Err(e) if attempt < 2 => {
                    attempt += 1;
                    eprintln!("[build {i}] retry {attempt} after error: {e}");
                    clear_layer_cache();
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(e) => {
                    eprintln!("[build {i}] FAILED after retries: {e}");
                    raw.push(json!({"phase": "build", "run": i, "error": e.to_string()}));
                    continue 'builds;
                }
            }
        };
        let spans = bench::drain();
        let m = &receipt.manifest;
        let bytes = |b: &Option<capsulefs::BlobManifest>| b.as_ref().map(|x| x.total_len).unwrap_or(0);
        sealed_bytes = json!({
            "rootfs": bytes(&m.layers.rootfs), "memory": bytes(&m.layers.memory),
            "vmstate": bytes(&m.layers.vmstate), "total": m.layers.iter().map(|(_, x)| x.total_len).sum::<u64>(),
        });
        raw.push(row("build", "-", i, total, &spans, json!({"sealed_bytes": sealed_bytes.clone()})));
        build_totals.push(total);
        eprintln!("[build {i}] {total:.0}ms");
        last = Some((dir, store, receipt.manifest));
    }
    let Some((keep_dir, store, manifest)) = last else {
        eprintln!("[bench] all builds failed — writing build-only receipt, skipping restore");
        let receipt = json!({
            "target": args.target, "host": host_facts(),
            "config": {"build_runs": args.build_runs, "restore_runs": args.restore_runs, "rootfs_input_bytes": rootfs.len()},
            "error": "all builds failed",
        });
        std::fs::write(out_dir.join("receipt.json"), serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        std::fs::write(out_dir.join("raw.jsonl"), raw.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let _ = std::fs::remove_dir_all(&scratch);
        std::process::exit(1);
    };

    // ── RESTORE phase: cold-cache then warm-cache. ───────────────────────────
    let mut restore_summ = serde_json::Map::new();
    for mode in ["cold-cache", "warm-cache"] {
        let mut totals = Vec::new();
        if mode == "warm-cache" {
            // prime the cache once (not counted)
            clear_layer_cache();
            let ov = keep_dir.join("prime");
            if let Ok(r) = backend.restore(RestoreReadyStateInput { store: &store, manifest: manifest.clone(), overlay_root: ov, host_runner_class: None, uffd_preview: false }) {
                let _ = backend.stop(r.session);
            }
            let _ = bench::drain();
        }
        for i in 0..args.restore_runs {
            if mode == "cold-cache" {
                clear_layer_cache();
            }
            let ov = keep_dir.join(format!("ov-{mode}-{i}"));
            let _ = bench::drain();
            let t = Instant::now();
            let r = match backend.restore(RestoreReadyStateInput { store: &store, manifest: manifest.clone(), overlay_root: ov, host_runner_class: None, uffd_preview: false }) {
                Ok(r) => r,
                Err(e) => { raw.push(json!({"phase":"restore","mode":mode,"run":i,"error": e.to_string()})); continue; }
            };
            let total = t.elapsed().as_secs_f64() * 1000.0;
            let spans = bench::drain();
            let restored = r.session.restored_bytes;
            let st = Instant::now();
            let _ = backend.stop(r.session);
            let stop_ms = st.elapsed().as_secs_f64() * 1000.0;
            raw.push(row("restore", mode, i, total, &spans, json!({"restored_bytes": restored, "stop_ms": stop_ms})));
            totals.push(total);
            if i % 5 == 0 { eprintln!("[restore {mode} {i}] {total:.0}ms"); }
        }
        restore_summ.insert(mode.to_string(), stats(&totals));
    }

    // ── Persist raw JSONL + receipt + markdown. ──────────────────────────────
    let jsonl: String = raw.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(out_dir.join("raw.jsonl"), jsonl).unwrap();

    let receipt = json!({
        "target": args.target,
        "host": host_facts(),
        "config": {"build_runs": args.build_runs, "restore_runs": args.restore_runs, "rootfs_input_bytes": rootfs.len()},
        "sealed_bytes": sealed_bytes,
        "build_total_ms": stats(&build_totals),
        "restore_total_ms": Value::Object(restore_summ.clone()),
    });
    std::fs::write(out_dir.join("receipt.json"), serde_json::to_string_pretty(&receipt).unwrap()).unwrap();

    let md = render_markdown(&args.target, &receipt);
    std::fs::write(out_dir.join("summary.md"), &md).unwrap();
    println!("{md}");
    eprintln!("[bench] wrote {}/{{raw.jsonl,receipt.json,summary.md}}", out_dir.display());
    let _ = std::fs::remove_dir_all(&scratch);
}

fn jstr(receipt: &Value, path: &[&str]) -> String {
    let mut cur = receipt;
    for p in path {
        match cur.get(p) {
            Some(v) => cur = v,
            None => return "-".to_string(),
        }
    }
    match cur {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "-".to_string(),
        other => other.to_string(),
    }
}

fn mb_cell(receipt: &Value, key: &str) -> String {
    receipt
        .get("sealed_bytes")
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_u64())
        .map(|b| format!("{:.1} MB", b as f64 / 1_048_576.0))
        .unwrap_or_else(|| "-".to_string())
}

fn stat_row(label: &str, receipt: &Value, base: &[&str]) -> String {
    let cell = |k: &str| -> String {
        let mut p = base.to_vec();
        p.push(k);
        jstr(receipt, &p)
    };
    format!(
        "| {} | {} | {} | {} | {} | {} |\n",
        label, cell("min"), cell("median"), cell("p90"), cell("p95"), cell("max")
    )
}

fn render_markdown(target: &str, receipt: &Value) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Ready-State latency — {target}\n\n"));
    s.push_str(&format!(
        "Host: {} / {} / {} / kernel {} / fc {} / cgroup v{} / rootfs_ro={}\n\n",
        jstr(receipt, &["host", "cpu_platform"]),
        jstr(receipt, &["host", "machine_type"]),
        jstr(receipt, &["host", "arch"]),
        jstr(receipt, &["host", "kernel"]),
        jstr(receipt, &["host", "firecracker_version"]),
        jstr(receipt, &["host", "cgroup_version"]),
        jstr(receipt, &["host", "rootfs_read_only"]),
    ));
    s.push_str(&format!(
        "Sealed: rootfs {} · memory {} · vmstate {} · total {}\n\n",
        mb_cell(receipt, "rootfs"),
        mb_cell(receipt, "memory"),
        mb_cell(receipt, "vmstate"),
        mb_cell(receipt, "total"),
    ));
    s.push_str("| metric (ms) | min | median | p90 | p95 | max |\n|---|---|---|---|---|---|\n");
    s.push_str(&stat_row("build total", receipt, &["build_total_ms"]));
    s.push_str(&stat_row("restore cold-cache", receipt, &["restore_total_ms", "cold-cache"]));
    s.push_str(&stat_row("restore warm-cache", receipt, &["restore_total_ms", "warm-cache"]));
    s.push_str("\nSee `raw.jsonl` for per-run span decomposition (Firecracker vs Ato overhead).\n");
    s
}
