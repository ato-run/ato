//! Bubblewrap containment for a `process` realization.
//!
//! ## What this buys, and what it does not
//!
//! A bare `ProcessAdapter` spawn shares the Runner's whole filesystem with the
//! workload. On a multi-tenant Runner that is not a launch, it is an
//! invitation. Here the workload gets a mount namespace containing exactly the
//! paths the spec named and nothing else, so "cannot read the host" is a
//! property of the namespace rather than of the workload behaving.
//!
//! The layering is taken from nacelle's Linux source launcher, part by part:
//! `--unshare-all`, a `--proc`/`--dev`/`--tmpfs /tmp` skeleton, read-only
//! system binds, `--tmpfs` over sensitive host paths, and Landlock applied by
//! an in-sandbox shim rather than to the bwrap wrapper. What did NOT come
//! across is nacelle's language and runtime inference: argv comes from
//! `RuntimeLaunchSpecV1` and from nowhere else.
//!
//! ## Mount semantics, shared with the future OCI realization
//!
//! ```text
//! workspace working copy  --ro-bind-->  /app
//! state working copy      --bind----->  attachment.mount_target   (e.g. /data)
//! ```
//!
//! This is why `ATO_STATE_PATH_<KEY>` stops being the ABI. A ComputeSchema can
//! now declare `DATABASE_PATH=/data/app.sqlite` and mean it, under both
//! realizations, because `mount_target` is a real path in both. The env var is
//! still exported as a convenience, but nothing has to read it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use ato_ipc::runtime_launch::StateAccessV1;
use ato_sandbox::{SandboxPolicy, filter_sensitive_paths, sensitive_paths};

use super::resolved::ResolvedRuntimeLaunchContext;

/// Where the workspace appears inside the sandbox.
pub const GUEST_APP_ROOT: &str = "/app";
/// Where the Runner binary is bound so it can act as the Landlock shim.
const GUEST_SHIM: &str = "/.ato/runner";
/// Where provisioned toolchains live, at the SAME absolute path Formation
/// built against.
///
/// A workspace's venv is a set of symlinks into its interpreter, because a
/// python-build-standalone build links against a shared libpython and copying
/// only the executable produces a venv that cannot start. The symlinks resolve
/// here or nowhere, which is why this path is fixed on both sides rather than
/// temporary on either.
pub const TOOLCHAIN_ROOT: &str = "/opt/ato/toolchains";
/// Where the serialized Landlock policy is bound.
const GUEST_POLICY: &str = "/.ato/sandbox-policy.json";

/// A mount the workload will see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMount {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

/// The bwrap invocation for one launch, as an argv.
///
/// Returned rather than executed so the whole containment decision is testable
/// on any host, including one without bwrap.
#[derive(Debug, Clone)]
pub struct SandboxedCommand {
    pub argv: Vec<String>,
    pub policy: SandboxPolicy,
}

/// Read-only host paths every workload needs to be able to exec at all.
///
/// `--ro-bind-try` for the optional ones: a strict bind against a missing
/// source makes bwrap abort before the workload ever runs, and a path that
/// does not exist cannot be exposed, so skipping it weakens nothing.
fn system_read_only_binds() -> [[&'static str; 3]; 8] {
    [
        // `/bin` and `/sbin` are usrmerge symlinks on most modern
        // distributions; binding only `/usr` leaves them dangling and a
        // `#!/bin/sh` fails with "No such file or directory".
        ["--ro-bind-try", "/bin", "/bin"],
        ["--ro-bind-try", "/sbin", "/sbin"],
        ["--ro-bind-try", "/lib", "/lib"],
        ["--ro-bind-try", "/lib64", "/lib64"],
        ["--ro-bind", "/usr", "/usr"],
        ["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"],
        ["--ro-bind-try", "/etc/hosts", "/etc/hosts"],
        ["--ro-bind-try", "/etc/ssl", "/etc/ssl"],
    ]
}

/// The mounts a launch requires, derived from the resolved context.
///
/// The workspace is read-only. A workload that writes to its own source tree
/// is writing to something the next Run rebuilds from the schema, so allowing
/// it would only produce data that silently disappears.
pub fn guest_mounts(context: &ResolvedRuntimeLaunchContext) -> Result<Vec<GuestMount>> {
    let mut mounts = vec![GuestMount {
        host_path: context.workspace_root().to_path_buf(),
        guest_path: GUEST_APP_ROOT.to_owned(),
        writable: false,
    }];
    for attachment in context.state_attachments() {
        let target = attachment.guest_target();
        ensure!(
            target.starts_with('/') && !target.contains("/../") && !target.ends_with("/.."),
            "state attachment mount target `{target}` is not an absolute, traversal-free path"
        );
        ensure!(
            target != GUEST_APP_ROOT,
            "state attachment would shadow the workspace at {GUEST_APP_ROOT}"
        );
        mounts.push(GuestMount {
            host_path: attachment.working_copy_for_mount().to_path_buf(),
            guest_path: target.to_owned(),
            writable: attachment.access() == StateAccessV1::ReadWrite,
        });
    }
    Ok(mounts)
}

/// The Landlock policy the in-sandbox shim will apply.
///
/// Expressed in GUEST paths, because that is the namespace the shim runs in.
/// Sensitive host paths are filtered out of the allow-lists by the shared
/// crate rather than by a list maintained here.
pub fn landlock_policy(mounts: &[GuestMount], bind_tcp_ports: &[u16]) -> SandboxPolicy {
    let (read_write, _) = filter_sensitive_paths(
        &mounts
            .iter()
            .filter(|mount| mount.writable)
            .map(|mount| PathBuf::from(&mount.guest_path))
            .chain([PathBuf::from("/tmp")])
            .collect::<Vec<_>>(),
    );
    let (read_only, _) = filter_sensitive_paths(
        &mounts
            .iter()
            .filter(|mount| !mount.writable)
            .map(|mount| PathBuf::from(&mount.guest_path))
            .chain([
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                PathBuf::from("/etc"),
                PathBuf::from(TOOLCHAIN_ROOT),
                // bwrap provides these with --proc and --dev rather than a
                // bind. Without /dev, CPython cannot open /dev/urandom and dies
                // during pre-initialization.
                PathBuf::from("/dev"),
                PathBuf::from("/proc"),
            ])
            .collect::<Vec<_>>(),
    );
    SandboxPolicy::new()
        .allow_read_write(read_write)
        .allow_read_only(read_only)
        .with_network(false)
        .allow_tcp_bind(bind_tcp_ports.iter().copied())
}

/// Build the bwrap argv that runs `argv` under containment.
///
/// `shim` is the Runner's own executable, bound in read-only and re-entered as
/// `sandbox-exec`. Landlock has to be applied by the process that execs the
/// workload — `restrict_self` survives `exec` — and applying it to bwrap
/// instead breaks bwrap's own uid-map write. Hence the shim, which is nacelle's
/// solution to the same ordering hazard.
pub fn sandboxed_command(
    context: &ResolvedRuntimeLaunchContext,
    workload_argv: &[String],
    shim: &Path,
    policy_host_path: &Path,
    share_network: bool,
) -> Result<SandboxedCommand> {
    ensure!(!workload_argv.is_empty(), "workload argv is empty");
    let mounts = guest_mounts(context)?;

    let mut argv: Vec<String> = vec!["bwrap".to_owned(), "--unshare-all".to_owned()];
    if share_network {
        // An App that serves HTTP needs a network namespace it can be reached
        // in. Sharing the host's is what the current Runner topology gives it;
        // narrowing that is a separate concern from filesystem containment and
        // is NOT solved here.
        argv.push("--share-net".to_owned());
    }
    // The workload must not outlive the Runner that is supervising it.
    argv.push("--die-with-parent".to_owned());
    argv.push("--new-session".to_owned());

    for flag in ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"] {
        argv.push(flag.to_owned());
    }
    for [flag, source, target] in system_read_only_binds() {
        argv.extend([flag.to_owned(), source.to_owned(), target.to_owned()]);
    }

    // Overlay every sensitive host path with an empty tmpfs. Belt and braces:
    // `--unshare-all` plus explicit binds already means these are not present,
    // and this makes a future accidental bind harmless.
    for sensitive in sensitive_paths() {
        argv.extend([
            "--tmpfs".to_owned(),
            sensitive.to_string_lossy().into_owned(),
        ]);
    }

    // The toolchain root, read-only. A Run may use an interpreter; it may not
    // change one, and a workload that could would change it for every other
    // tenant on the host.
    if std::path::Path::new(TOOLCHAIN_ROOT).is_dir() {
        argv.extend([
            "--ro-bind".to_owned(),
            TOOLCHAIN_ROOT.to_owned(),
            TOOLCHAIN_ROOT.to_owned(),
        ]);
    }

    for mount in &mounts {
        let host = mount
            .host_path
            .to_str()
            .context("mount host path is not valid UTF-8")?;
        argv.extend([
            if mount.writable {
                "--bind".to_owned()
            } else {
                "--ro-bind".to_owned()
            },
            host.to_owned(),
            mount.guest_path.clone(),
        ]);
    }

    let shim_host = shim.to_str().context("shim path is not valid UTF-8")?;
    let policy_host = policy_host_path
        .to_str()
        .context("policy path is not valid UTF-8")?;
    argv.extend([
        "--ro-bind".to_owned(),
        shim_host.to_owned(),
        GUEST_SHIM.to_owned(),
    ]);
    argv.extend([
        "--ro-bind".to_owned(),
        policy_host.to_owned(),
        GUEST_POLICY.to_owned(),
    ]);

    argv.extend(["--chdir".to_owned(), guest_cwd(context)?]);

    argv.extend([
        GUEST_SHIM.to_owned(),
        "sandbox-exec".to_owned(),
        "--policy".to_owned(),
        GUEST_POLICY.to_owned(),
        "--".to_owned(),
    ]);
    argv.extend(workload_argv.iter().cloned());

    Ok(SandboxedCommand {
        argv,
        policy: landlock_policy(
            &mounts,
            &context
                .endpoints()
                .iter()
                .map(|endpoint| endpoint.host_port)
                .collect::<Vec<_>>(),
        ),
    })
}

/// Where the workload starts, as a GUEST path.
///
/// The resolved context already placed `effective_cwd` inside the workspace
/// (lexically, and against the real paths when they exist). This re-derives
/// the relative remainder and mounts it under [`GUEST_APP_ROOT`], so
/// `cwd_relative = "apps/web"` starts the workload at `/app/apps/web` and no
/// host path reaches the guest.
pub fn guest_cwd(context: &ResolvedRuntimeLaunchContext) -> Result<String> {
    let relative = context
        .effective_cwd()
        .strip_prefix(context.workspace_root())
        .context("resolved cwd escaped the workspace after resolution")?;
    let mut guest = GUEST_APP_ROOT.to_owned();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(segment) => {
                guest.push('/');
                guest.push_str(
                    segment
                        .to_str()
                        .context("workload cwd is not valid UTF-8")?,
                );
            }
            other => bail!("workload cwd has a non-normal component: {other:?}"),
        }
    }
    Ok(guest)
}

/// The variable naming the host port the Runner actually allocated for an
/// endpoint.
///
/// **Process realization ABI.** A process realization shares the host network
/// namespace, so there is no guest->host NAT to translate a fixed guest port:
/// the port the workload binds IS a real host port, and only the Runner knows
/// which one was free. A workload that hardcoded 8000 would collide with the
/// next tenant, so the spec's endpoint name is lowered to a variable instead:
///
/// ```text
/// endpoint "http"  ->  Runner allocates a free host port
///                  ->  ATO_ENDPOINT_HTTP_PORT=<that port>
///                  ->  workload binds it; readiness probes the same port
/// ```
///
/// This is ABI, not wire schema — an OCI realization has real port mapping and
/// will not need it. B1 can adapt an OSS project that expects `PORT` at
/// Formation time rather than teaching the Runner about it.
pub fn endpoint_port_env_name(endpoint_name: &str) -> String {
    format!(
        "ATO_ENDPOINT_{}_PORT",
        endpoint_name
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

/// Environment the workload sees, in GUEST terms.
///
/// The resolved context's state paths are HOST paths; inside the sandbox the
/// same state is at `mount_target`. Exporting the host path here would hand
/// the workload a path that does not exist in its namespace and, worse, would
/// leak the Runner's directory layout into a user-visible variable.
pub fn guest_environment(context: &ResolvedRuntimeLaunchContext) -> BTreeMap<String, String> {
    let mut environment = context.environment_for_spawn();
    for attachment in context.state_attachments() {
        environment.insert(
            super::process_executor::state_path_env_name(attachment.state_key()),
            attachment.guest_target().to_owned(),
        );
    }
    for endpoint in context.endpoints() {
        environment.insert(
            endpoint_port_env_name(&endpoint.name),
            endpoint.host_port.to_string(),
        );
    }
    environment
}

/// Whether this host can contain a workload at all.
///
/// Checked before a launch rather than after: a Runner that cannot contain a
/// workload must refuse the Run, not run it unconfined and report success.
pub fn containment_available() -> bool {
    ato_sandbox::bubblewrap_containment_available()
}

/// Refuse rather than silently degrade.
pub fn require_containment() -> Result<()> {
    if containment_available() {
        return Ok(());
    }
    bail!(
        "this Runner cannot contain a process workload: bubblewrap is unavailable or the host \
         rejects the required namespaces. Refusing to launch unconfined on a multi-tenant host."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(root: &Path, cwd: &str) -> Result<ResolvedRuntimeLaunchContext, String> {
        ResolvedRuntimeLaunchContext::new(
            root.to_path_buf(),
            cwd,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn the_workload_starts_at_its_cwd_under_the_guest_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("apps/web")).unwrap();
        std::fs::create_dir_all(root.path().join("sub")).unwrap();
        for (cwd, guest) in [
            ("", "/app"),
            ("sub", "/app/sub"),
            ("apps/web", "/app/apps/web"),
        ] {
            let resolved = context(root.path(), cwd).unwrap();
            assert_eq!(guest_cwd(&resolved).unwrap(), guest, "{cwd:?}");
        }
    }

    #[test]
    fn the_sandbox_changes_into_the_guest_cwd() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("server")).unwrap();
        let resolved = context(root.path(), "server").unwrap();
        let command = sandboxed_command(
            &resolved,
            &["python3".to_owned(), "app.py".to_owned()],
            Path::new("/bin/true"),
            Path::new("/tmp/policy.json"),
            true,
        )
        .unwrap();
        let chdir = command
            .argv
            .iter()
            .position(|arg| arg == "--chdir")
            .expect("--chdir");
        assert_eq!(command.argv[chdir + 1], "/app/server");
        // No host path is handed to the guest as its cwd.
        assert!(!command.argv[chdir + 1].contains(&*root.path().to_string_lossy()));
    }

    #[test]
    fn an_escaping_cwd_is_refused_before_it_reaches_the_sandbox() {
        let root = tempfile::tempdir().unwrap();
        for cwd in ["../escape", "sub/../../escape", "/etc", "./sub"] {
            assert!(context(root.path(), cwd).is_err(), "{cwd:?} was accepted");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_cwd_that_leaves_the_workspace_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(context(root.path(), "link").is_err());
    }
}
