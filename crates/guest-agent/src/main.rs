//! Ready-State **guest-agent binary** (#863) — runs INSIDE the guest.
//!
//! Reads [`HostToAgent`] control messages as newline-delimited JSON, drives the
//! [`BindingSession`] + [`TmpfsBindingSink`] (materializing each binding at
//! `/run/ato/bindings/<name>`, `0600`), and writes value-free [`AgentToHost`]
//! responses back. Two transports, identical framing:
//!
//! - `ATO_GUEST_AGENT_MODE=stdio` (default): over stdin/stdout (tests + the smoke).
//! - `ATO_GUEST_AGENT_MODE=vsock` (PR B): AF_VSOCK listener on
//!   `ATO_GUEST_AGENT_VSOCK_PORT` (default 1025) — the host connects through
//!   Firecracker's vsock UDS. This is the production guest transport.
//!
//! v1.2 (supervisor mode): when `/etc/ato/supervisor.json` is present, the agent
//! also owns the WORKLOAD process — it starts it (with the env composed from the
//! delivered bindings) once the session is bound-ready, and stops it on
//! `StopWorkload`. This is how a secret reaches an env-delivery workload without a
//! host-side environ rewrite (impossible for a snapshotted process).
//!
//! No secret is ever logged: responses are value-free, and the value only lands on
//! tmpfs / the started child's environment.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use guest_agent::supervisor::{
    ChildWorkload, Supervisor, SupervisorConfig, Workload, bindings_root, config_path,
};
use guest_agent::volume_mount::{
    RealVolumeMounter, VolumeMounter, VolumeSpec, mount_all_volumes, unmount_all_volumes,
};
use guest_agent::vsock::{DEFAULT_VSOCK_PORT, serve_vsock};
use guest_agent::{BindingSession, BindingSink, TmpfsBindingSink};
use protocol::binding_control::AgentToHost;
use protocol::binding_control::HostToAgent;
use protocol::binding_lease::BindingName;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The agent runtime: the binding session plus an optional workload supervisor. The
/// supervisor is `None` for a no-binding / non-env-delivery (v1.0) capsule, in which
/// case the agent only delivers/scrubs bindings and the init launches the app itself.
///
/// v1.4 (ato#970) hard gate: the runtime also tracks an in-memory
/// `binding name → value digest` map (digests only — the VALUE lives on tmpfs, and
/// a digest is never logged, persisted, or sent to the host). It exists so the
/// effective binding set the RUNNING workload was started with can be compared
/// against the current one: a required binding that disappears stops the workload,
/// and one whose value rotates restarts it with the fresh env.
struct AgentRuntime<S: BindingSink, W: Workload> {
    session: BindingSession<S>,
    supervisor: Option<Supervisor<W>>,
    /// Required binding names (same list the session gates on).
    required: Vec<BindingName>,
    /// Latest successfully delivered value digest per binding.
    digests: std::collections::HashMap<BindingName, [u8; 32]>,
    /// Digest snapshot the CURRENTLY RUNNING workload was started with
    /// (`None` = not running).
    running_digests: Option<std::collections::HashMap<BindingName, [u8; 32]>>,
    /// v1.6 (ato#983) Slice 3: durable state volumes this capsule declares.
    volumes: Vec<VolumeSpec>,
    /// v1.6 (ato#983) Slice 3 revision: whether `volumes` is currently
    /// mounted. Mounting is a RESTORE-TIME binding (`MountVolumes`), never
    /// done at boot/build — see `MountVolumes`'s doc comment for why. Starts
    /// `true` when there is nothing to mount (a capsule with no durable
    /// state needs no trigger at all). Tracked for idempotency (a repeat
    /// `MountVolumes` is a no-op success) and so `Stop` only attempts to
    /// unmount what was actually mounted — NOT used to gate workload start:
    /// the host's own call ordering (always `MountVolumes` before any
    /// `Deliver` on restore — see `mount_volumes_before_expose`) is what
    /// keeps a real session from starting unmounted. A guest-side hard gate
    /// here would be indistinguishable from BUILD's own placeholder-delivery
    /// flow, which deliberately never sends `MountVolumes` at all (that's
    /// the whole fix — mounting must never happen at build/boot) and must
    /// still be able to start the workload to pass its health check.
    volumes_mounted: bool,
    /// v1.6 (ato#983) Slice 3 revision: injectable so tests can exercise the
    /// full MountVolumes flow (success, failure, idempotent retry) without a
    /// real Linux mount syscall or `blkid` — mirrors `S: BindingSink` /
    /// `W: Workload` already being generic for exactly this reason.
    mounter: Box<dyn VolumeMounter>,
}

impl<S: BindingSink, W: Workload> AgentRuntime<S, W> {
    fn new(
        session: BindingSession<S>,
        supervisor: Option<Supervisor<W>>,
        required: Vec<BindingName>,
        volumes: Vec<VolumeSpec>,
        mounter: Box<dyn VolumeMounter>,
    ) -> Self {
        let volumes_mounted = volumes.is_empty();
        AgentRuntime {
            session,
            supervisor,
            required,
            digests: std::collections::HashMap::new(),
            running_digests: None,
            volumes,
            volumes_mounted,
            mounter,
        }
    }

    /// v1.7 (ato#994): a ZERO-binding, ZERO-volume supervisor config — a
    /// Dockerfile import with no secrets — is vacuously bound-ready with
    /// nothing left to mount, and NO host message will ever arrive to trigger
    /// the start (Deliver and MountVolumes are the existing triggers, and
    /// there is nothing to deliver or mount). Evaluate the gate ONCE at
    /// startup so the workload starts immediately. Deliberately scoped to
    /// exactly the vacuous case: any required binding keeps the v1.2 bind
    /// gate (`bound_ready` false here), any declared volume keeps the v1.6
    /// mount-before-start contract (`volumes_mounted` false here) — both
    /// no-op this method and stay message-driven, byte-for-byte as before.
    /// A start failure is surfaced on the console and left to the host's
    /// health verification (which fails honestly) — there is no host channel
    /// to report an Error to yet.
    fn drive_initial_start(&mut self) {
        if !self.volumes_mounted || !self.session.bound_ready(now_ms()) {
            return;
        }
        if let Some(sup) = self.supervisor.as_mut()
            && !sup.started()
        {
            match sup.on_bound_ready(true) {
                Ok(started) => {
                    if started {
                        self.running_digests = Some(
                            self.required
                                .iter()
                                .filter_map(|n| self.digests.get(n).map(|d| (n.clone(), *d)))
                                .collect(),
                        );
                        eprintln!(
                            "ato-guest-agent: vacuously bound-ready — workload started at boot"
                        );
                    }
                }
                Err(e) => {
                    eprintln!("ato-guest-agent: initial workload start failed: {e}");
                }
            }
        }
    }

    /// Handle one control message → (response JSON, should-stop). `StopWorkload` is
    /// routed to the supervisor (not the binding session); every other message goes
    /// to the session, after which the supervisor is driven from the settled binding
    /// state (v1.4 hard gate):
    ///
    /// - bound-ready and not started → start with the composed env;
    /// - bound-ready, started, but a required value ROTATED → stop + start with the
    ///   fresh env (a stale secret must not keep serving);
    /// - NOT bound-ready (revoke / expiry) with a started workload → stop it,
    ///   SYNCHRONOUSLY, before the response/ack is written back to the host — the
    ///   traffic gate is the workload's existence, not a proxy flag. A fresh
    ///   pre-bind session (never started) is the normal state, not a stop.
    ///
    /// Any start/stop failure is reported as an agent `Error` (fail-closed: never
    /// claim a state the workload does not match).
    fn dispatch(&mut self, line: &str) -> (String, bool) {
        let msg = match serde_json::from_str::<HostToAgent>(line) {
            Ok(m) => m,
            Err(e) => {
                // Never echo the input back (it may carry a secret).
                return (
                    serde_json::to_string(&AgentToHost::Error {
                        message: format!("malformed control message: {e}"),
                    })
                    .unwrap(),
                    false,
                );
            }
        };

        // v1.6 (ato#983) Slice 3 revision: MountVolumes is a restore-time
        // binding, handled independently of the session/supervisor state
        // machine below (mirrors StopWorkload's own early-return shape).
        // Idempotent: a retry (or a capsule with nothing declared) is a
        // harmless no-op success, never a re-mount-over-mounted attempt.
        if let HostToAgent::MountVolumes = msg {
            let resp = if self.volumes_mounted {
                AgentToHost::VolumesMounted
            } else {
                match mount_all_volumes(self.mounter.as_ref(), &self.volumes) {
                    Ok(()) => {
                        self.volumes_mounted = true;
                        AgentToHost::VolumesMounted
                    }
                    Err(e) => AgentToHost::Error {
                        message: format!("mount volumes: {e}"),
                    },
                }
            };
            return (serde_json::to_string(&resp).unwrap(), false);
        }

        if let HostToAgent::StopWorkload = msg {
            let resp = match self.supervisor.as_mut() {
                Some(sup) => match sup.stop_workload() {
                    Ok(was_running) => {
                        self.running_digests = None;
                        AgentToHost::WorkloadStopped { was_running }
                    }
                    Err(e) => AgentToHost::Error {
                        message: format!("stop workload: {e}"),
                    },
                },
                None => AgentToHost::WorkloadStopped { was_running: false },
            };
            return (serde_json::to_string(&resp).unwrap(), false);
        }

        // Digest the delivered value BEFORE the session consumes it (the value is
        // never retained — only this digest, in memory, for rotation detection).
        let delivered_digest: Option<(BindingName, [u8; 32])> = match &msg {
            HostToAgent::Deliver(d) => {
                use sha2::{Digest, Sha256};
                Some((
                    d.name.clone(),
                    Sha256::digest(d.value.expose().as_bytes()).into(),
                ))
            }
            _ => None,
        };

        let is_stop = matches!(msg, HostToAgent::Stop);
        let now = now_ms();
        let mut resp = self.session.handle(msg, now);
        if let (Some((name, digest)), AgentToHost::Ack { .. }) = (delivered_digest, &resp) {
            self.digests.insert(name, digest);
        }

        // Drive the workload after the binding state settles. Everything below
        // completes BEFORE the response is written — the host's ack ordering can
        // rely on the gate having actually happened.
        if let Some(sup) = self.supervisor.as_mut() {
            if is_stop {
                let _ = sup.stop_workload();
                self.running_digests = None;
            } else if !self.session.bound_ready(now) {
                // Hard gate (revoke / expiry after a bound session): stop the
                // workload so the injected env dies with it. Idempotent; a fresh
                // pre-bind session has nothing started and is NOT an error.
                if sup.started() {
                    match sup.stop_workload() {
                        Ok(_) => {
                            eprintln!(
                                "ato-guest-agent: bound-ready dropped — workload stopped (hard gate)"
                            );
                        }
                        Err(e) => {
                            // The old process may still be serving a revoked secret —
                            // never report this as a clean scrub.
                            resp = AgentToHost::Error {
                                message: format!("hard gate stop failed: {e}"),
                            };
                        }
                    }
                    self.running_digests = None;
                }
            } else if !sup.started() {
                match sup.on_bound_ready(true) {
                    Ok(started) => {
                        if started {
                            self.running_digests = Some(
                                self.required
                                    .iter()
                                    .filter_map(|n| self.digests.get(n).map(|d| (n.clone(), *d)))
                                    .collect(),
                            );
                        }
                    }
                    Err(e) => {
                        // The bindings are present but the workload failed to start —
                        // do not let the host believe the session is serving.
                        resp = AgentToHost::Error {
                            message: format!("supervisor start: {e}"),
                        };
                    }
                }
            } else {
                // Bound-ready with a running workload: restart iff the effective
                // required binding set changed (rotation) — a renewal that re-delivers
                // the SAME values is a no-op.
                let rotated: Vec<String> = match &self.running_digests {
                    None => Vec::new(),
                    Some(running) => self
                        .required
                        .iter()
                        .filter(|n| self.digests.get(*n) != running.get(*n))
                        .map(|n| n.as_str().to_string())
                        .collect(),
                };
                if !rotated.is_empty() {
                    let restart = sup.stop_workload().and_then(|_| sup.on_bound_ready(true));
                    match restart {
                        Ok(_) => {
                            eprintln!(
                                "ato-guest-agent: binding value rotated — workload restarted ({})",
                                rotated.join(", ")
                            );
                            self.running_digests = Some(
                                self.required
                                    .iter()
                                    .filter_map(|n| self.digests.get(n).map(|d| (n.clone(), *d)))
                                    .collect(),
                            );
                        }
                        Err(e) => {
                            self.running_digests = None;
                            resp = AgentToHost::Error {
                                message: format!("rotation restart failed: {e}"),
                            };
                        }
                    }
                }
            }
        }
        // v1.6 (ato#983) Slice 3: `Stop` (not `StopWorkload`, not a bound-ready
        // → false revoke — both of those can be followed by a restart of the
        // SAME session) is the true session-terminal message: the stdio loop
        // `break`s on it (`is_stop` is this function's second return value)
        // and vsock serving ends too. The workload is stopped FIRST (above),
        // then every durable volume is unmounted — best-effort, logged, never
        // fatal, since the host is about to kill this VM outright regardless.
        // Guarded on `volumes_mounted`: a session stopped before MountVolumes
        // ever arrived (e.g. BUILD's own pre-seal StopWorkload/Stop sequence,
        // where mounting deliberately never happens — see `MountVolumes`'s
        // doc comment) has nothing to unmount.
        if is_stop && self.volumes_mounted {
            unmount_all_volumes(self.mounter.as_ref(), &self.volumes);
        }
        (serde_json::to_string(&resp).unwrap(), is_stop)
    }
}

fn main() -> std::io::Result<()> {
    // ato#1026: `ato-guest-agent tcp-relay --listen <ip:port> --target <ip:port>`
    // is a standalone subcommand the generated init backgrounds for imports
    // that opted into the localhost→guest-IP relay. It never touches the
    // binding session / supervisor path below, so it is dispatched FIRST.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("tcp-relay") {
        if let Err(e) = guest_agent::relay::run(&argv[1..]) {
            eprintln!("ato-guest-agent: {e}");
            std::process::exit(2);
        }
        return Ok(());
    }

    let mode = std::env::var("ATO_GUEST_AGENT_MODE").unwrap_or_else(|_| "stdio".to_string());

    // Required binding names from argv; secrets are delivered to the default tmpfs root
    // (`/run/ato/bindings`), or `ATO_BINDINGS_ROOT` when set (tests point it at a tmp dir).
    let required: Vec<BindingName> = std::env::args()
        .skip(1)
        .filter_map(|a| BindingName::parse(a).ok())
        .collect();
    let root = bindings_root();
    let sink = TmpfsBindingSink::new(&root);
    let session = BindingSession::new(required.clone(), sink);

    // Supervisor: present only when /etc/ato/supervisor.json exists. A malformed
    // config for a supervisor capsule fails closed (the agent exits) rather than
    // launching the workload unbound.
    let (supervisor, volumes) = match SupervisorConfig::load(&config_path()) {
        Ok(Some(cfg)) => {
            // v1.6 (ato#983) Slice 3 revision: mounting durable state does
            // NOT happen here anymore. This runs during BUILD's own cold
            // boot too, and whatever happens here gets frozen into the
            // snapshot — mounting at boot would freeze this restore-time-
            // only filesystem state (page cache, block bitmaps) into that
            // snapshot forever. Every later restore instead sends
            // `HostToAgent::MountVolumes` fresh (see `AgentRuntime::dispatch`),
            // the same restore-time-binding treatment already used for
            // secrets. `cfg.volumes` is only carried through here so the
            // runtime knows what to mount WHEN that arrives.
            let volumes = cfg.volumes.clone();
            (
                Some(Supervisor::new(cfg, root.clone(), ChildWorkload::default)),
                volumes,
            )
        }
        Ok(None) => (None, Vec::new()),
        Err(e) => {
            eprintln!("ato-guest-agent: {e}");
            std::process::exit(2);
        }
    };

    let mut runtime = AgentRuntime::new(
        session,
        supervisor,
        required,
        volumes,
        Box::new(RealVolumeMounter),
    );
    // v1.7 (ato#994): zero-binding/zero-volume configs start their workload
    // now; every other config no-ops here and stays message-driven.
    runtime.drive_initial_start();

    match mode.as_str() {
        "stdio" => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let (resp, stop) = runtime.dispatch(&line);
                writeln!(out, "{resp}")?;
                out.flush()?;
                if stop {
                    break;
                }
            }
            Ok(())
        }
        "vsock" => {
            let port = std::env::var("ATO_GUEST_AGENT_VSOCK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_VSOCK_PORT);
            eprintln!("ato-guest-agent: vsock listening on port {port}");
            serve_vsock(port, |line| runtime.dispatch(line))
        }
        other => {
            eprintln!(
                "ato-guest-agent: unknown ATO_GUEST_AGENT_MODE={other:?} (expected stdio|vsock)"
            );
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guest_agent::supervisor::{SpawnPlan, SupervisorConfig};
    use protocol::binding_lease::{BindingLease, BindingLeaseId, SecretValue};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    /// A workload that records each spawn plan + how many stops — shared via Rc so the
    /// test can inspect it after moving it into the Supervisor.
    #[derive(Clone, Default)]
    struct SpyWorkload(Rc<SpyState>);
    #[derive(Default)]
    struct SpyState {
        running: RefCell<bool>,
        starts: RefCell<Vec<SpawnPlan>>,
        stops: RefCell<u32>,
    }
    impl Workload for SpyWorkload {
        fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()> {
            self.0.starts.borrow_mut().push(plan.clone());
            *self.0.running.borrow_mut() = true;
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<bool> {
            let was = *self.0.running.borrow();
            *self.0.running.borrow_mut() = false;
            *self.0.stops.borrow_mut() += 1;
            Ok(was)
        }
        fn is_running(&self) -> bool {
            *self.0.running.borrow()
        }
        fn run_once(&mut self, _: &SpawnPlan) -> std::io::Result<i32> {
            Ok(0) // Phase 6: no run_once tasks in the agent-runtime tests.
        }
    }

    fn deliver_line(name: &str, secret: &str) -> String {
        // dispatch() stamps `now` from the real wall clock, so the lease must expire
        // in the real future — expires_at_ms = issued + ttl. Use a far-future value
        // (leases are unix-millis vs the guest's real clock).
        let lease = BindingLease::issue(
            BindingLeaseId::new(format!("lease-{name}")),
            BindingName::parse(name).unwrap(),
            SecretValue::new(secret),
            0,
            100_000_000_000_000, // ~year 5138
        );
        serde_json::to_string(&HostToAgent::Deliver(lease.to_delivery())).unwrap()
    }

    /// v1.7 (ato#994): a ZERO-binding, ZERO-volume supervisor runtime — the
    /// Dockerfile-import shape (no secrets, no durable state).
    fn runtime_with_vacuous_supervisor(
        dir: &std::path::Path,
    ) -> (AgentRuntime<TmpfsBindingSink, SpyWorkload>, Rc<SpyState>) {
        let spy = SpyWorkload::default();
        let state = spy.0.clone();
        let cfg = SupervisorConfig {
            cmd: vec!["/app/serve".into()],
            cwd: "/".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
            volumes: Vec::new(),
        };
        let session = BindingSession::new(vec![], TmpfsBindingSink::new(dir));
        let sup = Supervisor::new(cfg, dir.to_path_buf(), move || spy.clone());
        (
            AgentRuntime::new(
                session,
                Some(sup),
                vec![],
                vec![],
                Box::new(RealVolumeMounter),
            ),
            state,
        )
    }

    #[test]
    fn vacuous_supervisor_starts_workload_at_boot() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, state) = runtime_with_vacuous_supervisor(dir.path());
        assert_eq!(
            state.starts.borrow().len(),
            0,
            "nothing started before the initial drive"
        );
        rt.drive_initial_start();
        assert_eq!(
            state.starts.borrow().len(),
            1,
            "zero-binding/zero-volume config must start at boot"
        );
        assert!(rt.running_digests.as_ref().is_some_and(|d| d.is_empty()));
        // Idempotent: a second drive never double-starts.
        rt.drive_initial_start();
        assert_eq!(state.starts.borrow().len(), 1);
    }

    #[test]
    fn initial_drive_noops_when_bindings_are_required() {
        // The v1.2 bind gate is untouched: a required binding keeps the start
        // message-driven (placeholder/real delivery), never at boot.
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, state) = runtime_with_supervisor(dir.path());
        rt.drive_initial_start();
        assert_eq!(
            state.starts.borrow().len(),
            0,
            "a required binding must keep the bind gate"
        );
    }

    #[test]
    fn initial_drive_noops_when_volumes_are_declared() {
        // The v1.6 mount-before-start contract is untouched: a declared volume
        // keeps the start behind MountVolumes.
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, state) =
            runtime_with_supervisor_and_volume(dir.path(), FakeMounter::default());
        rt.drive_initial_start();
        assert_eq!(
            state.starts.borrow().len(),
            0,
            "a declared volume must keep mount-before-start"
        );
    }

    fn runtime_with_supervisor(
        dir: &std::path::Path,
    ) -> (AgentRuntime<TmpfsBindingSink, SpyWorkload>, Rc<SpyState>) {
        let spy = SpyWorkload::default();
        let state = spy.0.clone();
        let cfg = SupervisorConfig {
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
            volumes: Vec::new(),
        };
        let required = vec![BindingName::parse("openai").unwrap()];
        let session = BindingSession::new(required.clone(), TmpfsBindingSink::new(dir));
        // Factory yields state-sharing spies (SpyWorkload clones share the Rc), so a
        // multi-service group aggregates its starts/stops into the one `state`.
        let sup = Supervisor::new(cfg, dir.to_path_buf(), move || spy.clone());
        (
            AgentRuntime::new(
                session,
                Some(sup),
                required,
                vec![],
                Box::new(RealVolumeMounter),
            ),
            state,
        )
    }

    // ── v1.6 (ato#983) Slice 3 revision: MountVolumes as a restore-time binding ──

    /// Records mount/unmount calls instead of shelling out to `blkid`/`mount`
    /// (which don't exist on non-Linux dev/CI hosts) — same style as
    /// `volume_mount.rs`'s own test-only `FakeMounter`, duplicated locally
    /// here since that one is private to its module.
    #[derive(Clone, Default)]
    struct FakeMounter {
        fail_mount: bool,
        mounts: Rc<RefCell<Vec<(String, String)>>>,
    }
    impl guest_agent::volume_mount::VolumeMounter for FakeMounter {
        fn resolve_device(&self, fs_label: &str) -> Result<std::path::PathBuf, String> {
            Ok(std::path::PathBuf::from(format!("/dev/fake-{fs_label}")))
        }
        fn mount(&self, device: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
            if self.fail_mount {
                return Err("fake: injected mount failure".to_string());
            }
            self.mounts
                .borrow_mut()
                .push((device.display().to_string(), target.display().to_string()));
            Ok(())
        }
        fn sync_and_umount(&self, _target: &std::path::Path) -> Result<(), String> {
            Ok(())
        }
    }

    fn runtime_with_supervisor_and_volume(
        dir: &std::path::Path,
        mounter: FakeMounter,
    ) -> (AgentRuntime<TmpfsBindingSink, SpyWorkload>, Rc<SpyState>) {
        let spy = SpyWorkload::default();
        let state = spy.0.clone();
        let volumes = vec![guest_agent::volume_mount::VolumeSpec {
            state_name: "dbdata".to_string(),
            target: "/ato/state/dbdata".to_string(),
            fs_label: "ASdeadbeefcafe01".to_string(),
            drive_id: "state0".to_string(),
            size_mb: 64,
        }];
        let cfg = SupervisorConfig {
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
            volumes: volumes.clone(),
        };
        let required = vec![BindingName::parse("openai").unwrap()];
        let session = BindingSession::new(required.clone(), TmpfsBindingSink::new(dir));
        let sup = Supervisor::new(cfg, dir.to_path_buf(), move || spy.clone());
        (
            AgentRuntime::new(session, Some(sup), required, volumes, Box::new(mounter)),
            state,
        )
    }

    #[test]
    fn deliver_without_ever_sending_mount_volumes_still_starts_the_workload() {
        // Deliberately NOT gated on the guest side: BUILD's own placeholder-
        // delivery flow (firecracker.rs) never sends MountVolumes at all —
        // by design, since mounting there would freeze filesystem state into
        // the snapshot (the original bug) — and it MUST still be able to
        // start the workload to pass its health check. The guest cannot
        // distinguish that flow from a real restore that skipped
        // MountVolumes by mistake, so this responsibility lives entirely in
        // the HOST's call ordering (see `mount_volumes_before_expose`'s doc
        // comment) — always MountVolumes before Deliver on an actual
        // restore, never on build.
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor_and_volume(dir.path(), FakeMounter::default());
        let (resp, _) = rt.dispatch(&deliver_line("openai", "sk-KEY"));
        assert!(resp.contains("ack"), "{resp}");
        assert!(
            spy.running.borrow().to_owned(),
            "matches BUILD's own flow, which never mounts"
        );
    }

    #[test]
    fn mount_volumes_then_deliver_starts_the_workload() {
        let dir = tempfile::tempdir().unwrap();
        let mounter = FakeMounter::default();
        let mounts = mounter.mounts.clone();
        let (mut rt, spy) = runtime_with_supervisor_and_volume(dir.path(), mounter);

        let (resp, stop) = rt.dispatch(&serde_json::to_string(&HostToAgent::MountVolumes).unwrap());
        assert!(!stop);
        assert!(resp.contains("volumes_mounted"), "{resp}");
        assert_eq!(
            mounts.borrow().len(),
            1,
            "the durable volume was actually mounted"
        );
        assert_eq!(mounts.borrow()[0].1, "/ato/state/dbdata");

        let (resp, _) = rt.dispatch(&deliver_line("openai", "sk-KEY"));
        assert!(resp.contains("ack"), "{resp}");
        assert!(
            spy.running.borrow().to_owned(),
            "workload starts once mounted AND bound-ready"
        );
    }

    #[test]
    fn mount_volumes_is_idempotent_a_second_call_does_not_remount() {
        let dir = tempfile::tempdir().unwrap();
        let mounter = FakeMounter::default();
        let mounts = mounter.mounts.clone();
        let (mut rt, _spy) = runtime_with_supervisor_and_volume(dir.path(), mounter);

        rt.dispatch(&serde_json::to_string(&HostToAgent::MountVolumes).unwrap());
        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::MountVolumes).unwrap());
        assert!(
            resp.contains("volumes_mounted"),
            "a repeat call is still a success, not an error: {resp}"
        );
        assert_eq!(
            mounts.borrow().len(),
            1,
            "the second MountVolumes must not re-mount"
        );
    }

    #[test]
    fn mount_volumes_failure_is_reported_and_does_not_flip_mounted_state() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, _spy) = runtime_with_supervisor_and_volume(
            dir.path(),
            FakeMounter {
                fail_mount: true,
                ..Default::default()
            },
        );

        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::MountVolumes).unwrap());
        assert!(resp.contains("error"), "{resp}");
        assert!(resp.contains("injected mount failure"), "{resp}");
        assert!(
            !rt.volumes_mounted,
            "a failed mount must not be recorded as mounted"
        );

        // A retry (e.g. the host reconnecting after fixing the underlying
        // issue) must actually attempt the mount again, not short-circuit
        // on a false "already mounted" idempotency check.
        rt.mounter = Box::new(FakeMounter::default());
        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::MountVolumes).unwrap());
        assert!(
            resp.contains("volumes_mounted"),
            "a retry with a working mounter must succeed: {resp}"
        );
    }

    #[test]
    fn no_volumes_declared_needs_no_mount_volumes_call() {
        // A capsule with no durable state is trivially "mounted" from the
        // start (volumes.is_empty()) — the ordinary no-volume tests already
        // cover this via `runtime_with_supervisor`, exercised here again
        // explicitly as the documented contract.
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        rt.dispatch(&deliver_line("openai", "sk-KEY"));
        assert!(
            spy.running.borrow().to_owned(),
            "no volumes declared ⇒ nothing blocks start"
        );
    }

    #[test]
    fn build_flow_never_mounts_volumes_at_construction() {
        // The bug this whole revision exists to fix: mounting must never
        // happen implicitly at AgentRuntime construction (which mirrors
        // main()'s boot sequence, frozen into the BUILD-time snapshot) —
        // only an explicit MountVolumes control message may trigger it.
        let dir = tempfile::tempdir().unwrap();
        let mounter = FakeMounter::default();
        let mounts = mounter.mounts.clone();
        let (_rt, _spy) = runtime_with_supervisor_and_volume(dir.path(), mounter);
        assert!(
            mounts.borrow().is_empty(),
            "constructing the runtime must not mount anything"
        );
    }

    #[test]
    fn build_flow_delivers_placeholder_starts_workload_then_stopworkload_idles_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());

        // Deliver the PLACEHOLDER binding → session bound-ready → supervisor starts
        // the workload with the placeholder env.
        let (resp, _stop) = rt.dispatch(&deliver_line("openai", "ATO-PLACEHOLDER-nonce"));
        assert!(resp.contains("ack"), "deliver acked: {resp}");
        assert!(
            spy.running.borrow().to_owned(),
            "workload started on bound-ready"
        );
        // The plan carries the tmpfs PATH, never the value (read only in the child).
        assert_eq!(spy.starts.borrow()[0].secret_env[0].0, "OPENAI_API_KEY");
        assert!(
            !format!("{:?}", spy.starts.borrow()[0]).contains("PLACEHOLDER"),
            "value must not enter the plan"
        );

        // Host sends StopWorkload before the pre-bind snapshot → workload idled.
        let (resp, stop) = rt.dispatch(&serde_json::to_string(&HostToAgent::StopWorkload).unwrap());
        assert!(!stop);
        assert!(resp.contains("workload_stopped"), "{resp}");
        assert!(resp.contains("\"was_running\":true"), "{resp}");
        assert!(
            !spy.running.borrow().to_owned(),
            "workload idle for the snapshot"
        );
        assert_eq!(*spy.stops.borrow(), 1);
    }

    #[test]
    fn restore_flow_restarts_workload_with_the_real_value() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        // Restore delivers the REAL value to tmpfs → bound-ready → workload starts.
        rt.dispatch(&deliver_line("openai", "sk-REAL-KEY"));
        assert!(spy.running.borrow().to_owned());
        // Exactly one start; the plan references the binding path, not the value.
        assert_eq!(spy.starts.borrow().len(), 1);
        assert_eq!(spy.starts.borrow()[0].secret_env[0].0, "OPENAI_API_KEY");
        assert!(!format!("{:?}", spy.starts.borrow()[0]).contains("sk-REAL-KEY"));
    }

    // ── v1.4 (ato#970): hard gate + rotation ──

    fn revoke_line(name: &str) -> String {
        serde_json::to_string(&HostToAgent::Revoke {
            id: BindingLeaseId::new(format!("lease-{name}")),
        })
        .unwrap()
    }

    /// Revoke of a required binding after a bound session STOPS the workload —
    /// synchronously, within the same dispatch that scrubs (the response is only
    /// produced after the stop completed).
    #[test]
    fn revoke_after_bound_session_stops_workload_synchronously() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        rt.dispatch(&deliver_line("openai", "sk-KEY-A"));
        assert!(
            spy.running.borrow().to_owned(),
            "bound-ready started the workload"
        );

        let (resp, _) = rt.dispatch(&revoke_line("openai"));
        assert!(resp.contains("scrubbed"), "{resp}");
        assert!(
            !spy.running.borrow().to_owned(),
            "hard gate: workload stopped on revoke"
        );
        assert_eq!(*spy.stops.borrow(), 1);
    }

    /// A fresh pre-bind session is bound-ready=false but has never started —
    /// control traffic must not trigger stop calls or errors.
    #[test]
    fn pre_bind_not_ready_is_not_a_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::QueryBoundReady).unwrap());
        assert!(resp.contains("\"ready\":false"), "{resp}");
        assert!(!resp.contains("error"), "{resp}");
        assert_eq!(*spy.stops.borrow(), 0, "nothing to stop pre-bind");
        assert!(spy.starts.borrow().is_empty());
    }

    /// Re-grant after a revoke restarts the workload with the CURRENT value.
    #[test]
    fn regrant_after_revoke_restarts_the_workload() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        rt.dispatch(&deliver_line("openai", "sk-KEY-A"));
        rt.dispatch(&revoke_line("openai"));
        assert!(!spy.running.borrow().to_owned());

        rt.dispatch(&deliver_line("openai", "sk-KEY-B"));
        assert!(spy.running.borrow().to_owned(), "re-grant restarts");
        assert_eq!(spy.starts.borrow().len(), 2);
    }

    /// Rotation: a required binding whose VALUE changes while the workload runs
    /// restarts it (stop + start) so the stale env never keeps serving; a renewal
    /// that re-delivers the SAME value is a no-op.
    #[test]
    fn rotation_restarts_and_same_value_renewal_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rt, spy) = runtime_with_supervisor(dir.path());
        rt.dispatch(&deliver_line("openai", "sk-KEY-A"));
        assert_eq!(spy.starts.borrow().len(), 1);

        // Same-value renewal: no restart.
        let (resp, _) = rt.dispatch(&deliver_line("openai", "sk-KEY-A"));
        assert!(resp.contains("ack"), "{resp}");
        assert_eq!(spy.starts.borrow().len(), 1, "same value must not restart");
        assert_eq!(*spy.stops.borrow(), 0);

        // Rotated value: stop + start with the fresh env.
        let (resp, _) = rt.dispatch(&deliver_line("openai", "sk-KEY-B"));
        assert!(resp.contains("ack"), "{resp}");
        assert_eq!(*spy.stops.borrow(), 1, "rotation stops the stale workload");
        assert_eq!(
            spy.starts.borrow().len(),
            2,
            "rotation restarts with fresh env"
        );
        assert!(spy.running.borrow().to_owned());
        // Digests never appear in responses; values never in spawn plans.
        assert!(!resp.contains("sk-KEY-B"));
        assert!(!format!("{:?}", spy.starts.borrow()[1]).contains("sk-KEY-B"));
    }

    #[test]
    fn stopworkload_without_a_supervisor_is_a_clean_no_op() {
        // A no-binding capsule has no supervisor; StopWorkload must not error.
        let mut rt: AgentRuntime<TmpfsBindingSink, ChildWorkload> = AgentRuntime::new(
            BindingSession::new(vec![], TmpfsBindingSink::at_default()),
            None,
            vec![],
            vec![],
            Box::new(RealVolumeMounter),
        );
        let (resp, _) = rt.dispatch(&serde_json::to_string(&HostToAgent::StopWorkload).unwrap());
        assert!(
            resp.contains("workload_stopped") && resp.contains("\"was_running\":false"),
            "{resp}"
        );
    }

    #[test]
    fn malformed_line_never_echoes_input() {
        let mut rt: AgentRuntime<TmpfsBindingSink, ChildWorkload> = AgentRuntime::new(
            BindingSession::new(vec![], TmpfsBindingSink::at_default()),
            None,
            vec![],
            vec![],
            Box::new(RealVolumeMounter),
        );
        let (resp, _) = rt.dispatch("{\"kind\":\"deliver\",\"secret\":\"leak-me\"}");
        assert!(resp.contains("malformed"), "{resp}");
        assert!(
            !resp.contains("leak-me"),
            "a malformed control line must never be echoed"
        );
    }
}
