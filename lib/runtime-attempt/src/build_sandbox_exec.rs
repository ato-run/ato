//! The in-sandbox re-entry point every host binary shares.
//!
//! `bwrap` sets up the namespaces, then execs the CALLING binary again with
//! `sandbox-exec` as argv[1]. Any binary that runs a contained build — the
//! worker service, the CLI's local Formation — must therefore expose this
//! entry point, which is why it lives in the library rather than in one bin.

use std::path::Path;

use anyhow::{Result, anyhow};

/// Re-entry from INSIDE the sandbox. bwrap sets up the namespaces, then execs
/// this binary, which restricts itself with Landlock and execs the workload —
/// a build step or a Formation candidate. Landlock must be applied by the
/// process that will exec — `restrict_self` survives `exec` — and applying it
/// to bwrap instead denies bwrap its own `/proc/self/uid_map` write.
///
/// Handled before clap in each host binary because it is not a user-facing
/// subcommand.
pub fn sandbox_exec(args: &[String]) -> Result<()> {
    // The shim's own flags are the ones before `--`; everything after is the
    // workload's argv and is never read as a flag.
    let separator = args.iter().position(|arg| arg == "--");
    let (head, workload) = match separator {
        Some(index) => (&args[..index], args[index + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let policy_path = flag(head, "--policy").ok_or_else(|| anyhow!("--policy is required"))?;
    let max_processes = flag(head, "--max-processes").and_then(|value| value.parse::<u64>().ok());
    if workload.is_empty() {
        return Err(anyhow!("sandbox-exec: no workload to execute"));
    }
    // Environment and cwd for the workload alone, applied by its `exec`
    // after the restrictions below: never this process's own.
    let mut workload_env = Vec::new();
    for (index, arg) in head.iter().enumerate() {
        if arg == "--env" {
            let pair = head
                .get(index + 1)
                .ok_or_else(|| anyhow!("sandbox-exec: --env needs NAME=VALUE"))?;
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| anyhow!("sandbox-exec: --env needs NAME=VALUE"))?;
            workload_env.push((name.to_owned(), value.to_owned()));
        }
    }
    let workload_cwd = flag(head, "--cwd").map(Path::new);

    // A process ceiling, enforced by the kernel rather than by watching. A
    // fork bomb inside a build should exhaust its own limit, not the host's.
    if let Some(limit) = max_processes {
        set_process_limit(limit);
    }

    // Everything else is the Runtime's shim, not a copy of it: the same
    // no-new-privs, the same Landlock application, and the same refusal to
    // exec a workload whose network isolation was asked for and could not be
    // enforced. A build and a Formation candidate are contained by exactly
    // the rules a Run is.
    crate::launch::sandbox_exec::run_with(
        Path::new(policy_path),
        &workload,
        &workload_env,
        workload_cwd,
    )
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
