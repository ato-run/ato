//! Linux-specific sandbox implementation using bubblewrap (bwrap)
//!
//! Provides namespace-based isolation for source execution with minimal overhead.
//!
//! ## Security Layers
//! 1. **Bubblewrap (Namespace)**: Primary isolation — PID/mount/net namespaces,
//!    explicit bind-mounts only.  Sensitive paths are hidden via `--tmpfs` overlay.
//! 2. **Landlock LSM** (optional, kernel 5.13+): Supplementary file-system
//!    access control applied inside the namespace via `pre_exec`.
//!
//! ## Sensitive Path Protection
//! Sensitive user directories (`.ssh`, `.aws`, etc.) are protected at the
//! **Bubblewrap level** by explicitly *not* bind-mounting them.  When the
//! user requests the entire home directory, the launcher additionally hides
//! those paths with `--tmpfs` so they appear as empty directories inside
//! the sandbox.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::launcher::{
    LaunchRequest, LaunchResult, RuntimeError, SourceTarget, bwrap_namespace_args,
};
use crate::system::sandbox::{filter_sensitive_paths, sensitive_paths};

use super::SourceRuntime;
use super::visibility::WorkloadVisibilityPlan;

fn requested_guest_cwd(target: &SourceTarget) -> PathBuf {
    target
        .requested_cwd
        .clone()
        .unwrap_or_else(|| PathBuf::from("/app"))
}

fn sandbox_entrypoint_path(target: &SourceTarget) -> String {
    let entrypoint = PathBuf::from(&target.entrypoint);
    if entrypoint.is_absolute() {
        return entrypoint.display().to_string();
    }

    let normalized = entrypoint
        .strip_prefix(".")
        .unwrap_or(entrypoint.as_path())
        .to_path_buf();
    PathBuf::from("/app").join(normalized).display().to_string()
}

/// Read-only bind-mounts for host system paths the sandboxed workload needs.
///
/// `/usr` must exist on any supported host, so it is a hard `--ro-bind` (a
/// missing source there is a real error). The rest are host-environment
/// dependent and legitimately absent on some systems:
///   - `/lib64` does not exist on aarch64 (and other non-amd64 arches),
///   - `/lib` is absent on pure /usr-merge layouts,
///   - `/etc/resolv.conf` / `/etc/hosts` / `/etc/ssl` are absent in minimal
///     container/base images.
///
/// A strict `--ro-bind` against a non-existent source makes bwrap abort the
/// *entire* sandbox during mount setup ("Can't find source path …", exit 1,
/// before the workload ever execs). `--ro-bind-try` skips a missing optional
/// source instead of killing the launch. This does not weaken isolation — a
/// source that does not exist cannot be exposed.
fn system_ro_binds() -> [[&'static str; 3]; 6] {
    [
        ["--ro-bind-try", "/lib", "/lib"],
        ["--ro-bind-try", "/lib64", "/lib64"],
        ["--ro-bind", "/usr", "/usr"],
        ["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"],
        ["--ro-bind-try", "/etc/hosts", "/etc/hosts"],
        ["--ro-bind-try", "/etc/ssl", "/etc/ssl"],
    ]
}

/// Env keys the runtime re-applies inside the production sandbox after
/// `--clearenv`. This is a STRICT allowlist — only keys the runtime itself
/// synthesizes for the sandbox contract (the writable session data dir), never
/// general user/host env. `--clearenv` deliberately drops everything else.
const SANDBOX_RUNTIME_ENV_ALLOWLIST: &[&str] = &["ATO_DATA_DIR", "DATABASE_PATH"];

/// Select the entries of the workload env that the runtime is allowed to
/// re-inject past `--clearenv`. Order is preserved; bwrap's `--setenv` applies
/// last-write-wins for duplicate keys.
fn sandbox_runtime_setenv_pairs(env: Option<&[(String, String)]>) -> Vec<(String, String)> {
    env.into_iter()
        .flatten()
        .filter(|(key, _)| SANDBOX_RUNTIME_ENV_ALLOWLIST.contains(&key.as_str()))
        .cloned()
        .collect()
}

fn ensure_bwrap_dirs(cmd: &mut Command, path: &std::path::Path) {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current == std::path::Path::new("/") {
            continue;
        }
        cmd.args(["--dir", &current.display().to_string()]);
    }
}

fn mount_source_path(mount: &crate::launcher::InjectedMount) -> PathBuf {
    if mount.source.exists() || mount.source.is_dir() {
        return mount.source.clone();
    }

    mount
        .source
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| mount.source.clone())
}

/// Generate a `SandboxPolicy` from `IsolationPolicy` for Landlock enforcement.
///
/// The allow-list is constructed from the manifest's `read_only` / `read_write`
/// paths **after** filtering out any paths that overlap with sensitive
/// directories.  This ensures that even if the user specifies `~` as an
/// allowed path, `~/.ssh` will not be granted access via Landlock.
#[allow(dead_code)]
pub fn generate_landlock_policy(target: &SourceTarget) -> crate::system::sandbox::SandboxPolicy {
    use crate::system::sandbox::SandboxPolicy;

    let iso = match target.isolation.as_ref() {
        Some(iso) if iso.sandbox_enabled => iso,
        _ => return SandboxPolicy::for_capsule(&target.source_dir),
    };

    // Start from the manifest-level policy, which already filters
    // sensitive paths via `from_isolation_policy`.
    let mut policy = SandboxPolicy::from_isolation_policy(iso, target.dev_mode);

    // Ensure source_dir is always in the RW list (it is safe — it's the
    // capsule's own working directory).
    if !policy.read_write_paths.contains(&target.source_dir) {
        policy.read_write_paths.push(target.source_dir.clone());
    }

    // Ensure essential system directories are in the RO list so that
    // Landlock doesn't block basic process execution.
    let system_ro = [
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/etc"),
        PathBuf::from("/dev"),
        PathBuf::from("/proc"),
        PathBuf::from("/sys"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
    ];
    for p in &system_ro {
        if !policy.read_only_paths.contains(p) {
            policy.read_only_paths.push(p.clone());
        }
    }

    // Ensure /tmp paths are writable
    for tmp in ["/tmp", "/var/tmp"] {
        let p = PathBuf::from(tmp);
        if !policy.read_write_paths.contains(&p) {
            policy.read_write_paths.push(p);
        }
    }

    // Forward IPC socket paths from ato-cli (IPC Broker)
    if !target.ipc_socket_paths.is_empty() {
        debug!(
            "Adding {} IPC socket paths to Landlock policy",
            target.ipc_socket_paths.len()
        );
        policy.ipc_socket_paths = target.ipc_socket_paths.clone();
    }

    for mount in &target.injected_mounts {
        let source = mount_source_path(mount);
        if mount.readonly {
            if !policy.read_only_paths.contains(&source) {
                policy.read_only_paths.push(source);
            }
        } else if !policy.read_write_paths.contains(&source) {
            policy.read_write_paths.push(source);
        }
    }

    policy
}

/// Build the Landlock policy applied to the workload *inside* the sandbox by
/// the `nacelle sandbox-exec` shim. Paths are guest paths (the capsule source
/// is mounted at `/app`).
///
/// Reads are granted broadly (`/`) because bubblewrap already limits what is
/// visible inside the sandbox to the bind set — granting read on `/` exposes
/// nothing beyond those mounts, and keeps the read set complete (interpreter,
/// stdlib, shared libraries, source) without enumerating every bound path.
/// Landlock's contribution on this path is the *write* restriction: only the
/// workload's scratch space (`/tmp`), the source dir (already read-only-bound
/// by bwrap), injected writable mounts, and IPC sockets are writable.
fn guest_landlock_policy(target: &SourceTarget) -> crate::system::sandbox::SandboxPolicy {
    use crate::system::sandbox::SandboxPolicy;

    let mut policy = SandboxPolicy {
        read_only_paths: vec![PathBuf::from("/")],
        read_write_paths: vec![
            PathBuf::from("/tmp"),
            PathBuf::from("/var/tmp"),
            // Source is mounted read-only at /app; mirror the prior policy's
            // intent of treating the capsule's own dir as writable. bwrap's
            // read-only bind still wins, so this grants nothing extra.
            PathBuf::from("/app"),
        ],
        ..SandboxPolicy::default()
    };
    for mount in &target.injected_mounts {
        if !mount.readonly {
            policy.read_write_paths.push(mount.target.clone());
        }
    }
    policy.ipc_socket_paths = target.ipc_socket_paths.clone();
    policy.allow_network = target
        .isolation
        .as_ref()
        .map(|iso| iso.network_enabled)
        .unwrap_or(true);
    policy.development_mode = target.dev_mode;
    policy
}

/// Stage the in-sandbox Landlock shim. Serializes the guest policy to a file and
/// bind-mounts both the `nacelle` binary and that policy file (read-only) into
/// the sandbox, returning their guest paths `(nacelle_binary, policy_file)` for
/// the workload command line. bwrap then execs
/// `<nacelle> sandbox-exec --policy <policy> -- <workload>`, which applies
/// Landlock to the workload after bwrap's namespace setup.
fn prepare_landlock_shim(
    runtime: &SourceRuntime,
    workload_id: &str,
    target: &SourceTarget,
    cmd: &mut Command,
) -> Result<(String, String), RuntimeError> {
    const GUEST_NACELLE: &str = "/nacelle";
    const GUEST_POLICY: &str = "/nacelle-landlock.json";

    let nacelle_bin = std::env::current_exe().map_err(|e| RuntimeError::Io {
        path: PathBuf::from("<current_exe>"),
        source: e,
    })?;
    let nacelle_bin = nacelle_bin.to_string_lossy().to_string();

    let policy = guest_landlock_policy(target);
    let policy_json = serde_json::to_vec(&policy).map_err(|e| RuntimeError::CommandExecution {
        operation: "serialize landlock policy".to_string(),
        source: std::io::Error::other(e.to_string()),
    })?;
    let policy_host = runtime
        .config
        .log_dir
        .join(format!("{workload_id}.landlock.json"));
    std::fs::write(&policy_host, &policy_json).map_err(|e| RuntimeError::Io {
        path: policy_host.clone(),
        source: e,
    })?;
    let policy_host_str = policy_host.to_string_lossy().to_string();

    cmd.args(["--ro-bind", &nacelle_bin, GUEST_NACELLE]);
    cmd.args(["--ro-bind", &policy_host_str, GUEST_POLICY]);

    Ok((GUEST_NACELLE.to_string(), GUEST_POLICY.to_string()))
}

/// Add bubblewrap arguments that hide sensitive paths inside the namespace.
///
/// When the user's bind-mount set would expose a sensitive directory
/// (e.g., `$HOME` is bound, so `$HOME/.ssh` would be visible), this
/// function emits `--tmpfs <sensitive_path>` arguments that overlay
/// the sensitive directory with an empty tmpfs, effectively hiding it.
fn add_sensitive_path_hiding(cmd: &mut Command, bind_mounted_parents: &[&str]) {
    let sensitive = sensitive_paths();

    for sp in &sensitive {
        // Hide if a parent of this sensitive path is bound (the sensitive
        // dir is reachable through it), or if a bound path is the sensitive
        // dir itself or sits inside it (e.g. an individually-bound
        // ~/.aws/credentials) – the tmpfs is emitted after the binds, so it
        // over-mounts and shadows them.
        let exposed = bind_mounted_parents
            .iter()
            .any(|parent| sp.starts_with(parent) || std::path::Path::new(parent).starts_with(sp));
        if exposed && sp.exists() {
            let sp_str = sp.to_string_lossy();
            debug!("Hiding sensitive path in sandbox: {}", sp_str);
            cmd.args(["--tmpfs", &sp_str]);
        }
    }
}

/// Launch a source workload using bubblewrap sandbox
pub async fn launch_with_bubblewrap(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Find toolchain binary
    let toolchain_path = runtime
        .ensure_toolchain(target)
        .await
        .map_err(|e| RuntimeError::ToolchainError {
            message: format!("Failed to ensure {} toolchain", target.language),
            technical_reason: Some(e.to_string()),
            cloud_upsell: Some(
                "💡 This app requires a cloud environment. Run with '--mode=cloud' (Pro) to execute in a managed Linux VM with guaranteed compatibility."
                    .to_string(),
            ),
        })?;

    info!(
        "Launching with bubblewrap: {} {} (toolchain: {:?}, dev_mode: {})",
        target.language, target.entrypoint, toolchain_path, target.dev_mode
    );

    // Platform-neutral plan for which interpreter to run and which host paths
    // the workload must see (venv base install + toolchain install root). Lowered
    // below to bwrap `--ro-bind-try` binds and the in-sandbox interpreter path.
    let visibility = WorkloadVisibilityPlan::compute(target, &toolchain_path);

    // =====================================================================
    // Egress enforcement checks
    // =====================================================================
    if let Some(ref iso) = target.isolation {
        if iso.network_enabled && !iso.egress_allow.is_empty() {
            warn!(
                "Domain-level egress filtering (egress_allow: {:?}) is not enforceable via Bubblewrap/Landlock. \
                 Relies on Sidecar Proxy (tsnet/SOCKS5).",
                iso.egress_allow
            );
        }
        // Fail-closed: L3 CIDR rules require a tsnet sidecar that is not yet integrated.
        // Refuse launch rather than silently permit unrestricted network access.
        if iso.network_enabled && !iso.egress_id_allow.is_empty() && !target.dev_mode {
            return Err(RuntimeError::InvalidConfig(format!(
                "egress_id_allow ({:?}) requires a tsnet sidecar proxy, \
                 which is not available in this environment. \
                 Either remove egress_id_allow from capsule.toml, use dev_mode = true \
                 to bypass enforcement during development, \
                 or start the ato-tsnetd sidecar before running this capsule.",
                iso.egress_id_allow
            )));
        }
    }

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build bwrap command
    let mut cmd = Command::new("bwrap");

    // Namespace isolation — network policy from IsolationPolicy or dev_mode.
    // The decision itself lives in `launcher::shares_host_network` so it is
    // unit-testable without a toolchain or a Linux host (ato#786).
    cmd.args(bwrap_namespace_args(target).iter().copied());
    cmd.args(["--die-with-parent"]);

    // Basic filesystem setup
    cmd.args(["--proc", "/proc"]);
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--tmpfs", "/tmp"]);

    // Bind mount essential host system paths read-only (see `system_ro_binds`
    // for why most are non-fatal `--ro-bind-try`).
    for [flag, src, dst] in system_ro_binds() {
        cmd.args([flag, src, dst]);
    }

    // Bind mount the toolchain binary
    let toolchain_path_str = toolchain_path.to_string_lossy();
    cmd.args(["--ro-bind", &toolchain_path_str, &toolchain_path_str]);

    // Lower the platform-neutral visibility plan to bwrap binds. `read_paths`
    // carries the venv base install and the toolchain install ROOT (bin/ + lib/,
    // not just the binary) so the interpreter can load its runtime — e.g.
    // libpython*.so for a managed CPython, or node's lib/, and the base CPython a
    // venv shim execs into. Binding only the binary leaves those absent and the
    // interpreter exits with "error while loading shared libraries".
    // `--ro-bind-try` keeps each non-fatal; /usr and / are already excluded by
    // the plan (and bound above).
    for path in &visibility.read_paths {
        let path_str = path.to_string_lossy();
        cmd.args(["--ro-bind-try", &path_str, &path_str]);
    }

    // Bind mount the source directory read-only
    let source_dir_str = target.source_dir.to_string_lossy();
    cmd.args(["--ro-bind", &source_dir_str, "/app"]);

    for mount in &target.injected_mounts {
        let source = mount_source_path(mount);
        let target_path = mount.target.clone();
        if let Some(parent) = target_path.parent() {
            ensure_bwrap_dirs(&mut cmd, parent);
        }
        let source_str = source.to_string_lossy().to_string();
        let target_str = target_path.to_string_lossy().to_string();
        if mount.readonly {
            cmd.args(["--ro-bind", &source_str, &target_str]);
        } else {
            cmd.args(["--bind", &source_str, &target_str]);
        }
    }

    // =====================================================================
    // Apply IsolationPolicy bind-mounts (if provided in capsule.toml)
    // Paths overlapping with sensitive directories are pre-filtered.
    // =====================================================================
    let mut extra_bound_parents: Vec<String> = Vec::new();
    if let Some(ref iso) = target.isolation {
        let (clean_ro, removed_ro) = filter_sensitive_paths(&iso.read_only_paths);
        let (clean_rw, removed_rw) = filter_sensitive_paths(&iso.read_write_paths);
        for p in &removed_ro {
            warn!(
                "Sensitive path excluded from RO bind-mounts: {}",
                p.display()
            );
        }
        for p in &removed_rw {
            warn!(
                "Sensitive path excluded from RW bind-mounts: {}",
                p.display()
            );
        }
        for p in &clean_ro {
            if p.exists() {
                let ps = p.to_string_lossy();
                cmd.args(["--ro-bind", &ps, &ps]);
                extra_bound_parents.push(ps.to_string());
            }
        }
        for p in &clean_rw {
            if p.exists() {
                let ps = p.to_string_lossy();
                cmd.args(["--bind", &ps, &ps]);
                extra_bound_parents.push(ps.to_string());
            }
        }
    }

    // Bubblewrap hides the host /tmp behind a tmpfs. Re-bind IPC socket parent
    // directories so ato-cli provisioned sockets remain reachable inside the sandbox.
    let mut ipc_bind_targets: Vec<PathBuf> = Vec::new();
    for socket_path in &target.ipc_socket_paths {
        let bind_target = if socket_path.exists() {
            socket_path.clone()
        } else if let Some(parent) = socket_path.parent() {
            parent.to_path_buf()
        } else {
            continue;
        };

        if !bind_target.exists() || ipc_bind_targets.contains(&bind_target) {
            continue;
        }

        let bind_target_str = bind_target.to_string_lossy().to_string();
        debug!(
            "Binding IPC path into bubblewrap sandbox: {}",
            bind_target_str
        );
        cmd.args(["--bind", &bind_target_str, &bind_target_str]);
        ipc_bind_targets.push(bind_target);
    }

    // Bind PTY devices for interactive terminal sessions
    if target.interactive {
        // /dev/pts contains the PTY slave devices; /dev/ptmx is the PTY master
        if std::path::Path::new("/dev/pts").exists() {
            cmd.args(["--dev-bind", "/dev/pts", "/dev/pts"]);
        }
        if std::path::Path::new("/dev/ptmx").exists() {
            cmd.args(["--dev-bind", "/dev/ptmx", "/dev/ptmx"]);
        }
        debug!("Bound /dev/pts and /dev/ptmx into bwrap sandbox for interactive PTY");
    }

    // Hide sensitive paths that would be reachable via any parent bind-mount
    let all_parents: Vec<&str> = extra_bound_parents.iter().map(|s| s.as_str()).collect();
    add_sensitive_path_hiding(&mut cmd, &all_parents);

    // Set working directory
    let requested_cwd = requested_guest_cwd(target);
    ensure_bwrap_dirs(&mut cmd, &requested_cwd);
    cmd.args(["--chdir", &requested_cwd.display().to_string()]);

    // Security hardening - more restrictive in production mode
    cmd.args(["--new-session"]);
    cmd.args(["--cap-drop", "ALL"]);

    // Production mode: additional hardening
    if !target.dev_mode {
        // Restrict environment variables
        cmd.args(["--clearenv"]);
        // Re-add essential env vars
        cmd.args(["--setenv", "PATH", "/usr/bin:/bin"]);
        cmd.args(["--setenv", "HOME", "/tmp"]);
        cmd.args(["--setenv", "LANG", "C.UTF-8"]);

        // Re-apply runtime-owned env that must survive --clearenv. Strict
        // allowlist (SANDBOX_RUNTIME_ENV_ALLOWLIST) — e.g. ATO_DATA_DIR /
        // DATABASE_PATH for the writable session data dir. General user/host env
        // stays dropped.
        for (key, value) in sandbox_runtime_setenv_pairs(request.env.as_deref()) {
            cmd.args(["--setenv", &key, &value]);
        }

        // Apply sidecar (SOCKS5 proxy) environment variables
        if let Some(ref sidecar) = runtime.config.sidecar_config {
            let proxy_url = format!("socks5h://127.0.0.1:{}", sidecar.socks_port);
            cmd.args(["--setenv", "HTTP_PROXY", &proxy_url]);
            cmd.args(["--setenv", "HTTPS_PROXY", &proxy_url]);
            cmd.args(["--setenv", "ALL_PROXY", &proxy_url]);

            // Build NO_PROXY list
            let mut no_proxy = vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
            ];
            no_proxy.extend(sidecar.no_proxy.clone());
            no_proxy.push(".local".to_string());

            let no_proxy_str = no_proxy.join(",");
            cmd.args(["--setenv", "NO_PROXY", &no_proxy_str]);
            cmd.args(["--setenv", "no_proxy", &no_proxy_str]);

            info!("Applied SOCKS5 proxy {} to sandboxed process", proxy_url);
        }

        // Block access to sensitive kernel interfaces
        cmd.args(["--ro-bind", "/dev/null", "/dev/kvm"]);

        // Use seccomp filtering (if available)
        // Note: Would need seccomp-bpf profile file
    } else {
        // Development mode: apply proxy env vars without clearing
        runtime.apply_sidecar_env(&mut cmd);
    }

    // Interpreter selection comes from the visibility plan, which prefers a
    // project virtualenv when the build phase produced one. A bare
    // `python`/`python3` otherwise maps to the managed *base* toolchain
    // interpreter, which carries only the standard library — the capsule's
    // installed dependencies live in `.venv`, so the app would fail at import
    // time (`ModuleNotFoundError`). The venv interpreter runs at its guest path
    // (`/app/.venv/bin/python`); its base CPython install is already bound above
    // via `visibility.read_paths`.
    let venv_python = visibility.venv.as_ref();

    // Landlock is applied to the *workload*, not the bwrap wrapper, via an
    // in-sandbox shim (`nacelle sandbox-exec`). bwrap sets up the user
    // namespace first (writing /proc/self/uid_map); the shim then applies
    // Landlock and execs the workload. Applying Landlock to bwrap directly
    // would deny that /proc write and break user-namespace setup.
    let landlock_shim = if crate::system::sandbox::is_sandbox_supported() {
        match prepare_landlock_shim(runtime, request.workload_id, target, &mut cmd) {
            Ok(shim) => Some(shim),
            Err(err) => {
                warn!(
                    "Landlock shim setup failed ({err}); workload runs with bubblewrap \
                     namespace isolation only"
                );
                None
            }
        }
    } else {
        None
    };

    // Add the actual command
    cmd.arg("--");
    if let Some((ref shim_bin, ref policy_guest)) = landlock_shim {
        // bwrap execs the shim, which applies Landlock then execs the workload.
        cmd.arg(shim_bin);
        cmd.args(["sandbox-exec", "--policy", policy_guest, "--"]);
    }
    if let Some(explicit_cmd) = target.cmd.as_ref() {
        if let Some((binary, args)) = explicit_cmd.split_first() {
            let binary_path = match binary.as_str() {
                "python" | "python3" => match venv_python {
                    Some(venv) => PathBuf::from(&venv.guest_python),
                    None => toolchain_path.clone(),
                },
                "node" | "deno" | "ruby" => toolchain_path.clone(),
                _ => which::which(binary).unwrap_or_else(|_| PathBuf::from(binary)),
            };
            cmd.arg(binary_path);
            let mut entrypoint_rewritten = false;
            let sandbox_entrypoint = sandbox_entrypoint_path(target);
            for arg in args {
                if !entrypoint_rewritten && arg == &target.entrypoint {
                    cmd.arg(sandbox_entrypoint.clone());
                    entrypoint_rewritten = true;
                } else {
                    cmd.arg(arg);
                }
            }
        }
    } else {
        // No explicit command: synthesize `<interpreter> <entrypoint>` from the
        // declared language, preferring the venv interpreter for python.
        let python_interp = match venv_python {
            Some(venv) => PathBuf::from(&venv.guest_python),
            None => toolchain_path.clone(),
        };
        match target.language.to_lowercase().as_str() {
            "python" => {
                cmd.arg(&python_interp);
                cmd.args(["-B", &sandbox_entrypoint_path(target)]);
            }
            "node" | "nodejs" => {
                cmd.arg(&toolchain_path);
                cmd.arg(sandbox_entrypoint_path(target));
            }
            "deno" => {
                cmd.arg(&toolchain_path);
                let sandbox_entrypoint = sandbox_entrypoint_path(target);
                cmd.args(["run", "--allow-read=/app", &sandbox_entrypoint]);
            }
            _ => {
                cmd.arg(&toolchain_path);
                cmd.arg(sandbox_entrypoint_path(target));
            }
        }

        cmd.args(&target.args);
    }

    // Setup output redirection
    let log_path = runtime.workload_log_path(request.workload_id);
    let log_file = std::fs::File::create(&log_path).map_err(|e| RuntimeError::Io {
        path: log_path.clone(),
        source: e,
    })?;

    cmd.stdout(Stdio::from(log_file.try_clone().map_err(|e| {
        RuntimeError::Io {
            path: log_path.clone(),
            source: e,
        }
    })?));
    cmd.stderr(Stdio::from(log_file));

    // Socket Activation (Phase 2): Pass listening socket FD to child process
    if let Some(ref socket_manager) = request.socket_manager {
        socket_manager
            .prepare_command(&mut cmd)
            .map_err(|e| RuntimeError::CommandExecution {
                operation: "socket_activation_prepare".to_string(),
                source: std::io::Error::other(e.to_string()),
            })?;
        tracing::info!(
            "Socket Activation: Passing FD {} to child process",
            crate::manager::socket::SD_LISTEN_FDS_START
        );
    }

    debug!("Executing bwrap command: {:?}", cmd);

    // NOTE: Landlock is intentionally NOT applied here via `pre_exec`. Doing so
    // restricts the *bwrap wrapper* before it sets up the user namespace, and a
    // policy that (correctly) does not grant write access to `/proc` denies
    // bwrap's `/proc/self/uid_map` write — bwrap then fails with
    // "setting up uid map: Permission denied". Instead, Landlock is applied to
    // the *workload* from inside the sandbox by the `nacelle sandbox-exec` shim
    // (staged above via `prepare_landlock_shim`), after bwrap's namespace setup.

    // Put the bwrap wrapper in its own process group (mirror of macos.rs
    // `launch_with_sandbox_exec` and `launch_direct`). Without this the
    // cleanup-scope's `kill(-pgid, sig)` path in ato-cli targets the
    // wrong pgroup, leaving inner workload processes as orphans after bwrap exits.
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "bwrap spawn".to_string(),
        source: e,
    })?;

    let pid = child.id();
    info!(
        "Started source workload {} with PID {}",
        request.workload_id, pid
    );

    // Track the workload (PID for quick lookup)
    {
        let mut workloads = runtime.active_workloads.lock().unwrap();
        workloads.insert(request.workload_id.to_string(), pid);
    }

    // Register child handle for lifecycle management (keeps process alive)
    runtime.register_child(request.workload_id.to_string(), child);

    Ok(LaunchResult {
        pid: Some(pid),
        bundle_path: None,
        log_path: Some(log_path),
        port: None,
    })
}

/// Arguments for the bubblewrap availability probe.
///
/// bubblewrap starts with an empty filesystem namespace. Probing `/bin/true`
/// without binding a rootfs can fail even when bwrap/userns are usable — the
/// target binary is simply not present inside the empty namespace, so bwrap
/// exits non-zero with `execvp: No such file or directory`. The probe must
/// match the real sandbox launcher enough to verify execution, so it binds the
/// host rootfs read-only before exec. Missing bwrap, blocked user namespaces,
/// or other bwrap failures still surface as "unavailable".
fn bubblewrap_probe_args() -> [&'static str; 9] {
    [
        "--ro-bind",
        "/",
        "/", // rootfs must be present for /bin/true to exec
        "--unshare-user",
        "--uid",
        "1000",
        "--gid",
        "1000", //
        "/bin/true",
    ]
}

/// Check if bubblewrap is available and properly configured
#[allow(dead_code)]
pub fn verify_bubblewrap_available() -> Result<(), RuntimeError> {
    // Check binary exists
    let bwrap_path = which::which("bwrap").map_err(|_| {
        RuntimeError::SandboxSetupFailed("bubblewrap (bwrap) not found in PATH".to_string())
    })?;

    // Probe a real (rootfs-bound) bwrap invocation so the result reflects actual
    // sandbox usability — without the rootfs bind /bin/true is missing inside the
    // empty namespace and the probe fails even when sandboxing works. See
    // `bubblewrap_probe_args`.
    let output = Command::new(&bwrap_path)
        .args(bubblewrap_probe_args())
        .output();

    match output {
        Ok(result) if result.status.success() => {
            debug!("bubblewrap user namespace check passed");
            Ok(())
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            if stderr.contains("permission denied") || stderr.contains("Operation not permitted") {
                warn!(
                    "User namespaces may be disabled. Try: sudo sysctl kernel.unprivileged_userns_clone=1"
                );
                Err(RuntimeError::SandboxSetupFailed(
                    "User namespaces not available. Check kernel.unprivileged_userns_clone"
                        .to_string(),
                ))
            } else {
                Err(RuntimeError::SandboxSetupFailed(format!(
                    "bubblewrap check failed: {}",
                    stderr
                )))
            }
        }
        Err(e) => Err(RuntimeError::SandboxSetupFailed(format!(
            "Failed to execute bubblewrap: {}",
            e
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::IsolationPolicy;

    #[test]
    fn bubblewrap_probe_binds_rootfs_before_exec() {
        // Without a rootfs bind the probe runs in an empty namespace where
        // /bin/true does not exist, so it fails even when sandboxing works.
        let args = bubblewrap_probe_args();
        let rootfs = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == "/" && w[2] == "/")
            .expect("probe must bind the host rootfs read-only (--ro-bind / /)");
        let exec = args
            .iter()
            .position(|a| *a == "/bin/true")
            .expect("probe must exec a target binary");
        assert!(
            rootfs < exec,
            "rootfs bind must precede the exec target: {args:?}"
        );
    }

    #[test]
    fn bubblewrap_probe_keeps_user_namespace_flags() {
        let args = bubblewrap_probe_args();
        assert!(
            args.contains(&"--unshare-user"),
            "probe must unshare user ns"
        );
        assert!(args.contains(&"--uid"), "probe must set a uid map");
        assert!(args.contains(&"--gid"), "probe must set a gid map");
    }

    #[test]
    fn bubblewrap_probe_does_not_require_landlock() {
        // bwrap availability is independent of Landlock (a supplementary LSM);
        // the probe must not depend on landlock support.
        let args = bubblewrap_probe_args();
        assert!(
            !args.iter().any(|a| a.to_lowercase().contains("landlock")),
            "bwrap probe must not reference landlock: {args:?}"
        );
    }

    #[test]
    fn test_bubblewrap_command_construction() {
        // This test verifies the command structure is correct
        // Actual execution requires bubblewrap installed

        let mut cmd = Command::new("bwrap");
        cmd.args(["--unshare-all", "--share-net"]);
        cmd.args(["--die-with-parent"]);
        cmd.args(["--ro-bind", "/usr", "/usr"]);
        cmd.arg("--");
        cmd.args(["python3", "main.py"]);

        // Command can be constructed without errors
        let program = cmd.get_program();
        assert_eq!(program, "bwrap");
    }

    #[test]
    fn test_generate_landlock_policy_default() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/app/my-capsule"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: None, // no isolation config → default policy
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            ..Default::default()
        };

        let policy = generate_landlock_policy(&target);

        // Should use for_capsule defaults
        assert!(policy.read_write_paths.contains(&PathBuf::from("/tmp")));
        assert!(policy.read_only_paths.contains(&PathBuf::from("/usr")));
        assert!(policy.allow_network);
    }

    #[test]
    fn test_generate_landlock_policy_with_isolation() {
        let target = SourceTarget {
            language: "node".to_string(),
            version: Some("20".to_string()),
            entrypoint: "index.js".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/app/project"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: Some(IsolationPolicy {
                sandbox_enabled: true,
                read_only_paths: vec![PathBuf::from("/data/ro")],
                read_write_paths: vec![PathBuf::from("/data/rw")],
                network_enabled: false,
                egress_allow: vec![],
                egress_id_allow: vec![],
            }),
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            ..Default::default()
        };

        let policy = generate_landlock_policy(&target);

        // Source dir always present
        assert!(
            policy
                .read_write_paths
                .contains(&PathBuf::from("/app/project"))
        );
        // System dirs added
        assert!(policy.read_only_paths.contains(&PathBuf::from("/usr")));
        // Network from isolation policy
        assert!(!policy.allow_network);
    }

    #[test]
    fn test_generate_landlock_policy_filters_home() {
        if let Some(home) = dirs::home_dir() {
            let target = SourceTarget {
                language: "python".to_string(),
                version: None,
                entrypoint: "main.py".to_string(),
                dependencies: None,
                args: vec![],
                source_dir: PathBuf::from("/app/proj"),
                requested_cwd: None,
                cmd: None,
                dev_mode: false,
                isolation: Some(IsolationPolicy {
                    sandbox_enabled: true,
                    read_only_paths: vec![],
                    read_write_paths: vec![home.clone()],
                    network_enabled: true,
                    egress_allow: vec![],
                    egress_id_allow: vec![],
                }),
                ipc_socket_paths: vec![],
                injected_mounts: vec![],
                ..Default::default()
            };

            let policy = generate_landlock_policy(&target);

            // Home directory should be filtered out (it's a parent of ~/.ssh)
            assert!(
                !policy.read_write_paths.contains(&home),
                "Home directory should be filtered from Landlock allow-list"
            );
        }
    }

    #[test]
    fn test_add_sensitive_path_hiding_no_parents() {
        let mut cmd = Command::new("echo");
        // No bound parents → nothing to hide
        add_sensitive_path_hiding(&mut cmd, &[]);
        // Just ensure it doesn't panic
    }

    #[test]
    fn test_add_sensitive_path_hiding_bound_inside_sensitive_dir() {
        // A bound path that sits inside a sensitive dir must be shadowed by
        // a --tmpfs over the sensitive dir itself (issue #642).
        let Some(sp) = sensitive_paths().into_iter().find(|p| p.is_dir()) else {
            return; // no sensitive dirs on this machine — nothing to verify
        };
        let bound = sp.join("credentials");
        let bound_str = bound.to_string_lossy().to_string();

        let mut cmd = Command::new("echo");
        add_sensitive_path_hiding(&mut cmd, &[&bound_str]);

        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let sp_str = sp.to_string_lossy().to_string();
        assert!(
            args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == sp_str),
            "expected --tmpfs {sp_str} to shadow the bound path {bound_str}"
        );
    }

    #[test]
    fn test_generate_landlock_policy_with_ipc_socket_paths() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: None,
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/app/my-service"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: Some(IsolationPolicy {
                sandbox_enabled: true,
                read_only_paths: vec![],
                read_write_paths: vec![],
                network_enabled: true,
                egress_allow: vec![],
                egress_id_allow: vec![],
            }),
            ipc_socket_paths: vec![
                PathBuf::from("/tmp/capsule-ipc/greeter.sock"),
                PathBuf::from("/tmp/capsule-ipc/db-service.sock"),
            ],
            injected_mounts: vec![],
            ..Default::default()
        };

        let policy = generate_landlock_policy(&target);

        // IPC socket paths should be forwarded to the policy
        assert_eq!(policy.ipc_socket_paths.len(), 2);
        assert!(
            policy
                .ipc_socket_paths
                .contains(&PathBuf::from("/tmp/capsule-ipc/greeter.sock"))
        );
        assert!(
            policy
                .ipc_socket_paths
                .contains(&PathBuf::from("/tmp/capsule-ipc/db-service.sock"))
        );
    }

    #[test]
    fn test_generate_landlock_policy_empty_ipc_paths() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: None,
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/app/my-capsule"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: None,
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            ..Default::default()
        };

        let policy = generate_landlock_policy(&target);
        assert!(policy.ipc_socket_paths.is_empty());
    }

    fn python_target(source_dir: PathBuf) -> SourceTarget {
        SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir,
            requested_cwd: None,
            cmd: Some(vec![
                "python".to_string(),
                "-m".to_string(),
                "uvicorn".to_string(),
                "app.main:app".to_string(),
            ]),
            dev_mode: false,
            isolation: None,
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn sandbox_runtime_env_allowlist_filters_to_runtime_keys() {
        // Only runtime-owned keys survive --clearenv; general user/host env is
        // dropped (no FOO/secret leak into the sandbox).
        let env = vec![
            ("ATO_DATA_DIR".to_string(), "/runs/ato/session".to_string()),
            (
                "DATABASE_PATH".to_string(),
                "/runs/ato/session/app.db".to_string(),
            ),
            ("FOO".to_string(), "bar".to_string()),
            ("SECRET_TOKEN".to_string(), "shhh".to_string()),
        ];
        let pairs = sandbox_runtime_setenv_pairs(Some(&env));
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["ATO_DATA_DIR", "DATABASE_PATH"]);
        assert!(
            !keys.contains(&"FOO") && !keys.contains(&"SECRET_TOKEN"),
            "general user/host env must not survive --clearenv"
        );
        assert!(sandbox_runtime_setenv_pairs(None).is_empty());
    }

    #[test]
    fn lib64_bind_is_non_fatal_but_usr_is_strict() {
        // Regression guard for aarch64: `/lib64` does not exist there, and a
        // strict `--ro-bind` would abort the whole sandbox during mount setup.
        let binds = system_ro_binds();
        let flag_for = |path: &str| binds.iter().find(|b| b[1] == path).map(|b| b[0]);
        assert_eq!(
            flag_for("/lib64"),
            Some("--ro-bind-try"),
            "/lib64 is absent on aarch64; its bind must be non-fatal"
        );
        assert_eq!(
            flag_for("/usr"),
            Some("--ro-bind"),
            "/usr must exist on any supported host and stays a hard bind"
        );
        assert_eq!(flag_for("/etc/resolv.conf"), Some("--ro-bind-try"));
        // Every bind target must be absolute and self-mapped (src == dst).
        for [_flag, src, dst] in binds {
            assert_eq!(src, dst, "system binds are identity-mapped");
            assert!(src.starts_with('/'), "bind target must be absolute: {src}");
        }
    }

    #[test]
    fn guest_landlock_policy_restricts_writes_not_reads() {
        // The in-sandbox policy uses guest paths and grants read on `/` while
        // keeping writes to scratch space only. This is what the `sandbox-exec`
        // shim applies to the workload (Landlock applied to bwrap would break
        // user-namespace setup).
        let target = python_target(PathBuf::from("/host/src"));
        let policy = guest_landlock_policy(&target);
        assert_eq!(
            policy.read_only_paths,
            vec![PathBuf::from("/")],
            "reads granted on / (bwrap already limits what is visible)"
        );
        assert!(policy.read_write_paths.contains(&PathBuf::from("/tmp")));
        assert!(policy.read_write_paths.contains(&PathBuf::from("/app")));
        assert!(
            !policy.read_write_paths.contains(&PathBuf::from("/usr")),
            "/usr must not be writable"
        );
        assert!(
            !policy
                .read_write_paths
                .contains(&PathBuf::from("/host/src")),
            "policy must use guest paths (/app), not the host source dir"
        );
        assert!(
            policy.allow_network,
            "isolation None defaults to network allowed"
        );
    }

    #[test]
    fn guest_landlock_policy_grants_write_to_injected_writable_mount() {
        // The writable session data dir (/runs/ato/session) is injected as a
        // non-readonly mount; the Landlock guest policy must allow writes there,
        // while a read-only injected mount must NOT become writable.
        let mut target = python_target(PathBuf::from("/host/src"));
        target.injected_mounts = vec![
            crate::launcher::InjectedMount {
                source: PathBuf::from("/host/session-data/run-1"),
                target: PathBuf::from("/runs/ato/session"),
                readonly: false,
            },
            crate::launcher::InjectedMount {
                source: PathBuf::from("/host/ro"),
                target: PathBuf::from("/ro-grant"),
                readonly: true,
            },
        ];
        let policy = guest_landlock_policy(&target);
        assert!(
            policy
                .read_write_paths
                .contains(&PathBuf::from("/runs/ato/session")),
            "writable injected mount must be in the Landlock write allowlist: {:?}",
            policy.read_write_paths
        );
        assert!(
            !policy
                .read_write_paths
                .contains(&PathBuf::from("/ro-grant")),
            "read-only injected mount must not be writable"
        );
    }

    #[test]
    fn guest_landlock_policy_serde_roundtrips() {
        // The shim reads the policy back from a JSON file bound into the sandbox.
        let target = python_target(PathBuf::from("/host/src"));
        let policy = guest_landlock_policy(&target);
        let json = serde_json::to_vec(&policy).expect("serialize");
        let back: crate::system::sandbox::SandboxPolicy =
            serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.read_only_paths, policy.read_only_paths);
        assert_eq!(back.read_write_paths, policy.read_write_paths);
        assert_eq!(back.ipc_socket_paths, policy.ipc_socket_paths);
        assert_eq!(back.allow_network, policy.allow_network);
    }
}
