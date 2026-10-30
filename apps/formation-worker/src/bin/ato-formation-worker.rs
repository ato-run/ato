//! The Formation worker binary, and the in-sandbox shim it re-enters as.
//!
//! `serve` is a supervised service: it claims one job at a time, reports the
//! result terminally, and stops claiming when the control plane says the host
//! is drained or needs a new binary. It never installs a binary itself — the
//! operator swaps the file, and this process only reports and verifies what it
//! is running.
//!
//! ## `serve` flags
//!
//! | flag | meaning |
//! |---|---|
//! | `--api-base` | control plane base URL (required) |
//! | `--work-root` | worker-owned directory; holds `worker-update.json` |
//! | `--worker-id` | identity this process claims as |
//! | `--queue` | `all` (default) or `source-only`; requires a dedicated runner identity |
//! | `--poll-ms` | idle poll interval (default 2000) |
//! | `--heartbeat-ms` | lifecycle/identity report interval (default 15000) |
//! | `--expect-version` | refuse to serve unless the binary reports this version |
//! | `--expect-digest` | refuse to serve unless the binary's sha256 is this |
//!
//! The token is `ATO_FORMATION_TOKEN`.
//!
//! ## Non-claiming exit codes
//!
//! - `0` — finished normally (`--version`, or a one-shot job that succeeded).
//! - `74` — rollback required or the local update journal is quarantined. No
//!   work was claimed; an operator must reconcile the binary and the journal.
//!   See `docs/ops/formation-worker-update-lifecycle.md`.
//! - `75` — drained / quiescent, or stopped by SIGTERM/SIGINT without a drain
//!   request. No work was claimed; a supervisor may start the worker again
//!   after a binary swap or a maintenance window.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use ato_formation::source::SourceLimits;
use ato_formation_worker::api::{
    ClaimOutcome, ClaimRefusal, FailureReport, FormationApi, PublishOutcome, WorkerDirective,
    WorkerIdentityReport, WorkerStateReport, bounded_reason, classify_failure as classify,
};
use ato_formation_worker::build::BuildAttempt;
use ato_formation_worker::job::{JobContext, PinnedSourceFetcher, TreePacker, run_claimed_job};
use ato_formation_worker::lifecycle::{
    BinaryIdentity, EXIT_DRAINED, EXIT_ROLLBACK_REQUIRED, LoadOutcome, Recovery,
    UPDATE_JOURNAL_FILE, UpdateJournal, UpdatePhase, normalize_sha256, preserve_unreadable_journal,
    recover,
};
use ato_formation_worker::pack::pack_tree;
use ato_formation_worker::sandbox::{BuildLimits, require_containment};
use ato_formation_worker::source_archive::{
    SourceArchiveApi, SourceArchiveClaimOutcome, materialize_source_archive,
};

/// What produced this binary.
///
/// Printed at startup and by `--version`, so a deployed worker can be matched
/// to a revision without hashing it and grepping for strings — which is what
/// identifying one actually took. Any part a build could not determine reads
/// `unknown` rather than a plausible substitute: a binary claiming a commit it
/// was not built from ends an investigation with the wrong answer.
///
/// The fields are kept separate from the printed line because the lifecycle
/// report needs them individually; the printed line is exactly the line that
/// was already deployed and must stay byte-compatible.
struct BuildIdentity {
    version: &'static str,
    commit: &'static str,
    dirty: &'static str,
    build_id: &'static str,
    rustc: &'static str,
}

impl BuildIdentity {
    fn compiled() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            commit: env!("ATO_BUILD_COMMIT"),
            dirty: env!("ATO_BUILD_DIRTY"),
            build_id: env!("ATO_BUILD_ID"),
            rustc: env!("ATO_BUILD_RUSTC"),
        }
    }

    /// Pair the compile-time stamps with the digest of the bytes on disk.
    fn into_binary_identity(self) -> Result<BinaryIdentity> {
        BinaryIdentity::of_current_exe(
            self.version.to_owned(),
            self.commit.to_owned(),
            self.dirty.to_owned(),
            self.build_id.to_owned(),
            self.rustc.to_owned(),
        )
    }
}

impl fmt::Display for BuildIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ato-formation-worker {} commit={} {} build_id={} {}",
            self.version, self.commit, self.dirty, self.build_id, self.rustc
        )
    }
}

fn build_identity() -> BuildIdentity {
    BuildIdentity::compiled()
}

/// Set by the signal handler only. Everything that touches the journal, the
/// network or a child process runs after the current job has finished.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// An async-signal-safe handler: one atomic store, no allocation, no locks.
/// It must not report, journal or claim — those happen on the normal path.
#[cfg(unix)]
extern "C" fn request_stop(_signal: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
fn install_stop_handlers() -> Result<()> {
    // SAFETY: `action` is fully initialized before `sigaction` reads it, and
    // the handler only touches a lock-free `AtomicBool`.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = request_stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        // No SA_RESTART: a stop should interrupt a blocking wait so the loop
        // notices it without waiting for the next timeout.
        action.sa_flags = 0;
        for signal in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("cannot install the stop handler for signal {signal}")
                });
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_stop_handlers() -> Result<()> {
    // No POSIX signals to install; a supervisor on this platform stops the
    // process the ordinary way.
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();

    if args
        .get(1)
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("{}", build_identity());
        return Ok(());
    }

    // Re-entry from INSIDE the sandbox. bwrap sets up the namespaces, then
    // execs this binary, which restricts itself with Landlock and execs the
    // build step. Landlock must be applied by the process that will exec —
    // `restrict_self` survives `exec` — and applying it to bwrap instead denies
    // bwrap its own `/proc/self/uid_map` write.
    //
    // Handled before clap because it is not a user-facing subcommand.
    if args.get(1).is_some_and(|arg| arg == "sandbox-exec") {
        return ato_formation_worker::sandbox_exec::sandbox_exec(&args[2..]);
    }

    if args.get(1).is_some_and(|arg| arg == "serve") {
        return serve(&args[2..]);
    }

    if args.get(1).is_some_and(|arg| arg == "source-archive") {
        return source_archive_one(&args[2..]);
    }

    run_one(&args[1..])
}

/// What the heartbeat thread and the serve loop share.
struct ServingState {
    /// The phase last persisted. The heartbeat reports this.
    phase: Mutex<UpdatePhase>,
    /// The attempt this process is executing right now, if any.
    current_attempt: Mutex<Option<String>>,
    /// The last directive the control plane returned.
    directive: Mutex<Option<WorkerDirective>>,
    /// Set when the control plane said to stop claiming.
    drain_requested: AtomicBool,
    /// Asked of the heartbeat thread before a drain so its last report cannot
    /// race the quiescent report — the journal and the control plane should
    /// agree on the final phase.
    heartbeat_stopped: AtomicBool,
}

/// A poisoned lock still carries a usable value: nothing under these locks can
/// leave a half-written phase, so recovering the guard beats panicking out of
/// a service.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn worker_state_report(
    worker_id: &str,
    identity: &BinaryIdentity,
    phase: UpdatePhase,
    current_attempt_id: Option<&str>,
) -> WorkerStateReport {
    WorkerStateReport {
        worker_id: worker_id.to_owned(),
        agent_version: identity.version.clone(),
        lifecycle_state: phase.as_str().to_owned(),
        identity: WorkerIdentityReport {
            commit: identity.commit.clone(),
            dirty: identity.dirty.clone(),
            build_id: identity.build_id.clone(),
            rustc: identity.rustc.clone(),
            binary_sha256: identity.sha256.clone(),
        },
        current_attempt_id: current_attempt_id.map(ToOwned::to_owned),
    }
}

fn report_once(
    api: &FormationApi,
    worker_id: &str,
    identity: &BinaryIdentity,
    phase: UpdatePhase,
    current_attempt_id: Option<&str>,
) -> Result<WorkerDirective> {
    api.report_worker_state(&worker_state_report(
        worker_id,
        identity,
        phase,
        current_attempt_id,
    ))
}

/// Report a phase a few times before giving up. Used on the exit paths, where
/// the control plane learning the final state is worth a short retry but not
/// an infinite one.
fn report_with_retries(
    api: &FormationApi,
    worker_id: &str,
    identity: &BinaryIdentity,
    phase: UpdatePhase,
    current_attempt_id: Option<&str>,
) -> Result<WorkerDirective> {
    const ATTEMPTS: u32 = 3;
    let mut last_error = None;
    for attempt in 1..=ATTEMPTS {
        match report_once(api, worker_id, identity, phase, current_attempt_id) {
            Ok(directive) => return Ok(directive),
            Err(error) => {
                eprintln!(
                    "[formation] worker state report failed ({attempt}/{ATTEMPTS}): {error:#}"
                );
                last_error = Some(error);
                if attempt < ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
    Err(last_error.expect("a failed report leaves an error behind"))
}

/// Report `verifying` until the control plane has it.
///
/// The contract resumes the update only once the state report succeeds, so a
/// transient control-plane failure must keep the worker non-claiming rather
/// than push it into `active` on a report nobody received.
fn await_state_report(
    api: &FormationApi,
    worker_id: &str,
    identity: &BinaryIdentity,
    phase: UpdatePhase,
) -> WorkerDirective {
    loop {
        match report_once(api, worker_id, identity, phase, None) {
            Ok(directive) => return directive,
            Err(error) => {
                if STOP_REQUESTED.load(Ordering::SeqCst) {
                    eprintln!(
                        "[formation] stop requested while reporting {}; exited {} without claiming",
                        phase.as_str(),
                        EXIT_DRAINED
                    );
                    std::process::exit(EXIT_DRAINED);
                }
                eprintln!("[formation] worker state report failed: {error:#}; retrying");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

/// Sleep, but notice a stop or drain quickly enough to matter.
fn pause(state: &ServingState, duration: Duration) {
    let deadline = Instant::now() + duration;
    loop {
        if STOP_REQUESTED.load(Ordering::SeqCst) || state.drain_requested.load(Ordering::SeqCst) {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
    }
}

/// Is the control plane asking for a binary this process is not?
///
/// `drain` always stops claiming. A target version/digest is checked as well:
/// if a directive arrives without the drain flag but names a different binary,
/// treating that as "keep working" would let a stale worker serve through a
/// rollout.
fn update_is_pending(directive: &WorkerDirective, identity: &BinaryIdentity) -> bool {
    let Some(target_version) = directive.target_version.as_deref() else {
        return false;
    };
    if target_version != identity.version {
        return true;
    }
    match directive.target_digest.as_deref() {
        Some(digest) => normalize_sha256(digest)
            .map(|digest| digest != identity.sha256)
            .unwrap_or(true),
        None => false,
    }
}

/// Report lifecycle state and identity on its own thread.
///
/// The report is the only control-plane channel this worker has; the claim
/// response is the other. Neither is consulted inside a signal handler, and a
/// report failure is a log line rather than a reason to end the service.
fn spawn_heartbeat(
    api: FormationApi,
    identity: BinaryIdentity,
    worker_id: String,
    state: Arc<ServingState>,
    interval: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            if state.heartbeat_stopped.load(Ordering::SeqCst) {
                return;
            }
            let phase = *lock(&state.phase);
            let current_attempt = lock(&state.current_attempt).clone();
            match report_once(
                &api,
                &worker_id,
                &identity,
                phase,
                current_attempt.as_deref(),
            ) {
                Ok(directive) => {
                    if directive.drain || update_is_pending(&directive, &identity) {
                        state.drain_requested.store(true, Ordering::SeqCst);
                    }
                    *lock(&state.directive) = Some(directive);
                }
                Err(error) => eprintln!("[formation] worker state report failed: {error:#}"),
            }
            // Interruptible sleep so a drain does not wait a full interval for
            // the thread to notice it should stop reporting.
            let mut slept = Duration::ZERO;
            while slept < interval {
                if state.heartbeat_stopped.load(Ordering::SeqCst) {
                    return;
                }
                let step = (interval - slept).min(Duration::from_millis(100));
                std::thread::sleep(step);
                slept += step;
            }
        }
    })
}

/// Persist a phase and publish it to the heartbeat's view of this process.
fn begin_phase(
    record: &UpdateJournal,
    journal_path: &Path,
    state: &ServingState,
    next: UpdatePhase,
    detail: &str,
) -> Result<UpdateJournal> {
    let advanced = record
        .transition(next, Some(detail.to_owned()))
        .with_context(|| format!("cannot record the {} phase", next.as_str()))?;
    advanced.store(journal_path)?;
    *lock(&state.phase) = advanced.phase;
    Ok(advanced)
}

/// Stop claiming, wait for the in-flight job (already finished by the time the
/// loop breaks), record quiescent, report it, and exit 75.
#[allow(clippy::too_many_arguments)]
fn drain_to_quiescent(
    api: &FormationApi,
    worker_id: &str,
    identity: &BinaryIdentity,
    journal_path: &Path,
    journal: Option<UpdateJournal>,
    target: (Option<String>, Option<String>),
    state: &ServingState,
    heartbeat: std::thread::JoinHandle<()>,
) -> Result<()> {
    // Nothing is in flight: the serve loop is single-threaded and reaches
    // here only between jobs.
    let mut record = journal.unwrap_or_else(|| UpdateJournal::fresh(identity));
    if let (Some(version), digest) = target {
        record = record.with_target(version, digest);
    }

    if record.phase == UpdatePhase::Active {
        // Persist the stop before touching the heartbeat: if this process dies
        // during the join, the journal still says "draining", which a restart
        // reads as a non-claiming exit rather than as "never started".
        record = begin_phase(
            &record,
            journal_path,
            state,
            UpdatePhase::Draining,
            "stop requested; no new claims",
        )?;
        let _ = report_with_retries(api, worker_id, identity, UpdatePhase::Draining, None);
    }
    // Stop the heartbeat and wait for any report in flight, so the final
    // `quiescent` report is the last thing the control plane hears.
    state.heartbeat_stopped.store(true, Ordering::SeqCst);
    let _ = heartbeat.join();
    if record.phase == UpdatePhase::Draining {
        record = begin_phase(
            &record,
            journal_path,
            state,
            UpdatePhase::Quiescent,
            "no job in flight",
        )?;
        let _ = report_with_retries(api, worker_id, identity, UpdatePhase::Quiescent, None);
    }

    eprintln!(
        "[formation] drained (phase={}); exited {} without claiming",
        record.phase.as_str(),
        EXIT_DRAINED
    );
    std::process::exit(EXIT_DRAINED);
}

/// Source I/O only. Existing authenticated Formation queue/attempts are reused;
/// no build containment, toolchain, manifest or application is entered here.
fn source_archive_one(args: &[String]) -> Result<()> {
    let base = flag(args, "--api-base")
        .context("--api-base is required")?
        .to_owned();
    let root = PathBuf::from(flag(args, "--work-root").context("--work-root is required")?);
    let worker_id = flag(args, "--worker-id").unwrap_or("formation-source-archive");
    let token = std::env::var("ATO_FORMATION_TOKEN").context("ATO_FORMATION_TOKEN is required")?;
    let api =
        ato_formation_worker::source_archive::SourceArchiveApi::new(base.clone(), token.clone())?;
    let fetcher = PinnedSourceFetcher::new(base, token)?;
    let claim = match flag(args, "--job-id") {
        Some(job_id) => api.claim(worker_id, job_id)?,
        None => api.claim_next(worker_id)?,
    };
    let Some(claim) = claim else {
        println!("{{\"job_id\":null}}");
        return Ok(());
    };
    match ato_formation_worker::source_archive::materialize_source_archive(
        &api, &fetcher, &claim, &root,
    ) {
        Ok(output) => {
            println!("{}", String::from_utf8(serde_jcs::to_vec(&output)?)?);
            Ok(())
        }
        Err(error) => {
            if api.report_failure(&claim).is_err() {
                eprintln!("source archive failure report was not accepted");
            }
            Err(error)
        }
    }
}

/// Poll for work, run it, repeat.
///
/// The one-shot path below stays the acceptance and debugging tool; this is
/// what makes a person dropping an HTML file into the PWA possible, since
/// nobody is standing at a terminal to type the job id.
///
/// It is still not a scheduler. The control plane hands out the attempt and
/// its fence exactly as before — this loop only asks "is there anything?", and
/// takes whichever job it is given. Which attempt is current is decided in one
/// place, and that place is not here.
///
/// The update lifecycle is layered on top of that loop, not inside it: startup
/// recovery decides whether this process may claim at all, and the loop stops
/// claiming as soon as the control plane says so. The binary is never replaced
/// by this process; see `docs/ops/formation-worker-update-lifecycle.md`.
fn serve(args: &[String]) -> Result<()> {
    let api_base = flag(args, "--api-base")
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("--api-base is required"))?;
    let work_root =
        PathBuf::from(flag(args, "--work-root").ok_or_else(|| anyhow!("--work-root is required"))?);
    let worker_id = flag(args, "--worker-id").unwrap_or("formation-worker");
    let idle = Duration::from_millis(
        flag(args, "--poll-ms")
            .and_then(|value| value.parse().ok())
            .unwrap_or(2000),
    );
    // 15s matches the rest of the formation control plane; the floor keeps a
    // typo from turning the heartbeat into a busy loop.
    let heartbeat = Duration::from_millis(
        flag(args, "--heartbeat-ms")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(15_000)
            .max(1_000),
    );
    let expect_version = flag(args, "--expect-version");
    let expect_digest = flag(args, "--expect-digest");
    let source_only = source_only_queue(args)?;
    let token = std::env::var("ATO_FORMATION_TOKEN")
        .map_err(|_| anyhow!("ATO_FORMATION_TOKEN is required"))?;

    // First line in the journal, before anything can fail. The deployment
    // record pairs this with the binary's SHA-256; together they answer "what
    // is running" without a forensic exercise.
    let compiled = build_identity();
    eprintln!("[formation] {compiled}");
    let identity = compiled.into_binary_identity()?;
    eprintln!("[formation] serving api={api_base} worker_id={worker_id}");
    // The digest is what the control plane and the operator compare; printing
    // it here saves a `sha256sum` on the host after a swap.
    eprintln!("[formation] binary sha256={}", identity.sha256);

    install_stop_handlers()?;

    // Once, at startup. A host that cannot contain a build must refuse to
    // serve rather than accept jobs and fail them one at a time.
    require_containment()?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let api = FormationApi::new(client, api_base.clone(), token.clone());
    let source_api = match SourceArchiveApi::new(api_base.clone(), token.clone()) {
        Ok(api) => Some(api),
        Err(error) if source_only => {
            return Err(error.context("source-only polling is unavailable for this API origin"));
        }
        Err(_) => {
            eprintln!("[formation] source-only polling unavailable for this API origin");
            None
        }
    };
    let shim = std::env::current_exe().context("cannot locate this worker's own binary")?;
    let fetcher = PinnedSourceFetcher::new(api_base, token)?;
    let context = JobContext {
        api: &api,
        fetcher: &fetcher,
        packer: &DeterministicTreePacker,
        work_root: &work_root,
        shim: &shim,
        worker_id,
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
    };

    let journal_path = work_root.join(UPDATE_JOURNAL_FILE);
    let loaded = UpdateJournal::load(&journal_path);

    // An operator can pin the binary it expects systemd to have started. A
    // mismatch is never "close enough": it means the swap landed wrong or the
    // unit points at the wrong file, and claiming under the wrong identity is
    // how a stale binary keeps serving.
    if let Err(mismatch) = identity.verify_expected(expect_version, expect_digest) {
        if matches!(&loaded, LoadOutcome::Corrupt { .. }) {
            preserve_unreadable_journal(&journal_path)?;
        }
        let update_in_progress = matches!(
            &loaded,
            LoadOutcome::Present(journal)
                if matches!(journal.phase, UpdatePhase::Updating | UpdatePhase::Verifying)
        );
        let record = if update_in_progress {
            match &loaded {
                LoadOutcome::Present(journal) => journal
                    .transition(UpdatePhase::RolledBack, Some(mismatch.to_string()))
                    .with_context(|| format!("cannot record the rollback after: {mismatch}"))?,
                _ => unreachable!("checked above"),
            }
        } else {
            UpdateJournal::quarantined(&identity, mismatch.to_string())
        };
        record.store(&journal_path)?;
        let _ = report_with_retries(&api, worker_id, &identity, record.phase, None);
        eprintln!(
            "[formation] refusing to serve: {mismatch}; journal is {}; exited {} without claiming",
            record.phase.as_str(),
            EXIT_ROLLBACK_REQUIRED
        );
        std::process::exit(EXIT_ROLLBACK_REQUIRED);
    }

    // Crash recovery runs before the first claim, so a restart can never turn
    // an in-progress update into a serving worker of the wrong binary. An
    // `active` journal is kept so a later drain preserves what the last
    // update recorded (target, previous binary) instead of overwriting it.
    let mut journal: Option<UpdateJournal> = match &loaded {
        LoadOutcome::Present(record) => Some((**record).clone()),
        _ => None,
    };
    // A directive that arrived while verifying; the loop must not ignore a
    // drain just because it was fetched before the loop existed.
    let mut recovered_directive: Option<WorkerDirective> = None;
    match recover(loaded, &identity) {
        Recovery::Active => {}
        Recovery::Verifying { journal: verifying } => {
            let verifying = *verifying;
            verifying.store(&journal_path)?;
            let directive = await_state_report(&api, worker_id, &identity, UpdatePhase::Verifying);
            let active = verifying
                .transition(
                    UpdatePhase::Active,
                    Some("the running binary matches the update target".to_owned()),
                )
                .context("cannot mark the update verified")?;
            active.store(&journal_path)?;
            journal = Some(active);
            recovered_directive = Some(directive);
        }
        Recovery::Drained { journal: drained } => {
            let drained = *drained;
            let _ = report_with_retries(&api, worker_id, &identity, drained.phase, None);
            eprintln!(
                "[formation] {0} on restart; exited {1} without claiming",
                drained.phase.as_str(),
                EXIT_DRAINED
            );
            std::process::exit(EXIT_DRAINED);
        }
        Recovery::RolledBack {
            journal: rolled_back,
        } => {
            let rolled_back = *rolled_back;
            rolled_back.store(&journal_path)?;
            let _ = report_with_retries(&api, worker_id, &identity, rolled_back.phase, None);
            eprintln!(
                "[formation] rollback required: {}; exited {} without claiming",
                rolled_back
                    .detail
                    .as_deref()
                    .unwrap_or("the running binary does not match the update target"),
                EXIT_ROLLBACK_REQUIRED
            );
            std::process::exit(EXIT_ROLLBACK_REQUIRED);
        }
        Recovery::Quarantined {
            journal: quarantined,
            replaced_unreadable,
        } => {
            let quarantined = *quarantined;
            if replaced_unreadable {
                preserve_unreadable_journal(&journal_path)?;
            }
            quarantined.store(&journal_path)?;
            let _ = report_with_retries(&api, worker_id, &identity, quarantined.phase, None);
            eprintln!(
                "[formation] quarantined: {}; exited {} without claiming",
                quarantined
                    .detail
                    .as_deref()
                    .unwrap_or("the update lifecycle cannot be resumed"),
                EXIT_ROLLBACK_REQUIRED
            );
            std::process::exit(EXIT_ROLLBACK_REQUIRED);
        }
    }

    let state = Arc::new(ServingState {
        phase: Mutex::new(UpdatePhase::Active),
        current_attempt: Mutex::new(None),
        directive: Mutex::new(None),
        drain_requested: AtomicBool::new(false),
        heartbeat_stopped: AtomicBool::new(false),
    });
    if let Some(directive) = recovered_directive {
        if directive.drain || update_is_pending(&directive, &identity) {
            state.drain_requested.store(true, Ordering::SeqCst);
        }
        *lock(&state.directive) = Some(directive);
    }
    let heartbeat = spawn_heartbeat(
        api.clone(),
        identity.clone(),
        worker_id.to_owned(),
        Arc::clone(&state),
        heartbeat,
    );

    println!("[formation] serving as {worker_id}");

    // `Some` when the loop stopped because the control plane said so, rather
    // than a signal. It carries the target binary from the refusal.
    let mut refusal: Option<ClaimRefusal> = None;
    let mut poll_source = true;
    loop {
        // Checked before every claim: after a stop or a drain directive the
        // worker must never take another attempt fence.
        if STOP_REQUESTED.load(Ordering::SeqCst) || state.drain_requested.load(Ordering::SeqCst) {
            break;
        }

        // Alternate source and ordinary work so a continuously busy queue
        // cannot starve the other. Source-only uses its dedicated claim and
        // physical materializer; it never enters the application build lane.
        let source_polled = (source_only || poll_source) && source_api.is_some();
        poll_source = !poll_source;
        if let Some(source_api) = source_api.as_ref().filter(|_| source_polled) {
            match source_api.claim_next_controlled(worker_id) {
                Ok(SourceArchiveClaimOutcome::Work(claim)) => {
                    *lock(&state.current_attempt) = Some(claim.attempt_id.clone());
                    match materialize_source_archive(source_api, &fetcher, &claim, &work_root) {
                        Ok(_) => println!(
                            "[formation] source job={} attempt={} materialized",
                            claim.job_id, claim.attempt_id
                        ),
                        Err(_) => {
                            eprintln!(
                                "[formation] source job={} attempt={} failed",
                                claim.job_id, claim.attempt_id
                            );
                            if source_api.report_failure(&claim).is_err() {
                                eprintln!(
                                    "[formation] source archive failure report was not accepted"
                                );
                            }
                        }
                    }
                    *lock(&state.current_attempt) = None;
                    continue;
                }
                Ok(SourceArchiveClaimOutcome::Idle) => {}
                Ok(SourceArchiveClaimOutcome::Stopped(stop)) => {
                    refusal = Some(stop);
                    state.drain_requested.store(true, Ordering::SeqCst);
                    break;
                }
                Err(_) => eprintln!("[formation] source claim failed"),
            }
            // A heartbeat may have requested drain while the source poll was
            // in flight. Do not ask the other queue after that directive.
            if STOP_REQUESTED.load(Ordering::SeqCst) || state.drain_requested.load(Ordering::SeqCst)
            {
                break;
            }
        }

        // Dedicated source workers must never consume the normal Formation
        // queue. Give the source-only endpoint the same bounded idle interval.
        if source_only {
            pause(&state, idle);
            continue;
        }

        // A poll that fails is a transient control-plane or network problem,
        // not a reason to end the service: exiting here would need an operator
        // to notice and restart, for something that usually clears itself.
        let work = match api.claim_next_controlled(worker_id) {
            Ok(ClaimOutcome::Work(work)) => work,
            Ok(ClaimOutcome::Idle) => {
                if source_polled || source_api.is_none() {
                    pause(&state, idle);
                }
                continue;
            }
            // The control plane's stop signal is authoritative even if the
            // last state report failed: no attempt fence is consumed here.
            Ok(ClaimOutcome::Stopped(stop)) => {
                eprintln!("[formation] the control plane stopped claims: {stop:?}");
                refusal = Some(stop);
                state.drain_requested.store(true, Ordering::SeqCst);
                break;
            }
            Err(error) => {
                eprintln!("[formation] claim failed: {error:#}");
                pause(&state, idle);
                continue;
            }
        };

        *lock(&state.current_attempt) = Some(work.attempt_id.clone());
        let attempt = BuildAttempt {
            job_id: work.job_id.clone(),
            attempt_id: work.attempt_id.clone(),
            attempt_fence: work.attempt_fence,
        };
        // Empty rather than invented: a job submitted before the control plane
        // recorded a target has no target, and guessing one would attach this
        // result to something nobody asked for.
        let compute_id = work.compute_id.clone().unwrap_or_default();
        let revision_id = work.capsule_revision_id.clone().unwrap_or_default();

        match run_claimed_job(
            &context,
            &attempt,
            &work.job,
            &compute_id,
            &revision_id,
            work.operation_catalog_required,
        ) {
            Ok(outcome) => {
                println!(
                    "[formation] job={} attempt={} closure={} materialization={} outcome={:?}",
                    work.job_id,
                    work.attempt_id,
                    outcome.closure_ref,
                    outcome.materialization_ref,
                    outcome.outcome
                );
                match &outcome.outcome {
                    PublishOutcome::Accepted { .. } => {}
                    // A result the rollout discarded leaves nothing for whoever is
                    // waiting to poll for. Saying so ends their wait with a fact
                    // instead of a timeout.
                    PublishOutcome::NotRegistered { mode, reason } => {
                        let detail = bounded_reason(reason);
                        let message = if detail.is_empty() {
                            format!(
                                "this app was built but not kept: the {mode} lane is not live yet"
                            )
                        } else {
                            format!(
                                "this app was built but not kept: the {mode} lane is not live yet ({detail})"
                            )
                        };
                        if let Err(report) = api.report_failure(
                            &work.attempt_id,
                            &FailureReport::publish("formation_result_not_registered", message),
                        ) {
                            eprintln!("[formation] could not report the failure: {report:#}");
                        }
                    }
                    PublishOutcome::Refused { code } => {
                        // Superseded / already-accepted are settled elsewhere:
                        // a newer attempt owns the job, or the job already has
                        // its schema. Failing here would overwrite that state.
                        // Every other refusal leaves the job `running` with no
                        // result coming, so it must become a terminal failure.
                        if !is_settled_refusal(code) {
                            let message = format!(
                                "{code}: the build finished but the result was refused; try again"
                            );
                            if let Err(report) = api.report_failure(
                                &work.attempt_id,
                                &FailureReport::publish("formation_result_refused", message),
                            ) {
                                eprintln!("[formation] could not report the failure: {report:#}");
                            }
                        }
                    }
                }
            }
            // Every execution error must end the attempt terminally. Leaving
            // the job `running` is what polls forever.
            Err(error) => {
                // The full chain — and only the full chain — goes to the
                // operator log. It can carry a filesystem path, a token a tool
                // echoed back, or a dependency URL with a credential in it.
                eprintln!(
                    "[formation] job={} attempt={} failed: {error:#}",
                    work.job_id, work.attempt_id
                );
                let report = classify(&error);
                if let Err(report) = api.report_failure(&work.attempt_id, &report) {
                    eprintln!("[formation] could not report the failure: {report:#}");
                }
            }
        }
        // The in-flight job has been executed and reported; a stop that arrived
        // while it ran is honoured before the next claim.
        *lock(&state.current_attempt) = None;
    }

    let directive = lock(&state.directive).clone();
    let stopping_for_update = refusal.is_some()
        || directive
            .as_ref()
            .is_some_and(|directive| directive.drain || update_is_pending(directive, &identity));

    if stopping_for_update {
        let target = match &refusal {
            Some(ClaimRefusal::Drained) | None => directive
                .as_ref()
                .map(|directive| {
                    (
                        directive.target_version.clone(),
                        directive.target_digest.clone(),
                    )
                })
                .unwrap_or((None, None)),
            Some(ClaimRefusal::UpdateRequired {
                target_version,
                target_digest,
            }) => (target_version.clone(), target_digest.clone()),
        };
        drain_to_quiescent(
            &api,
            worker_id,
            &identity,
            &journal_path,
            journal,
            target,
            &state,
            heartbeat,
        )?;
        unreachable!("drain_to_quiescent exits the process");
    }

    // SIGTERM/SIGINT without a drain request: the in-flight job has finished
    // and reported, and the local phase is unchanged, so a plain restart picks
    // up where it left off.
    let _ = report_with_retries(&api, worker_id, &identity, UpdatePhase::Active, None);
    eprintln!(
        "[formation] stop requested; no drain was requested; exited {} without claiming",
        EXIT_DRAINED
    );
    std::process::exit(EXIT_DRAINED);
}

/// Run one job to completion.
///
/// One job per invocation rather than a polling daemon, deliberately. The
/// control plane already owns queueing, idempotency and the attempt fence; a
/// second scheduler inside the worker would be a second place where "which
/// attempt is current" gets decided — and the old builder's daemon is exactly
/// what B1 is not carrying forward.
fn run_one(args: &[String]) -> Result<()> {
    let need = |name: &str| -> Result<String> {
        flag(args, name)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("{name} is required"))
    };
    let api_base = need("--api-base")?;
    let job_id = need("--job-id")?;
    let compute_id = need("--compute-id")?;
    let capsule_revision_id = need("--capsule-revision-id")?;
    let work_root = PathBuf::from(need("--work-root")?);
    let worker_id = flag(args, "--worker-id").unwrap_or("formation-worker");
    let token = std::env::var("ATO_FORMATION_TOKEN")
        .map_err(|_| anyhow!("ATO_FORMATION_TOKEN is required"))?;

    // Refuse BEFORE claiming: a job this worker cannot contain should not
    // consume an attempt fence on its way to failing.
    require_containment()?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let api = FormationApi::new(client, api_base.clone(), token.clone());
    let shim = std::env::current_exe().context("cannot locate this worker's own binary")?;
    // The fetcher needs the API too: an uploaded source is asked for by job,
    // through the control plane, rather than from a public URL.
    let fetcher = PinnedSourceFetcher::new(api_base, token)?;

    let context = JobContext {
        api: &api,
        fetcher: &fetcher,
        packer: &DeterministicTreePacker,
        work_root: &work_root,
        shim: &shim,
        worker_id,
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
    };

    // Claim first so every path below holds the attempt id: a one-shot that
    // exits without reporting leaves the job `running` with nobody coming
    // back for it — the same infinite-Building as the daemon path.
    let claimed = context.api.claim(&job_id, worker_id)?;
    let attempt = BuildAttempt {
        job_id: job_id.clone(),
        attempt_id: claimed.attempt_id.clone(),
        attempt_fence: claimed.attempt_fence,
    };

    match run_claimed_job(
        &context,
        &attempt,
        &claimed.job,
        &compute_id,
        &capsule_revision_id,
        claimed.operation_catalog_required,
    ) {
        Ok(outcome) => {
            println!(
                "[formation] job={} attempt={} fence={} closure={} materialization={} outcome={:?}",
                job_id,
                outcome.attempt.attempt_id,
                outcome.attempt.attempt_fence,
                outcome.closure_ref,
                outcome.materialization_ref,
                outcome.outcome
            );
            match outcome.outcome {
                PublishOutcome::Accepted { .. } => Ok(()),
                // The build worked; the rollout said not to keep it. Report
                // terminally so pollers hear it, then exit non-zero so it
                // never reads as a finished App.
                PublishOutcome::NotRegistered { mode, reason } => {
                    let detail = bounded_reason(&reason);
                    let message = if detail.is_empty() {
                        format!("this app was built but not kept: the {mode} lane is not live yet")
                    } else {
                        format!(
                            "this app was built but not kept: the {mode} lane is not live yet ({detail})"
                        )
                    };
                    let _ = api.report_failure(
                        &attempt.attempt_id,
                        &FailureReport::publish("formation_result_not_registered", message),
                    );
                    Err(anyhow!("result not registered ({mode}): {reason}"))
                }
                // A refusal is the control plane doing its job. Terminal
                // refusals must still be reported or the job stays `running`;
                // settled ones (superseded / already accepted) belong to
                // another attempt or an already-minted schema.
                PublishOutcome::Refused { code } => {
                    if !is_settled_refusal(&code) {
                        let _ = api.report_failure(
                            &attempt.attempt_id,
                            &FailureReport::publish(
                                "formation_result_refused",
                                format!(
                                    "{code}: the build finished but the result was refused; try again"
                                ),
                            ),
                        );
                    }
                    Err(anyhow!("result refused: {code}"))
                }
            }
        }
        Err(error) => {
            eprintln!(
                "[formation] job={job_id} attempt={} failed: {error:#}",
                attempt.attempt_id
            );
            let _ = api.report_failure(&attempt.attempt_id, &classify(&error));
            Err(error)
        }
    }
}

/// A refusal settled elsewhere needs no failure report: superseded means a
/// newer attempt owns the job, already-accepted means the schema exists.
fn is_settled_refusal(code: &str) -> bool {
    matches!(
        code,
        "formation_attempt_superseded" | "formation_result_already_accepted"
    )
}

/// Packs a tree the way the Runner expects to receive it.
struct DeterministicTreePacker;

impl TreePacker for DeterministicTreePacker {
    fn pack(&self, root: &Path) -> Result<Vec<u8>> {
        pack_tree(root)
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn source_only_queue(args: &[String]) -> Result<bool> {
    let mut mode = None;
    for (index, arg) in args.iter().enumerate() {
        let value = if arg == "--queue" {
            Some(args.get(index + 1).map(String::as_str).unwrap_or(""))
        } else if let Some(value) = arg.strip_prefix("--queue=") {
            Some(value)
        } else if arg.starts_with("--queue") {
            return Err(anyhow!("unrecognised --queue option"));
        } else {
            None
        };
        if let Some(value) = value
            && mode.replace(value).is_some()
        {
            return Err(anyhow!("--queue must be specified at most once"));
        }
    }
    match mode {
        None => Ok(false),
        Some("all") => Ok(false),
        Some("source-only") => Ok(true),
        _ => Err(anyhow!("--queue must be all or source-only")),
    }
}

#[cfg(test)]
mod queue_tests {
    use super::source_only_queue;

    #[test]
    fn queue_mode_is_explicit_and_malformed_modes_fail_closed() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        assert!(!source_only_queue(&args(&[])).unwrap());
        assert!(!source_only_queue(&args(&["--queue", "all"])).unwrap());
        assert!(source_only_queue(&args(&["--queue", "source-only"])).unwrap());
        assert!(source_only_queue(&args(&["--queue=source-only"])).unwrap());
        for bad in [
            vec!["--queue"],
            vec!["--queue", "--worker-id"],
            vec!["--queue", "source"],
            vec!["--queue="],
            vec!["--queue-source-only"],
            vec!["--queue", "source-only", "--queue", "all"],
            vec!["--queue=source-only", "--queue=all"],
        ] {
            assert!(source_only_queue(&args(&bad)).is_err(), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod failure_classification_tests {
    use super::classify;
    use ato_formation::capsule_toml::CapsuleTomlError;
    use ato_formation::failure::{FailureStage, FormationFailure};

    /// The property that matters: the code comes from the TYPE. Nothing here
    /// reads a message to decide what happened, so improving a message can
    /// never silently change a code.
    #[test]
    fn a_typed_failure_reports_its_own_code_and_stage() {
        let failure: FormationFailure = CapsuleTomlError::LegacyStoreManifest.into();
        let error = anyhow::Error::new(failure);
        let report = classify(&error);
        assert_eq!(report.code, "legacy_store_manifest");
        assert_eq!(report.stage, "authoring");
        assert!(report.message.contains("store submission manifest"));
    }

    #[test]
    fn a_typed_failure_survives_added_context() {
        // job.rs adds context as an error travels out; the type must still be
        // recoverable underneath it or the code silently degrades to generic.
        let failure = FormationFailure::new("preset_no_match", FailureStage::Preset, "no lane");
        let error = anyhow::Error::new(failure).context("while forming the source");
        let report = classify(&error);
        assert_eq!(report.code, "preset_no_match");
        assert_eq!(report.stage, "preset");
    }

    /// An unrecognised failure stays anonymous. `{error:#}` is a chain of
    /// internal context that can name a path, echo a token, or carry a
    /// credentialed URL — it belongs in the operator log, not in a response.
    #[test]
    fn an_untyped_failure_never_leaks_its_chain() {
        let error = anyhow::anyhow!("/home/builder/.npmrc: //registry:_authToken=s3cret")
            .context("npm ci failed");
        let report = classify(&error);
        assert_eq!(report.code, "build_failed");
        assert_eq!(report.stage, "build");
        assert!(!report.message.contains("s3cret"));
        assert!(!report.message.contains("/home/builder"));
        assert_eq!(
            report.message,
            "the app could not be built from this source"
        );
    }

    #[test]
    fn a_typed_message_is_bounded_before_it_leaves() {
        let failure = FormationFailure::new("x", FailureStage::Authoring, "y".repeat(900));
        let report = classify(&anyhow::Error::new(failure));
        assert!(report.message.len() <= 400);
    }
}

#[cfg(test)]
mod terminal_reporting_tests {
    use super::is_settled_refusal;
    use ato_formation_worker::api::bounded_reason;

    #[test]
    fn settled_refusals_need_no_report() {
        assert!(is_settled_refusal("formation_attempt_superseded"));
        assert!(is_settled_refusal("formation_result_already_accepted"));
    }

    #[test]
    fn terminal_refusals_must_be_reported() {
        for code in [
            "formation_result_invalid",
            "formation_no_process_candidate",
            "formation_attempt_not_found",
            "unknown",
        ] {
            assert!(!is_settled_refusal(code), "code {code} must report");
        }
    }

    #[test]
    fn concrete_reason_survives_truncation() {
        let reason = "invalid capsule.toml: expected '.' at line 3 column 7";
        assert_eq!(bounded_reason(reason), reason);
        let long = "x".repeat(500);
        assert!(bounded_reason(&long).len() <= 400);
        assert_eq!(
            bounded_reason("a\nb\nc"),
            "a b c",
            "newlines become spaces for one readable sentence"
        );
    }
}

#[cfg(test)]
mod update_lifecycle_tests {
    use super::{BuildIdentity, update_is_pending};
    use ato_formation_worker::api::WorkerDirective;
    use ato_formation_worker::lifecycle::BinaryIdentity;

    fn identity(version: &str, sha_fill: char) -> BinaryIdentity {
        BinaryIdentity {
            version: version.to_owned(),
            commit: "17305afe0".to_owned(),
            dirty: "clean".to_owned(),
            build_id: "ci-run-1".to_owned(),
            rustc: "rustc 1.96.0 (ac68faa20 2026-05-25)".to_owned(),
            sha256: sha_fill.to_string().repeat(64),
        }
    }

    #[test]
    fn the_version_line_stays_byte_compatible() {
        let identity = BuildIdentity {
            version: "0.1.0",
            commit: "17305afe0",
            dirty: "clean",
            build_id: "ci-run-12345",
            rustc: "rustc 1.96.0 (ac68faa20 2026-05-25)",
        };
        assert_eq!(
            identity.to_string(),
            "ato-formation-worker 0.1.0 commit=17305afe0 clean build_id=ci-run-12345 rustc 1.96.0 (ac68faa20 2026-05-25)"
        );
    }

    #[test]
    fn the_compiled_build_identity_is_available_structurally() {
        let compiled = BuildIdentity::compiled();
        assert_eq!(compiled.version, env!("CARGO_PKG_VERSION"));
        assert!(!compiled.commit.is_empty());
        assert!(!compiled.dirty.is_empty());
        assert!(compiled.to_string().starts_with("ato-formation-worker "));
    }

    #[test]
    fn a_pending_update_is_detected_with_or_without_a_digest() {
        let running = identity("0.1.1", 'b');
        let directive = |version: Option<&str>, digest: Option<String>| WorkerDirective {
            ok: true,
            drain: true,
            target_version: version.map(ToOwned::to_owned),
            target_digest: digest,
            min_version: None,
            next_heartbeat_seconds: None,
        };
        assert!(!update_is_pending(&directive(None, None), &running));
        assert!(!update_is_pending(
            &directive(Some("0.1.1"), Some("b".repeat(64))),
            &running
        ));
        assert!(update_is_pending(&directive(Some("0.1.0"), None), &running));
        assert!(update_is_pending(
            &directive(Some("0.1.1"), Some("c".repeat(64))),
            &running
        ));
        // A malformed digest must not read as "already there".
        assert!(update_is_pending(
            &directive(Some("0.1.1"), Some("not-a-digest".to_owned())),
            &running
        ));
    }
}
