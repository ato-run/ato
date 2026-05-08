//! Dependency runtime orchestrator (P5 phase 2).
//!
//! Composes the four building blocks (`endpoint`, `ready`, `orphan`,
//! `teardown`) plus the credential resolver / materialization pipeline
//! from [`crate::application::dependency_credentials`] into the
//! end-to-end startup sequence specified in
//! `CAPSULE_DEPENDENCY_CONTRACTS.md` §10.2.
//!
//! # Pure with respect to fetching
//!
//! Like the lock-time verifier in `capsule-core`, this orchestrator does
//! **not** fetch provider capsules. The caller supplies an
//! [`OrchestratorProvider`] for each dep, containing the parsed provider
//! manifest body and the local filesystem path where the provider was
//! materialized. Registry / cache / network policy stays in the caller's
//! brick.
//!
//! # Scope (v1 MVP)
//!
//! - Topological start ordering (dependencies before dependents)
//! - state.dir derivation from `instance_hash` + `state.version` (RFC §7.7)
//! - Orphan detection (warn-only, RFC §10.4)
//! - Credential resolution + Rule M1 TempFile materialization
//! - Endpoint allocation when provider's `port` is `None` (interpreted as
//!   `port = "auto"` since the parsed `NamedTarget` shape does not yet
//!   carry the typed `EndpointSpec` from the dependency-contract grammar)
//! - Template expansion of provider's `run_command` (`{{credentials.X}}`
//!   to file path, `{{port}}` to allocated port, `{{state.dir}}` to dir,
//!   `{{params.X}}` to lockfile param value)
//! - Spawn provider via `std::process::Command`, capturing pid for
//!   teardown
//! - Ready probe (`tcp` / `probe` variants)
//! - Resolve provider's `runtime_exports` for consumer env injection
//!
//! Out of scope for MVP (deferred): `[provision]` block execution, exec
//! variants `Stdin` / `EnvVar` (TempFile is the v1 default), HTTP / unix
//! socket ready probes (lock-fail-closed in capsule-core).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use capsule_core::dependency_contracts::{DependencyLock, LockedDependencyEntry};
use capsule_core::types::{CapsuleManifest, ParamValue};
use thiserror::Error;

use super::endpoint::{EndpointAllocator, EndpointError};
use super::orphan::{
    detect_orphan_state, kill_orphan_provider, sweep_stale_sentinel, write_session_sentinel,
    OrphanCheckOutcome, OrphanError, OrphanProviderKillOutcome, SessionSentinel,
};
use super::ready::{wait_for_ready, ReadyError, ReadyProbeKind};
use super::teardown::{teardown_reverse_topological, TeardownError, TeardownTarget};
use crate::application::dependency_credentials::{
    materialize_credential, resolve_credential_template, CredentialError, HostEnv,
    MaterializationChannel, MaterializedRef, RedactionRegistry,
};

/// Caller-provided provider materialization. The orchestrator never reads
/// the network, registry, or cache — the caller has already placed the
/// provider's manifest body and root directory at known locations.
pub struct OrchestratorProvider {
    pub manifest: CapsuleManifest,
    /// Filesystem directory the provider's `run_command` should execute
    /// in. Caller is responsible for ensuring this is a hermetic copy
    /// (clonefile / hardlink / extracted artifact, depending on policy).
    pub provider_root: PathBuf,
    /// Immutable content reference (`capsule://...@sha256:...`) recorded
    /// in the sentinel for diagnostics. Should match
    /// `LockedDependencyEntry.resolved`.
    pub resolved: String,
}

pub struct OrchestratorInput<'a> {
    pub lock: &'a DependencyLock,
    pub providers: BTreeMap<String, OrchestratorProvider>,
    pub consumer: &'a CapsuleManifest,
    /// Root of the Ato home directory. State paths are derived as
    /// `<ato_home>/state/<parent_package_id>/<instance_hash>/<state.version>/<state.name>/`
    /// per RFC §7.7.
    pub ato_home: PathBuf,
    /// The consumer capsule's package id (typically `<name>@<version>` or
    /// a content-addressed id, depending on Ato's package id scheme).
    pub parent_package_id: String,
    pub host_env: &'a dyn HostEnv,
    pub redaction: Arc<RedactionRegistry>,
    /// Pid of the current Ato session, written into the sentinel.
    pub session_pid: i32,
    /// Per-dep ready probe timeout. Used when a contract does not declare
    /// `ready.timeout`.
    pub default_ready_timeout: Duration,
    /// Polling interval between ready probe attempts.
    pub ready_probe_interval: Duration,
    /// Label of the consumer target being launched. When `Some(_)`,
    /// `start_all` only spawns deps that the target actually `needs`
    /// (transitively) instead of every top-level `[dependencies.*]`
    /// entry. `None` preserves the legacy "start every declared dep"
    /// behaviour for callers that haven't been migrated yet.
    pub selected_target: Option<String>,
}

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("topological sort failed for consumer dependency graph: {detail}")]
    TopologySort { detail: String },

    #[error("dep '{alias}' has no provider in OrchestratorInput.providers")]
    MissingProvider { alias: String },

    #[error("dep '{alias}' provider has no contract '{contract}'")]
    MissingContract { alias: String, contract: String },

    #[error("dep '{alias}' provider has no target '{target}' bound by contract")]
    MissingTarget { alias: String, target: String },

    #[error("dep '{alias}' state.version missing in lock — required by RFC §7.7")]
    MissingStateVersion { alias: String },

    #[error("dep '{alias}' provider target has no `run`/`run_command` field")]
    MissingRunCommand { alias: String },

    #[error("dep '{alias}' state.dir is owned by ato session pid {session_pid}; provider={resolved}; state={}", state_dir.display())]
    OrphanAliveOtherSession {
        alias: String,
        session_pid: i32,
        resolved: String,
        state_dir: PathBuf,
    },

    #[error("dep '{alias}' state.dir setup failed: {detail}")]
    StateDirSetup { alias: String, detail: String },

    #[error("dep '{alias}' template expansion failed: {detail}")]
    TemplateExpansion { alias: String, detail: String },

    #[error("dep '{alias}' run_command split failed: {detail}")]
    CommandSplit { alias: String, detail: String },

    #[error("dep '{alias}' spawn failed: {detail}")]
    SpawnFailed { alias: String, detail: String },

    #[error("dep '{alias}' credential error: {source}")]
    Credential {
        alias: String,
        #[source]
        source: CredentialError,
    },

    #[error("dep '{alias}' endpoint allocation failed: {source}")]
    Endpoint {
        alias: String,
        #[source]
        source: EndpointError,
    },

    #[error("dep '{alias}' ready probe failed: {source}")]
    Ready {
        alias: String,
        #[source]
        source: ReadyError,
    },

    #[error(
        "dep '{alias}' ready probe expects host tool '{tool}' at {} but it is not installed; {suggestion}",
        expected_path.display()
    )]
    MissingProviderHostTool {
        alias: String,
        tool: String,
        expected_path: PathBuf,
        suggestion: String,
    },

    #[error("dep '{alias}' tool artifact resolution failed: {source}")]
    ToolArtifact {
        alias: String,
        #[source]
        source: crate::application::tool_artifact::ToolArtifactError,
    },

    #[error("dep '{alias}' orphan detection failed: {source}")]
    Orphan {
        alias: String,
        #[source]
        source: OrphanError,
    },
}

/// One started provider process and its associated runtime state.
pub struct RunningDep {
    pub alias: String,
    /// Immutable provider reference (`capsule://...@sha256:...` or
    /// `capsule://github.com/...@<commit>`) recorded for run-summary
    /// surfacing. Mirrors `OrchestratorProvider::resolved`.
    pub resolved: String,
    pub state_dir: PathBuf,
    pub child: Child,
    pub allocated_port: Option<u16>,
    /// Resolved provider `runtime_exports` ready for consumer env
    /// injection. Keys are the provider-side export names; values are
    /// the resolved strings (with credential placeholders already
    /// expanded — these values are also registered with the redaction
    /// registry if the provider marked them `secret = true`).
    pub runtime_exports: BTreeMap<String, String>,
    /// Filesystem path where this dep's redacted stdout/stderr is being
    /// captured. `None` if the orchestrator could not open the log file
    /// (e.g. permissions, missing parent).
    pub log_path: Option<PathBuf>,
    /// Materialized credentials kept alive for the duration of the
    /// running dep. Drop unlinks the temp files (Rule M1).
    credential_refs: Vec<MaterializedRef>,
    /// Recorded warnings (e.g. orphan stale sentinel was swept). The
    /// orchestrator returns these so the caller can surface them in the
    /// run-command UX.
    pub warnings: Vec<String>,
}

/// Aggregate handle for a fully-started dependency graph. Drops
/// teardown-on-drop so callers that early-return without explicit
/// teardown still clean up.
pub struct RunningGraph {
    deps: Vec<RunningDep>,
    /// Captured at start time so `Drop` can teardown without needing the
    /// `OrchestratorInput` lifetime.
    teardown_grace: Duration,
}

impl std::fmt::Debug for RunningGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningGraph")
            .field(
                "deps",
                &self.deps.iter().map(|d| &d.alias).collect::<Vec<_>>(),
            )
            .field("teardown_grace", &self.teardown_grace)
            .finish()
    }
}

impl RunningGraph {
    pub fn deps(&self) -> &[RunningDep] {
        &self.deps
    }

    pub fn runtime_exports(&self, alias: &str) -> Option<&BTreeMap<String, String>> {
        self.deps
            .iter()
            .find(|d| d.alias == alias)
            .map(|d| &d.runtime_exports)
    }

    /// Render a human-readable, secret-free summary of every started
    /// dep for surfacing post-launch in the run command. Each line
    /// covers one dep's provider source, allocated port, state dir, and
    /// log path. `runtime_exports` **values** are deliberately omitted
    /// — they may contain credentials that the redaction filter would
    /// otherwise have to scrub. Only the export key names are printed.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.deps.len() * 4 + 1);
        if self.deps.is_empty() {
            return lines;
        }
        lines.push(format!("Dependencies started ({}):", self.deps.len()));
        for dep in &self.deps {
            lines.push(format!("  {} ({})", dep.alias, dep.resolved));
            if let Some(port) = dep.allocated_port {
                lines.push(format!("    port:  127.0.0.1:{port}"));
            }
            lines.push(format!("    state: {}", dep.state_dir.display()));
            if let Some(log_path) = dep.log_path.as_ref() {
                lines.push(format!("    log:   {}", log_path.display()));
            }
            if !dep.runtime_exports.is_empty() {
                let mut keys: Vec<&str> = dep.runtime_exports.keys().map(String::as_str).collect();
                keys.sort();
                lines.push(format!("    exports: {}", keys.join(", ")));
            }
            for warning in &dep.warnings {
                lines.push(format!("    warn:  {warning}"));
            }
        }
        lines
    }

    /// Stop all started deps in reverse-topological order. Consumes the
    /// graph. Sentinels are swept on success.
    pub fn teardown(mut self, grace: Duration) -> Result<(), TeardownError> {
        let targets = self.teardown_targets();
        // Drop credential_refs before teardown so the temp files unlink
        // BEFORE the provider receives SIGTERM (best-effort defense
        // against secrets persisting in temp dirs after orphan crash).
        for dep in &mut self.deps {
            dep.credential_refs.clear();
        }
        let result = teardown_reverse_topological(targets, grace);
        // Sweep sentinels regardless of teardown outcome.
        for dep in &self.deps {
            let _ = sweep_stale_sentinel(&dep.state_dir);
        }
        result
    }

    fn teardown_targets(&self) -> Vec<TeardownTarget> {
        // Walk consumer.targets[*].needs is the orchestrator's
        // responsibility; here we only have alias → pid. Construct
        // targets with no `needs` so teardown_reverse_topological orders
        // them by alias name (deterministic but order-agnostic). For a
        // proper reverse topo we need the consumer manifest, which is
        // not held in RunningGraph. The orchestrator's start path
        // already preserves topo order; teardown reverses by reversing
        // `self.deps`.
        self.deps
            .iter()
            .rev()
            .map(|d| TeardownTarget {
                dep: d.alias.clone(),
                pid: d.child.id() as i32,
                state_dir: d.state_dir.clone(),
                needs: Vec::new(),
            })
            .collect()
    }
}

impl Drop for RunningGraph {
    fn drop(&mut self) {
        // Best-effort teardown for callers that drop the graph without
        // an explicit teardown call (e.g. on panic). Errors are
        // swallowed because Drop cannot return them; callers that need
        // a clean-failure signal must call teardown() explicitly.
        let targets = self.teardown_targets();
        for dep in &mut self.deps {
            dep.credential_refs.clear();
        }
        let _ = teardown_reverse_topological(targets, self.teardown_grace);
        for dep in &self.deps {
            let _ = sweep_stale_sentinel(&dep.state_dir);
        }
    }
}

/// Start every dep in the lock graph in topological order and return a
/// `RunningGraph`. Errors mid-start trigger a teardown of any deps that
/// were already started.
pub fn start_all(input: OrchestratorInput<'_>) -> Result<RunningGraph, OrchestratorError> {
    let order = topological_dep_order(input.consumer, input.selected_target.as_deref())?;
    let mut started: Vec<RunningDep> = Vec::new();
    let allocator = EndpointAllocator::new();
    let env_scope: BTreeSet<&str> = input
        .consumer
        .required_env
        .iter()
        .map(String::as_str)
        .collect();

    for alias in order {
        let entry = match input.lock.entries.get(&alias) {
            Some(e) => e,
            None => continue, // alias declared in dependencies but missing from lock; skip
        };
        let provider =
            input
                .providers
                .get(&alias)
                .ok_or_else(|| OrchestratorError::MissingProvider {
                    alias: alias.clone(),
                })?;

        match start_one(&alias, entry, provider, &input, &allocator, &env_scope) {
            Ok(running) => started.push(running),
            Err(err) => {
                // Roll back: teardown everything started so far.
                let graph = RunningGraph {
                    deps: started,
                    teardown_grace: Duration::from_secs(5),
                };
                let _ = graph.teardown(Duration::from_secs(5));
                return Err(err);
            }
        }
    }

    Ok(RunningGraph {
        deps: started,
        teardown_grace: Duration::from_secs(10),
    })
}

fn topological_dep_order(
    consumer: &CapsuleManifest,
    selected_target: Option<&str>,
) -> Result<Vec<String>, OrchestratorError> {
    // Build edges from each target's needs into deps. For start order, a
    // dep that another dep needs comes first. For v1 the consumer's
    // dependencies graph itself is flat (deps don't depend on other deps
    // — that's `transitive identity` follow-up). So we simply order
    // deps by the order they appear in consumer.dependencies (BTreeMap
    // ordering is alphabetical, deterministic).
    let all_deps: Vec<String> = consumer.dependencies.keys().cloned().collect();
    let Some(target_label) = selected_target else {
        return Ok(all_deps);
    };
    // When the caller named a target, only the deps that target needs
    // (matching `[targets.<label>] needs = [...]`) start. This avoids
    // spinning up irrelevant providers (e.g. running `--target web`
    // for a Vite frontend should not boot the postgres sidecar that
    // only the FastAPI backend uses). Targets that omit `needs` are
    // treated as "needs nothing" — the historical "start every dep"
    // semantics is reachable by passing `selected_target = None`.
    let Some(targets_block) = consumer.targets.as_ref() else {
        return Ok(all_deps);
    };
    let Some(target) = targets_block.named.get(target_label) else {
        // Unknown target — defer to caller error handling. Returning
        // every dep here is the conservative path; downstream code
        // already validates the target label and surfaces a clearer
        // error if it is missing.
        return Ok(all_deps);
    };
    let needs: BTreeSet<&str> = target.needs.iter().map(String::as_str).collect();
    Ok(all_deps
        .into_iter()
        .filter(|alias| needs.contains(alias.as_str()))
        .collect())
}

fn start_one(
    alias: &str,
    entry: &LockedDependencyEntry,
    provider: &OrchestratorProvider,
    input: &OrchestratorInput<'_>,
    allocator: &EndpointAllocator,
    env_scope: &BTreeSet<&str>,
) -> Result<RunningDep, OrchestratorError> {
    let mut warnings = Vec::new();
    let contract = provider
        .manifest
        .contracts
        .get(&entry.contract)
        .ok_or_else(|| OrchestratorError::MissingContract {
            alias: alias.to_string(),
            contract: entry.contract.clone(),
        })?;
    let target_label = &contract.target;
    let target_block = provider
        .manifest
        .targets
        .as_ref()
        .and_then(|tc| tc.named.get(target_label))
        .ok_or_else(|| OrchestratorError::MissingTarget {
            alias: alias.to_string(),
            target: target_label.clone(),
        })?;

    // §7.7 path: <ato_home>/state/<parent_package_id>/<instance_hash>/<state.version>/<state.name>/
    let state = entry
        .state
        .as_ref()
        .ok_or_else(|| OrchestratorError::StateDirSetup {
            alias: alias.to_string(),
            detail: "lock entry has no state block; provider requires state".to_string(),
        })?;
    let state_dir = derive_state_dir(
        &input.ato_home,
        &input.parent_package_id,
        &entry.instance_hash,
        &state.version,
        &state.name,
    );
    std::fs::create_dir_all(&state_dir).map_err(|err| OrchestratorError::StateDirSetup {
        alias: alias.to_string(),
        detail: format!("create_dir_all {}: {}", state_dir.display(), err),
    })?;

    // §10.4 orphan detection (warn-only, with abort for AliveOtherSession).
    let orphan = detect_orphan_state(&state_dir, input.session_pid).map_err(|err| {
        OrchestratorError::Orphan {
            alias: alias.to_string(),
            source: err,
        }
    })?;
    match orphan {
        OrphanCheckOutcome::NoSentinel => {}
        OrphanCheckOutcome::StaleDeadOwner { sentinel } => {
            warnings.push(format!(
                "swept stale sentinel for dep '{alias}' (owner pid {} is dead)",
                sentinel.session_pid
            ));
            // The owning Ato session is gone, but its provider process
            // can survive (postgres detaches its postmaster from the
            // parent's signal group, keeps PGDATA's postmaster.pid
            // locked, and refuses to let a fresh `bootstrap.sh exec
            // postgres` re-bind). SIGTERM the recorded provider_pid
            // before we sweep so the next start isn't blocked by the
            // previous run's leak. Best-effort: if the kill fails for
            // a non-ESRCH reason we still proceed and let the provider
            // surface its own startup error.
            match kill_orphan_provider(&sentinel) {
                OrphanProviderKillOutcome::NotPresent => {}
                OrphanProviderKillOutcome::KilledByTerm {
                    pid,
                    pgroup_signaled,
                } => {
                    warnings.push(format!(
                        "killed orphan provider for dep '{alias}' (pid {pid}, SIGTERM, pgroup={pgroup_signaled})"
                    ));
                }
                OrphanProviderKillOutcome::KilledByKill {
                    pid,
                    pgroup_signaled,
                } => {
                    warnings.push(format!(
                        "killed orphan provider for dep '{alias}' (pid {pid}, SIGKILL after SIGTERM grace, pgroup={pgroup_signaled})"
                    ));
                }
                OrphanProviderKillOutcome::KillFailed { pid, detail } => {
                    warnings.push(format!(
                        "failed to kill orphan provider for dep '{alias}' (pid {pid}): {detail}"
                    ));
                }
            }
            sweep_stale_sentinel(&state_dir).map_err(|err| OrchestratorError::Orphan {
                alias: alias.to_string(),
                source: err,
            })?;
        }
        OrphanCheckOutcome::AliveSameSession { .. } => {
            warnings.push(format!(
                "resuming dep '{alias}': sentinel matches current Ato session"
            ));
            // Caller could short-circuit here for true resume semantics;
            // v1 still re-spawns the provider to keep the running graph
            // accurate. Idempotent providers (postgres etc.) accept this.
        }
        OrphanCheckOutcome::AliveOtherSession { sentinel } => {
            return Err(OrchestratorError::OrphanAliveOtherSession {
                alias: alias.to_string(),
                session_pid: sentinel.session_pid,
                resolved: sentinel.resolved,
                state_dir,
            });
        }
    }

    // §7.3.2 credential resolution + Rule M1 TempFile materialization.
    // The lock stores credentials in template form ({{env.X}}); we
    // re-parse, resolve from host env, and materialize via TempFile.
    let mut credential_refs: Vec<MaterializedRef> = Vec::new();
    let mut credential_paths: BTreeMap<String, String> = BTreeMap::new();
    for (cred_key, template_str) in &entry.credentials {
        let template =
            capsule_core::types::TemplatedString::parse(template_str).map_err(|err| {
                OrchestratorError::TemplateExpansion {
                    alias: alias.to_string(),
                    detail: format!("credential '{cred_key}' template parse: {err}"),
                }
            })?;
        let resolved =
            resolve_credential_template(&template, env_scope, input.host_env).map_err(|err| {
                OrchestratorError::Credential {
                    alias: alias.to_string(),
                    source: err,
                }
            })?;
        // Register with the global redaction registry so any provider
        // stdout/stderr / receipt write that contains the value gets
        // scrubbed.
        input.redaction.register(resolved.as_str());
        let mref = materialize_credential(
            resolved,
            MaterializationChannel::TempFile {
                state_dir: state_dir.clone(),
                cred_key: cred_key.clone(),
            },
        )
        .map_err(|err| OrchestratorError::Credential {
            alias: alias.to_string(),
            source: err,
        })?;
        if let Some(path) = mref.file_path() {
            credential_paths.insert(cred_key.clone(), path.display().to_string());
        }
        credential_refs.push(mref);
    }

    // Endpoint allocation. The current parsed `NamedTarget` shape exposes
    // `port: Option<u16>` only — we treat `None` as `port = "auto"` and
    // allocate ephemerally. A `Some(N)` is honored as a fixed port.
    let allocated_port = match target_block.port {
        Some(p) => p,
        None => allocator
            .allocate_tcp()
            .map_err(|err| OrchestratorError::Endpoint {
                alias: alias.to_string(),
                source: err,
            })?,
    };
    let was_auto = target_block.port.is_none();

    // Template expansion of the provider's run command.
    let run_command_text = target_block.run_command.as_deref().ok_or_else(|| {
        OrchestratorError::MissingRunCommand {
            alias: alias.to_string(),
        }
    })?;
    let expanded = expand_template(
        alias,
        run_command_text,
        &state_dir,
        allocated_port,
        &entry.parameters,
        &credential_paths,
    )?;
    let argv = shell_split(&expanded).map_err(|detail| OrchestratorError::CommandSplit {
        alias: alias.to_string(),
        detail,
    })?;
    if argv.is_empty() {
        return Err(OrchestratorError::CommandSplit {
            alias: alias.to_string(),
            detail: "empty argv after template expansion".to_string(),
        });
    }

    // Resolve the ready probe shape and preflight it before spawning the
    // provider. The probe spec lives in the contract (not the provider's
    // run line), so its templated values (`{{state.dir}}`, `{{port}}`)
    // are already known here. Running this preflight ahead of the spawn
    // turns "host is missing /opt/homebrew/bin/pg_isready" into a typed
    // MissingProviderHostTool failure instead of a 90s ready timeout that
    // also leaves the just-spawned provider behind.
    let probe_kind = ready_probe_kind(contract, allocated_port, &state_dir)?;
    preflight_probe_host_tools(alias, &probe_kind)?;

    // Spawn provider. cwd = provider_root joined with target_block.working_dir if any.
    let cwd = match target_block.working_dir.as_deref() {
        Some(rel) => provider.provider_root.join(rel),
        None => provider.provider_root.clone(),
    };
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.current_dir(&cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Spawn the provider into its own process group on Unix so the
    // sweep path (orphan::kill_orphan_provider) can later reap the
    // entire tree with `kill(-pgid, ...)` — necessary because postgres'
    // postmaster forks backends + auxiliary workers under itself.
    // SIGTERM to just the postmaster pid waits indefinitely for active
    // sessions to disconnect; killing the whole pgroup short-circuits
    // that. process_group(0) makes the spawned child its own pgroup
    // leader (pid == pgid), so the sentinel's `provider_pid` doubles
    // as the pgid the sweep targets. See ato-run/ato#121.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    // Provider env: inherit consumer's required_env + provider's static env.
    cmd.env_clear();
    for key in &input.consumer.required_env {
        if let Some(value) = input.host_env.get(key) {
            cmd.env(key, value);
        }
    }
    for (k, v) in &target_block.env {
        cmd.env(k, v);
    }
    // Resolve any tool artifacts the provider's target declares
    // (`tool_artifacts = ["postgresql", ...]`) and inject their
    // ATO_TOOL_* env vars. The resolver downloads the verified
    // artifact on first use and is a no-op cache hit on subsequent
    // runs (#119). Failures bubble up as typed orchestrator errors
    // — the v0.5.x preflight already handles the
    // host-binary-missing case for legacy probe specs; this layer
    // handles the new artifact-driven path.
    if !target_block.tool_artifacts.is_empty() {
        let downloader =
            crate::application::tool_artifact::ReqwestDownloader::default();
        let env_map = crate::application::tool_artifact::resolve_target_tool_env(
            &target_block.tool_artifacts,
            &input.ato_home,
            &downloader,
        )
        .map_err(|err| OrchestratorError::ToolArtifact {
            alias: alias.to_string(),
            source: err,
        })?;
        for (k, v) in env_map {
            cmd.env(k, v);
        }
    }
    let mut child = cmd.spawn().map_err(|err| OrchestratorError::SpawnFailed {
        alias: alias.to_string(),
        detail: format!("spawn {}: {}", argv[0], err),
    })?;
    let log_path = attach_redacted_provider_logs(
        &mut child,
        &input.ato_home,
        &input.parent_package_id,
        alias,
        input.redaction.clone(),
    );
    let provider_pid = child.id() as i32;

    // Sentinel write (RFC §10.4).
    let sentinel = SessionSentinel {
        session_pid: input.session_pid,
        provider_pid: Some(provider_pid),
        started_at: now_rfc3339(),
        resolved: provider.resolved.clone(),
    };
    write_session_sentinel(&state_dir, &sentinel).map_err(|err| OrchestratorError::Orphan {
        alias: alias.to_string(),
        source: err,
    })?;

    // Ready probe. `probe_kind` was resolved + preflighted above so the
    // missing-host-tool case never reaches this loop.
    let timeout = ready_probe_timeout(contract, input.default_ready_timeout);
    wait_for_ready(&probe_kind, timeout, input.ready_probe_interval).map_err(|err| {
        OrchestratorError::Ready {
            alias: alias.to_string(),
            source: err,
        }
    })?;

    // Resolve provider's runtime_exports for consumer env injection.
    let runtime_exports = resolve_runtime_exports(
        alias,
        contract,
        &entry.parameters,
        &credential_paths_for_runtime(entry, env_scope, input.host_env)?,
        &state_dir,
        allocated_port,
    )?;
    // Register secret runtime_exports with redaction.
    for (key, value) in &runtime_exports {
        if let Some(spec) = contract.runtime_exports.get(key) {
            if matches!(
                spec,
                capsule_core::types::RuntimeExportSpec::Detailed(d) if d.secret
            ) {
                input.redaction.register(value);
            }
        }
    }

    let _ = was_auto; // currently informational; could be exposed later
    Ok(RunningDep {
        alias: alias.to_string(),
        resolved: provider.resolved.clone(),
        state_dir,
        child,
        allocated_port: Some(allocated_port),
        runtime_exports,
        log_path,
        credential_refs,
        warnings,
    })
}

fn derive_state_dir(
    ato_home: &Path,
    parent_pkg_id: &str,
    instance_hash: &str,
    state_version: &str,
    state_name: &str,
) -> PathBuf {
    ato_home
        .join("state")
        .join(parent_pkg_id)
        .join(instance_hash)
        .join(state_version)
        .join(state_name)
}

/// Wire the child's stdout/stderr through the redaction registry and into
/// a per-run log file. Returns the log path so the orchestrator can
/// surface it in the run summary; returns `None` if the log could not be
/// opened (the caller treats this as "no log available" rather than an
/// orchestration-blocking error — provider startup itself is unaffected).
fn attach_redacted_provider_logs(
    child: &mut Child,
    ato_home: &Path,
    parent_package_id: &str,
    alias: &str,
    redaction: Arc<RedactionRegistry>,
) -> Option<PathBuf> {
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let log_dir = ato_home
        .join("logs")
        .join("deps")
        .join(sanitize_path_component(parent_package_id))
        .join(sanitize_path_component(alias));
    let log_path = log_dir.join(format!("{timestamp}.log"));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if stdout.is_none() && stderr.is_none() {
        return None;
    }
    if std::fs::create_dir_all(&log_dir).is_err() {
        return None;
    }
    let Ok(writer) = OpenOptions::new().create(true).append(true).open(&log_path) else {
        return None;
    };
    let stderr_writer = writer.try_clone().ok();

    if let Some(stdout) = stdout {
        let redaction = redaction.clone();
        let mut writer = writer;
        std::thread::spawn(move || {
            write_redacted_lines(stdout, &mut writer, "stdout", redaction);
        });
    }
    if let (Some(stderr), Some(mut writer)) = (stderr, stderr_writer) {
        std::thread::spawn(move || {
            write_redacted_lines(stderr, &mut writer, "stderr", redaction);
        });
    }
    Some(log_path)
}

fn write_redacted_lines<R: std::io::Read>(
    reader: R,
    writer: &mut std::fs::File,
    stream: &str,
    redaction: Arc<RedactionRegistry>,
) {
    let reader = BufReader::new(reader);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let scrubbed = redaction.redact(&line);
        let _ = writeln!(writer, "[{stream}] {scrubbed}");
    }
}

fn sanitize_path_component(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

/// Re-resolve credentials specifically for runtime_exports template
/// expansion. Unlike Rule M1's file-path materialization (which is what
/// the provider's `run_command` sees), `runtime_exports` consumed by the
/// consumer needs the **actual value** so the consumer can connect.
/// These values are immediately registered with redaction.
fn credential_paths_for_runtime(
    entry: &LockedDependencyEntry,
    env_scope: &BTreeSet<&str>,
    host_env: &dyn HostEnv,
) -> Result<BTreeMap<String, String>, OrchestratorError> {
    let mut out = BTreeMap::new();
    for (key, template_str) in &entry.credentials {
        let template =
            capsule_core::types::TemplatedString::parse(template_str).map_err(|err| {
                OrchestratorError::TemplateExpansion {
                    alias: "<runtime_exports>".to_string(),
                    detail: format!("credential '{key}' parse: {err}"),
                }
            })?;
        let resolved =
            resolve_credential_template(&template, env_scope, host_env).map_err(|err| {
                OrchestratorError::Credential {
                    alias: "<runtime_exports>".to_string(),
                    source: err,
                }
            })?;
        out.insert(key.clone(), resolved.as_str().to_string());
    }
    Ok(out)
}

fn ready_probe_kind(
    contract: &capsule_core::types::ContractSpec,
    port: u16,
    state_dir: &Path,
) -> Result<ReadyProbeKind, OrchestratorError> {
    use capsule_core::types::ReadyProbe;
    match &contract.ready {
        ReadyProbe::Tcp { target, .. } => {
            // Render the `target` template against host/port. v1 supports
            // {{host}}/{{port}} only on the contract side.
            let rendered = render_runtime_template(
                "ready.target",
                target,
                &BTreeMap::new(),
                &BTreeMap::new(),
                state_dir,
                port,
            )?;
            let (host, port_str) =
                rendered
                    .rsplit_once(':')
                    .ok_or_else(|| OrchestratorError::TemplateExpansion {
                        alias: "<ready>".to_string(),
                        detail: format!("tcp target must be host:port, got '{rendered}'"),
                    })?;
            let port_num: u16 =
                port_str
                    .parse()
                    .map_err(|err| OrchestratorError::TemplateExpansion {
                        alias: "<ready>".to_string(),
                        detail: format!("tcp target port parse '{port_str}': {err}"),
                    })?;
            Ok(ReadyProbeKind::Tcp {
                host: host.to_string(),
                port: port_num,
            })
        }
        ReadyProbe::Probe { run, .. } => {
            let rendered = render_runtime_template(
                "ready.run",
                run,
                &BTreeMap::new(),
                &BTreeMap::new(),
                state_dir,
                port,
            )?;
            let argv =
                shell_split(&rendered).map_err(|detail| OrchestratorError::CommandSplit {
                    alias: "<ready>".to_string(),
                    detail,
                })?;
            Ok(ReadyProbeKind::Probe { argv })
        }
        ReadyProbe::Postgres {
            host,
            port: probe_port,
            user,
            database,
            ..
        } => {
            let rendered_host = render_runtime_template(
                "ready.host",
                host,
                &BTreeMap::new(),
                &BTreeMap::new(),
                state_dir,
                port,
            )?;
            let rendered_port = render_runtime_template(
                "ready.port",
                probe_port,
                &BTreeMap::new(),
                &BTreeMap::new(),
                state_dir,
                port,
            )?;
            let port_num: u16 =
                rendered_port
                    .parse()
                    .map_err(|err| OrchestratorError::TemplateExpansion {
                        alias: "<ready>".to_string(),
                        detail: format!(
                            "postgres ready.port must parse as u16, got '{rendered_port}': {err}"
                        ),
                    })?;
            let rendered_user = match user {
                Some(t) => render_runtime_template(
                    "ready.user",
                    t,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    state_dir,
                    port,
                )?,
                None => "postgres".to_string(),
            };
            let rendered_database = match database {
                Some(t) => render_runtime_template(
                    "ready.database",
                    t,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    state_dir,
                    port,
                )?,
                None => "postgres".to_string(),
            };
            Ok(ReadyProbeKind::Postgres {
                host: rendered_host,
                port: port_num,
                user: rendered_user,
                database: rendered_database,
            })
        }
        // Reserved variants are lock-fail-closed in capsule-core; if they
        // somehow reach here it's a programming bug.
        ReadyProbe::Http { .. } => Err(OrchestratorError::TemplateExpansion {
            alias: "<ready>".to_string(),
            detail: "ready.type = http is reserved-only in v1; lock should have rejected"
                .to_string(),
        }),
        ReadyProbe::UnixSocket { .. } => Err(OrchestratorError::TemplateExpansion {
            alias: "<ready>".to_string(),
            detail: "ready.type = unix_socket is reserved-only in v1; lock should have rejected"
                .to_string(),
        }),
    }
}

/// Reject `probe`-kind ready specs whose argv[0] is an absolute path that
/// does not exist on disk before the ready loop starts. Without this
/// check, a missing host tool (e.g. `/opt/homebrew/bin/pg_isready` on a
/// host without Homebrew PostgreSQL installed) is reported as the
/// generic 90 second `ReadyError::Timeout` whose `last_failure` string
/// happens to contain `spawn ... failed: No such file or directory` —
/// indistinguishable to Desktop callers from a slow startup.
///
/// Bare commands (no `/`) are intentionally **not** preflighted here:
/// the orchestrator clears the provider's PATH at spawn time, but a
/// probe with a bare `pg_isready` is still a manifest authoring choice
/// that the existing `wait_for_ready` retry loop surfaces as a normal
/// spawn-failure timeout. Adding PATH lookup here would couple the
/// preflight to the orchestrator's environment policy.
fn preflight_probe_host_tools(
    alias: &str,
    probe_kind: &ReadyProbeKind,
) -> Result<(), OrchestratorError> {
    let ReadyProbeKind::Probe { argv } = probe_kind else {
        return Ok(());
    };
    let Some(first) = argv.first() else {
        return Ok(());
    };
    let candidate = Path::new(first);
    if !candidate.is_absolute() || candidate.exists() {
        return Ok(());
    }
    let tool_name = candidate
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| first.clone());
    Err(OrchestratorError::MissingProviderHostTool {
        alias: alias.to_string(),
        tool: tool_name,
        expected_path: candidate.to_path_buf(),
        suggestion: format!(
            "install '{}' on the host or use a provider/tool capsule version that bundles it, then re-run",
            candidate.display()
        ),
    })
}

fn ready_probe_timeout(
    contract: &capsule_core::types::ContractSpec,
    default: Duration,
) -> Duration {
    use capsule_core::types::ReadyProbe;
    let raw = match &contract.ready {
        ReadyProbe::Tcp { timeout, .. }
        | ReadyProbe::Probe { timeout, .. }
        | ReadyProbe::Postgres { timeout, .. }
        | ReadyProbe::Http { timeout, .. }
        | ReadyProbe::UnixSocket { timeout, .. } => timeout,
    };
    raw.as_deref()
        .and_then(parse_duration_human)
        .unwrap_or(default)
}

fn parse_duration_human(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if let Some(num) = trimmed.strip_suffix("ms") {
        return num.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(num) = trimmed.strip_suffix('s') {
        return num.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    trimmed.parse::<u64>().ok().map(Duration::from_secs)
}

fn resolve_runtime_exports(
    alias: &str,
    contract: &capsule_core::types::ContractSpec,
    parameters: &BTreeMap<String, ParamValue>,
    credential_values: &BTreeMap<String, String>,
    state_dir: &Path,
    port: u16,
) -> Result<BTreeMap<String, String>, OrchestratorError> {
    use capsule_core::types::RuntimeExportSpec;
    let mut out = BTreeMap::new();
    for (key, spec) in &contract.runtime_exports {
        let template = match spec {
            RuntimeExportSpec::Shorthand(t) => t,
            RuntimeExportSpec::Detailed(d) => &d.value,
        };
        let value = render_runtime_template(
            &format!("runtime_exports.{key}"),
            template,
            parameters,
            credential_values,
            state_dir,
            port,
        )
        .map_err(|err| OrchestratorError::TemplateExpansion {
            alias: alias.to_string(),
            detail: format!("{err}"),
        })?;
        out.insert(key.clone(), value);
    }
    Ok(out)
}

/// Generic template renderer for runtime values. Accepts the full v1
/// template grammar (RFC §4.4): {{params.X}}, {{credentials.X}},
/// {{host}}, {{port}}, {{state.dir}}. Rejects {{env.X}} and
/// {{deps.<n>.runtime_exports.X}} / {{deps.<n>.identity_exports.X}}
/// because those are scoped to consumer-side templates.
fn render_runtime_template(
    location: &str,
    template: &capsule_core::types::TemplatedString,
    parameters: &BTreeMap<String, ParamValue>,
    credential_values: &BTreeMap<String, String>,
    state_dir: &Path,
    port: u16,
) -> Result<String, OrchestratorError> {
    use capsule_core::types::{TemplateExpr, TemplateSegment};
    let mut out = String::new();
    for segment in &template.segments {
        match segment {
            TemplateSegment::Literal(text) => out.push_str(text),
            TemplateSegment::Expr(expr) => match expr {
                TemplateExpr::Params(key) => match parameters.get(key) {
                    Some(ParamValue::String(s)) => out.push_str(s),
                    Some(ParamValue::Int(i)) => out.push_str(&i.to_string()),
                    Some(ParamValue::Bool(b)) => out.push_str(&b.to_string()),
                    None => {
                        return Err(OrchestratorError::TemplateExpansion {
                            alias: location.to_string(),
                            detail: format!("undeclared params.{key}"),
                        });
                    }
                },
                TemplateExpr::Credentials(key) => {
                    let v = credential_values.get(key).ok_or_else(|| {
                        OrchestratorError::TemplateExpansion {
                            alias: location.to_string(),
                            detail: format!("undeclared credentials.{key}"),
                        }
                    })?;
                    out.push_str(v);
                }
                TemplateExpr::Host => out.push_str("127.0.0.1"),
                TemplateExpr::Port => out.push_str(&port.to_string()),
                TemplateExpr::StateDir => out.push_str(&state_dir.display().to_string()),
                TemplateExpr::Socket
                | TemplateExpr::Env(_)
                | TemplateExpr::DepRuntimeExport { .. }
                | TemplateExpr::DepIdentityExport { .. } => {
                    return Err(OrchestratorError::TemplateExpansion {
                        alias: location.to_string(),
                        detail: format!(
                            "template expression {{{{{expr}}}}} not allowed in this position"
                        ),
                    });
                }
            },
        }
    }
    Ok(out)
}

/// Expand the provider's `run_command` template. Same grammar as
/// `render_runtime_template` but the credentials map is already resolved
/// to **file paths** (Rule M1 TempFile channel).
fn expand_template(
    alias: &str,
    run_command: &str,
    state_dir: &Path,
    port: u16,
    parameters: &BTreeMap<String, ParamValue>,
    credential_paths: &BTreeMap<String, String>,
) -> Result<String, OrchestratorError> {
    let template = capsule_core::types::TemplatedString::parse(run_command).map_err(|err| {
        OrchestratorError::TemplateExpansion {
            alias: alias.to_string(),
            detail: format!("run_command parse: {err}"),
        }
    })?;
    render_runtime_template(
        alias,
        &template,
        parameters,
        credential_paths,
        state_dir,
        port,
    )
}

/// Naive POSIX shell-style argv split. Sufficient for v1 — providers
/// that need true shell features (pipes, expansions) wrap their
/// `run_command` in `sh -c "..."` themselves.
fn shell_split(input: &str) -> Result<Vec<String>, String> {
    let mut argv: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    argv.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if in_single || in_double {
        return Err("unmatched quote in run_command".to_string());
    }
    if !current.is_empty() {
        argv.push(current);
    }
    Ok(argv)
}

fn now_rfc3339() -> String {
    // chrono is available in this crate (see Cargo.toml).
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_split_basic() {
        assert_eq!(
            shell_split("/bin/echo hello world").unwrap(),
            vec![
                "/bin/echo".to_string(),
                "hello".to_string(),
                "world".to_string()
            ]
        );
    }

    #[test]
    fn shell_split_quotes_preserved() {
        assert_eq!(
            shell_split("/bin/echo 'hello world'").unwrap(),
            vec!["/bin/echo".to_string(), "hello world".to_string()]
        );
        assert_eq!(
            shell_split(r#"/bin/echo "hello world""#).unwrap(),
            vec!["/bin/echo".to_string(), "hello world".to_string()]
        );
    }

    #[test]
    fn shell_split_rejects_unmatched_quote() {
        assert!(shell_split(r#"echo "hello"#).is_err());
    }

    #[test]
    fn parse_duration_human_handles_common_forms() {
        assert_eq!(parse_duration_human("30s"), Some(Duration::from_secs(30)));
        assert_eq!(
            parse_duration_human("500ms"),
            Some(Duration::from_millis(500))
        );
        assert_eq!(parse_duration_human("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration_human("garbage"), None);
    }

    #[test]
    fn derive_state_dir_matches_rfc_path() {
        let p = derive_state_dir(
            Path::new("/ato_home"),
            "wasedap2p-backend",
            "blake3:7f4a",
            "16",
            "data",
        );
        assert_eq!(
            p,
            PathBuf::from("/ato_home/state/wasedap2p-backend/blake3:7f4a/16/data")
        );
    }

    // ---------- end-to-end mock-provider integration ----------

    use crate::application::dependency_credentials::MapHostEnv;
    use capsule_core::dependency_contracts::{verify_and_lock, DependencyLockInput};

    const MOCK_CONSUMER: &str = r#"
schema_version = "0.3"
name = "demo-consumer"
version = "0.1.0"
type = "app"
default_target = "app"
required_env = ["MOCK_PASSWORD"]

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "main.py"
needs = ["svc"]

[dependencies.svc]
capsule = "capsule://ato/mock@1"
contract = "service@1"

[dependencies.svc.parameters]
mode = "test"

[dependencies.svc.credentials]
password = "{{env.MOCK_PASSWORD}}"

[dependencies.svc.state]
name = "data"
"#;

    // Provider that uses /bin/sleep as its long-running entrypoint and
    // /usr/bin/true as the ready probe. No template expansion needed —
    // the provider does not consume credentials in its run line. The
    // credential is still routed via Rule M1 TempFile materialization
    // because the lock declared one.
    const MOCK_PROVIDER: &str = r#"
schema_version = "0.3"
name = "mock"
version = "1.0.0"
type = "app"
default_target = "server"

[targets.server]
runtime = "source"
driver = "native"
run = "/bin/sleep 60"
port = 0

[contracts."service@1"]
target = "server"
ready = { type = "probe", run = "/usr/bin/true", timeout = "5s" }

[contracts."service@1".parameters]
mode = { type = "string", required = true }

[contracts."service@1".credentials]
password = { type = "string", required = true }

[contracts."service@1".identity_exports]
mode = "{{params.mode}}"

[contracts."service@1".runtime_exports]
MODE = "{{params.mode}}"

[contracts."service@1".state]
required = true
version = "1"
"#;

    #[test]
    fn start_all_spawns_provider_runs_ready_probe_and_tears_down() {
        let consumer = CapsuleManifest::from_toml(MOCK_CONSUMER).expect("consumer");
        let provider_manifest = CapsuleManifest::from_toml(MOCK_PROVIDER).expect("provider");

        // Build a DependencyLock by running the real verifier so the test
        // exercises the full pipeline end to end.
        let mut providers_for_lock = BTreeMap::new();
        providers_for_lock.insert(
            "svc".to_string(),
            capsule_core::dependency_contracts::ResolvedProviderManifest {
                requested: "capsule://ato/mock@1".to_string(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
                manifest: provider_manifest.clone(),
            },
        );
        let lock = verify_and_lock(DependencyLockInput {
            consumer: &consumer,
            providers: providers_for_lock,
        })
        .expect("verify_and_lock");

        // Now run the orchestrator.
        let ato_home = tempfile::tempdir().expect("ato_home");
        let provider_root = tempfile::tempdir().expect("provider_root");
        let host_env = MapHostEnv::new(&[("MOCK_PASSWORD", "shh-its-secret")]);
        let redaction = Arc::new(RedactionRegistry::new());

        let mut providers = BTreeMap::new();
        providers.insert(
            "svc".to_string(),
            OrchestratorProvider {
                manifest: provider_manifest,
                provider_root: provider_root.path().to_path_buf(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
            },
        );

        let input = OrchestratorInput {
            lock: &lock,
            providers,
            consumer: &consumer,
            ato_home: ato_home.path().to_path_buf(),
            parent_package_id: "demo-consumer".to_string(),
            host_env: &host_env,
            redaction: redaction.clone(),
            session_pid: std::process::id() as i32,
            default_ready_timeout: Duration::from_secs(5),
            ready_probe_interval: Duration::from_millis(20),
            selected_target: None,
        };

        let graph = start_all(input).expect("start_all");
        assert_eq!(graph.deps.len(), 1, "one dep started");
        let dep = &graph.deps[0];
        assert_eq!(dep.alias, "svc");
        // Provider's runtime_exports must be resolved against parameters.
        assert_eq!(
            dep.runtime_exports.get("MODE").map(String::as_str),
            Some("test")
        );
        // The credential value must have been registered with the
        // redaction registry — verify by feeding a string through.
        let scrubbed = redaction.redact("user said: shh-its-secret");
        assert!(
            !scrubbed.contains("shh-its-secret"),
            "credential value must have been registered with redaction"
        );
        // Sentinel must exist now.
        let sentinel_path = dep.state_dir.join(".ato-session");
        assert!(sentinel_path.exists(), "sentinel must be written");

        // Capture the pid for post-teardown verification.
        let pid = dep.child.id() as i32;

        // Teardown stops the provider.
        graph.teardown(Duration::from_secs(2)).expect("teardown");

        // After teardown the sentinel must be swept.
        assert!(
            !sentinel_path.exists(),
            "sentinel must be swept on teardown, got {}",
            sentinel_path.display()
        );
        // Process is no longer ours; we don't reap (Drop on Child after
        // teardown swallowed it). The kill itself is verified by
        // teardown_reverse_topological's exit code path. Best we can do
        // here without explicit reap is to check it's not still
        // accepting signals.
        let _ = pid;
    }

    #[test]
    fn start_all_aborts_on_alive_other_session_orphan() {
        let consumer = CapsuleManifest::from_toml(MOCK_CONSUMER).expect("consumer");
        let provider_manifest = CapsuleManifest::from_toml(MOCK_PROVIDER).expect("provider");

        let mut providers_for_lock = BTreeMap::new();
        providers_for_lock.insert(
            "svc".to_string(),
            capsule_core::dependency_contracts::ResolvedProviderManifest {
                requested: "capsule://ato/mock@1".to_string(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
                manifest: provider_manifest.clone(),
            },
        );
        let lock = verify_and_lock(DependencyLockInput {
            consumer: &consumer,
            providers: providers_for_lock,
        })
        .expect("verify_and_lock");

        // Pre-write a sentinel that points at pid=1 (init/launchd, alive
        // but not us) so the orchestrator must abort.
        let ato_home = tempfile::tempdir().expect("ato_home");
        let entry = lock.entries.get("svc").expect("svc entry");
        let state_block = entry.state.as_ref().expect("state");
        let state_dir = derive_state_dir(
            ato_home.path(),
            "demo-consumer",
            &entry.instance_hash,
            &state_block.version,
            &state_block.name,
        );
        std::fs::create_dir_all(&state_dir).expect("mkdir -p");
        write_session_sentinel(
            &state_dir,
            &SessionSentinel {
                session_pid: 1,
                provider_pid: None,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
            },
        )
        .expect("pre-write sentinel");

        let provider_root = tempfile::tempdir().expect("provider_root");
        let host_env = MapHostEnv::new(&[("MOCK_PASSWORD", "shh")]);
        let redaction = Arc::new(RedactionRegistry::new());
        let mut providers = BTreeMap::new();
        providers.insert(
            "svc".to_string(),
            OrchestratorProvider {
                manifest: provider_manifest,
                provider_root: provider_root.path().to_path_buf(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
            },
        );
        let input = OrchestratorInput {
            lock: &lock,
            providers,
            consumer: &consumer,
            ato_home: ato_home.path().to_path_buf(),
            parent_package_id: "demo-consumer".to_string(),
            host_env: &host_env,
            redaction,
            session_pid: std::process::id() as i32,
            default_ready_timeout: Duration::from_secs(2),
            ready_probe_interval: Duration::from_millis(20),
            selected_target: None,
        };

        let err = start_all(input).expect_err("must abort on alive other session");
        assert!(
            matches!(err, OrchestratorError::OrphanAliveOtherSession { .. }),
            "got {err:?}"
        );
    }

    // ---------- P7: real-Postgres end-to-end (host-bound) ----------
    //
    // Exercises the ato/postgres provider + minimal consumer fixture
    // against the verified tool artifact resolver (#119/#120) — the
    // postgres binaries come from $ATO_HOME/store/tools/postgresql-...
    // not the host. The fixtures live in `crates/ato-cli/tests/fixtures/p7/`.
    // Skipped on hosts whose platform isn't covered by the artifact
    // pin in ato-cli's built-in registry (currently darwin-aarch64 only).

    fn p7_fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p7")
    }

    fn p7_skip_if_unavailable() -> Option<()> {
        // The migrated fixture (#120) does not depend on
        // /opt/homebrew anymore — Postgres binaries come from ato's
        // verified tool artifact resolver. The only remaining
        // host-platform constraint is that the artifact pin in the
        // built-in registry covers this host.
        let host = crate::application::tool_artifact::host_platform();
        if host != Some("darwin-aarch64") {
            eprintln!(
                "[P7] skipping: tool_artifacts(\"postgresql\") pin currently covers darwin-aarch64 only (host: {host:?})"
            );
            return None;
        }
        let root = p7_fixture_root();
        let provider = root.join("ato-postgres/capsule.toml");
        let consumer = root.join("wasedap2p/capsule.toml");
        if provider.exists() && consumer.exists() {
            Some(())
        } else {
            eprintln!("[P7] skipping: missing P7 fixture files");
            None
        }
    }

    fn p7_parse_fixture(rel: &str) -> CapsuleManifest {
        let path = p7_fixture_root().join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("read fixture {}", path.display()));
        CapsuleManifest::from_toml(&text).expect("parse fixture")
    }

    /// P7 host-bound E2E. Marked `#[ignore]` because it spawns the real
    /// Postgres binary, sets process-global env vars (`PG_PASSWORD`),
    /// and writes to a fixed `/tmp/ato-p7/.ato-home`. Run explicitly:
    ///
    ///     cargo test -p ato-cli --lib p7_postgres -- --ignored --nocapture
    #[test]
    #[ignore]
    fn p7_postgres_provider_boots_via_orchestrator_and_passes_ready_probe() {
        if p7_skip_if_unavailable().is_none() {
            return;
        }

        let consumer = p7_parse_fixture("wasedap2p/capsule.toml");
        let provider_manifest = p7_parse_fixture("ato-postgres/capsule.toml");
        let provider_root = p7_fixture_root().join("ato-postgres");

        // Stage 1: real verifier produces the lock.
        let mut providers_for_lock = BTreeMap::new();
        providers_for_lock.insert(
            "db".to_string(),
            capsule_core::dependency_contracts::ResolvedProviderManifest {
                requested: "capsule://ato/postgres@16".to_string(),
                resolved: "capsule://ato/postgres@sha256:p7-fixture".to_string(),
                manifest: provider_manifest.clone(),
            },
        );
        let lock = capsule_core::dependency_contracts::verify_and_lock(
            capsule_core::dependency_contracts::DependencyLockInput {
                consumer: &consumer,
                providers: providers_for_lock,
            },
        )
        .expect("verify_and_lock");

        let pre_rotation_hash = lock.entries.get("db").unwrap().instance_hash.clone();
        eprintln!("[P7] instance_hash = {}", pre_rotation_hash);
        assert_eq!(
            lock.entries
                .get("db")
                .unwrap()
                .identity_exports
                .get("database")
                .map(String::as_str),
            Some("wasedap2p")
        );

        // Stage 2: orchestrate against real Postgres.
        if std::env::var("PG_PASSWORD").is_err() {
            std::env::set_var("PG_PASSWORD", "p7-test-password-change-me");
        }

        // Use a fixed ato_home under /tmp so logs survive failure for
        // inspection. Wipe it first so each run starts clean.
        let ato_home_path = PathBuf::from("/tmp/ato-p7/.ato-home");
        let _ = std::fs::remove_dir_all(&ato_home_path);
        std::fs::create_dir_all(&ato_home_path).expect("create ato_home");
        eprintln!("[P7] ato_home = {}", ato_home_path.display());

        let host_env = crate::application::dependency_credentials::ProcessHostEnv;
        let redaction = Arc::new(RedactionRegistry::new());

        let mut providers = BTreeMap::new();
        providers.insert(
            "db".to_string(),
            OrchestratorProvider {
                manifest: provider_manifest,
                provider_root,
                resolved: "capsule://ato/postgres@sha256:p7-fixture".to_string(),
            },
        );

        let input = OrchestratorInput {
            lock: &lock,
            providers,
            consumer: &consumer,
            ato_home: ato_home_path.clone(),
            parent_package_id: "wasedap2p-backend@0.1.0".to_string(),
            host_env: &host_env,
            redaction: redaction.clone(),
            session_pid: std::process::id() as i32,
            default_ready_timeout: Duration::from_secs(120),
            ready_probe_interval: Duration::from_millis(500),
            selected_target: None,
        };

        let graph = start_all(input).unwrap_or_else(|err| {
            // Dump any captured log so we can diagnose.
            let logs_root = ato_home_path.join("logs/deps");
            if logs_root.exists() {
                eprintln!("[P7] dumping captured dep logs:");
                for entry in walkdir::WalkDir::new(&logs_root)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    eprintln!("--- {} ---", entry.path().display());
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        eprintln!("{text}");
                    }
                }
            }
            panic!("start_all: {err:?}");
        });
        assert_eq!(graph.deps().len(), 1);

        let runtime_exports = graph.runtime_exports("db").expect("db exports").clone();
        eprintln!(
            "[P7] runtime_exports keys = {:?}",
            runtime_exports.keys().collect::<Vec<_>>()
        );
        let database_url = runtime_exports.get("DATABASE_URL").expect("DATABASE_URL");
        assert!(
            database_url.contains("postgresql+psycopg://postgres:"),
            "got: {database_url}"
        );
        assert!(database_url.contains("/wasedap2p"), "got: {database_url}");

        // Redaction must scrub the credential value.
        let pw = std::env::var("PG_PASSWORD").unwrap();
        let scrubbed = redaction.redact(&format!("debug: pw = {pw}"));
        assert!(!scrubbed.contains(&pw), "PG_PASSWORD must be redacted");

        // Confirm postgres really listens — independent of the
        // orchestrator's own native probe that already gated start_all
        // returning. A bare TCP connect is sufficient: if the
        // orchestrator's probe passed but the port were not actually
        // open, that would be a serious orchestrator bug, not a
        // postgres bug.
        let allocated = graph.deps()[0].allocated_port.expect("allocated_port");
        eprintln!("[P7] allocated port = {}", allocated);
        let stream = std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{allocated}").parse().unwrap(),
            Duration::from_secs(2),
        )
        .expect("post-orchestrator TCP connect to allocated postgres port");
        drop(stream);

        graph
            .teardown(Duration::from_secs(10))
            .expect("teardown postgres");

        // Stage 3: rotation invariant. Re-build the lock with a different
        // PG_PASSWORD; instance_hash must NOT change (RFC §7.3.1 hard
        // invariant 1 + §9.5).
        std::env::set_var("PG_PASSWORD", "p7-rotated-different-value");
        let consumer2 = p7_parse_fixture("wasedap2p/capsule.toml");
        let provider2 = p7_parse_fixture("ato-postgres/capsule.toml");
        let mut providers2 = BTreeMap::new();
        providers2.insert(
            "db".to_string(),
            capsule_core::dependency_contracts::ResolvedProviderManifest {
                requested: "capsule://ato/postgres@16".to_string(),
                resolved: "capsule://ato/postgres@sha256:p7-fixture".to_string(),
                manifest: provider2,
            },
        );
        let lock2 = capsule_core::dependency_contracts::verify_and_lock(
            capsule_core::dependency_contracts::DependencyLockInput {
                consumer: &consumer2,
                providers: providers2,
            },
        )
        .expect("verify_and_lock rotated");
        assert_eq!(
            pre_rotation_hash,
            lock2.entries.get("db").unwrap().instance_hash,
            "instance_hash MUST be stable across PG_PASSWORD rotation"
        );
        eprintln!("[P7] rotation invariant holds");
    }

    /// `topological_dep_order` must filter top-level [dependencies.*]
    /// down to the deps the selected target lists in `needs`. Without
    /// this, e.g. running `--target web` on a multi-target consumer
    /// boots backend-only providers (postgres) and collides with the
    /// real backend run via the shared state dir.
    #[test]
    fn topological_dep_order_filters_by_selected_target_needs() {
        let manifest = r#"
schema_version = "0.3"
name = "multi-target"
version = "0.1.0"
type = "app"
default_target = "app"

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

[targets.app]
runtime = "source"
driver = "python"
run = "python -m uvicorn main:app"
needs = ["db"]

[targets.web]
runtime = "source"
driver = "node"
run = "npm run dev"
"#;
        let consumer = CapsuleManifest::from_toml(manifest).expect("parse manifest");

        // None → legacy "every dep" behaviour preserved
        let all = topological_dep_order(&consumer, None).expect("legacy order");
        assert_eq!(all, vec!["db".to_string()]);

        // app `needs = ["db"]` → starts postgres
        let app = topological_dep_order(&consumer, Some("app")).expect("app order");
        assert_eq!(app, vec!["db".to_string()]);

        // web has no `needs` → no deps spawn (frontend skips postgres)
        let web = topological_dep_order(&consumer, Some("web")).expect("web order");
        assert!(
            web.is_empty(),
            "target without needs must skip top-level deps, got {web:?}"
        );

        // unknown target → fall back to all-deps so caller can surface
        // the missing-target error itself rather than silently empty
        let bogus = topological_dep_order(&consumer, Some("nope")).expect("bogus order");
        assert_eq!(bogus, vec!["db".to_string()]);
    }

    // ---------- preflight: missing host tool surfaces typed error ----------

    #[test]
    fn preflight_passes_for_existing_absolute_path() {
        let kind = ReadyProbeKind::Probe {
            argv: vec!["/usr/bin/true".to_string(), "--quiet".to_string()],
        };
        preflight_probe_host_tools("svc", &kind).expect("must accept existing absolute path");
    }

    #[test]
    fn preflight_passes_for_bare_command_name() {
        // PATH lookup is the orchestrator/spawn layer's concern; preflight
        // intentionally only validates absolute paths.
        let kind = ReadyProbeKind::Probe {
            argv: vec!["pg_isready".to_string()],
        };
        preflight_probe_host_tools("svc", &kind).expect("bare names skip preflight");
    }

    #[test]
    fn preflight_passes_for_tcp_kind() {
        let kind = ReadyProbeKind::Tcp {
            host: "127.0.0.1".to_string(),
            port: 5432,
        };
        preflight_probe_host_tools("svc", &kind).expect("tcp probes have nothing to preflight");
    }

    #[test]
    fn preflight_rejects_missing_absolute_host_tool() {
        // Build a path inside a tempdir that we never create — guaranteed
        // to be absolute and non-existent on any host.
        let tmp = tempfile::tempdir().expect("tempdir");
        let bogus = tmp.path().join("nope").join("pg_isready");
        let kind = ReadyProbeKind::Probe {
            argv: vec![
                bogus.to_string_lossy().into_owned(),
                "-h".to_string(),
                "127.0.0.1".to_string(),
            ],
        };
        let err = preflight_probe_host_tools("db", &kind)
            .expect_err("missing absolute host tool must be typed");
        match err {
            OrchestratorError::MissingProviderHostTool {
                alias,
                tool,
                expected_path,
                suggestion,
            } => {
                assert_eq!(alias, "db");
                assert_eq!(tool, "pg_isready");
                assert_eq!(expected_path, bogus);
                assert!(
                    suggestion.contains(&bogus.display().to_string()),
                    "suggestion should reference the missing path, got: {suggestion}"
                );
            }
            other => panic!("expected MissingProviderHostTool, got {other:?}"),
        }
    }

    /// End-to-end: a contract whose `ready.run` points at an absolute path
    /// that doesn't exist must abort `start_all` with the typed
    /// `MissingProviderHostTool` *before* the ready timeout fires. This
    /// is the v0.5.x blocker fix: the bug repro previously took 90s and
    /// surfaced as `ReadyError::Timeout`.
    #[test]
    fn start_all_returns_missing_host_tool_when_probe_argv_is_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bogus_probe = tmp.path().join("nope").join("pg_isready");
        let probe_str = bogus_probe.to_string_lossy().into_owned();

        // Same shape as MOCK_CONSUMER / MOCK_PROVIDER but the probe path
        // is rewritten to a guaranteed-missing absolute path.
        let provider_toml = format!(
            r#"
schema_version = "0.3"
name = "mock"
version = "1.0.0"
type = "app"
default_target = "server"

[targets.server]
runtime = "source"
driver = "native"
run = "/bin/sleep 60"
port = 0

[contracts."service@1"]
target = "server"
ready = {{ type = "probe", run = "{probe}", timeout = "5s" }}

[contracts."service@1".parameters]
mode = {{ type = "string", required = true }}

[contracts."service@1".credentials]
password = {{ type = "string", required = true }}

[contracts."service@1".identity_exports]
mode = "{{{{params.mode}}}}"

[contracts."service@1".runtime_exports]
MODE = "{{{{params.mode}}}}"

[contracts."service@1".state]
required = true
version = "1"
"#,
            probe = probe_str,
        );

        let consumer = CapsuleManifest::from_toml(MOCK_CONSUMER).expect("consumer");
        let provider_manifest = CapsuleManifest::from_toml(&provider_toml).expect("provider");

        let mut providers_for_lock = BTreeMap::new();
        providers_for_lock.insert(
            "svc".to_string(),
            capsule_core::dependency_contracts::ResolvedProviderManifest {
                requested: "capsule://ato/mock@1".to_string(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
                manifest: provider_manifest.clone(),
            },
        );
        let lock = verify_and_lock(DependencyLockInput {
            consumer: &consumer,
            providers: providers_for_lock,
        })
        .expect("verify_and_lock");

        let ato_home = tempfile::tempdir().expect("ato_home");
        let provider_root = tempfile::tempdir().expect("provider_root");
        let host_env = MapHostEnv::new(&[("MOCK_PASSWORD", "shh")]);
        let redaction = Arc::new(RedactionRegistry::new());
        let mut providers = BTreeMap::new();
        providers.insert(
            "svc".to_string(),
            OrchestratorProvider {
                manifest: provider_manifest,
                provider_root: provider_root.path().to_path_buf(),
                resolved: "capsule://ato/mock@sha256:e2e".to_string(),
            },
        );

        let started = std::time::Instant::now();
        let input = OrchestratorInput {
            lock: &lock,
            providers,
            consumer: &consumer,
            ato_home: ato_home.path().to_path_buf(),
            parent_package_id: "demo-consumer".to_string(),
            host_env: &host_env,
            redaction,
            session_pid: std::process::id() as i32,
            // Set the probe timeout much higher than this test's wall-clock
            // budget so a regression that re-routes through the ready loop
            // would visibly fail the elapsed-time assertion below.
            default_ready_timeout: Duration::from_secs(60),
            ready_probe_interval: Duration::from_millis(20),
            selected_target: None,
        };
        let err = start_all(input).expect_err("must reject missing host tool");
        let elapsed = started.elapsed();

        match err {
            OrchestratorError::MissingProviderHostTool {
                alias,
                tool,
                expected_path,
                ..
            } => {
                assert_eq!(alias, "svc");
                assert_eq!(tool, "pg_isready");
                assert_eq!(expected_path, bogus_probe);
            }
            other => panic!("expected MissingProviderHostTool, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "preflight must short-circuit, but took {elapsed:?}"
        );
    }
}
