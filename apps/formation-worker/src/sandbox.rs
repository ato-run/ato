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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
pub use ato_formation::intent::ToolchainAccess;
use ato_sandbox::{SandboxPolicy, filter_sensitive_paths, sensitive_paths};

/// Where the source appears inside the build sandbox. Read-only.
pub const GUEST_SOURCE_ROOT: &str = "/src";
/// Where the workspace is assembled. The ONLY writable declared path besides
/// the cache and `/tmp`.
pub const GUEST_WORKSPACE_ROOT: &str = "/app";
/// Where a dependency cache may live, when one is allowed.
pub const GUEST_CACHE_ROOT: &str = "/cache";
/// Where provisioned toolchains live — the same path at build time and at
/// runtime, because a venv records the absolute path of the interpreter that
/// made it.
pub const TOOLCHAIN_ROOT: &str = "/opt/ato/toolchains";

/// Where platform-managed build assets live: the compilers and runtimes Ato
/// owns, shipped with the builder rather than fetched by a build.
///
/// Re-exported from `ato_formation` so the bind list and the build plan cannot
/// disagree about the path — which is the drift that produced "Permission
/// denied" on a demonstrably-present directory once already.
pub use ato_formation::intent::BUILD_ASSET_ROOT;

/// System paths a build may read and execute.
///
/// ONE list, used for both the bind mounts and the Landlock policy. They
/// describe the same world and drifted apart once already: the policy omitted
/// `/bin`, Landlock denied exec of `/bin/sh`, and the failure read as
/// "Permission denied" on a path the mount namespace had faithfully provided.
const SYSTEM_READ_ONLY: &[&str] = &[
    // `/bin` and `/sbin` are usrmerge symlinks on most modern distributions,
    // and binding only `/usr` leaves them dangling.
    "/bin", "/sbin", "/lib", "/lib64", "/usr",
];

/// Configuration files a build needs, bound INDIVIDUALLY.
///
/// Not `/etc` wholesale, which is measured rather than stylistic: on a
/// systemd-resolved host `/etc/resolv.conf` is a symlink into `/run`, and
/// bind-mounting the directory carries the link without its target — DNS then
/// fails inside the sandbox with "Temporary failure in name resolution" while
/// `/etc` is demonstrably present. Binding the file lets bwrap resolve it.
///
/// Binding less of `/etc` is also simply better: a build has no business
/// reading the host's user database or its service configuration.
const SYSTEM_CONFIG_FILES: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
];

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
    /// Where the host writes the Landlock policy before each step. Must be
    /// outside every path the build can write: a step that could replace it
    /// with a link would redirect the host's next write.
    pub policy_host_path: &'a Path,
    pub network: NetworkPolicy,
    pub limits: BuildLimits,
    /// Read-only unless this step is the platform provisioning a toolchain.
    pub toolchain: ToolchainAccess,
}

pub fn sandboxed_build_command(
    workload_argv: &[String],
    sandbox: &BuildSandbox<'_>,
) -> Result<SandboxedBuildCommand> {
    sandboxed_build_step_command(workload_argv, None, &BTreeMap::new(), &[], sandbox)
}

/// [`sandboxed_build_command`] for a step with its own guest cwd and env.
///
/// Neither is given to bubblewrap. The sandbox starts from `--clearenv` and
/// its own fixed environment in `/app`, applies its restrictions, and only
/// then execs the workload with `workload_env` added and in `guest_cwd` — so
/// an authored `LD_PRELOAD`, `PATH` or `PYTHONPATH` reaches the workload and
/// never the shim that restricts it. With neither, the command is exactly
/// what it was before steps had them.
///
/// `toolchain_path` — the plan's provisioned toolchains' `bin` directories —
/// heads the sandbox's own PATH, so a shell a step starts finds the declared
/// `node` / `npm` / `pnpm` rather than anything on the host.
pub fn sandboxed_build_step_command(
    workload_argv: &[String],
    guest_cwd: Option<&str>,
    workload_env: &BTreeMap<String, String>,
    toolchain_path: &[String],
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
        toolchain,
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
    for path in SYSTEM_READ_ONLY.iter().chain(SYSTEM_CONFIG_FILES) {
        // `--ro-bind-try`: a strict bind against a missing source aborts bwrap
        // before the build runs, and a path that does not exist cannot be
        // exposed, so skipping it weakens nothing.
        argv.extend([
            "--ro-bind-try".to_owned(),
            (*path).to_owned(),
            (*path).to_owned(),
        ]);
    }
    // Toolchains are provisioned into a shared root and reused across builds
    // and attempts. Only the platform's provisioning step may write it; a step
    // running source-controlled or authored code could otherwise replace an
    // interpreter every later build and attempt executes.
    argv.extend([
        match toolchain {
            ToolchainAccess::Provision => "--bind",
            ToolchainAccess::ReadOnly => "--ro-bind",
        }
        .to_owned(),
        TOOLCHAIN_ROOT.to_owned(),
        TOOLCHAIN_ROOT.to_owned(),
    ]);

    // Platform build assets — the compilers and runtimes Ato owns — are
    // READ-ONLY. A toolchain is provisioned by the build that needs it, so its
    // root is writable; these are shipped with the builder, and a build that
    // could edit the compiler could change what every later build produces.
    // `--ro-bind-try`: a builder with no platform assets provisioned is a
    // deployment state, not a reason for every unrelated build to abort here.
    argv.extend([
        "--ro-bind-try".to_owned(),
        BUILD_ASSET_ROOT.to_owned(),
        BUILD_ASSET_ROOT.to_owned(),
    ]);

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
    for (name, value) in build_environment(network, toolchain_path) {
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
    ]);
    if let Some(cwd) = guest_cwd {
        argv.extend(["--cwd".to_owned(), cwd.to_owned()]);
    }
    for (name, value) in workload_env {
        argv.extend(["--env".to_owned(), format!("{name}={value}")]);
    }
    argv.push("--".to_owned());
    argv.extend(workload_argv.iter().cloned());

    Ok(SandboxedBuildCommand {
        argv,
        policy: landlock_policy(cache_root.is_some(), toolchain),
        network,
    })
}

/// The env a build sees. Everything else is cleared.
fn build_environment(network: NetworkPolicy, toolchain_path: &[String]) -> Vec<(String, String)> {
    let path = toolchain_path
        .iter()
        .map(String::as_str)
        .chain(["/usr/local/bin:/usr/bin:/bin"])
        .collect::<Vec<_>>()
        .join(":");
    let mut env = vec![
        ("PATH".to_owned(), path),
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
    // Package managers keep caches and stores under `$HOME` by default, and
    // `$HOME` here IS the workspace — which becomes the artifact. Every one of
    // them is pointed elsewhere: the cache mount when this step may fill it,
    // the step's own /tmp when it may not.
    let cache = if network == NetworkPolicy::DependencyResolution {
        GUEST_CACHE_ROOT.to_owned()
    } else {
        "/tmp/.ato-cache".to_owned()
    };
    env.push(("npm_config_update_notifier".to_owned(), "false".to_owned()));
    env.push(("XDG_CACHE_HOME".to_owned(), format!("{cache}/xdg")));
    env.push((
        "npm_config_store_dir".to_owned(),
        format!("{cache}/pnpm-store"),
    ));
    env.push(("YARN_CACHE_FOLDER".to_owned(), format!("{cache}/yarn")));
    env.push((
        "YARN_GLOBAL_FOLDER".to_owned(),
        format!("{cache}/yarn-global"),
    ));
    if network != NetworkPolicy::DependencyResolution {
        env.push(("npm_config_cache".to_owned(), format!("{cache}/npm")));
    }
    if network == NetworkPolicy::DependencyResolution {
        env.push(("UV_CACHE_DIR".to_owned(), GUEST_CACHE_ROOT.to_owned()));
        env.push(("PIP_CACHE_DIR".to_owned(), GUEST_CACHE_ROOT.to_owned()));
        // Never prompt: a build that blocks on input is a build that burns its
        // whole timeout and reports nothing useful.
        env.push(("PIP_NO_INPUT".to_owned(), "1".to_owned()));
        // npm and its friends default their caches to `$HOME`, which here IS
        // the workspace — so an install would write `.npm/` into the tree that
        // becomes the artifact. Point them at the cache mount instead.
        env.push((
            "npm_config_cache".to_owned(),
            format!("{GUEST_CACHE_ROOT}/npm"),
        ));
        env.push((
            "COREPACK_HOME".to_owned(),
            format!("{GUEST_CACHE_ROOT}/corepack"),
        ));
        // Corepack must not stop to ask whether it may download a package
        // manager, and must not refuse a `packageManager` pin it cannot match
        // byte-for-byte; both turn an install into a hung or failed build.
        env.push(("COREPACK_ENABLE_DOWNLOAD_PROMPT".to_owned(), "0".to_owned()));
        env.push(("COREPACK_ENABLE_STRICT".to_owned(), "0".to_owned()));
        // No interactive prompts, no funding/audit chatter that can fail a
        // build on a network hiccup unrelated to the dependency graph.
        env.push(("NPM_CONFIG_FUND".to_owned(), "false".to_owned()));
        env.push(("NPM_CONFIG_AUDIT".to_owned(), "false".to_owned()));
        env.push(("CI".to_owned(), "1".to_owned()));
    }
    env
}

/// The Landlock policy the shim applies, in GUEST paths.
fn landlock_policy(with_cache: bool, toolchain: ToolchainAccess) -> SandboxPolicy {
    let mut writable = vec![PathBuf::from(GUEST_WORKSPACE_ROOT), PathBuf::from("/tmp")];
    // Provisioning writes here on a first build; every other step only reads.
    if toolchain == ToolchainAccess::Provision {
        writable.push(PathBuf::from(TOOLCHAIN_ROOT));
    }
    if with_cache {
        writable.push(PathBuf::from(GUEST_CACHE_ROOT));
    }
    let (read_write, _) = filter_sensitive_paths(&writable);

    // The SAME lists the binds use. Two lists drift, and the drift arrives as
    // "Permission denied" on a path the mount namespace demonstrably provided
    // — which is the least useful shape a sandbox error can take.
    let mut readable: Vec<PathBuf> = SYSTEM_READ_ONLY
        .iter()
        .chain(SYSTEM_CONFIG_FILES)
        .map(PathBuf::from)
        .collect();
    readable.push(PathBuf::from(GUEST_SOURCE_ROOT));
    if toolchain == ToolchainAccess::ReadOnly {
        readable.push(PathBuf::from(TOOLCHAIN_ROOT));
    }
    // Read-only on purpose: a build that could edit the compiler could change
    // what every later build produces.
    readable.push(PathBuf::from(BUILD_ASSET_ROOT));
    // The symlink targets the config files resolve through.
    readable.push(PathBuf::from("/run"));
    // bwrap supplies these with --proc and --dev rather than a bind, so they
    // are absent from the bind list and must be named anyway. Without /dev,
    // CPython cannot open /dev/urandom and dies during pre-initialization.
    readable.push(PathBuf::from("/dev"));
    readable.push(PathBuf::from("/proc"));
    let (read_only, _) = filter_sensitive_paths(&readable);

    SandboxPolicy::new()
        .allow_read_write(read_write)
        .allow_read_only(read_only)
}

fn path_str(path: &Path, what: &str) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{what} path is not valid UTF-8"))
}

pub fn containment_available() -> bool {
    ato_sandbox::bubblewrap_containment_available()
}

/// Refuse rather than degrade.
pub fn require_containment() -> Result<()> {
    if containment_available() {
        return Ok(());
    }
    bail!(
        "this Formation worker cannot contain a build: bubblewrap is unavailable or the host \
         rejects the required namespaces. Refusing to run submitted code unconfined."
    )
}
