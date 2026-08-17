//! In-sandbox Landlock shim.
//!
//! This subcommand is **not** meant to be invoked by users. The Linux source
//! launcher uses it as a thin wrapper that bubblewrap execs *inside* the
//! sandbox: bwrap first sets up the namespaces (user/mount/pid/…), then execs
//!
//! ```text
//! nacelle sandbox-exec --policy <file> -- <real command> <args...>
//! ```
//!
//! The shim applies Landlock to *itself* and then `exec`s the real command, so
//! Landlock restricts the **workload** rather than the bwrap wrapper.
//!
//! ## Why a shim is required
//!
//! Landlock must be applied by the process that will run the workload, before
//! it `exec`s, because `restrict_self` survives `exec`. Applying it to the
//! bwrap wrapper instead breaks bwrap's own namespace setup: bwrap writes
//! `/proc/self/uid_map` to map the unprivileged user namespace, and a Landlock
//! policy that (correctly) does not grant write access to `/proc` denies that
//! write — bwrap then fails with `setting up uid map: Permission denied` before
//! the workload ever runs. Running Landlock *after* bwrap's setup, from this
//! shim, avoids that ordering hazard.

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;

use nacelle::system::sandbox::{SandboxPolicy, apply_sandbox, is_sandbox_supported};

/// Apply the serialized Landlock policy, then `exec` the trailing command.
///
/// `argv` is the full workload command line (program + args). This function
/// only returns on error; on success it replaces the process image via `exec`.
pub fn run(policy_path: PathBuf, argv: Vec<String>) -> anyhow::Result<()> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("sandbox-exec: missing command to execute"))?;

    // Set PR_SET_NO_NEW_PRIVS so Landlock does not require CAP_SYS_ADMIN. This
    // is inherited across the upcoming exec.
    // SAFETY: prctl with PR_SET_NO_NEW_PRIVS takes scalar args and has no
    // memory-safety implications.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        eprintln!(
            "[nacelle sandbox-exec] PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        );
    }

    if is_sandbox_supported() {
        let policy = read_policy(&policy_path)?;
        // Landlock is a supplementary, defence-in-depth layer on top of the
        // bubblewrap namespace + bind-mount isolation that is already in force.
        // If it cannot be applied, proceed namespace-isolated rather than
        // failing the workload (mirrors the previous launcher behaviour).
        if let Err(err) = apply_sandbox(&policy) {
            eprintln!(
                "[nacelle sandbox-exec] Landlock apply failed (continuing namespace-only): {err}"
            );
        }
    } else {
        eprintln!(
            "[nacelle sandbox-exec] Landlock unsupported on this kernel; running namespace-only"
        );
    }

    // Replace the process image with the workload. `exec` only returns on error.
    let err = std::process::Command::new(program).args(args).exec();
    Err(anyhow::anyhow!(
        "sandbox-exec: failed to exec {program:?}: {err}"
    ))
}

fn read_policy(path: &std::path::Path) -> anyhow::Result<SandboxPolicy> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: cannot read policy {path:?}: {e}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: invalid policy json {path:?}: {e}"))
}
