//! v1.6 (ato#983) Slice 4: durable-state live KVM smoke.
//!
//! The first end-to-end exercise of Slices 1-3 together on real hardware:
//! build → seal → **restore with REAL secret delivery over vsock** (the
//! production `bind_before_expose` gate, not a no-binding shortcut) → write
//! through the durable volume → stop (`ato stop`'s own vsock scrub path) →
//! **restore the SAME sealed artifact again** → prove the write survived →
//! build a SECOND, differently-NAMED capsule (different
//! `persistent_state_owner_scope`) → prove it does NOT see the first
//! capsule's data (durable state is identity-scoped, never baked into the
//! shared rootfs and never globally shared).
//!
//! Deliberately single-service (multi-service composition is already proven
//! live by `ato#984`/`#986`'s fixture) — this fixture's only job is durable
//! state specifically: mount-before-start (Slice 3), the backing-file
//! identity/lifecycle (Slice 2), and the schema/derivation that connects them
//! (Slice 1).
//!
//! `#[ignore]`d and self-skips unless `/dev/kvm` + `ATO_LIVE_KVM=1` +
//! `ATO_SMOKE_DURABLE_STATE=1` are present, so a normal `cargo test` never
//! runs it (same convention as `ready_state::kvm_smoke`):
//!
//! ```sh
//! sudo -E env ATO_LIVE_KVM=1 ATO_SMOKE_DURABLE_STATE=1 \
//!   cargo test --release -p cli -- --ignored --test-threads=1 --nocapture \
//!   durable_state_live_smoke
//! ```
//!
//! Every path this smoke touches (rootfs cache, build scratch, durable state
//! backing files/locks) lives under ONE `tempfile::tempdir()` — dropped at
//! the end of the test, success or panic — so this can never leave anything
//! behind on a host that also runs a real enrolled Connected Runner (a
//! separate `work_root`, e.g. `/var/lib/ato/...`, is never touched).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use capsule::foundation::types::manifest::CapsuleManifest;
use capsulefs::CasStore;
use protocol::binding_lease::SecretValue;
use snapshot::rootfs_builder::{SourceProbe, build_rootfs, derive_supervisor_build_spec};
use snapshot::state_volume::DurableVolumeSpec;
use snapshot::{
    BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, FirecrackerBackend, FirecrackerConfig,
    RestoreContract, RestoreReadyStateInput, RestoredSession, SanitizerContract, SnapshotBackend,
    SupervisorBindings,
};

use super::binding_host::{bind_before_expose, issue_leases, stop_scrub_over_vsock};

/// `{name}` and `{schema_hex}` are substituted per capsule instance — only the
/// capsule `name` differs between the two builds this smoke creates, which is
/// exactly what makes their `persistent_state_owner_scope()` (defaults to
/// `name` when no explicit `state_owner_scope` is set) differ, and therefore
/// their durable-state identity/backing-file path (see `state_volume.rs`).
const CAPSULE_TOML_TEMPLATE: &str = r#"schema_version = "0.3"
name = "{name}"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python3 app.py"
port = 8080
readiness_probe = { http_get = "/healthz" }

[secrets.openai_api_key]
required = true
env = "OPENAI_API_KEY"

[state.dbdata]
kind = "filesystem"
durability = "persistent"
attach = "explicit"
purpose = "durable state live smoke (ato#983 slice 4)"
schema_id = "sha256:{schema_hex}"
size_mb = 64

[services.web]
entrypoint = "python3 app.py"
secrets = ["openai_api_key"]
state_bindings = [{ state = "dbdata", target = "/ato/state/dbdata" }]

[services.web.network]
publish = true
"#;

/// `/healthz` for the readiness probe; `/write?value=X` + `/read` exercise the
/// durable mount directly (idempotent — `/read` returns an empty body, not an
/// error, when nothing has been written yet, e.g. this capsule's very first
/// boot or a differently-scoped capsule that never saw the marker);
/// `/secret-check` proves the REAL secret (not a placeholder) reached the
/// service's environment.
const APP_PY: &str = r#"import http.server
import os
import urllib.parse

STATE_DIR = "/ato/state/dbdata"
MARKER_PATH = os.path.join(STATE_DIR, "marker.txt")


class Handler(http.server.BaseHTTPRequestHandler):
    def _reply(self, status, body=b""):
        self.send_response(status)
        self.send_header("content-type", "text/plain")
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/healthz":
            self._reply(200, b"ok")
        elif parsed.path == "/write":
            qs = urllib.parse.parse_qs(parsed.query)
            value = qs.get("value", [""])[0]
            os.makedirs(STATE_DIR, exist_ok=True)
            with open(MARKER_PATH, "w") as f:
                f.write(value)
                f.flush()
                os.fsync(f.fileno())
            self._reply(200, b"written")
        elif parsed.path == "/read":
            try:
                with open(MARKER_PATH) as f:
                    content = f.read()
            except FileNotFoundError:
                content = ""
            self._reply(200, content.encode())
        elif parsed.path == "/secret-check":
            present = bool(os.environ.get("OPENAI_API_KEY"))
            self._reply(200, b"yes" if present else b"no")
        else:
            self._reply(404)

    def log_message(self, *args):
        pass


http.server.HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
"#;

fn capsule_toml(name: &str) -> String {
    CAPSULE_TOML_TEMPLATE.replace("{name}", name).replace("{schema_hex}", &"0".repeat(64))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// One blocking HTTP/1.1 GET; returns (status, body). No proxy layer — this
/// smoke connects directly to `RestoredSession.workload_addr` (already
/// host-reachable on the tap interface), since the reverse-proxy path is
/// exercised by other tests and isn't this fixture's concern.
fn http_get(addr: &str, path: &str, timeout: Duration) -> anyhow::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: smoke\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no HTTP status line in response: {head:?}"))?;
    Ok((status, body))
}

fn wait_for_health(addr: &str, tries: u32) -> anyhow::Result<()> {
    for _ in 0..tries {
        if let Ok((200, _)) = http_get(addr, "/healthz", Duration::from_secs(2)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    anyhow::bail!("/healthz never returned 200 after {tries} tries")
}

/// A fresh guest network path (tap re-up after the previous session's
/// teardown) can drop the very first connection attempt even once
/// `/healthz` has already answered once — this retries a plain connection-
/// level failure (not a non-200 status, which is a real application-level
/// answer and returned immediately) a few times before giving up, so a
/// transient reconnect blip doesn't fail the whole smoke.
fn http_get_retrying(addr: &str, path: &str, timeout: Duration, tries: u32) -> anyhow::Result<(u16, String)> {
    let mut last_err = None;
    for attempt in 0..tries {
        match http_get(addr, path, timeout) {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < tries {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("{path}: no attempts made")))
}

struct BuiltCapsule {
    sealed: BuildReadyStateReceipt,
    owner_scope: String,
}

fn build_capsule(
    backend: &FirecrackerBackend,
    store: &CasStore,
    workdir: &std::path::Path,
    name: &str,
) -> anyhow::Result<BuiltCapsule> {
    let toml = capsule_toml(name);
    let src = workdir.join(format!("src-{name}"));
    std::fs::create_dir_all(&src)?;
    std::fs::write(src.join("capsule.toml"), &toml)?;
    std::fs::write(src.join("app.py"), APP_PY)?;
    let manifest =
        CapsuleManifest::from_toml(&toml).map_err(|e| anyhow::anyhow!("fixture manifest ({name}): {e}"))?;
    let spec = derive_supervisor_build_spec(&manifest, &SourceProbe::scan(&src))
        .map_err(|e| anyhow::anyhow!("derive_supervisor_build_spec({name}): {e}"))?;
    let ext4 = workdir.join(format!("{name}.ext4"));
    build_rootfs(&src, &spec, &ext4, 1024).map_err(|e| anyhow::anyhow!("build_rootfs({name}): {e}"))?;
    let rootfs_bytes = std::fs::read(&ext4)?;

    let owner_scope = manifest
        .persistent_state_owner_scope()
        .ok_or_else(|| anyhow::anyhow!("fixture manifest ({name}) has no owner scope"))?;
    let sup = spec.supervisor.as_ref().ok_or_else(|| anyhow::anyhow!("({name}) is not a supervisor build"))?;
    let state_volumes: Vec<DurableVolumeSpec> = sup
        .services
        .iter()
        .flatten()
        .flat_map(|s| &s.volumes)
        .map(|v| DurableVolumeSpec { state_name: v.state_name.clone(), size_mb: v.size_mb })
        .collect();
    anyhow::ensure!(!state_volumes.is_empty(), "fixture ({name}) derived zero durable volumes");

    let sealed = backend
        .build_ready_state(BuildReadyStateInput {
            store,
            capsule_manifest_hash: format!("blake3:durable-state-smoke-{name}"),
            runner_class: None,
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
            execution_id: Some(format!("sha256:durable-state-smoke-{name}")),
            supervisor: Some(SupervisorBindings {
                binding_names: sup.binding_names.clone(),
                state_volumes,
                state_owner_scope: Some(owner_scope.clone()),
            }),
        })
        .map_err(|e| anyhow::anyhow!("build_ready_state({name}): {e}"))?;

    Ok(BuiltCapsule { sealed, owner_scope })
}

/// Restore + deliver the REAL secret over vsock via the SAME production gate
/// (`bind_before_expose`) `ato run`/the runner-fleet path uses — not a
/// no-binding shortcut — then wait for `/healthz`.
fn restore_and_bind(
    backend: &FirecrackerBackend,
    store: &CasStore,
    workdir: &std::path::Path,
    label: &str,
    sealed: &BuildReadyStateReceipt,
) -> anyhow::Result<RestoredSession> {
    let restored = backend
        .restore(RestoreReadyStateInput {
            store,
            manifest: sealed.manifest.clone(),
            overlay_root: workdir.join(format!("ov-{label}")),
            host_runner_class: None,
            uffd_preview: false,
        })
        .map_err(|e| anyhow::anyhow!("restore({label}): {e}"))?;
    let session = restored.session;
    let vsock = session.vsock_uds.clone().ok_or_else(|| anyhow::anyhow!("({label}) no vsock_uds"))?;
    let leases = issue_leases(vec![("openai_api_key".to_string(), SecretValue::new("sk-smoke-dummy"))], now_ms(), 60_000)
        .map_err(|e| anyhow::anyhow!("issue_leases({label}): {e}"))?;
    bind_before_expose(&vsock, &leases, Duration::from_secs(10))
        .map_err(|e| anyhow::anyhow!("bind_before_expose({label}): {e}"))?;
    let addr = session.workload_addr.clone().ok_or_else(|| anyhow::anyhow!("({label}) no workload_addr"))?;
    wait_for_health(&addr, 20).map_err(|e| anyhow::anyhow!("({label}) {e}"))?;
    let (code, body) = http_get_retrying(&addr, "/secret-check", Duration::from_secs(2), 5)?;
    anyhow::ensure!(code == 200 && body == "yes", "({label}) real secret did not reach the service (got {body:?})");
    Ok(session)
}

/// Stop, matching production `ato stop`: scrub over vsock FIRST (this is what
/// drives the guest-agent's true session-terminal `HostToAgent::Stop` — the
/// exact trigger Slice 3 wired volume-unmount to), then tear the VM down.
/// The vsock scrub is best-effort (a session that already lost its channel
/// must not block teardown); the durable state files must NOT be touched by
/// any of this — only their (session-scoped) locks are released.
fn stop_session(backend: &FirecrackerBackend, session: RestoredSession) -> anyhow::Result<()> {
    if let Some(vsock) = &session.vsock_uds {
        if let Err(e) = stop_scrub_over_vsock(vsock) {
            eprintln!("(non-fatal) stop-scrub over vsock: {e:#}");
        }
    }
    let overlay = session.overlay_root.clone();
    let td = backend.stop(session).map_err(|e| anyhow::anyhow!("stop: {e}"))?;
    anyhow::ensure!(td.overlay_removed && !overlay.exists(), "overlay not removed after stop");
    Ok(())
}

#[test]
#[ignore]
fn durable_state_live_smoke() {
    if std::env::var("ATO_LIVE_KVM").ok().as_deref() != Some("1")
        || std::env::var("ATO_SMOKE_DURABLE_STATE").ok().as_deref() != Some("1")
    {
        eprintln!("SKIP: set ATO_LIVE_KVM=1 ATO_SMOKE_DURABLE_STATE=1 to run this live smoke");
        return;
    }
    if !FirecrackerBackend::kvm_present() {
        eprintln!("SKIP: /dev/kvm absent");
        return;
    }
    let fc_bin = match crate::application::runner_bootstrap::checks::resolve_fc_bin() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: no firecracker binary (ato runner setup --fix, or set ATO_FC_BIN)");
            return;
        }
    };
    let kernel = match crate::application::runner_bootstrap::checks::resolve_guest_kernel() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: no guest kernel (ato runner setup --fix, or set ATO_FC_KERNEL)");
            return;
        }
    };
    // SAFETY: single-threaded (--test-threads=1 required for this whole suite,
    // same as ready_state::kvm_smoke) — a supervisor build requires the vsock
    // binding channel.
    unsafe { std::env::set_var("ATO_FC_VSOCK", "1") };

    // EVERYTHING this smoke creates — rootfs cache, build scratch, durable
    // state backing files/locks — lives under this ONE tempdir. Dropped at
    // the end (even on panic), so a real enrolled runner's own work_root
    // (e.g. /var/lib/ato/...) is never touched, and nothing needs a separate
    // "explicit cleanup" step.
    let dir = tempfile::tempdir().expect("tempdir");
    let workdir = dir.path();
    let backend = FirecrackerBackend::with_config(FirecrackerConfig {
        firecracker_bin: fc_bin,
        kernel_path: std::path::PathBuf::from(kernel),
        work_root: workdir.join("fc-work"),
        ..Default::default()
    });
    let store = CasStore::open(workdir.join("cas")).expect("CAS open");

    // ── Build capsule "a" and capsule "b" — same shape, different `name`
    // (⇒ different persistent_state_owner_scope ⇒ different durable-state
    // identity) — this is the isolation check's fixture setup. ──
    let a = build_capsule(&backend, &store, workdir, "durable-smoke-a").expect("build capsule a");
    let b = build_capsule(&backend, &store, workdir, "durable-smoke-b").expect("build capsule b");
    assert_ne!(a.owner_scope, b.owner_scope, "the two fixtures must have different owner scopes");

    // ── Run 1: restore a, write a unique marker through the durable mount ──
    let marker = format!("durable-state-smoke-marker-{}", now_ms());
    let session = restore_and_bind(&backend, &store, workdir, "a-run1", &a.sealed).expect("restore a run1");
    let addr = session.workload_addr.clone().unwrap();
    let (code, _) = http_get_retrying(&addr, &format!("/write?value={marker}"), Duration::from_secs(5), 5).expect("write");
    assert_eq!(code, 200, "write must succeed");
    let (code, body) = http_get_retrying(&addr, "/read", Duration::from_secs(5), 5).expect("read back");
    assert_eq!((code, body.as_str()), (200, marker.as_str()), "read-after-write within the same run");
    stop_session(&backend, session).expect("stop a run1");

    // ── Run 2: restore the SAME sealed artifact AGAIN — proves the marker
    // survived stop/restore (the whole point of durable state). ──
    let session = restore_and_bind(&backend, &store, workdir, "a-run2", &a.sealed).expect("restore a run2");
    let addr = session.workload_addr.clone().unwrap();
    let (code, body) = http_get_retrying(&addr, "/read", Duration::from_secs(5), 5).expect("read after restore");
    assert_eq!(
        (code, body.as_str()),
        (200, marker.as_str()),
        "durable state must survive a stop -> restore cycle of the SAME capsule"
    );
    stop_session(&backend, session).expect("stop a run2");

    // ── Isolation: restore capsule "b" (different owner_scope) — must NOT
    // see "a"'s marker. Proves durable state is identity-scoped, never baked
    // into the (shared, content-addressed) rootfs and never globally shared. ──
    let session = restore_and_bind(&backend, &store, workdir, "b-run1", &b.sealed).expect("restore b run1");
    let addr = session.workload_addr.clone().unwrap();
    let (code, body) = http_get_retrying(&addr, "/read", Duration::from_secs(5), 5).expect("read from b");
    assert_eq!((code, body.as_str()), (200, ""), "a differently-scoped capsule must never see another's durable state");
    stop_session(&backend, session).expect("stop b run1");

    eprintln!("durable_state_live_smoke: ALL STAGES PASSED (marker={marker:?}, owner_scope a={:?} b={:?})", a.owner_scope, b.owner_scope);
    // `dir` drops here — tempdir (rootfs cache + durable state backing files
    // + locks) removed; no separate cleanup step needed.
}
