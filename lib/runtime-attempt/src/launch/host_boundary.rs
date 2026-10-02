//! Launching a `process` realization where the MACHINE is the boundary.
//!
//! ## When this applies
//!
//! Some hosts cannot create a namespace at all. A rented GPU pod is an
//! unprivileged container: `unshare` fails for every namespace type, `mount`
//! fails, and bwrap fails in every configuration, so the namespace launch in
//! [`super::sandbox`] cannot run there. What such a host does allow is
//! changing user, `no_new_privs`, and Landlock.
//!
//! ## What this is, and is not
//!
//! ```text
//! machine (the security boundary: one owner, one Run at a time, then destroyed)
//!   └─ Runner            root; holds the Runner credential
//!        └─ workload     another uid; no_new_privs; Landlock
//! ```
//!
//! The uid change, `no_new_privs` and Landlock keep a workload away from the
//! Runner's own files. They do not contain a hostile workload: there is no
//! pid, mount or network namespace, and a kernel or driver escape reaches the
//! whole machine. Safety comes from what the Coordinator guarantees about the
//! machine, which is why this mode is a distinct capability
//! ([`ISOLATION_CAPABILITY`]) and is never reported as the namespace one.
//!
//! ## Differences from the namespace launch
//!
//! Nothing can be mounted, so there is no `/app` and no guest path for state:
//! the workload sees real host paths, and finds its state through
//! `ATO_STATE_PATH_<KEY>` rather than at `mount_target`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail, ensure};
use ato_ipc::runtime_launch::StateAccessV1;
use ato_sandbox::{SandboxPolicy, filter_sensitive_paths};

use super::resolved::ResolvedRuntimeLaunchContext;
use super::sandbox::{SandboxedCommand, TOOLCHAIN_ROOT};

/// What a Runner in this mode advertises in place of the namespace capability.
pub const ISOLATION_CAPABILITY: &str = "isolation=host-boundary-v1";

/// The unprivileged user a workload runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostBoundary {
    pub uid: u32,
    pub gid: u32,
}

static ACTIVE: OnceLock<HostBoundary> = OnceLock::new();

/// Switch this process to host-boundary launches. Call once, at start.
///
/// Refuses unless the mode can actually be delivered: the Runner must be root
/// (it has to change user), the workload user must not be root, and Landlock
/// must be available. A Runner that enabled the mode on a host that cannot
/// provide it would launch workloads as itself.
pub fn enable(boundary: HostBoundary) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "host-boundary isolation is only implemented on Linux"
    );
    ensure!(
        boundary.uid != 0 && boundary.gid != 0,
        "the workload user must not be root"
    );
    ensure!(
        ato_sandbox::effective_uid() == 0,
        "host-boundary isolation needs a root Runner: it must change user before running a workload"
    );
    ensure!(
        ato_sandbox::is_sandbox_supported(),
        "host-boundary isolation needs Landlock, which this kernel does not provide"
    );
    match ACTIVE.set(boundary) {
        Ok(()) => Ok(()),
        Err(_) if ACTIVE.get() == Some(&boundary) => Ok(()),
        Err(_) => bail!("host-boundary isolation was already enabled with a different user"),
    }
}

/// The active boundary, when this process launches in host-boundary mode.
pub fn active() -> Option<HostBoundary> {
    ACTIVE.get().copied()
}

/// Device nodes a workload may open read-write. A GPU workload talks to its
/// driver through the `nvidia*` nodes; the rest are what any runtime expects.
fn writable_devices() -> Vec<PathBuf> {
    let mut devices: Vec<PathBuf> = ["null", "zero", "full", "random", "urandom", "tty", "shm"]
        .iter()
        .map(|name| Path::new("/dev").join(name))
        .collect();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        devices.extend(
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("nvidia"))
                .map(|entry| entry.path()),
        );
    }
    devices
}

/// The Landlock policy for a host-boundary launch, in HOST paths.
///
/// The workspace is read-only, as in the namespace launch. Writable: each
/// read-write state attachment and the Run's own scratch directory. Nothing
/// under the Runner's work root is listed beyond those, so the Runner's
/// credentials and other leases stay unreadable even before file permissions
/// are considered.
pub fn landlock_policy(context: &ResolvedRuntimeLaunchContext, scratch: &Path) -> SandboxPolicy {
    let mut read_write: Vec<PathBuf> = vec![scratch.to_path_buf()];
    let mut read_only: Vec<PathBuf> = vec![context.workspace_root().to_path_buf()];
    for attachment in context.state_attachments() {
        let path = attachment.working_copy_for_mount().to_path_buf();
        if attachment.access() == StateAccessV1::ReadWrite {
            read_write.push(path);
        } else {
            read_only.push(path);
        }
    }
    read_write.extend(writable_devices());
    read_only.extend(
        [
            "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt", "/dev", "/proc", "/sys",
        ]
        .iter()
        .map(PathBuf::from),
    );
    read_only.push(PathBuf::from(TOOLCHAIN_ROOT));
    let (read_write, _) = filter_sensitive_paths(&read_write);
    let (read_only, _) = filter_sensitive_paths(&read_only);
    SandboxPolicy::new()
        .allow_read_write(read_write)
        .allow_read_only(read_only)
        // No outbound TCP: the workload cannot reach the Coordinator, the
        // provider's metadata endpoints, or anything else. It may bind only
        // the ports this Run was allocated.
        .with_network(false)
        .allow_tcp_bind(
            context
                .endpoints()
                .iter()
                .map(|endpoint| endpoint.host_port),
        )
}

/// Give the workload user what it must own: the scratch directory and every
/// read-write state working copy. Everything else stays the Runner's.
pub fn prepare_ownership(
    context: &ResolvedRuntimeLaunchContext,
    scratch: &Path,
    boundary: HostBoundary,
) -> Result<()> {
    std::fs::create_dir_all(scratch).context("failed to create the workload scratch directory")?;
    chown_tree(scratch, boundary)?;
    for attachment in context.state_attachments() {
        if attachment.access() == StateAccessV1::ReadWrite {
            chown_tree(attachment.working_copy_for_mount(), boundary)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn chown_tree(root: &Path, boundary: HostBoundary) -> Result<()> {
    let metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("cannot inspect {}", root.display()))?;
    // Never follow a link out of the tree being handed over.
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    std::os::unix::fs::chown(root, Some(boundary.uid), Some(boundary.gid))
        .with_context(|| format!("cannot hand {} to the workload user", root.display()))?;
    if metadata.is_dir() {
        for entry in
            std::fs::read_dir(root).with_context(|| format!("cannot list {}", root.display()))?
        {
            chown_tree(&entry?.path(), boundary)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_tree(_root: &Path, _boundary: HostBoundary) -> Result<()> {
    bail!("host-boundary isolation is only implemented on Unix")
}

/// The argv that runs `workload_argv` as the workload user under Landlock.
///
/// The Runner's own binary is re-entered as `sandbox-exec`, exactly as in the
/// namespace launch; here the shim also changes user, because there is no
/// bwrap to have done it.
pub fn host_boundary_command(
    context: &ResolvedRuntimeLaunchContext,
    workload_argv: &[String],
    shim: &Path,
    policy_host_path: &Path,
    scratch: &Path,
    boundary: HostBoundary,
) -> Result<SandboxedCommand> {
    ensure!(!workload_argv.is_empty(), "workload argv is empty");
    let utf8 = |path: &Path, what: &str| -> Result<String> {
        Ok(path
            .to_str()
            .with_context(|| format!("{what} path is not valid UTF-8"))?
            .to_owned())
    };
    let mut argv = vec![
        utf8(shim, "shim")?,
        "sandbox-exec".to_owned(),
        "--policy".to_owned(),
        utf8(policy_host_path, "policy")?,
        "--drop-uid".to_owned(),
        boundary.uid.to_string(),
        "--drop-gid".to_owned(),
        boundary.gid.to_string(),
        "--chdir".to_owned(),
        utf8(context.effective_cwd(), "workload cwd")?,
        "--".to_owned(),
    ];
    argv.extend(workload_argv.iter().cloned());
    Ok(SandboxedCommand {
        argv,
        policy: landlock_policy(context, scratch),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::resolved::{ResolvedStateAttachment, allocate_endpoint};
    use ato_ipc::runtime_launch::{EndpointAllocationV1, EndpointV1};
    use std::collections::BTreeMap;

    fn context(
        workspace: &Path,
        state: &Path,
        access: StateAccessV1,
    ) -> ResolvedRuntimeLaunchContext {
        ResolvedRuntimeLaunchContext::new(
            workspace.to_path_buf(),
            "",
            BTreeMap::new(),
            Vec::new(),
            vec![ResolvedStateAttachment::new(
                "data".to_owned(),
                None,
                state.to_path_buf(),
                "/data".to_owned(),
                access,
            )],
            vec![allocate_endpoint(
                &EndpointV1 {
                    name: "http".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8080),
                    allocation: EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                18420,
            )],
        )
        .unwrap()
    }

    #[test]
    fn the_command_reenters_the_shim_drops_the_user_and_runs_the_workload() {
        let workspace = tempfile::tempdir().unwrap();
        let state = workspace.path().join(".ato/state/data");
        std::fs::create_dir_all(&state).unwrap();
        let context = context(workspace.path(), &state, StateAccessV1::ReadWrite);
        let command = host_boundary_command(
            &context,
            &[
                "/usr/bin/python3".to_owned(),
                "-c".to_owned(),
                "pass".to_owned(),
            ],
            Path::new("/opt/ato/bin/runner"),
            Path::new("/work/policy.json"),
            Path::new("/work/scratch"),
            HostBoundary {
                uid: 20001,
                gid: 20001,
            },
        )
        .unwrap();
        let workspace_text = workspace.path().to_str().unwrap();
        assert_eq!(
            command.argv,
            [
                "/opt/ato/bin/runner",
                "sandbox-exec",
                "--policy",
                "/work/policy.json",
                "--drop-uid",
                "20001",
                "--drop-gid",
                "20001",
                "--chdir",
                workspace_text,
                "--",
                "/usr/bin/python3",
                "-c",
                "pass",
            ]
        );
        // No bwrap: this host cannot run it.
        assert!(!command.argv.iter().any(|argument| argument == "bwrap"));
    }

    #[test]
    fn the_policy_is_read_only_for_the_workspace_and_writable_only_where_declared() {
        let workspace = tempfile::tempdir().unwrap();
        let state = workspace.path().join(".ato/state/data");
        let scratch = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let writable = landlock_policy(
            &context(workspace.path(), &state, StateAccessV1::ReadWrite),
            scratch.path(),
        );
        assert!(writable.read_write_paths.contains(&state));
        assert!(
            writable
                .read_write_paths
                .contains(&scratch.path().to_path_buf())
        );
        assert!(
            writable
                .read_only_paths
                .contains(&workspace.path().to_path_buf())
        );
        assert!(
            !writable
                .read_write_paths
                .contains(&workspace.path().to_path_buf())
        );

        let read_only = landlock_policy(
            &context(workspace.path(), &state, StateAccessV1::ReadOnly),
            scratch.path(),
        );
        assert!(!read_only.read_write_paths.contains(&state));
        assert!(read_only.read_only_paths.contains(&state));
    }

    #[test]
    fn the_policy_denies_outbound_tcp_and_allows_only_the_allocated_port() {
        let workspace = tempfile::tempdir().unwrap();
        let state = workspace.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let policy = landlock_policy(
            &context(workspace.path(), &state, StateAccessV1::ReadWrite),
            Path::new("/work/scratch"),
        );
        assert!(!policy.allow_network);
        assert_eq!(policy.allowed_bind_tcp_ports, vec![18420]);
        assert!(policy.allowed_connect_tcp_ports.is_empty());
    }

    #[test]
    fn a_root_workload_user_is_refused() {
        assert!(enable(HostBoundary { uid: 0, gid: 20001 }).is_err());
        assert!(enable(HostBoundary { uid: 20001, gid: 0 }).is_err());
    }
}
