//! v1.2 supervisor mode — the guest-agent starts the workload with the composed
//! environment **after** bindings are delivered (the contract's named successor to
//! the impossible "rewrite a snapshotted process's environ", binding-lease.md §58).
//!
//! Flow (per the plan D1 / contract §7.1, `delivery = "env"`):
//!
//! - **Build:** boot → agent supervisor starts the workload with PLACEHOLDER
//!   bindings delivered over vsock → boot-verify health → host sends
//!   [`StopWorkload`](ato_ipc::binding_control::HostToAgent::StopWorkload) → agent
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

use ato_ipc::binding_lease::BindingName;
use serde::{Deserialize, Serialize};

use crate::BindingSink;
use crate::tmpfs::DEFAULT_BINDINGS_ROOT;
use crate::volume_mount::VolumeSpec;

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

/// Phase 6 (service DAG): a managed entry is either a long-running `service`
/// (starts on every bound-ready and stays up) or a `run_once` task (a
/// migration / one-shot that RUNS TO COMPLETION at its declared timing and must
/// exit 0). Default `service` keeps every pre-Phase-6 config byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    #[default]
    Service,
    RunOnce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StdioMode {
    #[default]
    Log,
    Pty,
}

/// Phase 6 (service DAG): WHEN a `run_once` task executes. A migration typically
/// declares `["seal_once","restore"]` — bake the schema at seal, re-apply it on
/// every restore. The supervisor runs a `run_once` only if the CURRENT phase is
/// in its `run_at` list.
/// - `seal_once`: once, during BUILD, before the pre-seal snapshot is taken.
/// - `restore`: on every restore (snapshot wake).
/// - `run`: on every plain run (non-snapshot boot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecPhase {
    SealOnce,
    Restore,
    Run,
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
    /// simply started when it declares no probe) first. LEGACY spelling; merged
    /// into the Phase-6 `depends_on_ready` set (both mean the readiness gate).
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// v1.5: how to tell this service is READY (so dependents may start). `None` =
    /// ready as soon as its process is started. Only valid for `kind = service`.
    #[serde(default)]
    pub readiness: Option<ReadinessSpec>,
    /// Phase 5 multi-image rootfs: when `Some`, this service's image was exported
    /// into its OWN subtree (`/opt/ato/services/<name>/rootfs`) instead of
    /// overlaying a single `/`, so the workload is launched inside a fresh MOUNT
    /// NAMESPACE and `chroot`ed into that subtree (see [`spawn_script`]). `None` =
    /// the legacy single-rootfs launch (byte-identical to before — skipped when
    /// serializing so every pre-Phase-5 config is unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<String>,
    /// Phase 6 (service DAG): `service` vs `run_once`. Default `service`.
    #[serde(default)]
    pub kind: ServiceKind,
    /// Phase 6: execution timing for a `run_once` task — the phases it runs at. A
    /// `run_once` MUST declare at least one phase (fail-closed — no implicit
    /// timing); a `service` MUST leave this empty.
    #[serde(default)]
    pub run_at: Vec<ExecPhase>,
    /// Phase 6: readiness-gate dependencies — each target must be a `service` and
    /// must be READY before this entry starts. Successor spelling of `depends_on`.
    #[serde(default)]
    pub depends_on_ready: Vec<String>,
    /// Phase 6: success-gate dependencies — each target must be a `run_once` and
    /// must have EXITED 0 (in this phase, or been sealed at an earlier phase)
    /// before this entry starts.
    #[serde(default)]
    pub depends_on_success: Vec<String>,
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

/// Phase 7 (generated internal bindings): how a run-time generated internal
/// secret's value is produced inside the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratorMethod {
    /// Draw `bytes` bytes from the OS RNG and standard-base64-encode them.
    RandomBase64,
}

/// Phase 7: the lifetime scope of a generated internal binding. Only `run`
/// (a fresh value per run) is defined today; recorded so the receipt/spec is
/// explicit and forward-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedScope {
    #[default]
    Run,
}

/// Phase 7 (generated internal bindings): a RUN-time generated INTERNAL secret
/// (DB password, redis password, internal session/JWT key). Distinct from an
/// EXTERNAL api key (which stays on the binding-lease path): the guest-agent
/// generates the VALUE at run from the OS RNG and materializes it to the tmpfs
/// binding sink — the same `bindings_env → /run/ato/bindings` mechanism a leased
/// secret uses — so every `targets` service reads the SAME value.
///
/// Holds NO value: only the NAME + generator method + scope + targets (the
/// SPEC). The spec is what the receipt records and what the artifact identity
/// hashes (baked into `supervisor.json`); the VALUE is generated per run, never
/// baked into the artifact, logged, or sent to the host. Two runs of the same
/// artifact therefore share identity but get different runtime values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedBindingSpec {
    /// The binding NAME (also the tmpfs filename). A valid [`BindingName`]. Each
    /// target service maps an env var to this name in its `bindings_env`.
    pub name: String,
    /// How the value is generated at run.
    pub generator: GeneratorMethod,
    /// Bytes of OS randomness drawn before encoding (bounded at validation).
    pub bytes: u32,
    /// Lifetime scope (`run` = a fresh value per run).
    #[serde(default)]
    pub scope: GeneratedScope,
    /// The services whose env receives this value. Each must be a declared
    /// service; recorded for audit. The value is shared (one tmpfs file), so
    /// every target reads the identical value within a run.
    pub targets: Vec<String>,
}

impl GeneratorMethod {
    /// Generate a fresh value from the OS RNG. `bytes` bytes of entropy are drawn
    /// and encoded per the method. Reads OS randomness AT RUN TIME in the guest.
    pub fn generate(&self, bytes: usize) -> std::io::Result<String> {
        match self {
            GeneratorMethod::RandomBase64 => Ok(base64_encode(&os_random_bytes(bytes)?)),
        }
    }
}

/// Read `n` bytes of OS randomness (`/dev/urandom`). The guest is Linux, so this
/// is always available; a short read is an error (fail-closed — never generate a
/// weak/partial secret).
fn os_random_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Standard base64 (RFC 4648, `+/`, `=` padding). Small, dependency-free — the
/// guest-agent has no base64 crate and this is the only encoder it needs.
fn base64_encode(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Phase 7: generate each spec's value from the OS RNG and materialize it to the
/// binding `sink` (one tmpfs file per binding name, shared by all target
/// services). The value is produced HERE, at run — never baked into the artifact
/// — and returned nowhere (only written to the sink). Returns the binding names
/// written so the caller can scrub them on stop. Fail-closed: a bad name / RNG /
/// sink error aborts before any partial state is reported ready.
pub fn materialize_generated_bindings(
    specs: &[GeneratedBindingSpec],
    sink: &dyn crate::BindingSink,
) -> std::io::Result<Vec<BindingName>> {
    let mut written = Vec::with_capacity(specs.len());
    for spec in specs {
        let name = BindingName::parse(spec.name.as_str()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("generated binding {:?}: invalid name: {e}", spec.name),
            )
        })?;
        let value = spec.generator.generate(spec.bytes as usize)?;
        sink.deliver(&name, &value)?;
        written.push(name);
    }
    Ok(written)
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
    /// `pty` is reserved for a single interactive service selected by a
    /// Terminal Surface. Existing artifacts default to per-service log files.
    #[serde(default)]
    pub stdio_mode: StdioMode,
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
    /// v1.6 (ato#983) Slice 3: durable state volumes to mount BEFORE any
    /// service starts. VM-wide (not nested under a service — mounted once at
    /// agent boot, unconditionally, regardless of which service's
    /// `state_bindings` declared the need for it at build time).
    #[serde(default)]
    pub volumes: Vec<VolumeSpec>,
    /// Phase 7 (generated internal bindings): RUN-time generated internal
    /// secrets. The guest generates each value from the OS RNG at run and
    /// materializes it to the tmpfs binding sink so every target service reads
    /// the SAME value. Holds NO value — only the spec (name/generator/scope/
    /// targets), which is what the artifact identity hashes.
    #[serde(default)]
    pub generated_bindings: Vec<GeneratedBindingSpec>,
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
            rootfs: None,
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        }]
    }
}

impl ServiceSpec {
    /// Phase 6: all READINESS-gate dependencies — the legacy `depends_on` ∪ the
    /// Phase-6 `depends_on_ready` (both mean "wait until the target service is
    /// READY"). Deduplication is unnecessary: a name repeated across the two lists
    /// only makes the gate idempotent.
    fn ready_deps(&self) -> impl Iterator<Item = &String> {
        self.depends_on.iter().chain(self.depends_on_ready.iter())
    }

    /// Phase 6: EVERY dependency edge (for the DAG / topological sort + cycle
    /// detection) — readiness-gate ∪ success-gate.
    fn all_deps(&self) -> impl Iterator<Item = &String> {
        self.ready_deps().chain(self.depends_on_success.iter())
    }

    /// Fail-closed per-service validation (see [`SupervisorConfig::validate`]).
    fn validate(&self) -> Result<(), String> {
        if self.cmd.is_empty() {
            return Err(format!(
                "supervisor.json: service {:?} has empty `cmd`",
                self.name
            ));
        }
        if self.name.trim().is_empty() {
            return Err("supervisor.json: a service has an empty `name`".into());
        }
        // Phase 6: kind/timing coherence, fail-closed. A `run_once` must declare
        // explicit timing and cannot carry a long-running readiness probe; a
        // `service` must not declare run timing.
        match self.kind {
            ServiceKind::RunOnce => {
                if self.run_at.is_empty() {
                    return Err(format!(
                        "supervisor.json: run_once service {:?} must declare `run_at` timing \
                         (one or more of seal_once/restore/run)",
                        self.name
                    ));
                }
                if self.readiness.is_some() {
                    return Err(format!(
                        "supervisor.json: run_once service {:?} must not declare `readiness` \
                         (a one-shot task has no long-running readiness)",
                        self.name
                    ));
                }
            }
            ServiceKind::Service => {
                if !self.run_at.is_empty() {
                    return Err(format!(
                        "supervisor.json: service {:?} declares `run_at` but is not a run_once \
                         (timing is only meaningful for run_once)",
                        self.name
                    ));
                }
            }
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
                format!(
                    "supervisor.json: service {:?} invalid binding name {binding:?}: {e}",
                    self.name
                )
            })?;
        }
        // Phase 5: the per-service rootfs is interpolated (chroot/mount targets)
        // into the spawn shell script, so a malformed path is REJECTED at config
        // load, never sanitized — same fail-closed discipline as env/binding names.
        if let Some(rootfs) = &self.rootfs {
            validate_service_rootfs(rootfs).map_err(|e| {
                format!(
                    "supervisor.json: service {:?} invalid rootfs: {e}",
                    self.name
                )
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
        if self.stdio_mode == StdioMode::Pty
            && (services.len() != 1 || services[0].kind != ServiceKind::Service)
        {
            return Err(
                "supervisor.json: stdio_mode=pty requires exactly one long-running service".into(),
            );
        }
        if services.is_empty() {
            return Err("supervisor.json: no service (`cmd` and `services` both empty)".into());
        }
        // Unique service names (they key per-service logs + diagnostics).
        let mut seen = std::collections::BTreeSet::new();
        for svc in &services {
            svc.validate()?;
            if !seen.insert(svc.name.clone()) {
                return Err(format!(
                    "supervisor.json: duplicate service name {:?}",
                    svc.name
                ));
            }
        }
        // v1.5 readiness graph: `depends_on` must reference declared services, must
        // not self-loop, and the graph must be acyclic (the start order is a
        // topological sort — a cycle has no valid order). Validate now, fail-closed.
        self.start_order()?;
        // Phase 6: dependency-EDGE kind coherence. A readiness gate can only wait on
        // a long-running `service` (a run_once has no lasting readiness); a success
        // gate can only wait on a `run_once` (a service never "exits 0"). Fail-closed
        // on a mismatch so a mis-declared DAG never silently gates on nothing.
        let by_name: BTreeMap<&str, &ServiceSpec> =
            services.iter().map(|s| (s.name.as_str(), s)).collect();
        for svc in &services {
            for dep in svc.ready_deps() {
                // Existence already guaranteed by start_order above.
                if by_name[dep.as_str()].kind != ServiceKind::Service {
                    return Err(format!(
                        "supervisor.json: service {:?} readiness-depends on {:?} which is a \
                         run_once (a readiness gate needs a long-running service)",
                        svc.name, dep
                    ));
                }
            }
            for dep in &svc.depends_on_success {
                if by_name[dep.as_str()].kind != ServiceKind::RunOnce {
                    return Err(format!(
                        "supervisor.json: service {:?} success-depends on {:?} which is not a \
                         run_once (a success gate needs a run_once that exits 0)",
                        svc.name, dep
                    ));
                }
            }
        }
        // v1.6 (ato#983) Slice 3: durable state volumes. Fail-closed at LOAD
        // time — before ever attempting a real mount — on anything the
        // builder should never have produced: an empty state_name/fs_label, a
        // malformed target (re-validated independently of the builder's own
        // check — this agent never trusts a config it did not produce
        // itself), a duplicate state_name, a duplicate fs_label (would make
        // device resolution ambiguous), or two targets that are identical or
        // nested under one another (mounting one under/over another).
        let mut seen_states = std::collections::BTreeSet::new();
        let mut seen_labels = std::collections::BTreeSet::new();
        let mut targets: Vec<std::path::PathBuf> = Vec::new();
        for vol in &self.volumes {
            if vol.state_name.trim().is_empty() {
                return Err("supervisor.json: a volume has an empty state_name".into());
            }
            if vol.fs_label.trim().is_empty() {
                return Err(format!(
                    "supervisor.json: volume {:?} has an empty fs_label",
                    vol.state_name
                ));
            }
            if !seen_states.insert(vol.state_name.clone()) {
                return Err(format!(
                    "supervisor.json: duplicate volume state_name {:?}",
                    vol.state_name
                ));
            }
            if !seen_labels.insert(vol.fs_label.clone()) {
                return Err(format!(
                    "supervisor.json: duplicate volume fs_label {:?}",
                    vol.fs_label
                ));
            }
            let target = crate::volume_mount::validate_mount_target(&vol.target)
                .map_err(|e| format!("supervisor.json: volume {:?}: {e}", vol.state_name))?;
            for prev in &targets {
                if *prev == target
                    || prev.starts_with(&target)
                    || target.starts_with(prev.as_path())
                {
                    return Err(format!(
                        "supervisor.json: volume {:?} target {} conflicts with another volume's \
                         target {} — targets must not be identical or nested under one another",
                        vol.state_name,
                        target.display(),
                        prev.display()
                    ));
                }
            }
            targets.push(target);
        }
        // Phase 7 (generated internal bindings): fail-closed at LOAD time. Every
        // generated binding must have a valid [`BindingName`] (it is the tmpfs
        // filename each target service reads), bounded `bytes` (never draw an
        // absurd amount of entropy, never zero), at least one target, and every
        // target must be a DECLARED service (a value generated for nobody is a
        // config error). Names must be unique (they key the tmpfs files).
        let service_names: std::collections::BTreeSet<String> =
            services.iter().map(|s| s.name.clone()).collect();
        let mut seen_generated = std::collections::BTreeSet::new();
        for g in &self.generated_bindings {
            BindingName::parse(g.name.as_str()).map_err(|e| {
                format!(
                    "supervisor.json: generated binding {:?} invalid name: {e}",
                    g.name
                )
            })?;
            if !seen_generated.insert(g.name.clone()) {
                return Err(format!(
                    "supervisor.json: duplicate generated binding name {:?}",
                    g.name
                ));
            }
            if !(1..=1024).contains(&g.bytes) {
                return Err(format!(
                    "supervisor.json: generated binding {:?} bytes must be 1..=1024 (got {})",
                    g.name, g.bytes
                ));
            }
            if g.targets.is_empty() {
                return Err(format!(
                    "supervisor.json: generated binding {:?} has no targets (nothing would consume it)",
                    g.name
                ));
            }
            for t in &g.targets {
                if !service_names.contains(t) {
                    return Err(format!(
                        "supervisor.json: generated binding {:?} target {t:?} is not a declared service",
                        g.name
                    ));
                }
            }
        }
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
            // Phase 6: the DAG is over EVERY edge kind (readiness-gate ∪
            // success-gate), so a cycle through any mix of edges is detected.
            let edges: Vec<String> = svc.all_deps().cloned().collect();
            for d in &edges {
                if d == &svc.name {
                    return Err(format!(
                        "supervisor.json: service {:?} depends on itself",
                        svc.name
                    ));
                }
                if !names.contains(d) {
                    return Err(format!(
                        "supervisor.json: service {:?} depends_on {:?} which is not a declared service",
                        svc.name, d
                    ));
                }
            }
            deps.insert(svc.name.clone(), edges);
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
                1 => {
                    return Err(format!(
                        "supervisor.json: dependency cycle involving service {n:?}"
                    ));
                }
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
    /// v1.5 per-service logs (ato#973): this service's stdout/stderr are redirected
    /// to these files so one service's output never mixes into another's (or the
    /// agent's). Deterministic per service name (see [`service_log_paths`]).
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
    /// Phase 5: when `Some`, the workload runs inside a fresh mount namespace and
    /// is `chroot`ed into this per-service rootfs subtree before exec (see
    /// [`spawn_script`]). `None` = the legacy single-rootfs launch.
    pub rootfs: Option<String>,
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
pub fn plan_spawn_service(
    service: &ServiceSpec,
    bindings_root: &Path,
) -> std::io::Result<SpawnPlan> {
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
    let (stdout_log, stderr_log) = service_log_paths(&service.name);
    Ok(SpawnPlan {
        cmd: service.cmd.clone(),
        cwd: service.cwd.clone(),
        base_env: service.base_env.clone(),
        secret_env,
        stdout_log,
        stderr_log,
        rootfs: service.rootfs.clone(),
    })
}

/// The service-logs directory (`$ATO_SERVICE_LOG_DIR`, else `/tmp/ato/services`).
pub fn service_log_dir() -> PathBuf {
    match std::env::var("ATO_SERVICE_LOG_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
        _ => PathBuf::from("/tmp/ato/services"),
    }
}

/// Deterministic per-service log paths: `<dir>/<name>.stdout.log` +
/// `<name>.stderr.log`. The service name is a DNS-safe label ([a-z0-9-]) — which is
/// also PATH-safe (no `/`, `.`, or `..` component), so it cannot escape the dir.
pub fn service_log_paths(name: &str) -> (PathBuf, PathBuf) {
    let dir = service_log_dir();
    (
        dir.join(format!("{name}.stdout.log")),
        dir.join(format!("{name}.stderr.log")),
    )
}

/// POSIX single-quote a string for safe embedding in an `sh -c` script.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` script that reads each secret from its tmpfs file into the env, then
/// `exec`s the workload — so the value only ever materializes inside the child (which
/// becomes the workload via exec), never in the agent. `cmd`/paths are shell-quoted.
pub(crate) fn spawn_script_for_workload(plan: &SpawnPlan) -> String {
    let mut script = String::new();
    for (var, path) in &plan.secret_env {
        // export VAR="$(cat 'path')" — the value is read by the subshell, held only
        // in this child's environment, and never appears in argv/the process table.
        script.push_str(&format!(
            "export {var}=\"$(cat {})\"\n",
            shell_single_quote(&path.to_string_lossy())
        ));
    }
    // Phase 5: a service with its own rootfs subtree is launched inside a fresh
    // MOUNT NAMESPACE and chroot'ed into it; the plain case execs the command
    // directly (byte-identical to before). Secrets are exported into the env
    // ABOVE, before the namespace/chroot, so the workload inherits them across the
    // chroot exec (the tmpfs binding files live in the OUTER rootfs and are gone
    // after chroot).
    match &plan.rootfs {
        None => {
            let quoted: Vec<String> = plan.cmd.iter().map(|a| shell_single_quote(a)).collect();
            script.push_str(&format!("exec {}\n", quoted.join(" ")));
        }
        Some(rootfs) => {
            script.push_str(&chroot_wrapped_exec(rootfs, &plan.cwd, &plan.cmd));
            script.push('\n');
        }
    }
    script
}

#[cfg(test)]
fn spawn_script(plan: &SpawnPlan) -> String {
    spawn_script_for_workload(plan)
}

/// Phase 5: a POSIX-shell one-liner that enters a fresh MOUNT NAMESPACE, mounts the
/// pseudo-filesystems inside the service's own rootfs subtree, then `chroot`s into it
/// and `exec`s the workload. Ordering is load-bearing: `unshare --mount` (private
/// namespace) → per-rootfs `mount` of proc/sys/dev/tmp → `chroot` → `cd <cwd>` →
/// `exec <cmd>`. Every interpolated value is single-quoted (the rootfs path was
/// already validated to a safe charset at config load; the cmd/cwd are quoted
/// defensively) so nothing can break out of the nested `sh -c` layers.
fn chroot_wrapped_exec(rootfs: &str, cwd: &str, cmd: &[String]) -> String {
    let rq = shell_single_quote(rootfs);
    let quoted_cmd: Vec<String> = cmd.iter().map(|a| shell_single_quote(a)).collect();
    // Innermost: inside the new root, cd into the workload's cwd then exec it.
    let in_chroot = format!(
        "cd {} && exec {}",
        shell_single_quote(cwd),
        quoted_cmd.join(" ")
    );
    // Middle: within the private mount namespace, mount the pseudo-filesystems under
    // the service rootfs, then chroot + run the innermost script.
    let in_ns = format!(
        "mount -t proc proc {r}/proc 2>/dev/null;          mount -t sysfs sysfs {r}/sys 2>/dev/null;          mount -t devtmpfs devtmpfs {r}/dev 2>/dev/null || mount --bind /dev {r}/dev 2>/dev/null;          mount -t tmpfs tmpfs {r}/tmp 2>/dev/null;          mkdir -p {r}/run 2>/dev/null; mount -t tmpfs tmpfs {r}/run 2>/dev/null;          ln -sf /proc/self/fd {r}/dev/fd 2>/dev/null;          ln -sf /proc/self/fd/0 {r}/dev/stdin 2>/dev/null;          ln -sf /proc/self/fd/1 {r}/dev/stdout 2>/dev/null;          ln -sf /proc/self/fd/2 {r}/dev/stderr 2>/dev/null;          exec chroot {r} /bin/sh -c {inner}",
        r = rq,
        inner = shell_single_quote(&in_chroot),
    );
    // Outer: launch the whole thing in a fresh private mount namespace.
    format!(
        "exec unshare --mount --propagation private /bin/sh -c {}",
        shell_single_quote(&in_ns)
    )
}

/// Phase 5: validate a per-service rootfs subtree path. It is rendered into the
/// chroot/mount lines of the spawn script, so the charset is restricted to what
/// cannot break out of those commands (no whitespace, quotes, or shell
/// metacharacters — fail-closed rather than escaped). Absolute, non-root, no `..`.
pub(crate) fn validate_service_rootfs(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path == "/" {
        return Err(format!("rootfs {path:?} is not an absolute non-root path"));
    }
    if path.len() > 200 {
        return Err(format!("rootfs {path:?} exceeds 200 chars"));
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    {
        return Err(format!(
            "rootfs {path:?} contains characters outside [A-Za-z0-9/_.-] — refusing to render it into the guest spawn script (fail-closed)"
        ));
    }
    if path.contains("..") {
        return Err(format!("rootfs {path:?} contains '..'"));
    }
    Ok(())
}

/// The workload process, behind a trait so the supervisor orchestration is testable
/// without spawning. `start` receives the [`SpawnPlan`] (secret paths, not values).
pub trait Workload {
    fn start(&mut self, plan: &SpawnPlan) -> std::io::Result<()>;
    /// Stop the workload (SIGTERM then reap). Idempotent — not running ⇒ Ok(false).
    fn stop(&mut self) -> std::io::Result<bool>;
    /// Whether the workload child is currently running.
    fn is_running(&self) -> bool;
    /// Phase 6: run a `run_once` task TO COMPLETION and return its exit code (0 =
    /// success). A signal death maps to `128 + signal` (always non-zero) so the
    /// caller treats it as a failure. Output goes to the plan's per-service log
    /// files, same as a long-running service — the secret is still read only in the
    /// child at exec (via [`spawn_script`]), never in the agent.
    fn run_once(&mut self, plan: &SpawnPlan) -> std::io::Result<i32>;
}

/// Map an [`ExitStatus`](std::process::ExitStatus) to an int exit code: the normal
/// code, or `128 + signal` for a signal death (always non-zero), or `-1` if neither
/// is available. Used so a `run_once` killed by a signal still reads as a failure.
fn exit_code_of(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    -1
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
        // Phase 5: for a chroot'ed service the real cwd lives INSIDE the service
        // rootfs (applied after chroot, inside spawn_script), so the OUTER process
        // must start from a directory that exists in the base rootfs ("/"). A
        // plain service keeps its cwd as the outer working directory (unchanged).
        let outer_cwd: &str = if plan.rootfs.is_some() {
            "/"
        } else {
            plan.cwd.as_str()
        };
        c.arg("-c")
            .arg(spawn_script_for_workload(plan))
            .current_dir(outer_cwd);
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
        // v1.5 per-service logs: redirect this service's stdout/stderr to its own
        // files so one service's output never mixes into another's (or the agent's
        // /tmp/agent.log). Best-effort: if the dir/files can't be created (read-only
        // fs, exotic host), fall back to inherited stdio rather than failing to start.
        if let Some(parent) = plan.stdout_log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(out) = std::fs::File::create(&plan.stdout_log) {
            c.stdout(std::process::Stdio::from(out));
        }
        if let Ok(err) = std::fs::File::create(&plan.stderr_log) {
            c.stderr(std::process::Stdio::from(err));
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

    fn run_once(&mut self, plan: &SpawnPlan) -> std::io::Result<i32> {
        if plan.cmd.is_empty() {
            return Err(std::io::Error::other("run_once cmd is empty"));
        }
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c")
            .arg(spawn_script_for_workload(plan))
            .current_dir(&plan.cwd);
        #[cfg(unix)]
        {
            // Own process group, same as a service spawn: a run_once may itself be a
            // shell wrapper whose real work is a grandchild.
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        for (k, v) in &plan.base_env {
            c.env(k, v); // non-secret only
        }
        // Per-task logs: capture the migration's stdout/stderr (diagnostics + the
        // receipt points at these paths). Best-effort file creation like a service.
        if let Some(parent) = plan.stdout_log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(out) = std::fs::File::create(&plan.stdout_log) {
            c.stdout(std::process::Stdio::from(out));
        }
        if let Ok(err) = std::fs::File::create(&plan.stderr_log) {
            c.stderr(std::process::Stdio::from(err));
        }
        // Run to completion (spawn + wait). A run_once is NOT stored in `self.child`:
        // it has already exited, so `is_running` stays false and `stop` is a no-op.
        let status = c.status()?;
        Ok(exit_code_of(status))
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
        let Ok(sockaddr) = addr.parse::<std::net::SocketAddr>() else {
            return false;
        };
        let Ok(mut stream) =
            std::net::TcpStream::connect_timeout(&sockaddr, Duration::from_millis(500))
        else {
            return false;
        };
        let Some(path) = &spec.http_path else {
            return true; // TCP accept is enough.
        };
        let p = if path.starts_with('/') {
            path.clone()
        } else {
            format!("/{path}")
        };
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

/// Phase 6: the execution phase this boot is running under. `ATO_EXEC_PHASE`
/// (`seal_once`|`restore`|`run`) selects it; anything else (incl. unset) is `run`,
/// so a capsule with no run_once tasks is unaffected. Tests set the phase directly
/// via [`Supervisor::with_phase`] rather than the env (no cross-test env races).
fn default_exec_phase() -> ExecPhase {
    match std::env::var("ATO_EXEC_PHASE")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("seal_once") => ExecPhase::SealOnce,
        Some("restore") => ExecPhase::Restore,
        _ => ExecPhase::Run,
    }
}

/// Phase 6: the recorded outcome of one `run_once` task execution — captured in the
/// supervisor's agent state so a boot receipt can report the migration's exit
/// status and where its captured output landed. Holds NO secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOnceResult {
    /// The run_once service name.
    pub name: String,
    /// The phase it ran at (one of its declared `run_at` phases).
    pub phase: ExecPhase,
    /// Its exit code (0 = success; non-zero fails the whole guest).
    pub exit_code: i32,
    /// Per-task captured stdout/stderr paths (service_log_paths style).
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
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
    /// Phase 6: which execution phase this boot runs under (gates run_once timing).
    phase: ExecPhase,
    /// Phase 6: recorded run_once outcomes (exit status + captured-log paths) — the
    /// agent-state surface a boot receipt reports.
    run_once_results: Vec<RunOnceResult>,
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
            phase: default_exec_phase(),
            run_once_results: Vec::new(),
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

    /// Phase 6: set the execution phase (gates which run_once tasks execute).
    /// Chainable. Production reads it from `ATO_EXEC_PHASE`; tests set it here.
    pub fn with_phase(mut self, phase: ExecPhase) -> Self {
        self.phase = phase;
        self
    }

    /// Phase 6: the execution phase this supervisor runs under.
    pub fn phase(&self) -> ExecPhase {
        self.phase
    }

    /// Phase 6: recorded run_once outcomes (exit status + captured-log paths) — the
    /// agent-state surface a boot receipt reports.
    pub fn run_once_results(&self) -> &[RunOnceResult] {
        &self.run_once_results
    }

    /// Start EVERY service exactly once, when the session is bound-ready, driving the
    /// Phase-6 dependency DAG: services start in topological order; a `run_once` task
    /// whose declared timing includes the current phase RUNS TO COMPLETION at its
    /// slot and must exit 0 (a non-zero run_once fails the WHOLE guest). Two gate
    /// kinds are honored before an entry starts — readiness-gate deps (each target
    /// service must be READY) and success-gate deps (each target run_once must have
    /// EXITED 0, or been sealed at an earlier phase and skipped now). A compose/spawn
    /// error or a failed run_once is fail-closed: already-started services in this
    /// call are stopped so the caller never sees a partially-running group reported
    /// healthy. No-op if already started or not yet bound-ready.
    pub fn on_bound_ready(&mut self, bound_ready: bool) -> std::io::Result<bool> {
        if self.started || !bound_ready {
            return Ok(false);
        }
        // Phase 7 (generated internal bindings): materialize each generated
        // internal secret's freshly-generated value to the tmpfs sink BEFORE any
        // service starts — the target services reference these binding files in
        // their `bindings_env`, so `plan_spawn_service`'s fail-closed
        // existence check requires them present. Generated on EVERY start (a
        // restore overwrites whatever build-time value a snapshot froze, so the
        // frozen value never reaches the restored workload; a rotation restart
        // regenerates too). Fail-closed: a generation/sink error aborts the
        // start rather than serving with a missing/partial internal secret.
        self.materialize_generated_bindings()?;
        // v1.5 readiness graph / Phase-6 DAG: start in dependency (topological)
        // order. `start_order()` already fail-closed on cycle/unknown dep at
        // validation; re-derive here for the concrete order.
        let order = self
            .config
            .start_order()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let services = self.config.services();
        let by_name: BTreeMap<&str, &ServiceSpec> =
            services.iter().map(|s| (s.name.as_str(), s)).collect();
        // Readiness of a started service, remembered so a later dependent can gate.
        let mut ready: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // Phase 6: run_once outcomes so far, so a success-gate dependent can gate.
        // A run_once that RAN and exited 0 is in `succeeded`; one whose timing
        // excludes this phase is in `skipped` (it was applied at an earlier sealed
        // phase — its success is baked into the image, so the gate is satisfied).
        let mut succeeded: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut skipped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for name in &order {
            let svc = by_name[name.as_str()];
            // Readiness gate: every readiness-dep (legacy `depends_on` ∪
            // `depends_on_ready`) must be READY first. Targets are validated to be
            // long-running services.
            for dep in svc.ready_deps() {
                if !ready.contains(dep) {
                    let dep_spec = by_name[dep.as_str()];
                    if let Err(e) = self.wait_ready(dep, dep_spec) {
                        self.stop_all_started();
                        return Err(e);
                    }
                    ready.insert(dep.clone());
                }
            }
            // Success gate: every success-dep run_once must have exited 0 (or been
            // skipped this phase). Topological order guarantees it was processed
            // already; a failed run_once would have returned before reaching here, so
            // this is a defensive fail-closed check.
            for dep in &svc.depends_on_success {
                if !succeeded.contains(dep) && !skipped.contains(dep) {
                    self.stop_all_started();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("service '{name}': success-dependency '{dep}' did not succeed"),
                    ));
                }
            }

            match svc.kind {
                ServiceKind::RunOnce => {
                    // Timing gate: only run when THIS phase is in the task's `run_at`.
                    if !svc.run_at.contains(&self.phase) {
                        skipped.insert(name.clone());
                        continue;
                    }
                    let plan = match plan_spawn_service(svc, &self.bindings_root) {
                        Ok(p) => p,
                        Err(e) => {
                            self.stop_all_started();
                            return Err(e);
                        }
                    };
                    let mut w = (self.make_workload)();
                    let code = match w.run_once(&plan) {
                        Ok(c) => c,
                        Err(e) => {
                            self.stop_all_started();
                            return Err(std::io::Error::new(
                                e.kind(),
                                format!("run_once '{name}' failed to execute: {e}"),
                            ));
                        }
                    };
                    // Record the outcome in agent state (a boot receipt reports it).
                    self.run_once_results.push(RunOnceResult {
                        name: name.clone(),
                        phase: self.phase,
                        exit_code: code,
                        stdout_log: plan.stdout_log.clone(),
                        stderr_log: plan.stderr_log.clone(),
                    });
                    if code != 0 {
                        // A failed migration fails the WHOLE guest, fail-closed.
                        self.stop_all_started();
                        return Err(std::io::Error::other(format!(
                            "run_once '{name}' failed with exit code {code} at phase {:?}",
                            self.phase
                        )));
                    }
                    succeeded.insert(name.clone());
                }
                ServiceKind::Service => {
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
                        // Name the service in the diagnostic (per-service logs at
                        // service_log_paths(name) hold its output).
                        return Err(std::io::Error::new(
                            e.kind(),
                            format!("service '{name}' failed to start: {e}"),
                        ));
                    }
                    self.workloads.push(w);
                }
            }
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
        // Phase 7: scrub the generated internal secrets from tmpfs when the group
        // stops. Load-bearing for the build boundary: the pre-snapshot
        // `StopWorkload` must leave no generated value in the frozen image
        // (matching the leased-secret scrub). A later start (restore or rotation)
        // regenerates a fresh value.
        self.scrub_generated_bindings();
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
        self.scrub_generated_bindings();
    }

    /// Phase 7: generate + materialize every generated internal binding to the
    /// tmpfs sink at `bindings_root` (the same sink leased secrets land on).
    fn materialize_generated_bindings(&self) -> std::io::Result<()> {
        if self.config.generated_bindings.is_empty() {
            return Ok(());
        }
        let sink = crate::tmpfs::TmpfsBindingSink::new(&self.bindings_root);
        materialize_generated_bindings(&self.config.generated_bindings, &sink)?;
        Ok(())
    }

    /// Phase 7: scrub every generated internal binding file (best-effort).
    fn scrub_generated_bindings(&self) {
        if self.config.generated_bindings.is_empty() {
            return;
        }
        let sink = crate::tmpfs::TmpfsBindingSink::new(&self.bindings_root);
        for g in &self.config.generated_bindings {
            if let Ok(name) = BindingName::parse(g.name.as_str()) {
                let _ = sink.scrub(&name);
            }
        }
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

// The guest-agent is a LINUX-ONLY runtime component (it runs inside the
// Firecracker guest): its behavior tests spawn `sh`, use unix paths/perms, and
// exercise mount/vsock semantics that do not exist on Windows CI runners.
#[cfg(all(test, unix))]
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
        assert_eq!(
            cfg.bindings_env.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai")
        );
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
        bad(
            r#"{"services":[{"name":"api","cmd":["python3","api.py"]}],"bindings_env":{"OPENAI_API_KEY":"openai_api_key"}}"#,
        );
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
        let one =
            SupervisorConfig::from_json(r#"{"services":[{"name":"a","cmd":["true"]}]}"#).unwrap();
        assert!(plan_spawn(&one, dir.path()).is_ok());
    }

    #[test]
    fn config_rejects_neither_shape_and_duplicate_names() {
        // Neither → rejected.
        assert!(
            SupervisorConfig::from_json(r#"{}"#).is_err(),
            "empty config rejected"
        );
        assert!(
            SupervisorConfig::from_json(r#"{"services":[]}"#).is_err(),
            "empty services rejected"
        );
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
    fn pty_mode_is_backward_compatible_and_single_service_only() {
        let legacy = SupervisorConfig::from_json(r#"{"cmd":["true"]}"#).unwrap();
        assert_eq!(legacy.stdio_mode, StdioMode::Log);

        let terminal = SupervisorConfig::from_json(
            r#"{"stdio_mode":"pty","services":[{"name":"tui","cmd":["tui"]}]}"#,
        )
        .unwrap();
        assert_eq!(terminal.stdio_mode, StdioMode::Pty);

        assert!(
            SupervisorConfig::from_json(
                r#"{"stdio_mode":"pty","services":[{"name":"a","cmd":["a"]},{"name":"b","cmd":["b"]}]}"#,
            )
            .is_err()
        );
        assert!(
            SupervisorConfig::from_json(
                r#"{"stdio_mode":"pty","services":[{"name":"task","cmd":["task"],"kind":"run_once","run_at":["run"]}]}"#,
            )
            .is_err()
        );
    }

    /// v1.6 (ato#983) Slice 3: `SupervisorConfig.volumes` validation.
    #[test]
    fn config_accepts_a_valid_volume() {
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[{"name":"x","cmd":["a"]}],
                "volumes":[{"state_name":"dbdata","target":"/ato/state/dbdata","fs_label":"ASlabel0000000"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.volumes.len(), 1);
        assert_eq!(cfg.volumes[0].state_name, "dbdata");
    }

    #[test]
    fn config_rejects_a_volume_with_an_empty_state_name_or_fs_label() {
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[{"state_name":"","target":"/ato/state/dbdata","fs_label":"ASlabel"}]}"#
            )
            .is_err(),
            "empty state_name rejected"
        );
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[{"state_name":"dbdata","target":"/ato/state/dbdata","fs_label":""}]}"#
            )
            .is_err(),
            "empty fs_label rejected"
        );
    }

    #[test]
    fn config_rejects_duplicate_volume_state_name_or_fs_label() {
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[
                        {"state_name":"dbdata","target":"/ato/state/a","fs_label":"LBL_A"},
                        {"state_name":"dbdata","target":"/ato/state/b","fs_label":"LBL_B"}
                    ]}"#
            )
            .is_err(),
            "duplicate state_name rejected"
        );
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[
                        {"state_name":"aaa","target":"/ato/state/a","fs_label":"LBL_SAME"},
                        {"state_name":"bbb","target":"/ato/state/b","fs_label":"LBL_SAME"}
                    ]}"#
            )
            .is_err(),
            "duplicate fs_label rejected (device resolution would be ambiguous)"
        );
    }

    #[test]
    fn config_rejects_a_malformed_or_conflicting_volume_target() {
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[{"state_name":"dbdata","target":"/etc/passwd","fs_label":"LBL_A"}]}"#
            )
            .is_err(),
            "target outside /ato/state/ rejected"
        );
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"x","cmd":["a"]}],
                    "volumes":[
                        {"state_name":"aaa","target":"/ato/state/db","fs_label":"LBL_A"},
                        {"state_name":"bbb","target":"/ato/state/db/backup","fs_label":"LBL_B"}
                    ]}"#
            )
            .is_err(),
            "nested/overlapping targets rejected"
        );
    }

    #[test]
    fn supervisor_starts_and_stops_the_whole_group_as_a_unit() {
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

        assert!(
            sup.on_bound_ready(true).is_err(),
            "missing binding fails the group"
        );
        assert!(!sup.is_running(), "the started service was rolled back");
        assert!(!sup.started(), "group is not marked started");
        assert_eq!(*st.live.borrow(), 0, "no service left running");
    }

    // ── v1.5 (ato#973): per-service logs ──

    #[test]
    fn per_service_log_paths_are_distinct_deterministic_and_path_safe() {
        // Env-independent: assert the FILENAME shape + distinctness + determinism
        // against whatever the current service_log_dir() is (no shared-env mutation,
        // so this can't race a parallel test).
        let dir = service_log_dir();
        let (api_out, api_err) = service_log_paths("api");
        let (redis_out, redis_err) = service_log_paths("redis");
        // api and redis never share a log file; stdout ≠ stderr.
        assert_ne!(api_out, redis_out);
        assert_ne!(api_out, api_err);
        assert_ne!(redis_out, redis_err);
        assert_eq!(api_out.file_name().unwrap(), "api.stdout.log");
        assert_eq!(redis_err.file_name().unwrap(), "redis.stderr.log");
        // Deterministic (same input → same path).
        assert_eq!(service_log_paths("api").0, api_out);
        // Path-safe: a DNS-safe service name yields a single filename component under
        // the dir — it cannot contain `/`, `.`-only, or `..` traversal.
        for name in ["api", "redis", "my-worker", "svc0"] {
            let (out, _) = service_log_paths(name);
            assert_eq!(out.parent().unwrap(), dir);
            let file = out.file_name().unwrap().to_string_lossy();
            assert!(
                !file.contains('/') && file != ".." && file != ".",
                "path-safe: {file}"
            );
        }
    }

    #[test]
    fn each_service_plan_carries_its_own_log_paths_no_value_leak() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-SECRET-VALUE");
        let api = ServiceSpec {
            name: "api".into(),
            cmd: vec!["python3".into(), "api.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".into(), "openai".into())]),
            depends_on: Vec::new(),
            readiness: None,
            rootfs: None,
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        };
        let plan = plan_spawn_service(&api, dir.path()).unwrap();
        assert!(
            plan.stdout_log
                .to_string_lossy()
                .ends_with("api.stdout.log")
        );
        assert!(
            plan.stderr_log
                .to_string_lossy()
                .ends_with("api.stderr.log")
        );
        // The plan (which is what feeds the spawn) never carries the secret VALUE —
        // only the tmpfs path. A reported diagnostic built from the plan is clean.
        assert!(
            !format!("{plan:?}").contains("sk-SECRET-VALUE"),
            "plan carries no value"
        );
    }

    #[test]
    fn a_real_child_writes_to_its_per_service_log_not_shared_stdout() {
        // Spawn a real child that prints to stdout + stderr; assert its output landed
        // in THIS service's own files (read via the PLAN's captured paths, so the
        // test is robust against any parallel change to the log-dir env).
        let bindings = tempfile::tempdir().unwrap();
        let svc = ServiceSpec {
            name: "logtest-api".into(), // distinct name → own files in the default dir
            cmd: vec!["sh".into(), "-c".into(), "echo OUT; echo ERR 1>&2".into()],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            depends_on: Vec::new(),
            readiness: None,
            rootfs: None,
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        };
        let plan = plan_spawn_service(&svc, bindings.path()).unwrap();
        let mut w = ChildWorkload::default();
        w.start(&plan).unwrap();
        // Wait for the short-lived child to finish.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while w.is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = std::fs::read_to_string(&plan.stdout_log).unwrap_or_default();
        let err = std::fs::read_to_string(&plan.stderr_log).unwrap_or_default();
        assert!(
            out.contains("OUT"),
            "stdout in the service's stdout log: {out:?}"
        );
        assert!(
            err.contains("ERR"),
            "stderr in the service's stderr log: {err:?}"
        );
        assert!(
            !out.contains("ERR"),
            "stderr must NOT leak into the stdout log"
        );
        let _ = w.stop();
        let _ = std::fs::remove_file(&plan.stdout_log);
        let _ = std::fs::remove_file(&plan.stderr_log);
    }

    #[test]
    fn a_failed_service_start_names_the_service_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        // A workload whose start() always fails, so on_bound_ready surfaces the name.
        #[derive(Clone, Default)]
        struct FailWorkload;
        impl Workload for FailWorkload {
            fn start(&mut self, _: &SpawnPlan) -> std::io::Result<()> {
                Err(std::io::Error::other("boom"))
            }
            fn stop(&mut self) -> std::io::Result<bool> {
                Ok(false)
            }
            fn is_running(&self) -> bool {
                false
            }
            fn run_once(&mut self, _: &SpawnPlan) -> std::io::Result<i32> {
                Err(std::io::Error::other("boom"))
            }
        }
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[{"name":"redis","cmd":["redis-server"]}]}"#,
        )
        .unwrap();
        let mut sup = Supervisor::new(cfg, dir.path(), FailWorkload::default);
        let err = sup.on_bound_ready(true).unwrap_err();
        assert!(
            err.to_string().contains("service 'redis'"),
            "names the failed service: {err}"
        );
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
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"api","cmd":["true"],"depends_on":["ghost"]}]}"#
            )
            .is_err()
        );
        // Self dependency → rejected.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"api","cmd":["true"],"depends_on":["api"]}]}"#
            )
            .is_err()
        );
        // Cycle → rejected.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[
                {"name":"a","cmd":["true"],"depends_on":["b"]},
                {"name":"b","cmd":["true"],"depends_on":["a"]}
            ]}"#
            )
            .is_err()
        );
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
        let mut sup = fast_readiness(
            Supervisor::new(cfg, dir.path(), move || fake.clone()),
            probe,
        );

        assert!(sup.on_bound_ready(true).unwrap());
        // Both started, redis before api (topological).
        let starts = st.starts.borrow();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].cmd, vec!["redis"], "redis starts first");
        assert_eq!(
            starts[1].cmd,
            vec!["api"],
            "api starts after redis is ready"
        );
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
        let mut sup = fast_readiness(
            Supervisor::new(cfg, dir.path(), move || fake.clone()),
            probe,
        );

        let err = sup.on_bound_ready(true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(!sup.is_running(), "the group rolled back");
        assert!(!sup.started());
        assert_eq!(*st.live.borrow(), 0);
        // api NEVER started (its dependency never became ready).
        assert!(
            st.starts
                .borrow()
                .iter()
                .all(|p| p.cmd != vec!["api".to_string()])
        );
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
        let mut sup = fast_readiness(
            Supervisor::new(cfg, dir.path(), move || fake.clone()),
            probe,
        );
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 2);
        assert!(
            pst.borrow().calls.is_empty(),
            "no probe for a probeless dependency"
        );
    }

    // ── Phase 6: run_once / migration DAG ──

    /// A run_once cfg mirroring the Blinko/Postgres migration shape from the plan.
    fn dag_cfg(migrate_run_at: &str) -> SupervisorConfig {
        SupervisorConfig::from_json(&format!(
            r#"{{"services":[
                {{"name":"postgres","cmd":["postgres"],"readiness":{{"port":5432}}}},
                {{"name":"migrate","cmd":["migrate","up"],"kind":"run_once",
                  "run_at":{migrate_run_at},"depends_on_ready":["postgres"]}},
                {{"name":"api","cmd":["api"],"depends_on_success":["migrate"],"depends_on_ready":["postgres"]}},
                {{"name":"worker","cmd":["worker"],"depends_on_ready":["postgres","api"]}}
            ]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn run_once_kind_and_timing_parse_and_defaults() {
        // kind defaults to "service"; a plain service has kind Service, run_at empty.
        let svc =
            SupervisorConfig::from_json(r#"{"services":[{"name":"a","cmd":["x"]}]}"#).unwrap();
        assert_eq!(svc.services()[0].kind, ServiceKind::Service);
        assert!(svc.services()[0].run_at.is_empty());
        // A run_once round-trips kind + timing.
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[{"name":"m","cmd":["migrate"],"kind":"run_once","run_at":["seal_once","restore"]}]}"#,
        )
        .unwrap();
        let m = &cfg.services()[0];
        assert_eq!(m.kind, ServiceKind::RunOnce);
        assert_eq!(m.run_at, vec![ExecPhase::SealOnce, ExecPhase::Restore]);
    }

    #[test]
    fn run_once_dag_topological_order_and_cycle_detection() {
        // The DAG is over BOTH edge kinds. postgres → migrate → api → worker.
        let order = dag_cfg(r#"["run"]"#).start_order().unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(
            pos("postgres") < pos("migrate"),
            "readiness dep orders postgres first"
        );
        assert!(
            pos("migrate") < pos("api"),
            "success dep orders migrate before api"
        );
        assert!(pos("api") < pos("worker"), "api before worker");

        // A cycle THROUGH a success edge is detected (fail-closed at parse):
        // a(run_once) depends_on_success b(run_once), b depends_on_ready a-as-service?
        // Build a pure cycle across the two edge kinds: svc → run_once → svc.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[
                    {"name":"svc","cmd":["x"],"depends_on_success":["m"]},
                    {"name":"m","cmd":["y"],"kind":"run_once","run_at":["run"],"depends_on_ready":["svc"]}
                ]}"#
            )
            .is_err(),
            "cycle across success+ready edges rejected"
        );
        // Self dependency via a new edge kind → rejected.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"m","cmd":["y"],"kind":"run_once","run_at":["run"],"depends_on_success":["m"]}]}"#
            )
            .is_err(),
            "self success-dependency rejected"
        );
        // Unknown target via a new edge kind → rejected.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"api","cmd":["x"],"depends_on_success":["ghost"]}]}"#
            )
            .is_err(),
            "unknown success-dependency target rejected"
        );
    }

    #[test]
    fn config_rejects_incoherent_kind_timing_and_edge_targets() {
        // run_once must declare timing.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"m","cmd":["x"],"kind":"run_once"}]}"#
            )
            .is_err(),
            "run_once without run_at rejected"
        );
        // run_once must not declare a long-running readiness probe.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"m","cmd":["x"],"kind":"run_once","run_at":["run"],"readiness":{"port":8080}}]}"#
            )
            .is_err(),
            "run_once with readiness rejected"
        );
        // a plain service must not declare run_at.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"a","cmd":["x"],"run_at":["run"]}]}"#
            )
            .is_err(),
            "service with run_at rejected"
        );
        // a readiness gate must point at a service, not a run_once.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[
                    {"name":"m","cmd":["x"],"kind":"run_once","run_at":["run"]},
                    {"name":"a","cmd":["y"],"depends_on_ready":["m"]}
                ]}"#
            )
            .is_err(),
            "depends_on_ready on a run_once rejected"
        );
        // a success gate must point at a run_once, not a service.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[
                    {"name":"svc","cmd":["x"]},
                    {"name":"a","cmd":["y"],"depends_on_success":["svc"]}
                ]}"#
            )
            .is_err(),
            "depends_on_success on a service rejected"
        );
        // The coherent DAG loads.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"svc","cmd":["x"]},
                {"name":"m","cmd":["y"],"kind":"run_once","run_at":["run"],"depends_on_ready":["svc"]},
                {"name":"a","cmd":["z"],"depends_on_success":["m"]}
            ]}"#
        )
        .is_ok());
    }

    #[test]
    fn depends_on_success_gates_a_service_on_run_once_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let probe = FakeProbe::ready_after(5432, 0); // postgres ready immediately
        // Phase = Run so the migrate (run_at=["run"]) executes.
        let mut sup = fast_readiness(
            Supervisor::new(dag_cfg(r#"["run"]"#), dir.path(), move || fake.clone()),
            probe,
        )
        .with_phase(ExecPhase::Run);

        assert!(sup.on_bound_ready(true).unwrap());
        // migrate ran exactly once (as a run_once, NOT a long-running service).
        assert_eq!(st.run_onces.borrow().len(), 1);
        assert_eq!(st.run_onces.borrow()[0].cmd, vec!["migrate", "up"]);
        // The long-running services started; migrate is NOT among them.
        let started: Vec<Vec<String>> = st.starts.borrow().iter().map(|p| p.cmd.clone()).collect();
        assert!(started.contains(&vec!["postgres".to_string()]));
        assert!(started.contains(&vec!["api".to_string()]));
        assert!(started.contains(&vec!["worker".to_string()]));
        assert!(
            !started
                .iter()
                .any(|c| c == &vec!["migrate".to_string(), "up".to_string()]),
            "a run_once is never a long-running service"
        );
        // api must have started AFTER migrate's run_once completed (success gate).
        let api_idx = st
            .starts
            .borrow()
            .iter()
            .position(|p| p.cmd == vec!["api"])
            .unwrap();
        assert!(api_idx > 0, "api gated behind its success-dependency");
    }

    #[test]
    fn run_once_timing_skips_out_of_phase_and_still_satisfies_success_gate() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let probe = FakeProbe::ready_after(5432, 0);
        // migrate runs only at seal_once; we're at Run → it must be SKIPPED, yet api
        // (depends_on_success=[migrate]) must still start (the migration was sealed
        // into the image at build, so the gate is satisfied by the skip).
        let mut sup = fast_readiness(
            Supervisor::new(dag_cfg(r#"["seal_once"]"#), dir.path(), move || {
                fake.clone()
            }),
            probe,
        )
        .with_phase(ExecPhase::Run);

        assert!(sup.on_bound_ready(true).unwrap());
        assert!(
            st.run_onces.borrow().is_empty(),
            "run_once skipped out of phase"
        );
        assert!(
            sup.run_once_results().is_empty(),
            "no result recorded for a skipped run_once"
        );
        // api + worker still started (success gate satisfied by the seal-time skip).
        assert!(
            st.starts.borrow().iter().any(|p| p.cmd == vec!["api"]),
            "api started"
        );
        assert!(
            st.starts.borrow().iter().any(|p| p.cmd == vec!["worker"]),
            "worker started"
        );
    }

    #[test]
    fn run_once_executes_when_phase_matches_timing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let probe = FakeProbe::ready_after(5432, 0);
        // migrate runs at seal_once; drive the SealOnce phase → it executes.
        let mut sup = fast_readiness(
            Supervisor::new(
                dag_cfg(r#"["seal_once","restore"]"#),
                dir.path(),
                move || fake.clone(),
            ),
            probe,
        )
        .with_phase(ExecPhase::SealOnce);
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(
            st.run_onces.borrow().len(),
            1,
            "run_once executes at a matching phase"
        );
        assert_eq!(sup.run_once_results()[0].phase, ExecPhase::SealOnce);
    }

    #[test]
    fn a_failed_run_once_fails_the_whole_guest_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        // migrate exits non-zero → the WHOLE guest fails; no service is left running.
        fake.set_run_once_exit("migrate up", 3);
        let probe = FakeProbe::ready_after(5432, 0);
        let mut sup = fast_readiness(
            Supervisor::new(dag_cfg(r#"["run"]"#), dir.path(), move || fake.clone()),
            probe,
        )
        .with_phase(ExecPhase::Run);

        let err = sup.on_bound_ready(true).unwrap_err();
        assert!(
            err.to_string().contains("exit code 3"),
            "names the failing exit code: {err}"
        );
        assert!(!sup.started(), "the group is not marked started");
        assert!(
            !sup.is_running(),
            "no service left running after a failed migration"
        );
        assert_eq!(*st.live.borrow(), 0);
        // The failed outcome is still RECORDED (a receipt reports the failure).
        assert_eq!(sup.run_once_results().len(), 1);
        assert_eq!(sup.run_once_results()[0].exit_code, 3);
        // api never started (its migration failed).
        assert!(
            st.starts
                .borrow()
                .iter()
                .all(|p| p.cmd != vec!["api".to_string()])
        );
    }

    #[test]
    fn run_once_exit_status_and_captured_output_recorded_in_receipt() {
        // Real child: a run_once that prints and exits 0. Its exit code + captured
        // stdout land in the supervisor's agent state (the receipt surface).
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"db-migrate","cmd":["sh","-c","echo APPLIED; exit 0"],"cwd":"/tmp","kind":"run_once","run_at":["run"]}
            ]}"#,
        )
        .unwrap();
        let mut sup =
            Supervisor::new(cfg, dir.path(), ChildWorkload::default).with_phase(ExecPhase::Run);
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(sup.run_once_results().len(), 1);
        let r = &sup.run_once_results()[0];
        assert_eq!(r.name, "db-migrate");
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.phase, ExecPhase::Run);
        // The task's stdout was captured to its per-task log (service_log_paths style).
        let out = std::fs::read_to_string(&r.stdout_log).unwrap_or_default();
        assert!(out.contains("APPLIED"), "run_once stdout captured: {out:?}");
        let _ = std::fs::remove_file(&r.stdout_log);
        let _ = std::fs::remove_file(&r.stderr_log);
    }

    #[test]
    fn a_real_run_once_nonzero_exit_fails_the_guest_and_records_the_code() {
        // Real child exiting 7 → run_once fails the guest; the exact code is recorded.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"m","cmd":["sh","-c","exit 7"],"cwd":"/tmp","kind":"run_once","run_at":["run"]}
            ]}"#,
        )
        .unwrap();
        let mut sup =
            Supervisor::new(cfg, dir.path(), ChildWorkload::default).with_phase(ExecPhase::Run);
        let err = sup.on_bound_ready(true).unwrap_err();
        assert!(err.to_string().contains("exit code 7"), "{err}");
        assert_eq!(sup.run_once_results()[0].exit_code, 7);
        let r = &sup.run_once_results()[0];
        let _ = std::fs::remove_file(&r.stdout_log);
        let _ = std::fs::remove_file(&r.stderr_log);
    }

    #[test]
    fn config_rejects_malformed_env_var_and_binding_names() {
        // A supervisor config is fail-closed: bad env var / binding names are
        // rejected at load, never sanitized or interpolated into the spawn script.
        let bad_var = r#"{"cmd":["true"],"bindings_env":{"BAD NAME":"openai"}}"#;
        assert!(
            SupervisorConfig::from_json(bad_var).is_err(),
            "space in env var name"
        );
        let inject = r#"{"cmd":["true"],"bindings_env":{"X; rm -rf /":"openai"}}"#;
        assert!(
            SupervisorConfig::from_json(inject).is_err(),
            "shell metachars in env var name"
        );
        let lead_digit = r#"{"cmd":["true"],"bindings_env":{"1KEY":"openai"}}"#;
        assert!(
            SupervisorConfig::from_json(lead_digit).is_err(),
            "env var starting with a digit"
        );
        let bad_binding = r#"{"cmd":["true"],"bindings_env":{"KEY":"../escape"}}"#;
        assert!(
            SupervisorConfig::from_json(bad_binding).is_err(),
            "path-traversal binding name"
        );
        let bad_base = r#"{"cmd":["true"],"base_env":{"BAD-VAR":"1"}}"#;
        assert!(
            SupervisorConfig::from_json(bad_base).is_err(),
            "invalid base_env var name"
        );
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
            stdio_mode: StdioMode::Log,
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::from([("PORT".to_string(), "8080".to_string())]),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
        };
        let plan = plan_spawn(&cfg, dir.path()).unwrap();
        assert_eq!(plan.base_env.get("PORT").map(String::as_str), Some("8080"));
        assert_eq!(plan.secret_env.len(), 1);
        assert_eq!(plan.secret_env[0].0, "OPENAI_API_KEY");
        assert_eq!(plan.secret_env[0].1, dir.path().join("openai"));
        // The value is NOT anywhere in the plan (never read into the agent).
        assert!(
            !format!("{plan:?}").contains("sk-REAL-VALUE"),
            "plan must not carry the value"
        );

        // The spawn script reads the value from the file at exec, in the child.
        let script = spawn_script(&plan);
        assert!(
            script.contains("export OPENAI_API_KEY=\"$(cat "),
            "{script}"
        );
        assert!(script.contains("exec 'python3' 'app.py'"), "{script}");
        assert!(
            !script.contains("sk-REAL-VALUE"),
            "script must not carry the value"
        );

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
        /// Phase 6: run_once executions recorded (plan per run), and a per-cmd exit
        /// code table (cmd.join(" ") → code, default 0) so a test can make a specific
        /// run_once fail.
        run_onces: RefCell<Vec<SpawnPlan>>,
        run_once_exit: RefCell<BTreeMap<String, i32>>,
    }
    impl FakeWorkload {
        /// Make the run_once whose cmd joins to `key` exit with `code`.
        fn set_run_once_exit(&self, key: &str, code: i32) {
            self.0
                .run_once_exit
                .borrow_mut()
                .insert(key.to_string(), code);
        }
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
        fn run_once(&mut self, plan: &SpawnPlan) -> std::io::Result<i32> {
            self.0.run_onces.borrow_mut().push(plan.clone());
            let key = plan.cmd.join(" ");
            Ok(self
                .0
                .run_once_exit
                .borrow()
                .get(&key)
                .copied()
                .unwrap_or(0))
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
            stdio_mode: StdioMode::Log,
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
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
            stdio_mode: StdioMode::Log,
            cmd: vec!["true".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), FakeWorkload::default);
        assert!(
            !sup.stop_workload().unwrap(),
            "nothing to stop ⇒ was_running=false"
        );
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
            stdio_mode: StdioMode::Log,
            cmd: vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "printf %s \"$OPENAI_API_KEY\" > {}; sleep 30",
                    out.display()
                ),
            ],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "openai".to_string())]),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
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
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "sk-CHILD-ONLY-VALUE"
        );
        assert!(
            sup.stop_workload().unwrap(),
            "a live child reports was_running=true"
        );
        assert!(!sup.is_running());
    }

    #[test]
    fn stop_is_bounded_and_sigkills_a_sigterm_ignoring_child() {
        // A workload that TRAPS/ignores SIGTERM must not stall the seal: stop must
        // return within ~grace + reap via SIGKILL. `trap '' TERM` ignores SIGTERM.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            stdio_mode: StdioMode::Log,
            cmd: vec![
                "sh".into(),
                "-c".into(),
                "trap '' TERM; while true; do sleep 1; done".into(),
            ],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        assert!(sup.is_running());
        let started = std::time::Instant::now();
        assert!(
            sup.stop_workload().unwrap(),
            "bounded stop still reports was_running"
        );
        let elapsed = started.elapsed();
        assert!(!sup.is_running());
        // Bounded: grace (2s) + SIGKILL reap, comfortably under 10s.
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "stop took {elapsed:?} — not bounded"
        );
    }

    #[test]
    fn stop_returns_promptly_for_a_well_behaved_child() {
        // A child that exits on SIGTERM is reaped well inside the grace window.
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig {
            stdio_mode: StdioMode::Log,
            cmd: vec!["sleep".into(), "300".into()], // default SIGTERM disposition = terminate
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        let started = std::time::Instant::now();
        assert!(sup.stop_workload().unwrap());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "normal stop should be fast"
        );
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
            stdio_mode: StdioMode::Log,
            cmd: vec!["sh".into(), "-c".into(), "sleep 300; true".into()],
            cwd: "/tmp".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            services: Vec::new(),
            volumes: Vec::new(),
            generated_bindings: Vec::new(),
        };
        let mut sup = Supervisor::new(cfg, dir.path(), ChildWorkload::default);
        assert!(sup.on_bound_ready(true).unwrap());
        // The spawn put the wrapper in its own process group (pgid = child pid) —
        // the property killpg-stop relies on.
        let pid = match sup.workloads.first().and_then(|w| w.child.as_ref()) {
            Some(ch) => ch.id() as i32,
            None => panic!("child running"),
        };
        assert_eq!(
            unsafe { libc::getpgid(pid) },
            pid,
            "workload must lead its own process group"
        );
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

    // ── Phase 5 (multi-image rootfs): per-service mount-ns + chroot launch ──

    #[test]
    fn validate_service_rootfs_accepts_the_layout_path_and_rejects_unsafe_ones() {
        assert!(validate_service_rootfs("/opt/ato/services/web/rootfs").is_ok());
        assert!(validate_service_rootfs("/opt/ato/services/postgres/rootfs").is_ok());
        for (bad, why) in [
            ("opt/x", "relative"),
            ("/", "root"),
            ("/opt/ato dir", "whitespace"),
            ("/opt/ato;rm", "shell metacharacter"),
            ("/opt/../etc", "dot-dot"),
        ] {
            assert!(validate_service_rootfs(bad).is_err(), "{why}");
        }
    }

    #[test]
    fn a_rootfs_service_spawn_script_wraps_the_launch_in_unshare_and_chroot() {
        let bindings = tempfile::tempdir().unwrap();
        let svc = ServiceSpec {
            name: "web".into(),
            cmd: vec!["node".into(), "server.js".into()],
            cwd: "/srv/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            depends_on: Vec::new(),
            readiness: None,
            rootfs: Some("/opt/ato/services/web/rootfs".into()),
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        };
        let plan = plan_spawn_service(&svc, bindings.path()).unwrap();
        assert_eq!(plan.rootfs.as_deref(), Some("/opt/ato/services/web/rootfs"));
        let script = spawn_script(&plan);
        // Ordering is load-bearing: unshare (new mount ns) → chroot → exec cmd.
        let ns = script
            .find("unshare --mount")
            .expect("enters a mount namespace");
        let cr = script
            .find("chroot")
            .expect("chroots into the service rootfs");
        assert!(ns < cr, "unshare must precede chroot:\n{script}");
        // The pseudo-filesystems are mounted under the SERVICE rootfs subtree, not
        // the base `/` (the rootfs path is single-quoted, then concatenated with
        // /proc etc. — assert on substrings that survive the nested quote layers).
        assert!(
            script.contains("mount -t proc proc"),
            "mounts proc:\n{script}"
        );
        assert!(
            script.contains("/opt/ato/services/web/rootfs"),
            "targets the service rootfs:\n{script}"
        );
        // The chroot must give the workload a WRITABLE /run and the /dev/fd
        // (+ std stream) symlinks — a fresh devtmpfs has neither, and many images
        // need both (e.g. postgres initdb's `<(...)` process substitution reads
        // /dev/fd, and its socket/pid dir under /run must be writable on the RO
        // guest root). Without these a DB-backed multi-service app never starts.
        assert!(
            script.contains("/run 2>/dev/null"),
            "mounts a writable /run in the chroot:\n{script}"
        );
        assert!(
            script.contains("ln -sf /proc/self/fd "),
            "creates /dev/fd -> /proc/self/fd in the chroot:\n{script}"
        );
        // The workload cwd is applied INSIDE the chroot, AFTER the chroot call.
        let cd = script
            .find("cd ")
            .expect("cd into workload cwd inside chroot");
        assert!(cr < cd, "cd happens after chroot:\n{script}");
        assert!(
            script.contains("/srv/app"),
            "cwd present inside chroot:\n{script}"
        );
        // The command tokens survive into the innermost exec (through the quote layers).
        assert!(
            script.contains("node") && script.contains("server.js"),
            "{script}"
        );
    }

    #[test]
    fn a_plain_service_without_rootfs_execs_directly_unchanged() {
        // No rootfs → byte-identical legacy launch: a direct `exec`, no unshare/chroot.
        let bindings = tempfile::tempdir().unwrap();
        let svc = ServiceSpec {
            name: "app".into(),
            cmd: vec!["python3".into(), "app.py".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::new(),
            depends_on: Vec::new(),
            readiness: None,
            rootfs: None,
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        };
        let script = spawn_script(&plan_spawn_service(&svc, bindings.path()).unwrap());
        assert!(script.contains("exec 'python3' 'app.py'"), "{script}");
        assert!(
            !script.contains("unshare") && !script.contains("chroot"),
            "{script}"
        );
    }

    #[test]
    fn a_rootfs_service_reads_its_secret_before_the_chroot() {
        // The secret export must precede the unshare/chroot so the workload inherits
        // it across the chroot exec (the tmpfs binding file is in the OUTER rootfs,
        // gone after chroot).
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "openai", "sk-VALUE");
        let svc = ServiceSpec {
            name: "web".into(),
            cmd: vec!["node".into(), "server.js".into()],
            cwd: "/srv".into(),
            base_env: BTreeMap::new(),
            bindings_env: BTreeMap::from([("OPENAI_API_KEY".into(), "openai".into())]),
            depends_on: Vec::new(),
            readiness: None,
            rootfs: Some("/opt/ato/services/web/rootfs".into()),
            kind: ServiceKind::Service,
            run_at: Vec::new(),
            depends_on_ready: Vec::new(),
            depends_on_success: Vec::new(),
        };
        let script = spawn_script(&plan_spawn_service(&svc, dir.path()).unwrap());
        let export = script
            .find("export OPENAI_API_KEY=")
            .expect("secret exported");
        let unshare = script.find("unshare").expect("enters ns");
        assert!(
            export < unshare,
            "secret must be read before the chroot:\n{script}"
        );
        assert!(
            !script.contains("sk-VALUE"),
            "value never appears in the script"
        );
    }

    #[test]
    fn supervisor_config_parses_and_validates_per_service_rootfs() {
        // A multi-image supervisor.json carries a rootfs per service; it loads and
        // round-trips the field.
        let cfg = SupervisorConfig::from_json(
            r#"{"services":[
                {"name":"web","cmd":["node","server.js"],"rootfs":"/opt/ato/services/web/rootfs"},
                {"name":"redis","cmd":["redis-server"],"rootfs":"/opt/ato/services/redis/rootfs"}
            ]}"#,
        )
        .unwrap();
        let svcs = cfg.services();
        assert_eq!(
            svcs[0].rootfs.as_deref(),
            Some("/opt/ato/services/web/rootfs")
        );
        assert_eq!(
            svcs[1].rootfs.as_deref(),
            Some("/opt/ato/services/redis/rootfs")
        );
        // A malformed rootfs is rejected AT LOAD (never sanitized).
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"web","cmd":["a"],"rootfs":"/opt/../etc"}]}"#
            )
            .is_err(),
            "dot-dot rootfs rejected"
        );
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"web","cmd":["a"],"rootfs":"relative/path"}]}"#
            )
            .is_err(),
            "relative rootfs rejected"
        );
        // A legacy config (no rootfs) serializes WITHOUT the field (skip_if None).
        let legacy = SupervisorConfig::from_json(r#"{"cmd":["true"]}"#).unwrap();
        let json = serde_json::to_string(&legacy.services()[0]).unwrap();
        assert!(
            !json.contains("rootfs"),
            "None rootfs must be omitted: {json}"
        );
    }

    // ── Phase 7 (generated internal bindings) ──

    fn generated_config_json() -> &'static str {
        // Two services both consume the SAME generated internal secret via their
        // own env var; a third (redis) does not. The spec carries NO value.
        r#"{
            "services":[
                {"name":"api","cmd":["true"],"bindings_env":{"DB_PASSWORD":"db_password"}},
                {"name":"postgres","cmd":["true"],"bindings_env":{"POSTGRES_PASSWORD":"db_password"}}
            ],
            "generated_bindings":[
                {"name":"db_password","generator":"random_base64","bytes":32,"scope":"run","targets":["api","postgres"]}
            ]
        }"#
    }

    #[test]
    fn config_parses_a_generated_binding_spec() {
        let cfg = SupervisorConfig::from_json(generated_config_json()).unwrap();
        assert_eq!(cfg.generated_bindings.len(), 1);
        let g = &cfg.generated_bindings[0];
        assert_eq!(g.name, "db_password");
        assert_eq!(g.generator, GeneratorMethod::RandomBase64);
        assert_eq!(g.bytes, 32);
        assert_eq!(g.scope, GeneratedScope::Run);
        assert_eq!(g.targets, vec!["api", "postgres"]);
    }

    #[test]
    fn config_rejects_bad_generated_bindings() {
        // Unknown target service.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"db_password","generator":"random_base64","bytes":32,"targets":["nope"]}]}"#
        )
        .is_err());
        // Empty targets.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"db_password","generator":"random_base64","bytes":32,"targets":[]}]}"#
        )
        .is_err());
        // bytes = 0 (weak) and bytes too large.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"db_password","generator":"random_base64","bytes":0,"targets":["api"]}]}"#
        )
        .is_err());
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"db_password","generator":"random_base64","bytes":99999,"targets":["api"]}]}"#
        )
        .is_err());
        // Invalid binding name (uppercase is not a valid BindingName).
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"DB_PASSWORD","generator":"random_base64","bytes":32,"targets":["api"]}]}"#
        )
        .is_err());
        // Unknown generator method.
        assert!(SupervisorConfig::from_json(
            r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[{"name":"db_password","generator":"nope","bytes":32,"targets":["api"]}]}"#
        )
        .is_err());
        // Duplicate generated binding name.
        assert!(
            SupervisorConfig::from_json(
                r#"{"services":[{"name":"api","cmd":["true"]}],
                "generated_bindings":[
                    {"name":"db_password","generator":"random_base64","bytes":32,"targets":["api"]},
                    {"name":"db_password","generator":"random_base64","bytes":16,"targets":["api"]}
                ]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn generated_spec_is_value_free_and_identity_stable() {
        // Two builds of the SAME spec serialize BYTE-IDENTICALLY (so they share
        // artifact identity), and NEITHER serialization carries any value — the
        // value only exists at run.
        let a = SupervisorConfig::from_json(generated_config_json()).unwrap();
        let b = SupervisorConfig::from_json(generated_config_json()).unwrap();
        let ja = serde_json::to_string(&a.generated_bindings).unwrap();
        let jb = serde_json::to_string(&b.generated_bindings).unwrap();
        assert_eq!(
            ja, jb,
            "same spec ⇒ identical serialization (identity stable)"
        );
        // The spec records only name/generator/scope/targets — no value/secret.
        assert!(ja.contains("db_password") && ja.contains("random_base64"));
        assert!(
            !ja.contains("=="),
            "base64 padding of a value must never appear in the spec"
        );
    }

    #[test]
    fn random_base64_generates_distinct_nonempty_values_per_call() {
        let v1 = GeneratorMethod::RandomBase64.generate(32).unwrap();
        let v2 = GeneratorMethod::RandomBase64.generate(32).unwrap();
        assert!(!v1.is_empty() && !v2.is_empty());
        // 32 bytes → 44 base64 chars (with padding).
        assert_eq!(v1.len(), 44);
        assert_ne!(v1, v2, "each RUN gets a different value (OS RNG)");
    }

    #[test]
    fn materialize_shares_one_value_across_all_targets_and_writes_no_value_to_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig::from_json(generated_config_json()).unwrap();
        let sink = crate::tmpfs::TmpfsBindingSink::new(dir.path());
        let written = materialize_generated_bindings(&cfg.generated_bindings, &sink).unwrap();
        assert_eq!(written.len(), 1);
        // The single tmpfs file is what BOTH api (DB_PASSWORD) and postgres
        // (POSTGRES_PASSWORD) read — so the value is identical across targets.
        let value = std::fs::read_to_string(dir.path().join("db_password")).unwrap();
        assert!(!value.is_empty());
        // Plan each target: the injected env resolves to the same file; the plan
        // carries the PATH, never the value.
        for svc in cfg.services() {
            if svc.bindings_env.is_empty() {
                continue;
            }
            let plan = plan_spawn_service(&svc, dir.path()).unwrap();
            assert_eq!(plan.secret_env.len(), 1);
            assert_eq!(plan.secret_env[0].1, dir.path().join("db_password"));
            assert!(
                !format!("{plan:?}").contains(value.trim()),
                "value must not enter the plan"
            );
        }
        // The spec itself never carries the value.
        assert!(!format!("{:?}", cfg.generated_bindings).contains(value.trim()));
    }

    #[test]
    fn supervisor_materializes_generated_before_start_and_scrubs_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SupervisorConfig::from_json(generated_config_json()).unwrap();
        let path = dir.path().join("db_password");
        assert!(!path.exists(), "no generated value exists before the run");

        let fake = FakeWorkload::default();
        let st = fake.0.clone();
        let mut sup = Supervisor::new(cfg, dir.path(), move || fake.clone());
        // Bound-ready (no leased bindings required) → generated value materialized
        // BEFORE the services start (plan_spawn_service's existence check passes).
        assert!(sup.on_bound_ready(true).unwrap());
        assert_eq!(st.starts.borrow().len(), 2, "both target services started");
        assert!(path.exists(), "generated value materialized before start");
        let run1 = std::fs::read_to_string(&path).unwrap();

        // Stop → generated value scrubbed (the build pre-snapshot must leave none).
        assert!(sup.stop_workload().unwrap());
        assert!(!path.exists(), "generated value scrubbed on stop");

        // A fresh start (restore/rotation) regenerates a DIFFERENT value.
        assert!(sup.on_bound_ready(true).unwrap());
        let run2 = std::fs::read_to_string(&path).unwrap();
        assert_ne!(run1, run2, "each run/start gets a fresh generated value");
    }
}
