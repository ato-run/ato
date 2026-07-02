//! `ato runner smoke` — the minimal local Ready-State E2E, no control plane involved:
//!
//! ```text
//! Docker→ext4 rootfs → build_ready_state (boot + /health + seal) → restore →
//! root proxy probe (GET /health == 200) → stop/teardown → orphan diff
//! ```
//!
//! This is the same pipeline the snapshot builder and the runner's restore path run in
//! production (same crates, same backend), against a built-in stdlib-python fixture —
//! so a green smoke means this host can actually build AND serve capsule snapshots,
//! not merely that the binaries exist. Requires root (Firecracker tap setup), KVM and
//! Docker. Honest by construction: every stage reports what actually happened, and the
//! orphan stage diffs host state (firecracker pids, tap devices, loop devices, ato
//! docker containers) captured BEFORE the run against AFTER teardown.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use capsule::foundation::types::manifest::CapsuleManifest;
use capsulefs::CasStore;
use serde::Serialize;
use snapshot::rootfs_builder::{SourceProbe, build_rootfs, derive_build_spec};
use snapshot::{
    BuildLayers, BuildReadyStateInput, FirecrackerBackend, FirecrackerConfig, RestoreContract,
    RestoreReadyStateInput, SanitizerContract, SnapshotBackend,
};

use super::checks::{resolve_fc_bin, resolve_guest_kernel};

/// The built-in fixture: a public, no-binding, stdlib-python web capsule (the same
/// shape as `crates/snapshot-builder/fixtures/py-web`).
const FIXTURE_CAPSULE_TOML: &str = r#"schema_version = "0.3"
name = "runner-smoke-py-web"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python3 app.py"
port = 8080
readiness_probe = { http_get = "/health" }
"#;
const FIXTURE_APP_PY: &str = r#"import http.server

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "text/plain")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a):
        pass

http.server.HTTPServer(("0.0.0.0", 8080), H).serve_forever()
"#;

#[derive(Debug, Serialize)]
struct StageResult {
    stage: &'static str,
    ok: bool,
    detail: String,
    ms: u128,
}

#[derive(Debug, Serialize)]
struct SmokeReport {
    stages: Vec<StageResult>,
    passed: bool,
}

/// Host resources that must be identical before the run and after teardown.
#[derive(Debug, PartialEq, Eq)]
struct ResourceSnapshot {
    firecracker_pids: usize,
    tap_devices: BTreeSet<String>,
    loop_devices: BTreeSet<String>,
    ato_containers: BTreeSet<String>,
}

fn shell(cmd: &str) -> String {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn resource_snapshot() -> ResourceSnapshot {
    ResourceSnapshot {
        firecracker_pids: shell("pgrep -c firecracker || true").parse().unwrap_or(0),
        tap_devices: shell("ip -o link | grep -oE 'fctap[0-9]+' | sort -u")
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect(),
        loop_devices: shell("losetup -an 2>/dev/null | awk '{print $1}'")
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect(),
        ato_containers: shell("docker ps -a --format '{{.Names}}' 2>/dev/null | grep -E '^ato' || true")
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect(),
    }
}

fn leftovers(before: &ResourceSnapshot, after: &ResourceSnapshot) -> Vec<String> {
    let mut out = Vec::new();
    if after.firecracker_pids > before.firecracker_pids {
        out.push(format!(
            "firecracker processes: {} -> {}",
            before.firecracker_pids, after.firecracker_pids
        ));
    }
    for (name, b, a) in [
        ("tap device", &before.tap_devices, &after.tap_devices),
        ("loop device", &before.loop_devices, &after.loop_devices),
        ("ato docker container", &before.ato_containers, &after.ato_containers),
    ] {
        for item in a.difference(b) {
            out.push(format!("{name}: {item}"));
        }
    }
    out
}

pub(crate) struct SmokeOptions {
    pub proxy_listen: Option<String>,
    pub keep: bool,
    pub json: bool,
}

pub(crate) async fn run(opts: SmokeOptions) -> Result<()> {
    let mut stages: Vec<StageResult> = Vec::new();
    let mut push = |stages: &mut Vec<StageResult>, stage: &'static str, ok: bool, detail: String, t: Instant| {
        stages.push(StageResult { stage, ok, detail, ms: t.elapsed().as_millis() });
        ok
    };
    let proxy_listen = opts.proxy_listen.clone().unwrap_or_else(|| "127.0.0.1:8431".to_string());

    // ── Stage 0: preflight (fail fast; nothing is created yet) ──
    let t = Instant::now();
    let preflight = (|| -> Result<(String, String)> {
        if shell("id -u") != "0" {
            bail!("smoke needs root (Firecracker tap setup): sudo -E ato runner smoke");
        }
        if !FirecrackerBackend::kvm_present() {
            bail!("/dev/kvm is not usable — run `ato doctor runner`");
        }
        let fc = resolve_fc_bin().context("no firecracker binary (ato runner setup --fix)")?;
        let ver = shell(&format!("{fc} --version | head -1"));
        if ver.is_empty() {
            bail!("{fc} did not answer --version");
        }
        let kernel = resolve_guest_kernel().context("no guest kernel (ato runner setup --fix)")?;
        if shell("docker version --format '{{.Server.Version}}'").is_empty() {
            bail!("Docker daemon not reachable");
        }
        Ok((fc, kernel))
    })();
    let (fc_bin, kernel) = match preflight {
        Ok(pair) => {
            push(&mut stages, "preflight", true, format!("root+kvm+docker ok; fc={} kernel={}", pair.0, pair.1), t);
            pair
        }
        Err(e) => {
            push(&mut stages, "preflight", false, format!("{e:#}"), t);
            return finish(stages, opts.json);
        }
    };

    // Workdir for everything this smoke creates (removed at the end unless --keep).
    let workdir = std::env::temp_dir().join(format!("ato-runner-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&workdir)?;
    let before = resource_snapshot();

    let result = smoke_pipeline(&mut stages, &mut push, &workdir, &fc_bin, &kernel, &proxy_listen).await;
    if let Err(e) = result {
        // The failing stage already recorded its detail; this catches setup errors
        // between stages so they are never silently dropped.
        if stages.last().map(|s| s.ok) != Some(false) {
            stages.push(StageResult { stage: "internal", ok: false, detail: format!("{e:#}"), ms: 0 });
        }
    }

    // ── Final stage: orphan diff (runs regardless of earlier failures) ──
    let t = Instant::now();
    let after = resource_snapshot();
    let left = leftovers(&before, &after);
    let ok = left.is_empty();
    push(
        &mut stages,
        "orphans",
        ok,
        if ok { "no leftover firecracker/tap/loop/docker resources".to_string() } else { left.join("; ") },
        t,
    );

    if opts.keep {
        println!("(--keep: work directory retained at {})", workdir.display());
    } else {
        let _ = std::fs::remove_dir_all(&workdir);
    }
    finish(stages, opts.json)
}

#[allow(clippy::too_many_arguments)]
async fn smoke_pipeline(
    stages: &mut Vec<StageResult>,
    push: &mut impl FnMut(&mut Vec<StageResult>, &'static str, bool, String, Instant) -> bool,
    workdir: &std::path::Path,
    fc_bin: &str,
    kernel: &str,
    proxy_listen: &str,
) -> Result<()> {
    // ── Stage 1: Docker→ext4 rootfs from the built-in fixture ──
    let t = Instant::now();
    let src = workdir.join("src");
    std::fs::create_dir_all(&src)?;
    std::fs::write(src.join("capsule.toml"), FIXTURE_CAPSULE_TOML)?;
    std::fs::write(src.join("app.py"), FIXTURE_APP_PY)?;
    let manifest = CapsuleManifest::from_toml(FIXTURE_CAPSULE_TOML)
        .map_err(|e| anyhow::anyhow!("fixture manifest: {e}"))?;
    let spec = derive_build_spec(&manifest, &SourceProbe::scan(&src))
        .map_err(|e| anyhow::anyhow!("derive_build_spec: {e}"))?;
    let ext4 = workdir.join("rootfs.ext4");
    let receipt = build_rootfs(&src, &spec, &ext4, 1024);
    match &receipt {
        Ok(r) => {
            push(stages, "rootfs_build", true, format!("{} ({} bytes)", ext4.display(), r.rootfs_bytes), t);
        }
        Err(e) => {
            push(stages, "rootfs_build", false, e.clone(), t);
            bail!("rootfs build failed");
        }
    }

    // ── Stage 2: build_ready_state (boot → /health → snapshot → seal) ──
    let t = Instant::now();
    let backend = FirecrackerBackend::with_config(FirecrackerConfig {
        firecracker_bin: fc_bin.to_string(),
        kernel_path: PathBuf::from(kernel),
        work_root: workdir.join("fc-work"),
        ..Default::default()
    });
    let store = CasStore::open(workdir.join("cas")).map_err(|e| anyhow::anyhow!("CAS open: {e}"))?;
    let rootfs_bytes = std::fs::read(&ext4)?;
    let built = backend.build_ready_state(BuildReadyStateInput {
        store: &store,
        capsule_manifest_hash: "blake3:runner-smoke".to_string(),
        runner_class: Some(capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id()),
        layers: BuildLayers {
            rootfs: rootfs_bytes,
            runtime: None,
            dependency: None,
            app: None,
            vmstate: Vec::new(),
            memory: Vec::new(),
        },
        restore_contract: RestoreContract {
            ports: vec![spec.port],
            healthcheck: Some(spec.healthcheck.clone()),
            expected_ready_ms: Some(8000),
        },
        sanitizer_contract: SanitizerContract::default(),
        declared_secret_markers: vec![],
        execution_id: None, // local smoke: no declared identity — nothing registers this artifact
    });
    let sealed = match built {
        Ok(r) => {
            push(stages, "build_ready_state", true, format!("sealed manifest {}", r.manifest.id()), t);
            r
        }
        Err(e) => {
            push(stages, "build_ready_state", false, e.to_string(), t);
            bail!("build_ready_state failed");
        }
    };

    // ── Stage 3: restore ──
    let t = Instant::now();
    let restored = backend.restore(RestoreReadyStateInput {
        store: &store,
        manifest: sealed.manifest.clone(),
        overlay_root: workdir.join("restore-ov"),
        host_runner_class: None,
        uffd_preview: false,
    });
    let session = match restored {
        Ok(r) => {
            push(
                stages,
                "restore",
                true,
                format!(
                    "session {} (workload {})",
                    r.session.session_id,
                    r.session.workload_addr.as_deref().unwrap_or("<none>")
                ),
                t,
            );
            r.session
        }
        Err(e) => {
            push(stages, "restore", false, e.to_string(), t);
            bail!("restore failed");
        }
    };

    // ── Stage 4: root proxy + HTTP probe (the exact serving path the runner uses) ──
    let t = Instant::now();
    let mut proxy_handle = None;
    let probe = match session.workload_addr.clone() {
        Some(addr) => {
            match crate::application::runner_agent::start_root_proxy_to(proxy_listen, addr.clone()).await {
                Ok(handle) => {
                    proxy_handle = Some(handle);
                    probe_health(proxy_listen, &spec.healthcheck).await.map(|code| (addr, code))
                }
                Err(e) => Err(anyhow::anyhow!("proxy did not start: {e:#}")),
            }
        }
        None => Err(anyhow::anyhow!("restored session reported no workload address")),
    };
    let proxy_ok = match probe {
        Ok((addr, code)) if code == 200 => push(
            stages,
            "proxy_health",
            true,
            format!("GET {proxy_listen}{} -> 200 (upstream {addr})", spec.healthcheck),
            t,
        ),
        Ok((addr, code)) => push(
            stages,
            "proxy_health",
            false,
            format!("GET {proxy_listen}{} -> {code} (upstream {addr})", spec.healthcheck),
            t,
        ),
        Err(e) => push(stages, "proxy_health", false, format!("{e:#}"), t),
    };

    // ── Stage 5: stop/teardown (always attempted — the session must never leak) ──
    let t = Instant::now();
    if let Some(h) = proxy_handle {
        h.abort();
    }
    let overlay = session.overlay_root.clone();
    match backend.stop(session) {
        Ok(td) => {
            let overlay_gone = !overlay.exists();
            push(
                stages,
                "teardown",
                td.overlay_removed && overlay_gone,
                format!("overlay_removed={} (dir gone={overlay_gone})", td.overlay_removed),
                t,
            );
        }
        Err(e) => {
            push(stages, "teardown", false, e.to_string(), t);
        }
    }

    if !proxy_ok {
        bail!("proxy/health probe failed");
    }
    Ok(())
}

/// One HTTP/1.1 GET through the proxy; returns the status code. Bounded so a hung
/// upstream cannot hang the smoke.
async fn probe_health(addr: &str, path: &str) -> Result<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fut = async {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: smoke\r\nConnection: close\r\n\r\n").as_bytes())
            .await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        let head = String::from_utf8_lossy(&buf);
        parse_http_status(&head).context("no HTTP status line in response")
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), fut)
        .await
        .context("health probe timed out")?
}

/// Status code from an HTTP/1.x response head. Pure.
pub(crate) fn parse_http_status(response: &str) -> Option<u16> {
    let line = response.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn finish(stages: Vec<StageResult>, json: bool) -> Result<()> {
    let passed = stages.iter().all(|s| s.ok);
    if json {
        println!("{}", serde_json::to_string_pretty(&SmokeReport { stages, passed })?);
    } else {
        println!();
        println!("Ato Runner Smoke");
        for s in &stages {
            println!("  {} {} ({} ms): {}", if s.ok { "✓" } else { "✗" }, s.stage, s.ms, s.detail);
        }
        println!();
        println!(
            "{}",
            if passed { "SMOKE PASSED — this host can build and serve capsule snapshots." } else { "SMOKE FAILED — see the failing stage above." }
        );
    }
    if passed {
        Ok(())
    } else {
        // The report above IS the failure surface (same pattern as gpu_provision):
        // exiting directly keeps stdout a single JSON document in --json mode
        // instead of appending the generic error object after it.
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_parse_is_strict() {
        assert_eq!(parse_http_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status("HTTP/1.0 404 Not Found\r\nX: y\r\n"), Some(404));
        assert_eq!(parse_http_status("garbage 200"), None);
        assert_eq!(parse_http_status(""), None);
    }

    #[test]
    fn orphan_diff_reports_only_new_resources() {
        let before = ResourceSnapshot {
            firecracker_pids: 0,
            tap_devices: ["fctap9".to_string()].into(), // pre-existing tap is NOT an orphan
            loop_devices: ["/dev/loop3".to_string()].into(),
            ato_containers: BTreeSet::new(),
        };
        let clean = ResourceSnapshot {
            firecracker_pids: 0,
            tap_devices: ["fctap9".to_string()].into(),
            loop_devices: ["/dev/loop3".to_string()].into(),
            ato_containers: BTreeSet::new(),
        };
        assert!(leftovers(&before, &clean).is_empty());

        let dirty = ResourceSnapshot {
            firecracker_pids: 1,
            tap_devices: ["fctap9".to_string(), "fctap0".to_string()].into(),
            loop_devices: ["/dev/loop3".to_string(), "/dev/loop7".to_string()].into(),
            ato_containers: ["ato-rootfs-x".to_string()].into(),
        };
        let l = leftovers(&before, &dirty);
        assert_eq!(l.len(), 4);
        assert!(l.iter().any(|s| s.contains("firecracker")));
        assert!(l.iter().any(|s| s.contains("fctap0")));
        assert!(l.iter().any(|s| s.contains("/dev/loop7")));
        assert!(l.iter().any(|s| s.contains("ato-rootfs-x")));
    }

    #[test]
    fn fixture_is_a_valid_no_binding_capsule() {
        let m = CapsuleManifest::from_toml(FIXTURE_CAPSULE_TOML).expect("fixture must parse");
        // The smoke must exercise the same eligibility class production seals:
        // public shape, no bindings/secrets/external — derive_build_spec accepts it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), FIXTURE_APP_PY).unwrap();
        let spec = derive_build_spec(&m, &SourceProbe::scan(dir.path())).expect("spec derives");
        assert_eq!(spec.port, 8080);
        assert_eq!(spec.healthcheck, "/health");
    }
}
