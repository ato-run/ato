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
    run_with(policy_path, argv, &[], None)
}

/// The host-boundary shim: become the workload user, then restrict and exec.
///
/// There is no bwrap in front of this call, so nothing has changed user and
/// nothing isolates the workload but what happens here. Every step is
/// therefore required: a failure to change user, to set `no_new_privs`, or to
/// apply Landlock fully is a refusal, never a degraded launch. (The namespace
/// shim tolerates a missing Landlock because bwrap already contains the
/// workload; here it is the containment.)
pub fn run_host_boundary(
    policy_path: &Path,
    argv: &[String],
    uid: u32,
    gid: u32,
    workload_cwd: &Path,
) -> Result<()> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| anyhow!("sandbox-exec: no workload to execute"))?;
    // Read the policy while still able to: it lives in the Runner's files.
    let policy = read_policy(policy_path)?;
    ato_sandbox::drop_to_user(uid, gid)
        .context("sandbox-exec: could not become the workload user")?;
    ato_sandbox::set_no_new_privs()
        .context("sandbox-exec: PR_SET_NO_NEW_PRIVS is required in host-boundary mode")?;
    let applied = apply_sandbox(&policy)
        .context("sandbox-exec: Landlock is required in host-boundary mode")?;
    if !applied.fully_enforced {
        return Err(anyhow!(
            "sandbox-exec: Landlock is not fully enforced: {}",
            applied.message
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(program)
            .args(arguments)
            .current_dir(workload_cwd)
            .exec();
        Err(anyhow!("sandbox-exec: failed to exec {program}: {error}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, arguments, workload_cwd);
        Err(anyhow!("sandbox-exec is only available on Unix"))
    }
}

/// [`run`], with environment and a working directory for the WORKLOAD only.
///
/// Both are applied by the `exec` itself, after the policy is in force: this
/// process never has `workload_env` in its own environment and never runs
/// from `workload_cwd`, so a variable such as `LD_PRELOAD` or `PATH` reaches
/// what the workload runs and never the shim that restricts it.
pub fn run_with(
    policy_path: &Path,
    argv: &[String],
    workload_env: &[(String, String)],
    workload_cwd: Option<&Path>,
) -> Result<()> {
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
        match apply_sandbox(&policy) {
            Ok(applied) if !policy.allow_network && !applied.fully_enforced => {
                return Err(anyhow!(
                    "sandbox-exec: network isolation is not fully enforced: {}",
                    applied.message
                ));
            }
            Ok(_) => {}
            Err(error) if !policy.allow_network => {
                return Err(error)
                    .context("sandbox-exec: required network isolation could not be applied");
            }
            Err(error) => {
                eprintln!("[sandbox-exec] Landlock not applied (namespace-only): {error}");
            }
        }
    } else {
        eprintln!("[sandbox-exec] Landlock unsupported on this kernel; namespace-only");
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let mut command = std::process::Command::new(program);
        command
            .args(arguments)
            .envs(workload_env.iter().map(|(name, value)| (name, value)));
        if let Some(cwd) = workload_cwd {
            command.current_dir(cwd);
        }
        let error = command.exec();
        Err(anyhow!("sandbox-exec: failed to exec {program}: {error}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, arguments, workload_env, workload_cwd);
        Err(anyhow!("sandbox-exec is only available on Unix"))
    }
}

fn read_policy(path: &Path) -> Result<SandboxPolicy> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("sandbox-exec: cannot read policy {}", path.display()))?;
    serde_json::from_slice(&bytes).context("sandbox-exec: policy is malformed")
}
