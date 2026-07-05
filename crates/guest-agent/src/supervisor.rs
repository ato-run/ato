//! v1.2 supervisor mode — the guest-agent starts the workload with the composed
//! environment **after** bindings are delivered (the contract's named successor to
//! the impossible "rewrite a snapshotted process's environ", binding-lease.md §58).
//!
//! Flow (per the plan D1 / contract §7.1, `delivery = "env"`):
//!
//! - **Build:** boot → agent supervisor starts the workload with PLACEHOLDER
//!   bindings delivered over vsock → boot-verify health → host sends
//!   [`StopWorkload`](protocol::binding_control::HostToAgent::StopWorkload) → agent
//!   stops the app + the session scrubs the tmpfs → snapshot a workload-idle,
//!   secret-free image.
//! - **Restore:** deliver the REAL bindings → bound-ready → the agent (re)starts
//!   the workload with the env composed from the tmpfs binding files → health →
//!   expose. The value lives only on tmpfs + in the running process's env, never
//!   in the snapshot.
//!
//! The value is read from tmpfs into the child's environment at spawn and never
//! logged. Env composition and the child lifecycle are split behind [`Workload`]
//! so the orchestration is unit-testable without spawning real processes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use protocol::binding_lease::BindingName;
use serde::{Deserialize, Serialize};

use crate::tmpfs::DEFAULT_BINDINGS_ROOT;

/// Grace window between SIGTERM and SIGKILL when stopping the workload. Bounded so
/// `StopWorkload` (the pre-snapshot build boundary) always returns even if the
/// workload traps SIGTERM.
const STOP_GRACE_MS: u64 = 2000;

/// A POSIX-ish environment variable name: `^[A-Za-z_][A-Za-z0-9_]*$`. The name is
/// interpolated into the spawn shell script, so a malformed name is rejected at
/// config load (fail-closed), not sanitized.
fn valid_env_var_name(name: &str) -> bool {
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// v1.5 (ato#973): one process the supervisor manages. A capsule with a single
/// service is the common case (byte-identical to the pre-v1.5 single-`cmd`
/// config); a multi-service capsule (frontend + backend + redis…) lists several.
/// Holds NO secret value — `bindings_env` maps an env var name to the binding
/// NAME whose tmpfs value the agent reads at spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// Stable service name (diagnostics + per-service logs). Defaults to "app".
    #[serde(default = "default_service_name")]
    pub name: String,
    /// The workload argv (`["python3", "app.py"]` / `["/bin/sh", "-lc", "…"]`).
    pub cmd: Vec<String>,
    /// Working directory (default `/app`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Static environment (non-secret) applied before bindings.
    #[serde(default)]
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> binding name`. At spawn the agent reads
    /// `<bindings_root>/<binding>` and sets `ENV_VAR` to its contents.
    #[serde(default)]
    pub bindings_env: BTreeMap<String, String>,
    /// v1.5 readiness graph (ato#973): services this one must NOT start before —
    /// each named dependency must be READY (its readiness probe passes, or it is
    /// simply started when it declares no probe) first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// v1.5: how to tell this service is READY (so dependents may start). `None` =
    /// ready as soon as its process is started.
    #[serde(default)]
    pub readiness: Option<ReadinessSpec>,
}

/// v1.5 (ato#973): a service's readiness check. A dependent starts only once this
/// passes. Baseline is a TCP connect to `127.0.0.1:<port>`; when `http_path` is set
/// the probe additionally sends a minimal `GET <path>` and requires a response byte
/// (the app answered, not just accepted). Non-secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessSpec {
    /// The in-guest port the service listens on (loopback).
    pub port: u16,
    /// Optional HTTP path to GET; `None` = TCP-accept is enough.
    #[serde(default)]
    pub http_path: Option<String>,
}

/// `/etc/ato/supervisor.json` — how the guest-agent launches the workload(s).
/// Written into the rootfs by the builder for a supervisor (env-secret) capsule.
/// Holds NO secret value.
///
/// v1.5: backward-compatible superset. A single-service config keeps the legacy
/// top-level `cmd`/`cwd`/`base_env`/`bindings_env` (and no `services`); a
/// multi-service config lists `services` and omits the top-level `cmd`. Exactly
/// one shape must be present — see [`SupervisorConfig::services`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// LEGACY single-service argv. Empty when `services` is used.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Working directory for the legacy single service (default `/app`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Static environment for the legacy single service.
    #[serde(default)]
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> binding name` for the legacy single service.
    #[serde(default)]
    pub bindings_env: BTreeMap<String, String>,
    /// v1.5 multi-service list. When non-empty this is authoritative and the
    /// legacy top-level fields are ignored.
    #[serde(default)]
    pub services: Vec<ServiceSpec>,
}

fn default_cwd() -> String {
    "/app".to_string()
}

fn default_service_name() -> String {
    "app".to_string()
}

impl SupervisorConfig {
    /// The NORMALIZED service list the supervisor drives: `services` when present,
    /// else a single service synthesized from the legacy top-level fields. The
    /// synthesized service is named "app" (matching the legacy sole workload).
    pub fn services(&self) -> Vec<ServiceSpec> {
        if !self.services.is_empty() {
            return self.services.clone();
        }
        vec![ServiceSpec {
            name: default_service_name(),
            cmd: self.cmd.clone(),
            cwd: self.cwd.clone(),
            base_env: self.base_env.clone(),
            bindings_env: self.bindings_env.clone(),
            depends_on: Vec::new(),
            readiness: None,
        }]
    }
}

impl ServiceSpec {
    /// Fail-closed per-service validation (see [`SupervisorConfig::validate`]).
    fn validate(&self) -> Result<(), String> {
        if self.cmd.is_empty() {
            return Err(format!("supervisor.json: service {:?} has empty `cmd`", self.name));
        }
        if self.name.trim().is_empty() {
            return Err("supervisor.json: a service has an empty `name`".into());
        }
        for var in self.base_env.keys() {
            if !valid_env_var_name(var) {
                return Err(format!(
                    "supervisor.json: service {:?} invalid base_env var name {var:?}",
                    self.name
                ));
            }
        }
        for (var, binding) in &self.bindings_env {
            if !valid_env_var_name(var) {
                return Err(format!(
                    "supervisor.json: service {:?} invalid bindings_env var name {var:?}",
                    self.name
                ));
            }
            BindingName::parse(binding.as_str()).map_err(|e| {
                format!("supervisor.json: service {:?} invalid binding name {binding:?}: {e}", self.name)
            })?;
        }
        Ok(())
    }
}

impl SupervisorConfig {
    /// Parse `/etc/ato/supervisor.json`; a malformed/empty config is an error
    /// (fail-closed — a supervisor capsule must not fall back to launching with no
    /// bindings).
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let cfg: SupervisorConfig =
            serde_json::from_str(raw).map_err(|e| format!("supervisor.json parse: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Fail-closed validation: a malformed config must be rejected, never sanitized
    /// or silently launched. `cmd` non-empty; every `bindings_env` env var name is a
    /// valid POSIX identifier (it is interpolated into the spawn shell script) and
    /// every binding name is a valid [`BindingName`] (it is joined onto the tmpfs
    /// root). `base_env` names are validated too.
    pub fn validate(&self) -> Result<(), String> {
        // Exactly one shape: either the legacy top-level single service OR a
        // `services` list — never both, never neither (fail-closed on an ambiguous
        // config). When `services` is set, EVERY legacy top-level field must be
        // empty: `services()` treats the list as authoritative and would otherwise
        // SILENTLY IGNORE a top-level `base_env`/`bindings_env` — dropping a secret
        // requirement (a builder that mistakenly puts a common binding at the top
        // level would ship a supervisor that starts unbound). `cwd` has a serde
        // default so an explicit "/app" is indistinguishable from the default; it
        // is intentionally not checked here (schema hardening is a follow-up).
        if !self.services.is_empty() {
            let mut leaked = Vec::new();
            if !self.cmd.is_empty() {
                leaked.push("cmd");
            }
            if !self.base_env.is_empty() {
                leaked.push("base_env");
            }
            if !self.bindings_env.is_empty() {
                leaked.push("bindings_env");
            }
            if !leaked.is_empty() {
                return Err(format!(
                    "supervisor.json: `services` is set, so top-level {} must be empty \
                     (put per-service config inside each service, never at the top level)",
                    leaked.join("/")
                ));
            }
        }
        let services = self.services();
        if services.is_empty() {
            return Err("supervisor.json: no service (`cmd` and `services` both empty)".into());
        }
        // Unique service names (they key per-service logs + diagnostics).
        let mut seen = std::collections::BTreeSet::new();
        for svc in &services {
            svc.validate()?;
            if !seen.insert(svc.name.clone()) {
                return Err(format!("supervisor.json: duplicate service name {:?}", svc.name));
            }
        }
        // v1.5 readiness graph: `depends_on` must reference declared services, must
        // not self-loop, and the graph must be acyclic (the start order is a
        // topological sort — a cycle has no valid order). Validate now, fail-closed.
        self.start_order()?;
        Ok(())
    }

    /// v1.5 (ato#973): the deterministic START ORDER — a topological sort of the
    /// services by `depends_on` (a service comes after everything it depends on).
    /// Ties break by name for reproducibility. Errors fail-closed on an unknown or
    /// self dependency and on a cycle. Returns names in start order.
    pub fn start_order(&self) -> Result<Vec<String>, String> {
        let services = self.services();
        let names: std::collections::BTreeSet<String> =
            services.iter().map(|s| s.name.clone()).collect();
        // Adjacency (deps) validated against the declared set.
        let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for svc in &services {
            for d in &svc.depends_on {
                if d == &svc.name {
                    return Err(format!("supervisor.json: service {:?} depends on itself", svc.name));
                }
                if !names.contains(d) {
                    return Err(format!(
                        "supervisor.json: service {:?} depends_on {:?} which is not a declared service",
                        svc.name, d
                    ));
                }
            }
            deps.insert(svc.name.clone(), svc.depends_on.clone());
        }
        // Kahn-ish DFS post-order over a name-sorted iteration (deterministic).
        let mut order: Vec<String> = Vec::with_capacity(services.len());
        let mut state: BTreeMap<String, u8> = BTreeMap::new(); // 0=unseen 1=in-progress 2=done
        fn visit(
            n: &str,
            deps: &BTreeMap<String, Vec<String>>,
            state: &mut BTreeMap<String, u8>,
            order: &mut Vec<String>,
        ) -> Result<(), String> {
            match state.get(n).copied().unwrap_or(0) {
                2 => return Ok(()),
                1 => return Err(format!("supervisor.json: dependency cycle involving service {n:?}")),
                _ => {}
            }
            state.insert(n.to_string(), 1);
            let mut ds = deps.get(n).cloned().unwrap_or_default();
            ds.sort();
            for d in ds {
                visit(&d, deps, state, order)?;
            }
            state.insert(n.to_string(), 2);
            order.push(n.to_string());
            Ok(())
        }
        for name in &names {
            visit(name, &deps, &mut state, &mut order)?;
        }
        Ok(order)
    }

    /// Load from `path` (default `/etc/ato/supervisor.json`, or `ATO_SUPERVISOR_CONFIG`).
    /// Returns `Ok(None)` when the file is absent — a no-supervisor (v1.0) capsule.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path) {
            Ok(raw) => Self::from_json(&raw).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

/// How to spawn the workload. The secret bindings are carried as **tmpfs FILE
/// PATHS, never values** — a KVM finding (PR 3b): a long-lived agent that read the
/// value into its own heap left the secret resident in guest RAM (init_on_free only
/// zeroes *freed* pages, not the live agent's heap). So the value is read **only in
/// the workload child**, at exec time, and lives solely in that child's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub cmd: Vec<String>,
    pub cwd: String,
    /// Non-secret env, applied to the child directly.
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> tmpfs file path`. The child reads each at exec; the agent never
    /// reads the value.
    pub secret_env: Vec<(String, PathBuf)>,
}

/// Build the spawn plan from the config + bindings root. Verifies each binding file
/// **exists** (fail-closed — never start half-bound) WITHOUT reading its contents, so
/// no value enters the agent's address space.
/// LEGACY single-service entry point. Plans the sole normalized service. A
/// MULTI-service config is rejected (`InvalidInput`) rather than silently
/// planning only `services[0]` — a caller with >1 service must plan each via
/// [`plan_spawn_service`] (as [`Supervisor::on_bound_ready`] does). This
/// fail-closed guard surfaces a config-plumbing mistake immediately instead of
/// silently starting only the first service.
pub fn plan_spawn(config: &SupervisorConfig, bindings_root: &Path) -> std::io::Result<SpawnPlan> {
    // Defense in depth: never plan a spawn from a config that would not have passed
    // load-time validation (invalid env/binding names never reach the shell script).
    config
        .validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    if config.services.len() > 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plan_spawn is single-service only; a multi-service config must plan each \
             service via plan_spawn_service",
        ));
    }
    let services = config.services();
    plan_spawn_service(&services[0], bindings_root)
}

/// Plan the spawn for ONE service (v1.5). Verifies each binding file EXISTS
/// (fail-closed — never start half-bound) without reading its contents.
pub fn plan_spawn_service(service: &ServiceSpec, bindings_root: &Path) -> std::io::Result<SpawnPlan> {
    service
        .validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut secret_env = Vec::with_capacity(service.bindings_env.len());
    for (var, binding) in &service.bindings_env {
        let path = bindings_root.join(binding);
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "service '{}': binding '{binding}' for env '{var}' absent at {}",
                    service.name,
                    path.display()
                ),
            ));
        }
        secret_env.push((var.clone(), path));
    }
    Ok(SpawnPlan {
        cmd: service.cmd.clone(),
        cwd: service.cwd.clone(),
        base_env: service.base_env.clone(),
        secret_env,
    })
}

/// POSIX single-quote a string for safe embedding in an `sh -c` script.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` script that reads each secret from its tmpfs file into the env, then
/// `exec`s the workload — so the value only ever materializes inside the child (which
/// becomes the workload via exec), never in the agent. `cmd`/paths are shell-quoted.
fn spawn_script(plan: &SpawnPlan) -> String {
    let mut script = String::new();
    for (var, path) in &plan.secret_env {
        // export VAR="$(cat 'path')" — the value is read by the subshell, held only
        // in this child's environment, and never appears in argv/the process table.
        script.push_str(&format!(
            "export {var}=\"$(cat {})\"\n",
            shell_single_quote(&path.to_string_lossy())
        ));
    }
    let quoted: Vec<String> = plan.cmd.iter().map(|a| shell_single_quote(a)).collect();
    script.push_str(&format!("exec {}\n", quoted.join(" ")));
    script
}

/// The workload process, behind a trait so the supervisor orchestration is testable
/// without spawning. `start` receives the [`SpawnPlan`] (secret paths, not values).
pub trait Workload {
    fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()>;
    /// Stop the workload (SIGTERM then reap). Idempotent — not running ⇒ Ok(false).
    fn stop(&mut self) -> std::io::Result<bool>;
    /// Whether the workload child is currently running.
    fn is_running(&self) -> bool;
}

/// A real OS process workload (`std::process::Command`), used by the guest binary.
/// Spawns `sh -c <script>` so the secret is read in the child at exec, never in the
/// agent's heap.
#[derive(Default)]
pub struct ChildWorkload {
    child: Option<std::process::Child>,
}

impl Workload for ChildWorkload {
    fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()> {
        if plan.cmd.is_empty() {
            return Err(std::io::Error::other("supervisor cmd is empty"));
        }
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(spawn_script(plan)).current_dir(&plan.cwd);
        #[cfg(unix)]
        {
            // Own process group (pgid = child pid). The supervisor cmd is often a
            // shell wrapper (the rootfs builder emits `/bin/sh -lc <start_cmd>`), so
            // the real app can be a GRANDCHILD of the spawned pid — `stop` must take
            // down the whole tree via killpg, not kill. (PR 3d finding: single-PID
            // SIGTERM killed only the wrapper shell, the orphaned app kept serving,
            // and the "stopped" pre-seal snapshot captured a RUNNING workload.)
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        for (k, v) in &plan.base_env {
            c.env(k, v); // non-secret only
        }
        self.child = Some(c.spawn()?);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<bool> {
        match self.child.take() {
            Some(mut ch) => {
                // BOUNDED stop (this is the pre-snapshot build boundary — StopWorkload
                // must always return): SIGTERM the whole PROCESS GROUP, wait up to a
                // grace window, then SIGKILL a workload that ignored SIGTERM, and
                // reap. killpg (pgid = child pid, set at spawn) is load-bearing: the
                // cmd may be a shell wrapper whose real app is a grandchild — a
                // single-PID kill would orphan it still serving (PR 3d finding). A
                // workload that traps SIGTERM cannot stall the seal.
                #[cfg(unix)]
                let pgid = ch.id() as i32;
                #[cfg(unix)]
                unsafe {
                    libc::killpg(pgid, libc::SIGTERM);
                }
                let grace = std::time::Duration::from_millis(STOP_GRACE_MS);
                let step = std::time::Duration::from_millis(20);
                let deadline = std::time::Instant::now() + grace;
                let mut reaped = false;
                loop {
                    // Reap the direct child as soon as it exits (a zombie leader
                    // would otherwise keep the group probe alive forever).
                    if !reaped && ch.try_wait()?.is_some() {
                        reaped = true;
                    }
                    // "Stopped" = the whole GROUP is gone, not just the direct
                    // child — a wrapper shell can exit while its grandchild (the
                    // real app) survives. killpg(sig 0) probes remaining members.
                    #[cfg(unix)]
                    let group_alive = unsafe { libc::killpg(pgid, 0) == 0 };
                    #[cfg(not(unix))]
                    let group_alive = !reaped;
                    if !group_alive {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        // Grace expired with survivors — SIGKILL the group
                        // (unblockable, so the seal boundary stays bounded).
                        #[cfg(unix)]
                        unsafe {
                            libc::killpg(pgid, libc::SIGKILL);
                        }
                        break;
                    }
                    std::thread::sleep(step);
                }
                if !reaped {
                    let _ = ch.wait(); // reap the (SIGKILLed) direct child
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn is_running(&self) -> bool {
        self.child.is_some()
    }
}

/// Ties a [`SupervisorConfig`] to a [`Workload`] + bindings root. The guest binary
/// calls [`Supervisor::on_bound_ready`] after each control message (idempotent start
/// once all bindings are present) and [`Supervisor::stop_workload`] on `StopWorkload`.
/// v1.5 (ato#973): the supervisor manages a GROUP of service processes as a unit.
/// One workload per normalized service; the whole group starts on bound-ready,
/// stops on revoke/teardown, and restarts on rotation (the v1.4 hard gate, now
/// applied to every service). A single-service capsule is the group-of-one case,
/// byte-identical to the pre-v1.5 behaviour.
///
/// Workloads are produced by a factory so the caller controls the concrete type
/// (production: `ChildWorkload::default`; tests: a shared-state spy). The group is
/// empty until the first `on_bound_ready(true)`.
/// v1.5 (ato#973): a single, non-blocking readiness check. The supervisor loops
/// this (with a bounded budget) between dependency levels. Injected so tests can
/// drive readiness deterministically without real sockets.
pub trait ReadinessProbe {
    /// One check: is the service described by `spec` accepting/answering NOW?
    fn is_ready(&self, spec: &ReadinessSpec) -> bool;
}

/// Production probe: TCP connect to `127.0.0.1:<port>` and, when `http_path` is set,
/// send a minimal `GET` and require a response byte (the app answered, not just
/// accepted). Loopback only — a service is reached inside its own guest.
pub struct TcpReadinessProbe;

impl ReadinessProbe for TcpReadinessProbe {
    fn is_ready(&self, spec: &ReadinessSpec) -> bool {
        use std::io::{Read, Write};
        let addr = format!("127.0.0.1:{}", spec.port);
        let Ok(sockaddr) = addr.parse::<std::net::SocketAddr>() else { return false };
        let Ok(mut stream) =
            std::net::TcpStream::connect_timeout(&sockaddr, Duration::from_millis(500))
        else {
            return false;
        };
        let Some(path) = &spec.http_path else {
            return true; // TCP accept is enough.
        };
        let p = if path.starts_with('/') { path.clone() } else { format!("/{path}") };
        let req = format!("GET {p} HTTP/1.0\r\nHost: ato-readiness\r\n\r\n");
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        if stream.write_all(req.as_bytes()).is_err() {
            return false;
        }
        let mut buf = [0u8; 1];
        matches!(stream.read(&mut buf), Ok(n) if n > 0)
    }
}

/// Default budget (ms) to wait for a dependency to become ready before failing the
/// group. `ATO_READINESS_TIMEOUT_MS` overrides; clamped ≥ 1s.
fn default_readiness_timeout() -> Duration {
    let ms = std::env::var("ATO_READINESS_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.max(1_000))
        .unwrap_or(30_000);
    Duration::from_millis(ms)
}

pub struct Supervisor<W: Workload> {
    config: SupervisorConfig,
    bindings_root: PathBuf,
    make_workload: Box<dyn FnMut() -> W>,
    workloads: Vec<W>,
    started: bool,
    probe: Box<dyn ReadinessProbe>,
    readiness_timeout: Duration,
    /// Poll interval while waiting for a dependency's readiness.
    readiness_poll: Duration,
}

impl<W: Workload> Supervisor<W> {
    pub fn new(
        config: SupervisorConfig,
        bindings_root: impl Into<PathBuf>,
        make_workload: impl FnMut() -> W + 'static,
    ) -> Self {
        Supervisor {
            config,
            bindings_root: bindings_root.into(),
            make_workload: Box::new(make_workload),
            workloads: Vec::new(),
            started: false,
            probe: Box::new(TcpReadinessProbe),
            readiness_timeout: default_readiness_timeout(),
            readiness_poll: Duration::from_millis(100),
        }
    }

    /// Override the readiness probe (tests). Chainable.
    pub fn with_probe(mut self, probe: impl ReadinessProbe + 'static) -> Self {
        self.probe = Box::new(probe);
        self
    }

    /// Override the readiness wait budget + poll interval (tests). Chainable.
    pub fn with_readiness_timing(mut self, timeout: Duration, poll: Duration) -> Self {
        self.readiness_timeout = timeout;
        self.readiness_poll = poll;
        self
    }

    /// Start EVERY service exactly once, when the session is bound-ready. Each
    /// service's env is composed from tmpfs and spawned. A compose/spawn error on
    /// any service is fail-closed: already-started services in this call are
    /// stopped so the caller never sees a partially-running group reported healthy.
    /// No-op if already started or not yet bound-ready.
    pub fn on_bound_ready(&mut self, bound_ready: bool) -> std::io::Result<bool> {
        if self.started || !bound_ready {
            return Ok(false);
        }
        // v1.5 readiness graph: start in dependency (topological) order and, before
        // starting a service, WAIT for each of its dependencies to be READY — a
        // dependency with a readiness probe must actually answer, not merely have a
        // process spawned. `start_order()` already fail-closed on cycle/unknown dep
        // at validation; re-derive here for the concrete order.
        let order = self
            .config
            .start_order()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let services = self.config.services();
        let by_name: BTreeMap<&str, &ServiceSpec> =
            services.iter().map(|s| (s.name.as_str(), s)).collect();
        // Readiness of a started service, remembered so a later dependent can gate.
        let mut ready: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for name in &order {
            let svc = by_name[name.as_str()];
            // Gate: every dependency must be READY first.
            for dep in &svc.depends_on {
                if !ready.contains(dep) {
                    let dep_spec = by_name[dep.as_str()];
                    if let Err(e) = self.wait_ready(dep, dep_spec) {
                        self.stop_all_started();
                        return Err(e);
                    }
                    ready.insert(dep.clone());
                }
            }
            let plan = match plan_spawn_service(svc, &self.bindings_root) {
                Ok(p) => p,
                Err(e) => {
                    self.stop_all_started();
                    return Err(e);
                }
            };
            let mut w = (self.make_workload)();
            if let Err(e) = w.start(&plan) {
                self.stop_all_started();
                return Err(e);
            }
            self.workloads.push(w);
        }
        self.started = true;
        Ok(true)
    }

    /// Block until `svc` passes its readiness probe, up to the readiness budget. A
    /// service with NO readiness spec is ready as soon as it is started (this is
    /// only called for a started service). Fail-closed: a dependency that never
    /// becomes ready is a `TimedOut` error (the caller rolls the group back).
    fn wait_ready(&self, name: &str, svc: &ServiceSpec) -> std::io::Result<()> {
        let Some(spec) = &svc.readiness else {
            return Ok(()); // started == ready when no probe is declared.
        };
        let deadline = std::time::Instant::now() + self.readiness_timeout;
        loop {
            if self.probe.is_ready(spec) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "service {name:?} did not become ready on 127.0.0.1:{} within {}ms",
                        spec.port,
                        self.readiness_timeout.as_millis()
                    ),
                ));
            }
            std::thread::sleep(self.readiness_poll);
        }
    }

    /// Stop the group (pre-snapshot at build, or teardown). Returns whether ANY
    /// service was running. Leaves `started=false` so a later bound-ready starts a
    /// fresh group with the real env. Every service is stopped even if one errors.
    pub fn stop_workload(&mut self) -> std::io::Result<bool> {
        let mut any_running = false;
        let mut first_err: Option<std::io::Error> = None;
        for mut w in self.workloads.drain(..) {
            match w.stop() {
                Ok(was) => any_running |= was,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        self.started = false;
        match first_err {
            Some(e) => Err(e),
            None => Ok(any_running),
        }
    }

    /// Best-effort stop of workloads started so far (partial-start rollback). Never
    /// errors — this is the fail-closed cleanup path.
    fn stop_all_started(&mut self) {
        for mut w in self.workloads.drain(..) {
            let _ = w.stop();
        }
        self.started = false;
    }

    pub fn is_running(&self) -> bool {
        self.workloads.iter().any(|w| w.is_running())
    }

    /// Whether a bound-ready start has happened (and no stop since). The v1.4 hard
    /// gate uses this to distinguish "fresh pre-bind session" (false — normal, no
    /// stop needed) from "bound session that lost a binding" (true — stop NOW).
    pub fn started(&self) -> bool {
        self.started
    }
}

/// The default supervisor config path inside the guest (`ATO_SUPERVISOR_CONFIG`
/// overrides for tests).
pub fn config_path() -> PathBuf {
    match std::env::var("ATO_SUPERVISOR_CONFIG") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from("/etc/ato/supervisor.json"),
    }
}

/// The bindings root the supervisor reads values from (matches the delivery sink).
pub fn bindings_root() -> PathBuf {
    match std::env::var("ATO_BINDINGS_ROOT") {
        Ok(r) if !r.is_empty() => PathBuf::from(r),
        _ => PathBuf::from(DEFAULT_BINDINGS_ROOT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn write(dir: &Path, name: &str, value: &str) {
        std::fs::write(dir.join(name), value).unwrap();
    }

    #[test]
    fn config_parses_and_rejects_empty_cmd() {
        let cfg = SupervisorConfig::from_json(
            r#"{"cmd":["python3","app.py"],"bindings_env":{"OPENAI_API_KEY":"openai"}}"#,
        )
        .unwrap();
        assert_eq!(cfg.cmd, vec!["python3", "app.py"]);
        assert_eq!(cfg.cwd, "/app"); // default
        assert_eq!(cfg.bindings_env.get("OPENAI_API_KEY").map(String::as_str), Some("openai"));
        assert!(SupervisorConfig::from_json(r#"{"cmd":[]}"#).is_err());
        assert!(SupervisorConfig::from_json("not json").is_err());
    }

    // ── v1.5 (ato#973): multi-service process group ──

    #[test]
    fn config_parses_a_multi_service_list_and_back_compat_single_cmd() {
        // Legacy single-cmd config normalizes to one "app" service.
        let legacy = SupervisorConfig::from_json(r#"{"cmd":["python3","app.py"]}"#).unwrap();
        let svcs = legacy.services();
        assert_eq!(svcs.len(), 1);
        assert_eq!(svcs[0].name, "app");
        assert_eq!(svcs[0].cmd, vec!["python3", "app.py"]);

        // Multi-service config: authoritative `services`, top-level cmd omitted.
        let multi = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"backend","cmd":["python3","api.py"],"cwd":"/app/api"},
                {"name":"redis","cmd":["redis-server"]}
            ]}"#,
        )
        .unwrap();
        let svcs = multi.services();
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[0].name, "backend");
        assert_eq!(svcs[0].cwd, "/app/api");
        assert_eq!(svcs[1].name, "redis");
        assert_eq!(svcs[1].cwd, "/app"); // default
    }

    #[test]
    fn config_rejects_any_legacy_top_level_field_mixed_with_services() {
        // `services` is authoritative, so ANY legacy top-level field alongside it
        // would be SILENTLY IGNORED — a top-level bindings_env would drop a secret
        // requirement. Every mix must fail-close, not just cmd.
        let bad = |json: &str| assert!(SupervisorConfig::from_json(json).is_err(), "{json}");
        // services + top-level cmd (existing).
        bad(r#"{"cmd":["a"],"services":[{"cmd":["b"]}]}"#);
        // services + top-level bindings_env → MUST reject (dropped secret).
        bad(r#"{"services":[{"name":"api","cmd":["python3","api.py"]}],"bindings_env":{"OPENAI_API_KEY":"openai_api_key"}}"#);
        // services + top-level base_env → MUST reject.
        bad(r#"{"services":[{"name":"api","cmd":["a"]}],"base_env":{"NODE_ENV":"production"}}"#);
        // services-only → accepted.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"api","cmd":["a"],"bindings_env":{"K":"openai"}}]}"#
            )
            .is_ok(),
            "per-service bindings_env is the correct place"
        );
        // legacy top-level only → accepted.
        assert!(
            SupervisorConfig::from_json(
                r#"{"cmd":["a"],"bindings_env":{"K":"openai"},"base_env":{"NODE_ENV":"x"}}"#
            )
            .is_ok(),
            "legacy single-service shape still accepted"
        );
    }

    #[test]
    fn plan_spawn_rejects_a_multi_service_config_instead_of_planning_services_zero() {
        let dir = tempfile::tempdir().unwrap();
        let multi = SupervisorConfig::from_json(
            r#"{"services":[{"name":"a","cmd":["true"]},{"name":"b","cmd":["true"]}]}"#,
        )
        .unwrap();
        let err = plan_spawn(&multi, dir.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        // A single-service `services` list is still plannable via the legacy entry.
        let one = SupervisorConfig::from_json(r#"{"services":[{"name":"a","cmd":["true"]}]}"#).unwrap();
        assert!(plan_spawn(&one, dir.path()).is_ok());
    }

    #[test]
    fn config_rejects_neither_shape_and_duplicate_names() {
        // Neither → rejected.
        assert!(SupervisorConfig::from_json(r#"{}"#).is_err(), "empty config rejected");
        assert!(SupervisorConfig::from_json(r#"{"services":[]}"#).is_err(), "empty services rejected");
        // A service with empty cmd → rejected.
        assert!(
            SupervisorConfig::from_json(r#"{"services":[{"cmd":[]}]}"#).is_err(),
            "service with empty cmd rejected"
        );
        // Duplicate service names → rejected (they key per-service logs).
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]},{"name":"x","cmd":["b"]}]}"#
            )
            .is_err(),
            "duplicate service names rejected"
        );
        // Per-service binding/env validation still fail-closed.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"],"bindings_env":{"BAD NAME":"openai"}}]}"#
            )
            .is_err(),
            "invalid env var in a service rejected"
        );
    }

    #[test]
    fn supervisor_starts_and_stops_the_WHOLE_group_as_a_unit() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-KEY");
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"backend","cmd":["true"],"bindings_env":{"OPENAI_API_KEY":"openai"}},
                {"name":"worker","cmd":["true"]},
                {"name":"redis","cmd":["true"]}
            ]}"#,
        )
        .unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let mut sup = Supervisor::new(cfg, dir.path(), move || fake.clone());

        // Bound-ready → ALL three services start.
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        assert_eq!(st.starts.borrow().len(), 3, "every service started");
        assert_eq!(*st.live.borrow(), 3);

        // Idempotent.
        assert!(!sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 3);

        // Stop → the WHOLE group is torn down (was_running=true, all live gone).
        assert!(sup.stop_workload().unwrap());
        assert!(!sup.is_running());
        assert_eq!(*st.live.borrow(), 0);
        assert_eq!(*st.stops.borrow(), 3, "every service stopped");

        // Rotation-style restart re-plans + restarts the whole group.
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 6);
        assert_eq!(*st.live.borrow(), 3);
    }

    #[test]
    fn a_service_that_fails_to_start_rolls_back_the_group_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        // Second service requires a binding that is ABSENT ⇒ plan fails after the
        // first already started ⇒ the group must roll back (no partial serving).
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"ok","cmd":["true"]},
                {"name":"broken","cmd":["true"],"bindings_env":{"KEY":"absent"}}
            ]}"#,
        )
        .unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let mut sup = Supervisor::new(cfg, dir.path(), move || fake.clone());

        assert!(sup.on_bound_ready(true).is_err(), "missing binding fails the group");
        assert!(!sup.is_running(), "the started service was rolled back");
        assert!(!sup.started(), "group is not marked started");
        assert_eq!(*st.live.borrow(), 0, "no service left running");
    }

    // ── v1.5 (ato#973): readiness graph — dependency-ordered start + wait ──

    #[test]
    fn start_order_is_a_topological_sort_and_rejects_cycles() {
        // api depends on redis → redis starts first. Ties break by name.
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"api","cmd":["true"],"depends_on":["redis"]},
                {"name":"redis","cmd":["true"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(cfg.start_order().unwrap(), vec!["redis", "api"]);

        // Unknown dependency → rejected at parse (validate calls start_order).
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"],"depends_on":["ghost"]}]}"#
        )
        .is_err());
        // Self dependency → rejected.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"],"depends_on":["api"]}]}"#
        )
        .is_err());
        // Cycle → rejected.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"a","cmd":["true"],"depends_on":["b"]},
                {"name":"b","cmd":["true"],"depends_on":["a"]}
            ]}"#
        )
        .is_err());
    }

    #[test]
    fn a_dependent_waits_for_its_dependency_to_become_ready_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        // api depends on redis; redis declares a readiness probe on 6379 that only
        // passes after 3 checks. api must start AFTER redis is ready.
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"api","cmd":["api"],"depends_on":["redis"]},
                {"name":"redis","cmd":["redis"],"readiness":{"port":6379}}
            ]}"#,
        )
        .unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let probe = FakeProbe::ready_after(6379, 3);
        let pst = probe.0.clone();
        let mut sup = fast_readiness(Supervisor::new(cfg, dir.path(), move || fake.clone()), probe);

        assert!(sup.on_bound_ready(true).unwrap());
        // Both started, redis before api (topological).
        let starts = st.starts.borrow();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].cmd, vec!["redis"], "redis starts first");
        assert_eq!(starts[1].cmd, vec!["api"], "api starts after redis is ready");
        // The probe was polled until redis answered (≥ the countdown of 3 + the
        // final success = 4).
        assert!(pst.borrow().calls.iter().filter(|p| **p == 6379).count() >= 4);
    }

    #[test]
    fn a_dependency_that_never_becomes_ready_fails_the_group_closed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"api","cmd":["api"],"depends_on":["redis"]},
                {"name":"redis","cmd":["redis"],"readiness":{"port":6379}}
            ]}"#,
        )
        .unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        // Never ready → the readiness wait times out and the group rolls back.
        let probe = FakeProbe::ready_after(6379, i32::MAX);
        let mut sup = fast_readiness(Supervisor::new(cfg, dir.path(), move || fake.clone()), probe);

        let err = sup.on_bound_ready(true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(!sup.is_running(), "the group rolled back");
        assert!(!sup.started());
        assert_eq!(*st.live.borrow(), 0);
        // api NEVER started (its dependency never became ready).
        assert!(st.starts.borrow().iter().all(|p| p.cmd != vec!["api".to_string()]));
    }

    #[test]
    fn a_dependency_without_a_probe_is_ready_once_started() {
        let dir = tempfile::tempdir().unwrap();
        // redis has no readiness spec → ready as soon as started; api starts right after.
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"api","cmd":["api"],"depends_on":["redis"]},
                {"name":"redis","cmd":["redis"]}
            ]}"#,
        )
        .unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        // Probe should never be consulted for a probeless dependency.
        let probe = FakeProbe::default();
        let pst = probe.0.clone();
        let mut sup = fast_readiness(Supervisor::new(cfg, dir.path(), move || fake.clone()), probe);
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 2);
        assert!(pst.borrow().calls.is_empty(), "no probe for a probeless dependency");
    }

    #[test]
    fn config_rejects_malformed_env_var_and_binding_names() {
        // A supervisor config is fail-closed: bad env var / binding names are
        // rejected at load, never sanitized or interpolated into the spawn script.
        let bad_var = r#"{"cmd":["true"],"bindings_env":{"BAD NAME":"openai"}}"#;
        assert!(SupervisorConfig::from_json(bad_var).is_err(), "space in env var name");
        let inject = r#"{"cmd":["true"],"bindings_env":{"X; rm -rf /":"openai"}}"#;
        assert!(SupervisorConfig::from_json(inject).is_err(), "shell metachars in env var name");
        let lead_digit = r#"{"cmd":["true"],"bindings_env":{"1KEY":"openai"}}"#;
        assert!(SupervisorConfig::from_json(lead_digit).is_err(), "env var starting with a digit");
        let bad_binding = r#"{"cmd":["true"],"bindings_env":{"KEY":"../escape"}}"#;
        assert!(SupervisorConfig::from_json(bad_binding).is_err(), "path-traversal binding name");
        let bad_base = r#"{"cmd":["true"],"base_env":{"BAD-VAR":"1"}}"#;
        assert!(SupervisorConfig::from_json(bad_base).is_err(), "invalid base_env var name");
        // The valid shape still loads.
        assert!(SupervisorConfig::from_json(
            r#"{"cmd":["python3","app.py"],"base_env":{"PORT":"8080"},"bindings_env":{"OPENAI_API_KEY":"openai"}}"#
        )
        .is_ok());
    }

    #[test]
    fn plan_carries_paths_not_values_and_fails_closed_on_missing_binding() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-REAL-VALUE"); // the value must NOT enter the plan
        let cfg = SupervisorConfig {
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::from([("PORT".to_string(), "8080".to_string())]),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
        };
        let plan = plan_spawn(&cfg, dir.path()).unwrap();
        assert_eq!(plan.base_env.get("PORT").map(String::as_str), Some("8080"));
        assert_eq!(plan.secret_env.len(), 1);
        assert_eq!(plan.secret_env[0].0, "OPENAI_API_KEY");
        assert_eq!(plan.secret_env[0].1, dir.path().join("openai"));
        // The value is NOT anywhere in the plan (never read into the agent).
        assert!(!format!("{plan:?}").contains("sk-REAL-VALUE"), "plan must not carry the value");

        // The spawn script reads the value from the file at exec, in the child.
        let script = spawn_script(&plan);
        assert!(script.contains("export OPENAI_API_KEY=\"$(cat "), "{script}");
        assert!(script.contains("exec 'python3' 'app.py'"), "{script}");
        assert!(!script.contains("sk-REAL-VALUE"), "script must not carry the value");

        // A missing binding must fail closed, never start half-bound.
        let cfg2 = SupervisorConfig {
            bindings_env: BTreeMap::from([("X".to_string(), "absent".to_string())]),
            ..cfg
        };
        assert!(plan_spawn(&cfg2, dir.path()).is_err());
    }

    /// Records lifecycle + the plans the workload was started with (proves the agent
    /// hands the child a PATH, never a value, and that stop/restart re-plans).
    /// v1.5: SHARED state (Rc) so a factory yields state-aggregating clones — the
    /// `Supervisor` builds one workload per service via the factory, and the test
    /// inspects the aggregate `starts`/`stops` plus a live-instance count (so
    /// `is_running` = any live, `starts.len()` = cumulative across restarts).
    #[derive(Clone, Default)]
    struct FakeWorkload(Rc<FakeState>);
    #[derive(Default)]
    struct FakeState {
        starts: RefCell<Vec<SpawnPlan>>,
        stops: RefCell<u32>,
        live: RefCell<i32>,
    }
    impl Workload for FakeWorkload {
        fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()> {
            self.0.starts.borrow_mut().push(plan.clone());
            *self.0.live.borrow_mut() += 1;
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<bool> {
            let was = *self.0.live.borrow() > 0;
            if was {
                *self.0.live.borrow_mut() -= 1;
            }
            *self.0.stops.borrow_mut() += 1;
            Ok(was)
        }
        fn is_running(&self) -> bool {
            *self.0.live.borrow() > 0
        }
    }

    /// A controllable readiness probe (no real sockets): each probed port becomes
    /// ready after a per-port countdown of `is_ready` calls (0 = ready immediately,
    /// `i32::MAX` = never). Records every probed port so a test can assert a
    /// dependent WAITED for its dependency.
    #[derive(Clone, Default)]
    struct FakeProbe(Rc<RefCell<FakeProbeState>>);
    #[derive(Default)]
    struct FakeProbeState {
        ready_after: BTreeMap<u16, i32>,
        calls: Vec<u16>,
    }
    impl FakeProbe {
        fn ready_after(port: u16, n: i32) -> Self {
            let p = FakeProbe::default();
            p.0.borrow_mut().ready_after.insert(port, n);
            p
        }
    }
    impl ReadinessProbe for FakeProbe {
        fn is_ready(&self, spec: &ReadinessSpec) -> bool {
            let mut st = self.0.borrow_mut();
            st.calls.push(spec.port);
            let c = st.ready_after.entry(spec.port).or_insert(0);
            if *c <= 0 {
                true
            } else {
                *c -= 1;
                false
            }
        }
    }

    fn fast_readiness<W: Workload>(sup: Supervisor<W>, probe: FakeProbe) -> Supervisor<W> {
        sup.with_probe(probe)
            .with_readiness_timing(Duration::from_millis(500), Duration::from_millis(1))
    }

    #[test]
    fn supervisor_starts_once_on_bound_ready_and_restarts_after_stop() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-PLACEHOLDER");
        let cfg = SupervisorConfig {
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
        };
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let mut sup = Supervisor::new(cfg, dir.path(), move || fake.clone());

        // Not bound-ready → no start.
        assert!(!sup.on_bound_ready(false).unwrap());
        assert!(!sup.is_running());

        // Bound-ready → starts once, handed the openai PATH (not the value).
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        // Idempotent: a second bound-ready does not double-start.
        assert!(!sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 1);
        assert_eq!(st.starts.borrow()[0].secret_env[0].0, "OPENAI_API_KEY");
        assert!(!format!("{:?}", st.starts.borrow()[0]).contains("sk-PLACEHOLDER"));

        // StopWorkload (pre-snapshot) → stops, allows a fresh start on restore.
        assert!(sup.stop_workload().unwrap());
        assert!(!sup.is_running());

        // Restore: real value on tmpfs, bound-ready again → re-plans + restarts.
        write(dir.path(), "openai", "sk-REAL");
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 2);
    }

    #[test]
    fn stop_workload_on_a_never_started_supervisor_reports_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["true".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), FakeWorkload::default);
        assert!(!sup.stop_workload().unwrap(), "nothing to stop ⇒ was_running=false");
    }

    #[test]
    fn real_child_reads_the_secret_from_tmpfs_at_exec_not_the_agent() {
        // The value lives only in the workload child's env — proven by having the
        // child WRITE its own env var to an output file, then reading it back. The
        // agent (this test process) only ever handled the PATH.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        write(dir.path(), "openai", "sk-CHILD-ONLY-VALUE");
        let cfg = SupervisorConfig {
            cmd: vec![
                "sh".into(),
                "-c".into(),
                format!("printf %s \"$OPENAI_API_KEY\" > {}; sleep 30", out.display()),
            ],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        // Wait for the child to write the file (it read the value at exec).
        for _ in 0..50 {
            if out.exists() && !std::fs::read_to_string(&out).unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "sk-CHILD-ONLY-VALUE");
        assert!(sup.stop_workload().unwrap(), "a live child reports was_running=true");
        assert!(!sup.is_running());
    }

    #[test]
    fn stop_is_bounded_and_sigkills_a_sigterm_ignoring_child() {
        // A workload that TRAPS/ignores SIGTERM must not stall the seal: stop must
        // return within ~grace + reap via SIGKILL. `trap '' TERM` ignores SIGTERM.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["sh".into(), "-c".into(), "trap '' TERM; while true; do sleep 1; done".into()],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        let started = std::time::Instant::now();
        assert!(sup.stop_workload().unwrap(), "bounded stop still reports was_running");
        let elapsed = started.elapsed();
        assert!(!sup.is_running());
        // Bounded: grace (2s) + SIGKILL reap, comfortably under 10s.
        assert!(elapsed < std::time::Duration::from_secs(10), "stop took {elapsed:?} — not bounded");
    }

    #[test]
    fn stop_returns_promptly_for_a_well_behaved_child() {
        // A child that exits on SIGTERM is reaped well inside the grace window.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["sleep".into(), "300".into()], // default SIGTERM disposition = terminate
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        let started = std::time::Instant::now();
        assert!(sup.stop_workload().unwrap());
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "normal stop should be fast");
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_the_whole_process_group_not_just_the_wrapper_shell() {
        // REGRESSION (PR 3d live E2E): the rootfs builder emits the workload cmd as
        // a shell wrapper (`/bin/sh -lc <start_cmd>`), so the real app can be a
        // GRANDCHILD of the spawned pid. A single-PID SIGTERM killed only the
        // wrapper, the orphaned app kept serving, and the "stopped" pre-seal
        // snapshot captured a RUNNING workload (restore woke with /health ok).
        // Model that exact shape: wrapper sh whose compound body (`…; true` defeats
        // dash/bash's exec optimization) keeps the sleeper as a grandchild.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["sh".into(), "-c".into(), "sleep 300; true".into()],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        // The spawn put the wrapper in its own process group (pgid = child pid) —
        // the property killpg-stop relies on.
        let pid = match sup.workloads.first().and_then(|w| w.child.as_ref()) {
            Some(ch) => ch.id() as i32,
            None => panic!("child running"),
        };
        assert_eq!(unsafe { libc::getpgid(pid) }, pid, "workload must lead its own process group");
        assert!(sup.stop_workload().unwrap());
        // The WHOLE group must be gone — poll killpg(sig 0) until ESRCH (the
        // grandchild sleeper included; reparented orphans are reaped by init).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let alive = unsafe { libc::killpg(pid, 0) == 0 };
            if !alive {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "process group survived stop — the grandchild workload outlived StopWorkload"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
