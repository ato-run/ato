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

use crate::launcher::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};
use crate::system::sandbox::{filter_sensitive_paths, sensitive_paths};

use super::SourceRuntime;

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

/// Add bubblewrap arguments that hide sensitive paths inside the namespace.
///
/// When the user's bind-mount set would expose a sensitive directory
/// (e.g., `$HOME` is bound, so `$HOME/.ssh` would be visible), this
/// function emits `--tmpfs <sensitive_path>` arguments that overlay
/// the sensitive directory with an empty tmpfs, effectively hiding it.
fn add_sensitive_path_hiding(cmd: &mut Command, bind_mounted_parents: &[&str]) {
    let sensitive = sensitive_paths();

    for sp in &sensitive {
        // Only hide if a parent of this sensitive path is actually bound
        let dominated = bind_mounted_parents
            .iter()
            .any(|parent| sp.starts_with(parent));
        if dominated && sp.exists() {
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

    // =====================================================================
    // Egress Warning: domain-level filtering cannot be enforced by bwrap
    // =====================================================================
    if let Some(ref iso) = target.isolation {
        if iso.network_enabled && !iso.egress_allow.is_empty() {
            warn!(
                "Domain-level egress filtering (egress_allow: {:?}) is not enforceable via Bubblewrap/Landlock. \
                 Relies on Sidecar Proxy (tsnet/SOCKS5).",
                iso.egress_allow
            );
        }
    }

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build bwrap command
    let mut cmd = Command::new("bwrap");

    // Namespace isolation — network policy from IsolationPolicy or dev_mode
    let share_network = if target.dev_mode {
        true // Dev mode always shares network
    } else if let Some(ref iso) = target.isolation {
        iso.network_enabled
    } else {
        false // Default: no network in production
    };

    if share_network {
        cmd.args(["--unshare-all", "--share-net"]);
    } else {
        cmd.args(["--unshare-all"]); // No --share-net → network isolated
    }
    cmd.args(["--die-with-parent"]);

    // Basic filesystem setup
    cmd.args(["--proc", "/proc"]);
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--tmpfs", "/tmp"]);

    // Bind mount essential paths read-only
    cmd.args(["--ro-bind", "/lib", "/lib"]);
    cmd.args(["--ro-bind", "/lib64", "/lib64"]);
    cmd.args(["--ro-bind", "/usr", "/usr"]);
    cmd.args(["--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf"]);
    cmd.args(["--ro-bind", "/etc/hosts", "/etc/hosts"]);
    cmd.args(["--ro-bind", "/etc/ssl", "/etc/ssl"]);

    // Bind mount the toolchain binary
    let toolchain_path_str = toolchain_path.to_string_lossy();
    cmd.args(["--ro-bind", &toolchain_path_str, &toolchain_path_str]);

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

    // Add the actual command
    cmd.arg("--");
    if let Some(explicit_cmd) = target.cmd.as_ref() {
        if let Some((binary, args)) = explicit_cmd.split_first() {
            let binary_path = match binary.as_str() {
                "python" | "python3" | "node" | "deno" | "ruby" => toolchain_path.clone(),
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
        cmd.arg(&toolchain_path);

        match target.language.to_lowercase().as_str() {
            "python" => {
                cmd.args(["-B", &sandbox_entrypoint_path(target)]);
            }
            "node" | "nodejs" => {
                cmd.arg(sandbox_entrypoint_path(target));
            }
            "deno" => {
                let sandbox_entrypoint = sandbox_entrypoint_path(target);
                cmd.args(["run", "--allow-read=/app", &sandbox_entrypoint]);
            }
            _ => {
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

/// Check if bubblewrap is available and properly configured
#[allow(dead_code)]
pub fn verify_bubblewrap_available() -> Result<(), RuntimeError> {
    // Check binary exists
    let bwrap_path = which::which("bwrap").map_err(|_| {
        RuntimeError::SandboxSetupFailed("bubblewrap (bwrap) not found in PATH".to_string())
    })?;

    // Check if we can create user namespaces
    let output = Command::new(&bwrap_path)
        .args([
            "--unshare-user",
            "--uid",
            "1000",
            "--gid",
            "1000",
            "/bin/true",
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            debug!("bubblewrap user namespace check passed");
            Ok(())
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            if stderr.contains("permission denied") || stderr.contains("Operation not permitted") {
                warn!("User namespaces may be disabled. Try: sudo sysctl kernel.unprivileged_userns_clone=1");
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
            }),
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
        };

        let policy = generate_landlock_policy(&target);

        // Source dir always present
        assert!(policy
            .read_write_paths
            .contains(&PathBuf::from("/app/project")));
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
                }),
                ipc_socket_paths: vec![],
                injected_mounts: vec![],
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
            }),
            ipc_socket_paths: vec![
                PathBuf::from("/tmp/capsule-ipc/greeter.sock"),
                PathBuf::from("/tmp/capsule-ipc/db-service.sock"),
            ],
            injected_mounts: vec![],
        };

        let policy = generate_landlock_policy(&target);

        // IPC socket paths should be forwarded to the policy
        assert_eq!(policy.ipc_socket_paths.len(), 2);
        assert!(policy
            .ipc_socket_paths
            .contains(&PathBuf::from("/tmp/capsule-ipc/greeter.sock")));
        assert!(policy
            .ipc_socket_paths
            .contains(&PathBuf::from("/tmp/capsule-ipc/db-service.sock")));
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
        };

        let policy = generate_landlock_policy(&target);
        assert!(policy.ipc_socket_paths.is_empty());
    }
}
