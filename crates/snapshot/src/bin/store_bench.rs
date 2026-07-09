//! L6 (#912): Store Capsule Snapshot Benchmark Harness.
//!
//! Takes a **capsule list** (approved, no-binding, `capsule.toml`-only public capsules)
//! and, per capsule, runs the real Linux/KVM pipeline —
//! **eligibility → build → boot → verify → snapshot → seal → no-secret scan → restore →
//! healthcheck → stop** — plus a File cold/warm restore benchmark, then writes a
//! structured JSON + Markdown report. **Failures are recorded, never hidden** — the
//! point is to learn how many real Store capsules snapshot cleanly, which fail, and why,
//! to inform the Track C production builder. This is NOT the production builder and it
//! does not write `capsule_snapshots` rows.
//!
//! ```sh
//! sudo -E env ATO_READY_STATE_BENCH=1 ATO_FC_BIN=… ATO_FC_KERNEL=… \
//!   store_bench --capsules capsules.json --iterations 5 --out out/
//! ```
//! `capsules.json`: `[{ "capsule_id": "…", "rootfs": "/path/app.ext4", "healthcheck":
//! "/health", "port": 8080, "target_label": "web" }, …]`. The `rootfs` is a prebuilt
//! bootable ext4 for the capsule; **no client-controlled source_ref is fetched here** —
//! refs are resolved server-side / from an approved store record beforehand.

use std::path::PathBuf;
use std::time::Instant;

use capsulefs::CasStore;
use serde_json::{Value, json};
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, RestoreContract, RestoreReadyStateInput,
    SanitizerContract, SnapshotBackend, bench, no_secret_scan,
};

struct Args {
    capsules: PathBuf,
    iterations: usize,
    out: PathBuf,
}

fn parse_args() -> Args {
    let mut a = Args {
        capsules: PathBuf::new(),
        iterations: 5,
        out: PathBuf::from("store-bench-out"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().expect("missing value");
        match k.as_str() {
            "--capsules" => a.capsules = PathBuf::from(v()),
            "--iterations" => a.iterations = v().parse().unwrap_or(5),
            "--out" => a.out = PathBuf::from(v()),
            _ => {}
        }
    }
    a
}

fn stats(v: &[f64]) -> Value {
    if v.is_empty() {
        return json!({ "n": 0 });
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| {
        s[((s.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(s.len() - 1)]
    };
    json!({ "n": s.len(), "min": s[0], "p50": pct(0.50), "p95": pct(0.95), "max": s[s.len()-1] })
}

fn clear_mem_cache() {
    let work = std::env::var("ATO_FC_WORK").unwrap_or_else(|_| "/tmp/ato-fc".into());
    let _ = std::fs::remove_dir_all(std::path::Path::new(&work).join("mem"));
}

/// Run the full pipeline for one capsule; a failure at any stage is captured in the
/// receipt (`failure_stage` + `failure_reason`), never a panic.
fn run_capsule(
    backend: &FirecrackerBackend,
    spec: &Value,
    iterations: usize,
    dir: &std::path::Path,
) -> Value {
    let id = spec
        .get("capsule_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let hc = spec
        .get("healthcheck")
        .and_then(|v| v.as_str())
        .unwrap_or("/health")
        .to_string();
    let port = spec.get("port").and_then(|v| v.as_u64()).unwrap_or(8080) as u16;
    let mut r = json!({ "capsule_id": id, "eligible": false, "success": false, "failure_stage": Value::Null, "failure_reason": Value::Null });

    // ── eligibility ──────────────────────────────────────────────────────────────
    let rootfs_path = match spec.get("rootfs").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            r["failure_stage"] = json!("eligibility");
            r["failure_reason"] = json!("no rootfs in spec");
            return r;
        }
    };
    let rootfs = match std::fs::read(&rootfs_path) {
        Ok(b) => b,
        Err(e) => {
            r["failure_stage"] = json!("eligibility");
            r["failure_reason"] = json!(format!("read rootfs: {e}"));
            return r;
        }
    };
    r["eligible"] = json!(true);

    let capdir = dir.join(&id);
    let _ = std::fs::remove_dir_all(&capdir);
    let store = CasStore::open(capdir.join("cas")).unwrap();

    // ── build → boot → verify → snapshot → seal ────────────────────────────────────
    let t_build = Instant::now();
    let receipt = match backend.build_ready_state(BuildReadyStateInput {
        store: &store,
        capsule_manifest_hash: format!("blake3:{id}"),
        runner_class: None,
        layers: BuildLayers {
            rootfs,
            runtime: None,
            dependency: None,
            app: None,
            vmstate: Vec::new(),
            memory: Vec::new(),
        },
        restore_contract: RestoreContract {
            ports: vec![port],
            healthcheck: Some(hc.clone()),
            expected_ready_ms: Some(8000),
        },
        sanitizer_contract: SanitizerContract::default(),
        declared_secret_markers: vec![],
        execution_id: None,
        supervisor: None,
    }) {
        Ok(rc) => rc,
        Err(e) => {
            r["failure_stage"] = json!("build");
            r["failure_reason"] = json!(e.to_string());
            return r;
        }
    };
    let build_to_seal_ms = t_build.elapsed().as_millis();
    let manifest = receipt.manifest;

    // ── no-secret scan (build gate + the reusable L4 scanner over the CAS) ──────────
    let no_secret_clean = receipt.no_secret_proof.is_clean()
        && no_secret_scan::scan(
            &no_secret_scan::ScanTargets {
                cas: Some(capdir.join("cas")),
                ..Default::default()
            },
            // no-binding capsules inject no secret; a couple of canary markers keep the
            // gate honest (they must never appear in a sealed artifact).
            &[b"BEGIN PRIVATE KEY", b"AKIA"],
        )
        .clean;

    let sizes = json!({
        "rootfs_bytes": manifest.layers.rootfs.as_ref().map(|m| m.total_len).unwrap_or(0),
        "mem_bytes": manifest.layers.memory.as_ref().map(|m| m.total_len).unwrap_or(0),
        "vmstate_bytes": manifest.layers.vmstate.as_ref().map(|m| m.total_len).unwrap_or(0),
        "cas_chunks": manifest.layers.memory.as_ref().map(|m| m.chunks.len()).unwrap_or(0),
        "artifact_manifest_hash": manifest.id(),
        "runner_class_id": manifest.runner_class_id.as_ref().map(|c| c.to_string()),
    });

    // ── restore benchmark: File cold + File warm ───────────────────────────────────
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut restore_ok = true;
    for i in 0..iterations.max(1) {
        clear_mem_cache();
        let ov = capdir.join(format!("ov-cold-{i}"));
        let t = Instant::now();
        match backend.restore(RestoreReadyStateInput {
            store: &store,
            manifest: manifest.clone(),
            overlay_root: ov,
            host_runner_class: None,
            uffd_preview: false,
        }) {
            Ok(rs) => {
                cold.push(t.elapsed().as_secs_f64() * 1000.0);
                let _ = backend.stop(rs.session);
            }
            Err(e) => {
                r["failure_stage"] = json!("restore");
                r["failure_reason"] = json!(e.to_string());
                restore_ok = false;
                break;
            }
        }
    }
    if restore_ok {
        // prime warm cache, then warm runs.
        if let Ok(rs) = backend.restore(RestoreReadyStateInput {
            store: &store,
            manifest: manifest.clone(),
            overlay_root: capdir.join("ov-warm-prime"),
            host_runner_class: None,
            uffd_preview: false,
        }) {
            let _ = backend.stop(rs.session);
        }
        for i in 0..iterations.max(1) {
            let ov = capdir.join(format!("ov-warm-{i}"));
            let t = Instant::now();
            match backend.restore(RestoreReadyStateInput {
                store: &store,
                manifest: manifest.clone(),
                overlay_root: ov,
                host_runner_class: None,
                uffd_preview: false,
            }) {
                Ok(rs) => {
                    warm.push(t.elapsed().as_secs_f64() * 1000.0);
                    let _ = backend.stop(rs.session);
                }
                Err(_) => break,
            }
        }
    }

    r["success"] = json!(restore_ok && no_secret_clean && !cold.is_empty());
    r["no_secret_scan_clean"] = json!(no_secret_clean);
    r["build_to_seal_ms"] = json!(build_to_seal_ms);
    r["artifact"] = sizes;
    r["benchmark"] = json!({ "file_cold_ms": stats(&cold), "file_warm_ms": stats(&warm) });
    eprintln!(
        "[store-bench] {id}: success={} cold_p50={:?}",
        r["success"],
        stats(&cold).get("p50")
    );
    r
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
    let specs: Vec<Value> =
        serde_json::from_slice(&std::fs::read(&args.capsules).expect("read --capsules"))
            .expect("parse capsule list");
    let backend = FirecrackerBackend::new();
    let dir = std::env::temp_dir().join("store-bench");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let results: Vec<Value> = specs
        .iter()
        .map(|s| run_capsule(&backend, s, args.iterations, &dir))
        .collect();
    let ok = results
        .iter()
        .filter(|r| r["success"].as_bool().unwrap_or(false))
        .count();

    std::fs::create_dir_all(&args.out).unwrap();
    std::fs::write(
        args.out.join("results.json"),
        serde_json::to_string_pretty(
            &json!({ "capsules": results.len(), "succeeded": ok, "results": results }),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(args.out.join("summary.md"), markdown(&results, ok)).unwrap();
    println!("{}", markdown(&results, ok));
    eprintln!(
        "[store-bench] {ok}/{} capsules snapshot+restore succeeded",
        results.len()
    );
}

fn markdown(results: &[Value], ok: usize) -> String {
    let g = |r: &Value, path: &[&str]| -> String {
        let mut v = r;
        for p in path {
            v = v.get(p).unwrap_or(&Value::Null);
        }
        match v {
            Value::Number(n) => format!("{n}"),
            Value::Bool(b) => b.to_string(),
            Value::String(s) => s.clone(),
            _ => "-".into(),
        }
    };
    let mut s = format!(
        "# Store Capsule Snapshot Benchmark\n\n{ok}/{} capsules snapshot + restore succeeded.\n\n",
        results.len()
    );
    s.push_str("| capsule | eligible | success | no-secret | build→seal ms | cold p50 | cold p95 | warm p50 | rootfs MB | mem MB | failure |\n");
    s.push_str("|---|---|---|---|---:|---:|---:|---:|---:|---:|---|\n");
    for r in results {
        let rb = r
            .get("artifact")
            .and_then(|a| a.get("rootfs_bytes"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            / 1024
            / 1024;
        let mb = r
            .get("artifact")
            .and_then(|a| a.get("mem_bytes"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            / 1024
            / 1024;
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {rb} | {mb} | {} |\n",
            g(r, &["capsule_id"]),
            g(r, &["eligible"]),
            g(r, &["success"]),
            g(r, &["no_secret_scan_clean"]),
            g(r, &["build_to_seal_ms"]),
            g(r, &["benchmark", "file_cold_ms", "p50"]),
            g(r, &["benchmark", "file_cold_ms", "p95"]),
            g(r, &["benchmark", "file_warm_ms", "p50"]),
            r.get("failure_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
        ));
    }
    s.push_str("\n## Failure taxonomy\n\n");
    for r in results
        .iter()
        .filter(|r| !r["success"].as_bool().unwrap_or(false))
    {
        s.push_str(&format!(
            "- **{}**: {} — {}\n",
            g(r, &["capsule_id"]),
            g(r, &["failure_stage"]),
            r.get("failure_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("-")
        ));
    }
    s
}
