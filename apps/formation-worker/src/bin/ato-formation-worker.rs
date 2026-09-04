//! The Formation worker binary, and the in-sandbox shim it re-enters as.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use ato_formation::source::SourceLimits;
use ato_formation_worker::api::{FormationApi, PublishOutcome};
use ato_formation_worker::job::{PinnedSourceFetcher, JobContext, TreePacker, run_job};
use ato_formation_worker::pack::pack_tree;
use ato_formation_worker::sandbox::{BuildLimits, require_containment};
use ato_sandbox::{SandboxPolicy, apply_sandbox, is_sandbox_supported, set_no_new_privs};

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();

    // Re-entry from INSIDE the sandbox. bwrap sets up the namespaces, then
    // execs this binary, which restricts itself with Landlock and execs the
    // build step. Landlock must be applied by the process that will exec —
    // `restrict_self` survives `exec` — and applying it to bwrap instead denies
    // bwrap its own `/proc/self/uid_map` write.
    //
    // Handled before clap because it is not a user-facing subcommand.
    if args.get(1).is_some_and(|arg| arg == "sandbox-exec") {
        return sandbox_exec(&args[2..]);
    }

    run_one(&args[1..])
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
    let api = FormationApi::new(client.clone(), api_base.clone(), token.clone());
    let shim = std::env::current_exe().context("cannot locate this worker's own binary")?;
    // The fetcher needs the API too: an uploaded source is asked for by job,
    // through the control plane, rather than from a public URL.
    let fetcher = PinnedSourceFetcher::new(client, api_base, token);

    let outcome = run_job(
        &JobContext {
            api: &api,
            fetcher: &fetcher,
            packer: &DeterministicTreePacker,
            work_root: &work_root,
            shim: &shim,
            worker_id,
            limits: BuildLimits::default(),
            source_limits: SourceLimits::default(),
        },
        &job_id,
        &compute_id,
        &capsule_revision_id,
    )?;

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
        // A refusal is the control plane doing its job. Exiting non-zero says
        // so without pretending the build itself failed.
        PublishOutcome::Refused { code } => Err(anyhow!("result refused: {code}")),
    }
}

/// Packs a tree the way the Runner expects to receive it.
struct DeterministicTreePacker;

impl TreePacker for DeterministicTreePacker {
    fn pack(&self, root: &Path) -> Result<Vec<u8>> {
        pack_tree(root)
    }
}

fn sandbox_exec(args: &[String]) -> Result<()> {
    let policy_path = flag(args, "--policy").ok_or_else(|| anyhow!("--policy is required"))?;
    let max_processes = flag(args, "--max-processes").and_then(|value| value.parse::<u64>().ok());
    let workload = args
        .iter()
        .position(|arg| arg == "--")
        .map(|index| args[index + 1..].to_vec())
        .unwrap_or_default();
    let (program, arguments) = workload
        .split_first()
        .ok_or_else(|| anyhow!("sandbox-exec: no build step to execute"))?;

    // Landlock needs either CAP_SYS_ADMIN or this flag, and the flag survives
    // the coming exec.
    if let Err(error) = set_no_new_privs() {
        eprintln!("[formation sandbox-exec] PR_SET_NO_NEW_PRIVS failed: {error}");
    }

    // A process ceiling, enforced by the kernel rather than by watching. A
    // fork bomb inside a build should exhaust its own limit, not the host's.
    if let Some(limit) = max_processes {
        set_process_limit(limit);
    }

    if is_sandbox_supported() {
        let policy: SandboxPolicy = serde_json::from_slice(&std::fs::read(policy_path)?)?;
        // Defence in depth on top of the bubblewrap namespace and bind mounts,
        // which are what actually contain the build. Recorded when it cannot be
        // applied, so "namespace-only" is never silently reported as fully
        // sandboxed.
        if let Err(error) = apply_sandbox(&policy) {
            eprintln!("[formation sandbox-exec] Landlock not applied (namespace-only): {error}");
        }
    } else {
        eprintln!("[formation sandbox-exec] Landlock unsupported on this kernel; namespace-only");
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(program).args(arguments).exec();
        Err(anyhow!("sandbox-exec: cannot exec {program}: {error}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, arguments);
        Err(anyhow!("sandbox-exec is only available on Unix"))
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

#[cfg(target_os = "linux")]
fn set_process_limit(limit: u64) {
    if let Err(error) = ato_sandbox::set_process_limit(limit) {
        eprintln!("[formation sandbox-exec] RLIMIT_NPROC failed: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
fn set_process_limit(_limit: u64) {}
