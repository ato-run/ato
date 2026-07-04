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
        cfg.validate()?;
        Ok(cfg)
    }

    /// Fail-closed validation: a malformed config must be rejected, never sanitized
    /// or silently launched. `cmd` non-empty; every `bindings_env` env var name is a
    /// valid POSIX identifier (it is interpolated into the spawn shell script) and
    /// every binding name is a valid [`BindingName`] (it is joined onto the tmpfs
    /// root). `base_env` names are validated too.
    pub fn validate(&self) -> Result<(), String> {
        if self.cmd.is_empty() {
            return Err("supervisor.json: `cmd` is empty".into());
        }
        for var in self.base_env.keys() {
            if !valid_env_var_name(var) {
                return Err(format!("supervisor.json: invalid base_env var name {var:?}"));
            }
        }
        for (var, binding) in &self.bindings_env {
            if !valid_env_var_name(var) {
                return Err(format!("supervisor.json: invalid bindings_env var name {var:?}"));
            }
            BindingName::parse(binding.as_str())
                .map_err(|e| format!("supervisor.json: invalid binding name {binding:?}: {e}"))?;
        }
        Ok(())
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
pub fn plan_spawn(config: &SupervisorConfig, bindings_root: &Path) -> std::io::Result<SpawnPlan> {
    // Defense in depth: never plan a spawn from a config that would not have passed
    // load-time validation (invalid env/binding names never reach the shell script).
    config
        .validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut secret_env = Vec::with_capacity(config.bindings_env.len());
    for (var, binding) in &config.bindings_env {
        let path = bindings_root.join(binding);
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("binding '{binding}' for env '{var}' absent at {}", path.display()),
            ));
        }
        secret_env.push((var.clone(), path));
    }
    Ok(SpawnPlan {
        cmd: config.cmd.clone(),
        cwd: config.cwd.clone(),
        base_env: config.base_env.clone(),
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
                // must always return): SIGTERM, wait up to a grace window, then SIGKILL
                // a workload that ignored SIGTERM, and reap. A workload that traps
                // SIGTERM cannot stall the seal.
                #[cfg(unix)]
                unsafe {
                    libc::kill(ch.id() as i32, libc::SIGTERM);
                }
                let grace = std::time::Duration::from_millis(STOP_GRACE_MS);
                let step = std::time::Duration::from_millis(20);
                let deadline = std::time::Instant::now() + grace;
                loop {
                    match ch.try_wait()? {
                        Some(_) => return Ok(true), // exited within grace
                        None if std::time::Instant::now() >= deadline => break,
                        None => std::thread::sleep(step),
                    }
                }
                #[cfg(unix)]
                unsafe {
                    libc::kill(ch.id() as i32, libc::SIGKILL);
                }
                let _ = ch.wait(); // reap the killed child (bounded: SIGKILL is unblockable)
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
        let plan = plan_spawn(&self.config, &self.bindings_root)?;
        self.workload.start(&plan)?;
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
    #[derive(Default)]
    struct FakeWorkload {
        running: bool,
        starts: RefCell<Vec<SpawnPlan>>,
        stops: RefCell<u32>,
    }
    impl Workload for FakeWorkload {
        fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()> {
            self.starts.borrow_mut().push(plan.clone());
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

        // Bound-ready → starts once, handed the openai PATH (not the value).
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        // Idempotent: a second bound-ready does not double-start.
        assert!(!sup.on_bound_ready(true).unwrap());
        assert_eq!(sup.workload.starts.borrow().len(), 1);
        assert_eq!(sup.workload.starts.borrow()[0].secret_env[0].0, "OPENAI_API_KEY");
        assert!(!format!("{:?}", sup.workload.starts.borrow()[0]).contains("sk-PLACEHOLDER"));

        // StopWorkload (pre-snapshot) → stops, allows a fresh start on restore.
        assert!(sup.stop_workload().unwrap());
        assert!(!sup.is_running());

        // Restore: real value on tmpfs, bound-ready again → re-plans + restarts.
        write(dir.path(), "openai", "sk-REAL");
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(sup.workload.starts.borrow().len(), 2);
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
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default());
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
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default());
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
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default());
        assert!(sup.on_bound_ready(true).unwrap());
        let started = std::time::Instant::now();
        assert!(sup.stop_workload().unwrap());
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "normal stop should be fast");
    }
}
