//! ato#994 PR 6: **Dockerfile-to-Snapshot Import live KVM smoke** — three real
//! apps from the Store Dockerfile-import queue (inventory report, ato-api#187),
//! pinned by commit, driven end to end on a real builder host:
//!
//! clone → `run_dockerfile_import` (digest-pinned effective-Dockerfile build →
//! plan → packed supervisor ext4) → `build_ready_state` (boot → verify →
//! snapshot → seal) → restore → readiness → stop → orphan 0.
//!
//! Evidence fixture ONLY — no runtime semantics change in this slice (the
//! split-live-fixture discipline). The three apps are chosen to cover distinct
//! import shapes, and are PREFLIGHTED for the v0 read-only-rootfs constraint
//! (see `ImportApp`'s doc):
//! * `jarun/buku` — ENTRYPOINT-only argv, `HEALTHCHECK` →
//!   `docker_healthcheck_ignored`, single env-resolved `EXPOSE` → the
//!   derived-port lane.
//! * `miroslavpejic85/mirotalkc2c` — single-stage node image with NO `EXPOSE`
//!   → exercises the explicit port-override lane.
//! * `miroslavpejic85/mirotalk` (P2P) — second no-EXPOSE app on a different
//!   default port → the override lane is per-app, not a one-off.
//!
//! v0 spec boundary this smoke enforces by selection (documented in #994):
//! Dockerfile Import v0 supports images that can run with a READ-ONLY rootfs.
//! Images that write to their own application directory at startup are not
//! Snapshot Ready unless those writes move to `/tmp`, a declared Ato state
//! binding under `/ato/state/…`, or a future writable-scratch mapping.
//!
//! `#[ignore]`d and self-skips unless `/dev/kvm` + `ATO_LIVE_KVM=1` +
//! `ATO_SMOKE_DOCKER_IMPORT=1` + a container build tool are present (same
//! convention as `durable_state_kvm_smoke`). Needs network egress (clones +
//! registry pulls) and `ATO_GUEST_AGENT_BIN` (imports always run the
//! supervisor):
//!
//! ```sh
//! # MUSL-STATIC agent, not the host-glibc build: an imported image picks its
//! # own base (2 of the 3 apps below are alpine/musl), and a glibc-linked
//! # agent cannot exec inside a musl rootfs. Static works everywhere.
//! rustup target add x86_64-unknown-linux-musl
//! cargo build --release -p guest-agent --target x86_64-unknown-linux-musl
//! sudo -E env ATO_LIVE_KVM=1 ATO_SMOKE_DOCKER_IMPORT=1 \
//!   ATO_GUEST_AGENT_BIN=target/x86_64-unknown-linux-musl/release/guest-agent \
//!   cargo test --release -p cli -- --ignored --test-threads=1 --nocapture \
//!   docker_import_live_smoke
//! ```
//!
//! Everything this smoke creates (clones, ext4s, CAS, overlays) lives under
//! ONE `tempfile::tempdir()`, dropped at the end even on panic. The only
//! host-global artifacts are the container images the import builds — and the
//! import's own pack script removes the built image in its cleanup trap.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use capsulefs::CasStore;
use snapshot::docker_import::build::SystemImportCommandRunner;
use snapshot::docker_import::{
    DockerImportSpec, DockerImportWarning, DockerfileImportRequest, SecretEnvPolicy,
    import_identity_digest, run_dockerfile_import,
};
use snapshot::{
    BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt, FirecrackerBackend,
    FirecrackerConfig, RestoreContract, RestoreReadyStateInput, RestoredSession, SanitizerContract,
    SnapshotBackend, SupervisorBindings,
};

use super::binding_host::stop_scrub_over_vsock;

/// One pinned import-queue app + what the import of it must prove.
///
/// Selection is PREFLIGHTED: v0 Snapshot-Ready imports must tolerate a
/// READ-ONLY rootfs (Ready-State's boot model) — an app that writes its own
/// application directory at startup crashes before readiness. Candidates are
/// screened with `docker run --rm -d --read-only --tmpfs /tmp <img>` + a
/// still-running check; the full candidate table lives in the PR discussion,
/// this fixture carries only the green trio (a live fixture is a capability
/// proof, not an exploration log).
struct ImportApp {
    slug: &'static str,
    owner: &'static str,
    repo: &'static str,
    /// Full 40-hex commit — the smoke must be reproducible, never HEAD.
    commit: &'static str,
    /// Explicit public port (`None` = derived from the single EXPOSE).
    port_override: Option<u16>,
    /// Explicit readiness path (`None` = the synthesized `GET /`).
    readiness: Option<&'static str>,
    expect_user_warning: bool,
    expect_healthcheck_warning: bool,
}

const APPS: &[ImportApp] = &[
    ImportApp {
        // Single-stage python/alpine, ENTRYPOINT-only argv, HEALTHCHECK ->
        // docker_healthcheck_ignored, single env-resolved EXPOSE -> the
        // DERIVED-port lane (no override).
        slug: "community/buku",
        owner: "jarun",
        repo: "buku",
        commit: "c1e2968c4b613337bef758853c8cfd1e562be518",
        port_override: None,
        readiness: None,
        expect_user_warning: false,
        expect_healthcheck_warning: true,
    },
    ImportApp {
        // Single-stage node image with NO EXPOSE — exercises the explicit
        // --port lane (the app listens on its default 8080).
        slug: "community/mirotalk-c2c",
        owner: "miroslavpejic85",
        repo: "mirotalkc2c",
        commit: "d7be5baa43103d3f9eebfa30a26f1d68c350c35f",
        port_override: Some(8080),
        readiness: None,
        expect_user_warning: false,
        expect_healthcheck_warning: false,
    },
    ImportApp {
        // Second no-EXPOSE node app on a DIFFERENT default port — proves the
        // port-override lane is per-app, not a one-off. (The ARG-FROM +
        // heavy-multi-stage candidate, manage-my-damn-life, exceeded the
        // preflight build budget and moved to the follow-up queue; the
        // ARG-substitution lane is unit-covered in the snapshot crate.)
        slug: "community/mirotalk-p2p",
        owner: "miroslavpejic85",
        repo: "mirotalk",
        commit: "67a541019bf2956b8e7ddef08aab2cd665edb4fe",
        port_override: Some(3000),
        readiness: None,
        expect_user_warning: false,
        expect_healthcheck_warning: false,
    },
];

/// Shallow-clone a pinned commit. Deliberately NOT `materialize_source` — that
/// gate requires a `capsule.toml`, and a Dockerfile-import candidate by
/// definition has none yet.
fn clone_pinned(owner: &str, repo: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let run = |args: &[&str]| -> anyhow::Result<()> {
        let out = Command::new("git").args(args).current_dir(dest).output()?;
        anyhow::ensure!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    };
    run(&["init", "-q"])?;
    run(&[
        "remote",
        "add",
        "origin",
        &format!("https://github.com/{owner}/{repo}.git"),
    ])?;
    run(&["fetch", "-q", "--depth", "1", "origin", commit])?;
    run(&["checkout", "-q", "FETCH_HEAD"])?;
    Ok(())
}

fn http_get(addr: &str, path: &str, timeout: Duration) -> anyhow::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: smoke\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no HTTP status line in response: {head:?}"))?;
    Ok((status, String::new()))
}

/// Wait for the app's readiness path to answer 200 (a 3xx/401 is a REAL
/// answer from the app but not readiness-200 — fail loudly with the code).
fn wait_ready(addr: &str, path: &str, tries: u32) -> anyhow::Result<()> {
    let mut last: Option<u16> = None;
    for _ in 0..tries {
        if let Ok((code, _)) = http_get(addr, path, Duration::from_secs(2)) {
            if code == 200 {
                return Ok(());
            }
            last = Some(code);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    anyhow::bail!("GET {path} never returned 200 after {tries} tries (last status: {last:?})")
}

fn firecracker_proc_count() -> usize {
    Command::new("pgrep")
        .args(["-c", "-x", "firecracker"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0)
}

/// Import + seal one app; returns the sealed receipt + the proxied port +
/// readiness path used.
fn import_and_seal(
    backend: &FirecrackerBackend,
    store: &CasStore,
    workdir: &Path,
    app: &ImportApp,
) -> anyhow::Result<(BuildReadyStateReceipt, u16, String)> {
    let t_clone = std::time::Instant::now();
    let src = workdir.join(format!("src-{}", app.repo));
    clone_pinned(app.owner, app.repo, app.commit, &src)?;

    let t_import = std::time::Instant::now();
    let spec = DockerImportSpec::new("Dockerfile", BTreeMap::new())
        .map_err(|e| anyhow::anyhow!("spec({}): {e}", app.slug))?;
    let ext4 = workdir.join(format!("{}.ext4", app.repo));
    let req = DockerfileImportRequest {
        context_dir: &src,
        spec,
        policy: SecretEnvPolicy::Reject,
        port_override: app.port_override,
        readiness_http_path: app.readiness.map(String::from),
        volume_policy: Default::default(),
        ephemeral_mounts: Vec::new(),
        host_bind_relay: false,
        pixel_rfb_port: None,
        image_tag: format!("ato-import-smoke-{}", app.repo),
        out_ext4: &ext4,
        size_mib: 2048,
    };
    let outcome = run_dockerfile_import(&SystemImportCommandRunner, &req)
        .map_err(|e| anyhow::anyhow!("import({}): {e}", app.slug))?;
    let clone_ms = t_import.duration_since(t_clone).as_millis();
    let import_ms = t_import.elapsed().as_millis();

    // ── Provenance assertions: every base digest-pinned; identity computable;
    // the expected per-app warnings (and no unexpected rejections). ──
    anyhow::ensure!(
        !outcome.receipt.resolved_base_images.is_empty(),
        "({}) no base images pinned",
        app.slug
    );
    for b in &outcome.receipt.resolved_base_images {
        anyhow::ensure!(
            b.resolved_digest.contains("@sha256:"),
            "({}) base {:?} not digest-pinned: {:?}",
            app.slug,
            b.original_ref,
            b.resolved_digest
        );
    }
    let identity = import_identity_digest(&outcome.receipt);
    anyhow::ensure!(
        identity.starts_with("sha256:"),
        "({}) bad identity {identity:?}",
        app.slug
    );
    let warns = &outcome.receipt.warnings;
    anyhow::ensure!(
        warns.contains(&DockerImportWarning::DockerUserIgnored) == app.expect_user_warning,
        "({}) docker_user_ignored expectation mismatch: {warns:?}",
        app.slug
    );
    anyhow::ensure!(
        warns.contains(&DockerImportWarning::DockerHealthcheckIgnored)
            == app.expect_healthcheck_warning,
        "({}) docker_healthcheck_ignored expectation mismatch: {warns:?}",
        app.slug
    );
    eprintln!(
        "  [{slug}] imported ({clone_ms} ms clone, {import_ms} ms import): port={port} identity={identity} bases={bases:?} warnings={warns:?}",
        slug = app.slug,
        port = outcome.plan.port,
        bases = outcome
            .receipt
            .resolved_base_images
            .iter()
            .map(|b| b.resolved_digest.as_str())
            .collect::<Vec<_>>(),
    );

    let t_seal = std::time::Instant::now();
    let readiness = app.readiness.unwrap_or("/").to_string();
    let rootfs_bytes = std::fs::read(&ext4)?;
    let sealed = backend
        .build_ready_state(BuildReadyStateInput {
            store,
            capsule_manifest_hash: format!("blake3:docker-import-smoke-{}", app.repo),
            runner_class: None,
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: rootfs_bytes,
                runtime: None,
                dependency: None,
                app: None,
                vmstate: Vec::new(),
                memory: Vec::new(),
            },
            restore_contract: RestoreContract {
                ports: vec![outcome.plan.port],
                healthcheck: Some(readiness.clone()),
                expected_ready_ms: Some(20_000),
                ..Default::default()
            },
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: vec![],
            execution_id: Some(identity.clone()),
            execution_identity_schema: None,
            // A v0 import with ZERO bindings is an honest NO-BINDING artifact:
            // the backend's supervisor path requires ≥1 binding name (it exists
            // to gate on them), while the in-guest agent with an empty required
            // set is vacuously bound-ready and starts the service immediately —
            // so the v1.0 no-binding seal contract ("boot, healthcheck answers")
            // is exactly what holds. A WITH-bindings import exercises the
            // supervisor path once a Store job shape carries secrets (later
            // slice); this smoke's three apps are all zero-binding.
            supervisor: (!outcome.plan.supervisor.binding_names.is_empty()).then(|| {
                SupervisorBindings {
                    binding_names: outcome.plan.supervisor.binding_names.clone(),
                    state_volumes: vec![],
                    state_owner_scope: None,
                }
            }),
        })
        .map_err(|e| anyhow::anyhow!("build_ready_state({}): {e}", app.slug))?;
    eprintln!(
        "  [{}] sealed ({} ms seal)",
        app.slug,
        t_seal.elapsed().as_millis()
    );
    Ok((sealed, outcome.plan.port, readiness))
}

fn restore_and_verify(
    backend: &FirecrackerBackend,
    store: &CasStore,
    workdir: &Path,
    slug: &str,
    sealed: &BuildReadyStateReceipt,
    readiness: &str,
) -> anyhow::Result<RestoredSession> {
    let restored = backend
        .restore(RestoreReadyStateInput {
            store,
            manifest: sealed.manifest.clone(),
            overlay_root: workdir.join(format!("ov-{}", slug.replace('/', "-"))),
            host_runner_class: None,
            uffd_preview: false,
        })
        .map_err(|e| anyhow::anyhow!("restore({slug}): {e}"))?;
    let session = restored.session;
    // A v0 import has ZERO bindings and ZERO volumes — the supervisor gate is
    // vacuously bound-ready, so no vsock delivery is required before the
    // workload starts. Readiness alone is the verification.
    let addr = session
        .workload_addr
        .clone()
        .ok_or_else(|| anyhow::anyhow!("({slug}) no workload_addr"))?;
    wait_ready(&addr, readiness, 40).map_err(|e| anyhow::anyhow!("({slug}) {e}"))?;
    Ok(session)
}

fn stop_session(backend: &FirecrackerBackend, session: RestoredSession) -> anyhow::Result<()> {
    if let Some(vsock) = &session.vsock_uds
        && let Err(e) = stop_scrub_over_vsock(vsock)
    {
        eprintln!("(non-fatal) stop-scrub over vsock: {e:#}");
    }
    let overlay = session.overlay_root.clone();
    let td = backend
        .stop(session)
        .map_err(|e| anyhow::anyhow!("stop: {e}"))?;
    anyhow::ensure!(
        td.overlay_removed && !overlay.exists(),
        "overlay not removed after stop"
    );
    Ok(())
}

#[test]
#[ignore]
fn docker_import_live_smoke() {
    if std::env::var("ATO_LIVE_KVM").ok().as_deref() != Some("1")
        || std::env::var("ATO_SMOKE_DOCKER_IMPORT").ok().as_deref() != Some("1")
    {
        eprintln!("SKIP: set ATO_LIVE_KVM=1 ATO_SMOKE_DOCKER_IMPORT=1 to run this live smoke");
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
    if snapshot::docker_import::build::probe_build_tool(&SystemImportCommandRunner).is_err() {
        eprintln!("SKIP: no container build tool (podman or docker) on this host");
        return;
    }
    if std::env::var("ATO_GUEST_AGENT_BIN").is_err() {
        eprintln!("SKIP: ATO_GUEST_AGENT_BIN not set (imports always run the supervisor)");
        return;
    }
    // SAFETY: single-threaded (--test-threads=1 required, same as the other
    // ready_state smokes) — supervisor builds require the vsock channel.
    unsafe { std::env::set_var("ATO_FC_VSOCK", "1") };

    let fc_baseline = firecracker_proc_count();
    let dir = tempfile::tempdir().expect("tempdir");
    let workdir = dir.path();
    let backend = FirecrackerBackend::with_config(FirecrackerConfig {
        firecracker_bin: fc_bin,
        kernel_path: std::path::PathBuf::from(kernel),
        work_root: workdir.join("fc-work"),
        ..Default::default()
    });
    let store = CasStore::open(workdir.join("cas")).expect("CAS open");

    let mut passed: Vec<&str> = Vec::new();
    for app in APPS {
        eprintln!("── {} ({}@{}) ──", app.slug, app.repo, &app.commit[..12]);
        let (sealed, port, readiness) =
            import_and_seal(&backend, &store, workdir, app).expect(app.slug);
        let t_restore = std::time::Instant::now();
        let session = restore_and_verify(&backend, &store, workdir, app.slug, &sealed, &readiness)
            .expect(app.slug);
        eprintln!(
            "  [{}] restored + ready on port {port} ({} ms restore->ready)",
            app.slug,
            t_restore.elapsed().as_millis()
        );
        stop_session(&backend, session).expect(app.slug);
        eprintln!("  [{}] stopped, overlay removed", app.slug);
        passed.push(app.slug);
    }

    let fc_after = firecracker_proc_count();
    assert_eq!(
        fc_after, fc_baseline,
        "orphan firecracker processes left behind ({fc_baseline} -> {fc_after})"
    );
    eprintln!("docker_import_live_smoke: ALL STAGES PASSED for {passed:?} (orphans: 0)");
    // tempdir drops here — clones, ext4s, CAS, overlays all removed.
}
