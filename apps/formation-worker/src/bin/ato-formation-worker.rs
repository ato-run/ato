//! The Formation worker binary, and the in-sandbox shim it re-enters as.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use ato_formation::source::SourceLimits;
use ato_formation_worker::api::{FormationApi, PublishOutcome};
use ato_formation_worker::build::BuildAttempt;
use ato_formation_worker::job::{JobContext, PinnedSourceFetcher, TreePacker, run_claimed_job};
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

    if args.get(1).is_some_and(|arg| arg == "serve") {
        return serve(&args[2..]);
    }

    run_one(&args[1..])
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
    let token = std::env::var("ATO_FORMATION_TOKEN")
        .map_err(|_| anyhow!("ATO_FORMATION_TOKEN is required"))?;

    // Once, at startup. A host that cannot contain a build must refuse to
    // serve rather than accept jobs and fail them one at a time.
    require_containment()?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let api = FormationApi::new(client.clone(), api_base.clone(), token.clone());
    let shim = std::env::current_exe().context("cannot locate this worker's own binary")?;
    let fetcher = PinnedSourceFetcher::new(client, api_base, token);
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

    println!("[formation] serving as {worker_id}");
    loop {
        // A poll that fails is a transient control-plane or network problem,
        // not a reason to end the service: exiting here would need an operator
        // to notice and restart, for something that usually clears itself.
        let work = match api.claim_next(worker_id) {
            Ok(work) => work,
            Err(error) => {
                eprintln!("[formation] claim failed: {error:#}");
                std::thread::sleep(idle);
                continue;
            }
        };
        let Some(work) = work else {
            std::thread::sleep(idle);
            continue;
        };

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

        match run_claimed_job(&context, &attempt, &work.job, &compute_id, &revision_id) {
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
                        let detail = truncate_failure_reason(reason);
                        let message = if detail.is_empty() {
                            format!(
                                "this app was built but not kept: the {mode} lane is not live yet"
                            )
                        } else {
                            format!(
                                "this app was built but not kept: the {mode} lane is not live yet ({detail})"
                            )
                        };
                        if let Err(report) = api.report_failure(&work.attempt_id, &message) {
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
                            if let Err(report) = api.report_failure(&work.attempt_id, &message) {
                                eprintln!("[formation] could not report the failure: {report:#}");
                            }
                        }
                    }
                }
            }
            // Every execution error must end the attempt terminally. Leaving
            // the job `running` is what polls forever.
            Err(error) => {
                eprintln!(
                    "[formation] job={} attempt={} failed: {error:#}",
                    work.job_id, work.attempt_id
                );
                // The full chain stays in this log; what crosses back is the
                // concrete, user-actionable sentence (invalid capsule.toml,
                // missing preset, network-denied dependency install, …),
                // truncated to the control plane's 400-char bound. A generic
                // "could not be built" throws away the only part that tells
                // somebody what to change.
                let reason = truncate_failure_reason(&format!("{error:#}"));
                let reason = if reason.is_empty() {
                    "the app could not be built from this source".to_owned()
                } else {
                    reason
                };
                if let Err(report) = api.report_failure(&work.attempt_id, &reason) {
                    eprintln!("[formation] could not report the failure: {report:#}");
                }
            }
        }
    }
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
                    let detail = truncate_failure_reason(&reason);
                    let message = if detail.is_empty() {
                        format!("this app was built but not kept: the {mode} lane is not live yet")
                    } else {
                        format!(
                            "this app was built but not kept: the {mode} lane is not live yet ({detail})"
                        )
                    };
                    let _ = api.report_failure(&attempt.attempt_id, &message);
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
                            &format!(
                                "{code}: the build finished but the result was refused; try again"
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
            let reason = truncate_failure_reason(&format!("{error:#}"));
            let reason = if reason.is_empty() {
                "the app could not be built from this source".to_owned()
            } else {
                reason
            };
            let _ = api.report_failure(&attempt.attempt_id, &reason);
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

/// Single-line, bounded failure text for the control plane (which caps at 400
/// chars). Newlines become spaces so one failure stays one readable sentence.
fn truncate_failure_reason(reason: &str) -> String {
    const LIMIT: usize = 400;
    let single: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = single.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim_end().to_owned()
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

#[cfg(test)]
mod terminal_reporting_tests {
    use super::{is_settled_refusal, truncate_failure_reason};

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
        assert_eq!(truncate_failure_reason(reason), reason);
        let long = "x".repeat(500);
        assert!(truncate_failure_reason(&long).len() <= 400);
        assert_eq!(
            truncate_failure_reason("a\nb\nc"),
            "a b c",
            "newlines become spaces for one readable sentence"
        );
    }
}
