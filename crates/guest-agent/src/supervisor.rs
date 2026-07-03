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

use serde::{Deserialize, Serialize};

use crate::tmpfs::DEFAULT_BINDINGS_ROOT;

/// `/etc/ato/supervisor.json` — how the guest-agent launches the workload. Written
/// into the rootfs by the builder for a supervisor (env-secret) capsule. Holds NO
/// secret value: `bindings_env` maps an env var name to the **binding name** whose
/// value the agent reads from tmpfs at spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// The workload argv (`["python3", "app.py"]` / `["/bin/sh", "-lc", "…"]`).
    pub cmd: Vec<String>,
    /// Working directory for the workload (default `/app`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Static environment (non-secret) applied before bindings.
    #[serde(default)]
    pub base_env: BTreeMap<String, String>,
    /// `ENV_VAR -> binding name`. At spawn the agent reads
    /// `<bindings_root>/<binding>` and sets `ENV_VAR` to its contents.
    #[serde(default)]
    pub bindings_env: BTreeMap<String, String>,
}

fn default_cwd() -> String {
    "/app".to_string()
}

impl SupervisorConfig {
    /// Parse `/etc/ato/supervisor.json`; a malformed/empty config is an error
    /// (fail-closed — a supervisor capsule must not fall back to launching with no
    /// bindings).
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let cfg: SupervisorConfig =
            serde_json::from_str(raw).map_err(|e| format!("supervisor.json parse: {e}"))?;
        if cfg.cmd.is_empty() {
            return Err("supervisor.json: `cmd` is empty".into());
        }
        Ok(cfg)
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

/// Compose the workload environment: `base_env`, then each `bindings_env` entry with
/// its value read from `<bindings_root>/<binding>`. A missing binding file is an
/// error (fail-closed — the workload must never start half-bound). Returns the full
/// env map; the caller applies it to the child. Values are never logged.
pub fn compose_env(
    config: &SupervisorConfig,
    bindings_root: &Path,
) -> std::io::Result<BTreeMap<String, String>> {
    let mut env = config.base_env.clone();
    for (var, binding) in &config.bindings_env {
        let path = bindings_root.join(binding);
        let value = std::fs::read_to_string(&path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("binding '{binding}' for env '{var}' not readable at {}", path.display()),
            )
        })?;
        // tmpfs files are written without a trailing newline, but trim defensively so
        // an accidental newline never rides into the credential.
        env.insert(var.clone(), value.trim_end_matches(['\n', '\r']).to_string());
    }
    Ok(env)
}

/// The workload process, behind a trait so the supervisor orchestration is testable
/// without spawning. `start` receives the fully composed env; `stop` kills the child.
pub trait Workload {
    /// Spawn the workload with `env` (in addition to, and overriding, the inherited
    /// environment) and `cwd`. Idempotent callers guard against double-start.
    fn start(&mut self, cmd: &[String], cwd: &str, env: &BTreeMap<String, String>) -> std::io::Result<()>;
    /// Stop the workload (SIGTERM then reap). Idempotent — not running ⇒ Ok(false).
    fn stop(&mut self) -> std::io::Result<bool>;
    /// Whether the workload child is currently running.
    fn is_running(&self) -> bool;
}

/// A real OS process workload (`std::process::Command`), used by the guest binary.
#[derive(Default)]
pub struct ChildWorkload {
    child: Option<std::process::Child>,
}

impl Workload for ChildWorkload {
    fn start(&mut self, cmd: &[String], cwd: &str, env: &BTreeMap<String, String>) -> std::io::Result<()> {
        let (prog, args) = cmd
            .split_first()
            .ok_or_else(|| std::io::Error::other("supervisor cmd is empty"))?;
        let mut c = std::process::Command::new(prog);
        c.args(args).current_dir(cwd);
        for (k, v) in env {
            c.env(k, v);
        }
        self.child = Some(c.spawn()?);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<bool> {
        match self.child.take() {
            Some(mut ch) => {
                // Best-effort graceful stop then reap. The VM teardown would kill it
                // regardless; this makes the pre-snapshot image workload-idle.
                #[cfg(unix)]
                unsafe {
                    libc::kill(ch.id() as i32, libc::SIGTERM);
                }
                // Give it a moment, then ensure it's gone.
                let _ = ch.wait();
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
pub struct Supervisor<W: Workload> {
    config: SupervisorConfig,
    bindings_root: PathBuf,
    workload: W,
    started: bool,
}

impl<W: Workload> Supervisor<W> {
    pub fn new(config: SupervisorConfig, bindings_root: impl Into<PathBuf>, workload: W) -> Self {
        Supervisor { config, bindings_root: bindings_root.into(), workload, started: false }
    }

    /// Start the workload exactly once, when the session is bound-ready. Composes the
    /// env from tmpfs and spawns. A compose/spawn error is returned (fail-closed —
    /// the caller must not report a healthy serving state). No-op if already started
    /// or not yet bound-ready.
    pub fn on_bound_ready(&mut self, bound_ready: bool) -> std::io::Result<bool> {
        if self.started || !bound_ready {
            return Ok(false);
        }
        let env = compose_env(&self.config, &self.bindings_root)?;
        self.workload.start(&self.config.cmd, &self.config.cwd, &env)?;
        self.started = true;
        Ok(true)
    }

    /// Stop the workload (pre-snapshot at build, or teardown). Returns whether a
    /// process was actually running. Leaves `started=false` so a later bound-ready
    /// (restore) starts a fresh workload with the real env.
    pub fn stop_workload(&mut self) -> std::io::Result<bool> {
        let was_running = self.workload.stop()?;
        self.started = false;
        Ok(was_running)
    }

    pub fn is_running(&self) -> bool {
        self.workload.is_running()
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

    #[test]
    fn compose_env_reads_tmpfs_and_fails_closed_on_missing_binding() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-REAL-VALUE\n"); // trailing newline trimmed
        let cfg = SupervisorConfig {
            cmd: vec!["true".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::from([("PORT".to_string(), "8080".to_string())]),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
        };
        let env = compose_env(&cfg, dir.path()).unwrap();
        assert_eq!(env.get("OPENAI_API_KEY").map(String::as_str), Some("sk-REAL-VALUE"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));

        // A missing binding must fail closed, never start half-bound.
        let cfg2 = SupervisorConfig {
            bindings_env: BTreeMap::from([("X".to_string(), "absent".to_string())]),
            ..cfg
        };
        assert!(compose_env(&cfg2, dir.path()).is_err());
    }

    /// Records lifecycle + the env the workload was started with (proves the value
    /// reaches the child env, and that stop/restart composes fresh).
    #[derive(Default)]
    struct FakeWorkload {
        running: bool,
        starts: RefCell<Vec<BTreeMap<String, String>>>,
        stops: RefCell<u32>,
    }
    impl Workload for FakeWorkload {
        fn start(&mut self, _cmd: &[String], _cwd: &str, env: &BTreeMap<String, String>) -> std::io::Result<()> {
            self.starts.borrow_mut().push(env.clone());
            self.running = true;
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<bool> {
            let was = self.running;
            self.running = false;
            *self.stops.borrow_mut() += 1;
            Ok(was)
        }
        fn is_running(&self) -> bool {
            self.running
        }
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
        };
        let mut sup = Supervisor::new(cfg, dir.path(), FakeWorkload::default());

        // Not bound-ready → no start.
        assert!(!sup.on_bound_ready(false).unwrap());
        assert!(!sup.is_running());

        // Bound-ready → starts once (build: placeholder env).
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        // Idempotent: a second bound-ready does not double-start.
        assert!(!sup.on_bound_ready(true).unwrap());
        assert_eq!(sup.workload.starts.borrow().len(), 1);
        assert_eq!(
            sup.workload.starts.borrow()[0].get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-PLACEHOLDER")
        );

        // StopWorkload (pre-snapshot) → stops, allows a fresh start on restore.
        assert!(sup.stop_workload().unwrap());
        assert!(!sup.is_running());

        // Restore: real value delivered, bound-ready again → restart with REAL env.
        write(dir.path(), "openai", "sk-REAL");
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(sup.workload.starts.borrow().len(), 2);
        assert_eq!(
            sup.workload.starts.borrow()[1].get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-REAL")
        );
    }

    #[test]
    fn stop_workload_on_a_never_started_supervisor_reports_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["true".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), FakeWorkload::default());
        assert!(!sup.stop_workload().unwrap(), "nothing to stop ⇒ was_running=false");
    }

    #[test]
    fn real_child_workload_starts_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            cmd: vec!["sleep".into(), "30".into()],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default());
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        assert!(sup.stop_workload().unwrap(), "a live child reports was_running=true");
        assert!(!sup.is_running());
    }
}
