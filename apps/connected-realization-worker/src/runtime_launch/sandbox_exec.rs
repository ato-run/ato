//! The in-sandbox Landlock shim.
//!
//! Not a user-facing subcommand. `bwrap` execs the Runner's own binary as
//!
//! ```text
//! /.ato/runner sandbox-exec --policy /.ato/sandbox-policy.json -- <workload argv...>
//! ```
//!
//! after it has finished setting up the namespaces. The shim applies Landlock
//! to ITSELF and then `exec`s the workload, so the restriction lands on the
//! workload rather than on the bwrap wrapper.
//!
//! ## Why the shim exists at all
//!
//! Landlock must be applied by the process that will `exec` the workload,
//! because `restrict_self` survives `exec`. Applying it to bwrap instead
//! breaks bwrap's own setup: bwrap writes `/proc/self/uid_map` to map the
//! unprivileged user namespace, and a policy that correctly withholds write
//! access to `/proc` denies that write — bwrap then fails before the workload
//! ever runs. Running Landlock after bwrap's setup avoids the ordering hazard.
//!
//! Taken from nacelle's `sandbox-exec`, which solved this first.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use ato_sandbox::{SandboxPolicy, apply_sandbox, is_sandbox_supported};

/// Apply the serialized policy, then `exec` the workload.
///
/// Only returns on error; on success the process image is replaced.
pub fn run(policy_path: &Path, argv: &[String]) -> Result<()> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| anyhow!("sandbox-exec: no workload to execute"))?;

    // Landlock needs either CAP_SYS_ADMIN or PR_SET_NO_NEW_PRIVS, and the flag
    // is inherited across the coming exec. The primitive itself lives in
    // `ato-sandbox`, because this crate denies `unsafe_code`.
    if let Err(error) = ato_sandbox::set_no_new_privs() {
        eprintln!("[sandbox-exec] PR_SET_NO_NEW_PRIVS failed: {error}");
    }

    if is_sandbox_supported() {
        let policy = read_policy(policy_path)?;
        // Landlock is defence in depth on top of the bubblewrap namespace and
        // bind mounts, which are already in force and are what actually
        // contains the workload. A kernel that cannot apply it is not a reason
        // to fail the Run — but it IS recorded, so "namespace-only" is never
        // silently reported as fully sandboxed.
        if let Err(error) = apply_sandbox(&policy) {
            eprintln!("[sandbox-exec] Landlock not applied (namespace-only): {error}");
        }
    } else {
        eprintln!("[sandbox-exec] Landlock unsupported on this kernel; namespace-only");
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(program).args(arguments).exec();
        Err(anyhow!("sandbox-exec: failed to exec {program}: {error}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, arguments);
        Err(anyhow!("sandbox-exec is only available on Unix"))
    }
}

fn read_policy(path: &Path) -> Result<SandboxPolicy> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("sandbox-exec: cannot read policy {}", path.display()))?;
    serde_json::from_slice(&bytes).context("sandbox-exec: policy is malformed")
}
