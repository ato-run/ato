//! The isolation a Formation build runs under (ADR-018).
//!
//! ## A different threat model from the runtime sandbox
//!
//! P3's runtime sandbox confines a workload an owner already chose to run.
//! This confines code chosen by whoever submitted a repository: `uv sync` runs
//! arbitrary build-backend hooks, a `setup.py` runs at install time, and a
//! postinstall script is ordinary practice. The substrate is therefore chosen
//! on the assumption that **the build is the attacker**.
//!
//! The two policies share a crate (`ato-sandbox`) and must not share settings.
//! A runtime workload is allowed the network; a build is not, unless its plan
//! declared it needs one and its policy permits it.
//!
//! ## What this refuses to do
//!
//! Run unconfined. A worker that cannot contain a build refuses the job, and
//! says so, rather than producing an artifact nobody can vouch for.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use ato_sandbox::{SandboxPolicy, filter_sensitive_paths, sensitive_paths};

/// Where the source appears inside the build sandbox. Read-only.
pub const GUEST_SOURCE_ROOT: &str = "/src";
/// Where the workspace is assembled. The ONLY writable declared path besides
/// the cache and `/tmp`.
pub const GUEST_WORKSPACE_ROOT: &str = "/app";
/// Where a dependency cache may live, when one is allowed.
pub const GUEST_CACHE_ROOT: &str = "/cache";

/// What the build may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// `--unshare-net`. Real isolation, and the only policy this build can
    /// currently ENFORCE for an untrusted source.
    Denied,
    /// The host's network, shared. Honest about what it is: bubblewrap cannot
    /// express "the package index and nothing else", so this is unrestricted
    /// egress and is confined to trusted sources by policy above, not here.
    DependencyResolution,
}

impl NetworkPolicy {
    /// The exact string recorded in provenance, so a later reader can tell
    /// whether an artifact was built under isolation or not.
    pub fn provenance(self) -> &'static str {
        match self {
            Self::Denied => "bubblewrap+landlock;network=denied",
            Self::DependencyResolution => {
                "bubblewrap+landlock;network=host-unrestricted;trusted-only"
            }
        }
    }
}

/// Resource ceilings. Enforced, not advisory.
#[derive(Debug, Clone, Copy)]
pub struct BuildLimits {
    pub wall_clock_seconds: u64,
    pub max_processes: u64,
    pub max_output_bytes: u64,
}

impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            wall_clock_seconds: 15 * 60,
            max_processes: 512,
            max_output_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// A build step, lowered onto the host.
#[derive(Debug, Clone)]
pub struct SandboxedBuildCommand {
    pub argv: Vec<String>,
    pub policy: SandboxPolicy,
    pub network: NetworkPolicy,
}

/// Build the bwrap argv for one build step.
///
/// Mirrors the runtime sandbox's shape deliberately — the same namespaces, the
/// same sensitive-path tmpfs overlay, the same Landlock shim — and differs in
/// exactly the places a build must differ:
///
/// ```text
/// source     --ro-bind-->  /src      the build may read it, never write it
/// workspace  --bind----->  /app      the only place output may appear
/// cache      --bind----->  /cache    when a network policy allows one
/// ```
///
/// The source is read-only because a build that edits its own source produces
/// an artifact whose closure ref no longer describes it.
pub struct BuildSandbox<'a> {
    /// Read-only inside the sandbox.
    pub source_root: &'a Path,
    /// The only place output may appear.
    pub workspace_root: &'a Path,
    /// Present only when the policy allows a network to fill it.
    pub cache_root: Option<&'a Path>,
    /// This worker's own binary, re-entered as the Landlock shim.
    pub shim: &'a Path,
    pub policy_host_path: &'a Path,
    pub network: NetworkPolicy,
    pub limits: BuildLimits,
}

pub fn sandboxed_build_command(
    workload_argv: &[String],
    sandbox: &BuildSandbox<'_>,
) -> Result<SandboxedBuildCommand> {
    let BuildSandbox {
        source_root,
        workspace_root,
        cache_root,
        shim,
        policy_host_path,
        network,
        limits,
    } = *sandbox;
    ensure!(!workload_argv.is_empty(), "build step has no argv");
    require_containment()?;

    let mut argv: Vec<String> = vec![
        "bwrap".to_owned(),
        "--unshare-all".to_owned(),
        "--die-with-parent".to_owned(),
        "--new-session".to_owned(),
    ];
    if network == NetworkPolicy::DependencyResolution {
        argv.push("--share-net".to_owned());
    }

    for flag in ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"] {
        argv.push(flag.to_owned());
    }
    for [flag, source, target] in [
        ["--ro-bind-try", "/lib", "/lib"],
        ["--ro-bind-try", "/lib64", "/lib64"],
        ["--ro-bind", "/usr", "/usr"],
        ["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"],
        ["--ro-bind-try", "/etc/hosts", "/etc/hosts"],
        ["--ro-bind-try", "/etc/ssl", "/etc/ssl"],
    ] {
        argv.extend([flag.to_owned(), source.to_owned(), target.to_owned()]);
    }

    // Every credential directory becomes an empty tmpfs. `--unshare-all` plus
    // explicit binds already means they are absent; this makes a future
    // accidental bind harmless, and it is cheap.
    for sensitive in sensitive_paths() {
        argv.extend([
            "--tmpfs".to_owned(),
            sensitive.to_string_lossy().into_owned(),
        ]);
    }

    let source = path_str(source_root, "source root")?;
    let workspace = path_str(workspace_root, "workspace root")?;
    argv.extend(["--ro-bind".to_owned(), source, GUEST_SOURCE_ROOT.to_owned()]);
    argv.extend([
        "--bind".to_owned(),
        workspace,
        GUEST_WORKSPACE_ROOT.to_owned(),
    ]);
    if let Some(cache) = cache_root {
        argv.extend([
            "--bind".to_owned(),
            path_str(cache, "cache root")?,
            GUEST_CACHE_ROOT.to_owned(),
        ]);
    }

    argv.extend([
        "--ro-bind".to_owned(),
        path_str(shim, "shim")?,
        "/.ato/formation".to_owned(),
    ]);
    argv.extend([
        "--ro-bind".to_owned(),
        path_str(policy_host_path, "policy")?,
        "/.ato/build-policy.json".to_owned(),
    ]);

    // `--clearenv` then a strict allowlist. A build inherits nothing: an
    // ambient token in the worker's environment is exactly what an untrusted
    // build would go looking for.
    argv.push("--clearenv".to_owned());
    for (name, value) in build_environment(network) {
        argv.extend(["--setenv".to_owned(), name, value]);
    }

    argv.extend(["--chdir".to_owned(), GUEST_WORKSPACE_ROOT.to_owned()]);
    argv.extend([
        "/.ato/formation".to_owned(),
        "sandbox-exec".to_owned(),
        "--policy".to_owned(),
        "/.ato/build-policy.json".to_owned(),
        "--max-processes".to_owned(),
        limits.max_processes.to_string(),
        "--".to_owned(),
    ]);
    argv.extend(workload_argv.iter().cloned());

    Ok(SandboxedBuildCommand {
        argv,
        policy: landlock_policy(cache_root.is_some()),
        network,
    })
}

/// The env a build sees. Everything else is cleared.
fn build_environment(network: NetworkPolicy) -> Vec<(String, String)> {
    let mut env = vec![
        ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), GUEST_WORKSPACE_ROOT.to_owned()),
        ("TMPDIR".to_owned(), "/tmp".to_owned()),
        // Byte-code writing off: `/src` is read-only, and CPython dies trying
        // to create `__pycache__` beside a module it imported from there.
        ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
        // A build must not pick up a user site-packages that is not part of
        // its declared dependencies.
        ("PYTHONNOUSERSITE".to_owned(), "1".to_owned()),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
    ];
    if network == NetworkPolicy::DependencyResolution {
        env.push(("UV_CACHE_DIR".to_owned(), GUEST_CACHE_ROOT.to_owned()));
        env.push(("PIP_CACHE_DIR".to_owned(), GUEST_CACHE_ROOT.to_owned()));
        // Never prompt: a build that blocks on input is a build that burns its
        // whole timeout and reports nothing useful.
        env.push(("PIP_NO_INPUT".to_owned(), "1".to_owned()));
    }
    env
}

/// The Landlock policy the shim applies, in GUEST paths.
fn landlock_policy(with_cache: bool) -> SandboxPolicy {
    let mut writable = vec![PathBuf::from(GUEST_WORKSPACE_ROOT), PathBuf::from("/tmp")];
    if with_cache {
        writable.push(PathBuf::from(GUEST_CACHE_ROOT));
    }
    let (read_write, _) = filter_sensitive_paths(&writable);
    let (read_only, _) = filter_sensitive_paths(&[
        PathBuf::from(GUEST_SOURCE_ROOT),
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/etc"),
    ]);
    SandboxPolicy::new()
        .allow_read_write(read_write)
        .allow_read_only(read_only)
}

fn path_str(path: &Path, what: &str) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{what} path is not valid UTF-8"))
}

fn which_bwrap() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("bwrap"))
            .find(|candidate| candidate.is_file())
    })
}

pub fn containment_available() -> bool {
    which_bwrap().is_some()
}

/// Refuse rather than degrade.
pub fn require_containment() -> Result<()> {
    if containment_available() {
        return Ok(());
    }
    bail!(
        "this Formation worker cannot contain a build: `bwrap` is not on PATH. Refusing to run \
         submitted code unconfined."
    )
}
