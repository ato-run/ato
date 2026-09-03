//! The Formation worker binary, and the in-sandbox shim it re-enters as.

use anyhow::{Result, anyhow};
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

    Err(anyhow!(
        "the Formation worker's job loop is not implemented in this slice (B1-E wires \
         publication; B1-H wires the gateway). `sandbox-exec` is available."
    ))
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
