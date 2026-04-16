//! macOS-specific sandbox implementation
//!
//! Provides sandbox-exec based Seatbelt sandboxing for source workloads.
//!
//! Reference:
//! - sandbox-exec: https://igorstechnoclub.com/sandbox-exec/

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::launcher::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

fn requested_host_cwd(target: &SourceTarget) -> PathBuf {
    target
        .requested_cwd
        .clone()
        .unwrap_or_else(|| target.source_dir.clone())
}

fn source_entrypoint_host_path(target: &SourceTarget) -> PathBuf {
    let entrypoint = PathBuf::from(&target.entrypoint);
    if entrypoint.is_absolute() {
        entrypoint
    } else {
        target.source_dir.join(entrypoint)
    }
}

fn allowed_mount_host_path(mount: &crate::launcher::InjectedMount) -> PathBuf {
    if mount.source.exists() || mount.source.is_dir() {
        return mount.source.clone();
    }

    mount
        .source
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| mount.source.clone())
}

/// Resolve the executable and arguments based on target.cmd or language detection
///
/// This function handles various scenarios:
/// 1. Explicit cmd (Generic Source Runtime): Use cmd[0] as executable, cmd[1..] as args
/// 2. Known language with toolchain: Use JIT-provisioned toolchain binary
/// 3. Unknown command: Try to find it in PATH
///
/// Returns (executable_path, arguments)
async fn resolve_executable_and_args(
    runtime: &SourceRuntime,
    target: &SourceTarget,
) -> Result<(PathBuf, Vec<String>), RuntimeError> {
    // Case 1: Explicit cmd provided (Generic Source Runtime)
    if let Some(ref explicit_cmd) = target.cmd {
        if let Some((binary, args)) = explicit_cmd.split_first() {
            // Check if the binary needs JIT provisioning (python, node, etc.)
            let executable = resolve_binary(runtime, target, binary).await?;

            // Prepare arguments
            let mut final_args: Vec<String> = Vec::new();

            // Add -B (disable bytecode caching) only for the Python interpreter itself,
            // not for Python ecosystem tools like uv, pip, etc.
            if target.language == "python"
                && is_python_interpreter(binary)
                && !args.iter().any(|a| a == "-B")
            {
                final_args.push("-B".to_string());
            }

            let mut entrypoint_rewritten = false;
            for arg in args {
                if !entrypoint_rewritten && arg == &target.entrypoint {
                    final_args.push(source_entrypoint_host_path(target).display().to_string());
                    entrypoint_rewritten = true;
                } else {
                    final_args.push(arg.clone());
                }
            }

            return Ok((executable, final_args));
        }
    }

    // Case 2: No explicit cmd, use language + entrypoint
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

    let args = match target.language.to_lowercase().as_str() {
        "python" | "python3" => vec![
            "-B".to_string(),
            source_entrypoint_host_path(target).display().to_string(),
        ],
        "deno" => vec![
            "run".to_string(),
            "--allow-read=.".to_string(),
            source_entrypoint_host_path(target).display().to_string(),
        ],
        _ => vec![source_entrypoint_host_path(target).display().to_string()],
    };

    Ok((toolchain_path, args))
}

/// Check if the binary name is a Python interpreter (not a tool like uv/pip)
///
/// `-B` (disable .pyc bytecode caching) is a Python interpreter flag and must
/// NOT be passed to ecosystem tools like `uv`, `pip`, `pipx`, etc.
fn is_python_interpreter(binary: &str) -> bool {
    let basename = std::path::Path::new(binary)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| binary.to_string());
    matches!(
        basename.to_lowercase().as_str(),
        "python"
            | "python3"
            | "python3.10"
            | "python3.11"
            | "python3.12"
            | "python3.13"
            | "python3.14"
    )
}

/// Resolve a binary name to an executable path
///
/// Priority:
/// 1. If it's a known runtime (python, node), use JIT provisioning
/// 2. If it's a tool that depends on a runtime (npm → node), ensure the runtime first
/// 3. Otherwise, look for it in PATH
async fn resolve_binary(
    runtime: &SourceRuntime,
    target: &SourceTarget,
    binary: &str,
) -> Result<PathBuf, RuntimeError> {
    let normalized = binary.to_lowercase();

    // Known language runtimes that can be JIT-provisioned
    match normalized.as_str() {
        "python" | "python3" => {
            return runtime.ensure_toolchain(target).await.map_err(|e| {
                RuntimeError::ToolchainError {
                    message: "Failed to ensure python toolchain".to_string(),
                    technical_reason: Some(e.to_string()),
                    cloud_upsell: None,
                }
            });
        }
        "node" => {
            return runtime.ensure_toolchain(target).await.map_err(|e| {
                RuntimeError::ToolchainError {
                    message: "Failed to ensure node toolchain".to_string(),
                    technical_reason: Some(e.to_string()),
                    cloud_upsell: None,
                }
            });
        }
        _ => {}
    }

    // For npm, yarn, pnpm, etc. - these are found via PATH but need node available
    if matches!(normalized.as_str(), "npm" | "yarn" | "pnpm" | "npx") {
        // Ensure node is available first (for proper npm execution)
        let _ = runtime.ensure_toolchain(target).await;
        // But use the system npm/yarn/pnpm
    }

    // Look for the binary in PATH
    which::which(binary).map_err(|_| RuntimeError::BinaryNotFound {
        tried: vec![binary.to_string()],
    })
}

/// Launch with native macOS sandbox.
/// In dev_mode, skip sandbox entirely for better debugging and log forwarding
pub async fn launch_native_macos(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Dev mode: skip sandbox for better debugging and log forwarding
    if target.dev_mode || runtime.config.dev_mode {
        info!("Dev mode: skipping sandbox, launching directly");
        return launch_direct(runtime, request, target).await;
    }

    if is_seatbelt_available() {
        if !target.ipc_socket_paths.is_empty() {
            info!("Using sandbox-exec for macOS IPC socket passthrough");
        }
        return launch_with_sandbox_exec(runtime, request, target).await;
    }

    Err(RuntimeError::SandboxSetupFailed(
        "No macOS sandbox backend available. Restore sandbox-exec.".to_string(),
    ))
}

/// Check if sandbox-exec (Seatbelt launcher) is available.
pub fn is_seatbelt_available() -> bool {
    which::which("sandbox-exec").is_ok()
}

/// Check if native sandbox is available on macOS
#[cfg_attr(not(test), allow(dead_code))]
pub fn is_native_available() -> bool {
    is_seatbelt_available()
}

/// Launch using sandbox-exec with dynamic Seatbelt profile
///
/// sandbox-exec is deprecated but still functional on macOS.
/// We generate a minimal profile that:
/// - Denies most access by default
/// - Allows read-only access to source directory
/// - Allows write access to /tmp
/// - Allows network access (configurable)
async fn launch_with_sandbox_exec(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Find toolchain binary with JIT provisioning
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
        "Launching with sandbox-exec: {} {} (toolchain: {:?})",
        target.language, target.entrypoint, toolchain_path
    );

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Generate dynamic Seatbelt profile
    let profile = generate_seatbelt_profile(target, &toolchain_path);

    // Write profile to temp file
    let profile_path = runtime
        .config
        .state_dir
        .join(format!("{}.sb", request.workload_id));
    std::fs::create_dir_all(&runtime.config.state_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.state_dir.clone(),
        source: e,
    })?;
    std::fs::write(&profile_path, &profile).map_err(|e| RuntimeError::Io {
        path: profile_path.clone(),
        source: e,
    })?;

    // Build sandbox-exec command
    let mut cmd = Command::new("sandbox-exec");
    cmd.args(["-f", &profile_path.to_string_lossy()]);

    // Check if explicit cmd is provided (Generic Source Runtime)
    if let Some(ref explicit_cmd) = target.cmd {
        // Use explicit command directly
        // The first element is the binary, rest are arguments
        if let Some((binary, args)) = explicit_cmd.split_first() {
            // Find the actual binary path using toolchain manager or PATH
            let binary_path = if binary == &target.language
                || binary == "python"
                || binary == "python3"
                || binary == "node"
                || binary == "ruby"
                || binary == "deno"
            {
                toolchain_path.clone()
            } else {
                which::which(binary).unwrap_or_else(|_| PathBuf::from(binary))
            };
            cmd.arg(&binary_path);

            // For Python interpreter, add -B to disable bytecode caching
            // Skip for tools like uv, pip that are not the interpreter itself
            if target.language == "python" && is_python_interpreter(binary) {
                cmd.arg("-B");
            }

            cmd.args(args);
        }
    } else {
        // Legacy path: use toolchain + language-specific arguments
        cmd.arg(&toolchain_path);

        // Add language-specific arguments
        match target.language.to_lowercase().as_str() {
            "python" | "python3" => {
                cmd.args(["-B", &target.entrypoint]);
            }
            "deno" => {
                cmd.args(["run", "--allow-read=.", &target.entrypoint]);
            }
            _ => {
                cmd.arg(&target.entrypoint);
            }
        }
    }

    // Add user-provided arguments
    cmd.args(&target.args);

    // Set working directory (canonicalize for macOS symlink resolution)
    // e.g., /tmp -> /private/tmp
    let canonical_cwd = requested_host_cwd(target)
        .canonicalize()
        .unwrap_or_else(|_| requested_host_cwd(target));
    cmd.current_dir(&canonical_cwd);

    // Apply user-provided environment variables
    if let Some(ref envs) = request.env {
        for (key, value) in envs {
            cmd.env(key, value);
        }
    }

    // Apply sidecar (SOCKS5 proxy) environment variables
    runtime.apply_sidecar_env(&mut cmd);

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
        info!(
            "Socket Activation: Passing FD {} to child process",
            crate::manager::socket::SD_LISTEN_FDS_START
        );
    }

    debug!("Executing sandbox-exec command: {:?}", cmd);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "sandbox-exec spawn".to_string(),
        source: e,
    })?;

    let pid = child.id();
    info!(
        "Started source workload {} with sandbox-exec, PID {}",
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

/// Launch directly without sandbox (dev mode only)
///
/// This is used in development mode for:
/// - Better debugging experience
/// - Real-time log forwarding to parent process (nacelle)
/// - Simpler process management
///
/// Uses `tokio::process` for non-blocking I/O and proper async waiting.
async fn launch_direct(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    use std::process::Stdio;
    use tokio::process::Command as TokioCommand;

    // Determine the executable and arguments based on cmd or language
    let (executable, args) = resolve_executable_and_args(runtime, target).await?;

    info!("Launching directly (dev mode): {:?} {:?}", executable, args);

    // Build command using tokio::process::Command for async I/O
    let mut cmd = TokioCommand::new(&executable);

    // Set working directory to source
    cmd.current_dir(requested_host_cwd(target));

    // Add arguments
    cmd.args(&args);

    // Add user-provided arguments
    cmd.args(&target.args);

    // Apply user-provided environment variables
    if let Some(ref envs) = request.env {
        for (key, value) in envs {
            cmd.env(key, value);
        }
    }

    // CRITICAL: Use piped stdout/stderr for log forwarding
    // nacelle will forward these to its own stderr so Ato Desktop can capture them
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Prevent child from being killed when nacelle's stdin closes
    cmd.kill_on_drop(false);

    debug!("Executing direct command (tokio): {:?}", cmd);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "direct spawn (tokio)".to_string(),
        source: e,
    })?;

    let pid = child.id().ok_or_else(|| RuntimeError::CommandExecution {
        operation: "get pid".to_string(),
        source: std::io::Error::other("Failed to get child PID"),
    })?;

    info!(
        "Started source workload {} directly (dev mode, async), PID {}",
        request.workload_id, pid
    );

    // Track the workload (PID for quick lookup)
    {
        let mut workloads = runtime.active_workloads.lock().unwrap();
        workloads.insert(request.workload_id.to_string(), pid);
    }

    // Register async child handle for supervisor mode
    runtime
        .register_async_child(request.workload_id.to_string(), child)
        .await;

    Ok(LaunchResult {
        pid: Some(pid),
        bundle_path: None,
        log_path: None, // No log file in direct mode - logs forwarded via pipes
        port: None,
    })
}

/// Generate a dynamic Seatbelt profile for sandbox-exec
///
/// Profile structure:
/// - (version 1) - Required version declaration
/// - For dev mode: allow default for simplicity
/// - For production mode: deny default with explicit allowlist based on IsolationPolicy
fn generate_seatbelt_profile(target: &SourceTarget, toolchain_path: &std::path::Path) -> String {
    // Get isolation policy from target, or use default
    let isolation = target.isolation.as_ref();

    if target.dev_mode {
        // Development mode: permissive profile for debugging and rapid iteration
        r#"(version 1)
(allow default)

; Even in dev mode, protect critical system paths
(deny file-write* (subpath "/System") (with send-signal SIGKILL))
(deny file-write* (subpath "/Library") (with send-signal SIGKILL))
"#
        .to_string()
    } else {
        // Production mode: build profile from IsolationPolicy
        generate_production_seatbelt_profile(target, toolchain_path, isolation)
    }
}

/// Generate production Seatbelt profile from IsolationPolicy
///
/// Strategy: "Allow by default, deny sensitive paths"
/// This approach is more practical for complex runtimes (Python, Node.js)
/// that need access to many system resources.
fn generate_production_seatbelt_profile(
    target: &SourceTarget,
    _toolchain_path: &std::path::Path,
    isolation: Option<&crate::launcher::IsolationPolicy>,
) -> String {
    let source_dir = target.source_dir.to_string_lossy();

    let mut profile = String::new();

    // =========================================================================
    // Version declaration
    // =========================================================================
    profile.push_str("(version 1)\n\n");

    // =========================================================================
    // Default policy: Allow most operations
    // This is more practical than (deny default) for complex runtimes
    // =========================================================================
    profile.push_str("; Default: allow most operations, deny specific dangerous paths\n");
    profile.push_str("(allow default)\n\n");

    // =========================================================================
    // Debug: Trace denied operations to system log
    // View with: log stream --predicate 'process == "sandboxd"'
    // =========================================================================
    profile.push_str("; Debug: log denied operations\n");
    profile.push_str("(trace deny)\n\n");

    // =========================================================================
    // Capsule source directory - explicitly allow (for clarity)
    // =========================================================================
    profile.push_str("; Capsule source directory (always allowed)\n");
    profile.push_str(&format!(
        "(allow file-read* file-write* (subpath \"{}\"))\n\n",
        source_dir
    ));

    // =========================================================================
    // Apply IsolationPolicy from capsule.toml
    // =========================================================================
    if let Some(iso) = isolation {
        // Read-only paths from policy
        if !iso.read_only_paths.is_empty() {
            profile.push_str("; Read-only paths from capsule.toml\n");
            for path in &iso.read_only_paths {
                if let Some(escaped) = escape_path_for_sbpl(path) {
                    profile.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", escaped));
                }
            }
            profile.push('\n');
        }

        // Read-write paths from policy
        if !iso.read_write_paths.is_empty() {
            profile.push_str("; Read-write paths from capsule.toml\n");
            for path in &iso.read_write_paths {
                if let Some(escaped) = escape_path_for_sbpl(path) {
                    profile.push_str(&format!(
                        "(allow file-read* file-write* (subpath \"{}\"))\n",
                        escaped
                    ));
                }
            }
            profile.push('\n');
        }

        // Network policy
        if !iso.network_enabled {
            profile.push_str(
                "; Network DENIED (from capsule.toml [isolation.network.enabled = false])\n",
            );
            profile.push_str("(deny network*)\n\n");
        } else if !iso.egress_allow.is_empty() {
            // Network enabled with domain restrictions – Seatbelt cannot
            // enforce domain-level filtering (only IP-based). The actual
            // filtering is delegated to the Sidecar Proxy (tsnet/SOCKS5).
            profile
                .push_str(";; Note: egress_allow domains are handled by Sidecar Proxy (tsnet),\n");
            profile.push_str(";; not enforced at the Seatbelt sandbox level.\n");
            profile.push_str(&format!(";; Configured domains: {:?}\n", iso.egress_allow));
            profile.push_str("(allow network*)\n\n");
            warn!(
                "Domain-level egress filtering (egress_allow: {:?}) is not enforceable via Seatbelt. Relies on Sidecar Proxy.",
                iso.egress_allow
            );
        }
        // else: network enabled, no egress restrictions → full network via (allow default)
    } else {
        // Default behavior: network denied for safety
        profile.push_str("; Network DENIED (default when no [isolation] section)\n");
        profile.push_str("(deny network*)\n\n");
    }

    // =========================================================================
    // IPC socket paths (injected by ato-cli IPC Broker)
    // =========================================================================
    if !target.ipc_socket_paths.is_empty() {
        profile.push_str("; IPC socket paths (ato-cli IPC Broker)\n");
        for path in &target.ipc_socket_paths {
            if let Some(escaped) = escape_path_for_sbpl(path) {
                profile.push_str(&format!(
                    "(allow file-read* file-write* (subpath \"{}\"))\n",
                    escaped
                ));
            } else if let Some(parent) = path.parent() {
                // Socket may not exist yet; allow the parent directory
                if let Some(escaped_parent) = escape_path_for_sbpl(parent) {
                    profile.push_str(&format!(
                        "(allow file-read* file-write* (subpath \"{}\"))\n",
                        escaped_parent
                    ));
                }
            }
        }
        profile.push('\n');
    }

    if !target.injected_mounts.is_empty() {
        profile.push_str("; Injected sandbox mounts\n");
        for mount in &target.injected_mounts {
            let allowed_path = allowed_mount_host_path(mount);
            if let Some(escaped) = escape_path_for_sbpl(&allowed_path) {
                if mount.readonly {
                    profile.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", escaped));
                } else {
                    profile.push_str(&format!(
                        "(allow file-read* file-write* (subpath \"{}\"))\n",
                        escaped
                    ));
                }
            }
        }
        profile.push('\n');
    }

    // =========================================================================
    // PTY device access (interactive terminal sessions)
    // =========================================================================
    if target.interactive {
        profile.push_str("; PTY device access (interactive terminal)\n");
        profile.push_str("(allow file-read* file-write* (subpath \"/dev/ptmx\"))\n");
        profile.push_str("(allow file-read* file-write* (subpath \"/dev/pty\"))\n");
        profile.push_str("(allow file-read* file-write* (regex #\"^/dev/tty[a-z0-9]+\"))\n");
        profile.push_str("(allow ioctl*)\n");
        profile.push('\n');
    }

    // =========================================================================
    // CRITICAL: Deny access to sensitive user paths
    // Uses the shared sensitive_paths() from system::sandbox
    // This is the core security boundary – protect user's secrets
    // =========================================================================
    let sensitive = crate::system::sandbox::sensitive_paths();
    if !sensitive.is_empty() {
        profile.push_str("; SECURITY: Deny access to sensitive user directories\n");
        profile.push_str("(deny file-read* file-write*\n");
        for path in &sensitive {
            let path_str = path.to_string_lossy();
            let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
            profile.push_str(&format!("    (subpath \"{}\")\n", escaped));
        }
        profile.push_str(")\n");
    }

    profile
}

/// Escape path for SBPL profile (resolve symlinks, escape special chars)
fn escape_path_for_sbpl(path: &std::path::Path) -> Option<String> {
    // Try to canonicalize (resolve symlinks like /tmp -> /private/tmp on macOS)
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path_str = resolved.to_str()?;

    // Escape special characters for SBPL
    let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
    Some(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::IsolationPolicy;
    use crate::launcher::LaunchRequest;
    use tempfile::TempDir;

    #[test]
    fn test_native_available() {
        // sandbox-exec is always available on macOS
        assert!(is_native_available());
    }

    #[test]
    fn test_seatbelt_profile_generation_dev_mode() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/Users/test/project"),
            requested_cwd: None,
            cmd: None,
            dev_mode: true,
            isolation: None,
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };
        let toolchain = PathBuf::from("/usr/bin/python3");

        let profile = generate_seatbelt_profile(&target, &toolchain);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(allow default)"));
    }

    #[test]
    fn test_seatbelt_profile_generation_production_mode() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/Users/test/project"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: None,
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };
        let toolchain = PathBuf::from("/usr/bin/python3");

        let profile = generate_seatbelt_profile(&target, &toolchain);

        assert!(profile.contains("(version 1)"));
        // New approach: allow default, deny specific paths
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("/Users/test/project"));
        // Default policy denies network
        assert!(profile.contains("(deny network*)"));
        // Check for trace directive
        assert!(profile.contains("(trace deny)"));
        // Check sensitive paths are denied
        assert!(profile.contains("/.ssh"));
    }

    #[test]
    fn test_seatbelt_profile_with_isolation_policy() {
        let isolation = IsolationPolicy {
            sandbox_enabled: true,
            read_only_paths: vec![PathBuf::from("/data/readonly")],
            read_write_paths: vec![PathBuf::from("/data/writable")],
            network_enabled: true,
            egress_allow: vec!["api.example.com".to_string()],
        };

        let target = SourceTarget {
            language: "node".to_string(),
            version: Some("20".to_string()),
            entrypoint: "index.js".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/Users/test/app"),
            requested_cwd: None,
            cmd: Some(vec![
                "npm".to_string(),
                "run".to_string(),
                "dev".to_string(),
            ]),
            dev_mode: false,
            isolation: Some(isolation),
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };
        let toolchain = PathBuf::from("/usr/local/bin/node");

        let profile = generate_seatbelt_profile(&target, &toolchain);

        // New approach: allow default, network not denied when enabled
        assert!(profile.contains("(allow default)"));
        // When network_enabled = true, there's no deny network* line
        assert!(!profile.contains("(deny network*)"));
        // Check ato source dir
        assert!(profile.contains("/Users/test/app"));
        // Check sensitive paths are denied
        assert!(profile.contains("/.ssh"));
        assert!(profile.contains("/.aws"));
        // Check egress_allow generates a sidecar proxy comment
        assert!(
            profile.contains("egress_allow domains are handled by Sidecar Proxy"),
            "Profile should contain egress_allow sidecar note"
        );
        assert!(
            profile.contains("api.example.com"),
            "Profile should list the configured domain in comments"
        );
    }

    #[test]
    fn test_seatbelt_profile_network_enabled_no_egress() {
        // network_enabled=true but egress_allow is empty -> no warning/comment
        let isolation = IsolationPolicy {
            sandbox_enabled: true,
            read_only_paths: vec![],
            read_write_paths: vec![],
            network_enabled: true,
            egress_allow: vec![],
        };

        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/Users/test/app"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: Some(isolation),
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };
        let toolchain = PathBuf::from("/usr/bin/python3");

        let profile = generate_seatbelt_profile(&target, &toolchain);

        // No deny network, no egress comment
        assert!(!profile.contains("(deny network*)"));
        assert!(!profile.contains("egress_allow"));
    }

    #[test]
    fn test_seatbelt_profile_includes_ipc_socket_parent_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("capsule-ipc").join("service.sock");

        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/Users/test/app"),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: None,
            ipc_socket_paths: vec![socket_path.clone()],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };
        let toolchain = PathBuf::from("/usr/bin/python3");

        let profile = generate_seatbelt_profile(&target, &toolchain);
        let parent = socket_path.parent().unwrap().to_string_lossy().to_string();

        assert!(
            profile.contains(&parent),
            "profile should allow IPC parent directory: {profile}"
        );
    }

    #[test]
    fn test_escape_path_for_sbpl() {
        // Test basic path
        let path = PathBuf::from("/tmp/test");
        let escaped = escape_path_for_sbpl(&path);
        assert!(escaped.is_some());

        // Test path with quotes (edge case)
        let path_with_quotes = PathBuf::from("/tmp/test\"file");
        if let Some(escaped) = escape_path_for_sbpl(&path_with_quotes) {
            assert!(escaped.contains("\\\""));
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_sandbox_exec_launches_python_workload() {
        if !is_seatbelt_available() || which::which("python3").is_err() {
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        let log_dir = temp_dir.path().join("logs");
        let state_dir = temp_dir.path().join("state");
        std::fs::create_dir_all(&source_dir).unwrap();

        let output_path = source_dir.join("sandbox-output.txt");
        let script_path = source_dir.join("main.py");
        std::fs::write(
            &script_path,
            format!(
                "from pathlib import Path\nPath({:?}).write_text('ok', encoding='utf-8')\n",
                output_path
            ),
        )
        .unwrap();

        let runtime = SourceRuntime::new(crate::launcher::source::SourceRuntimeConfig {
            dev_mode: false,
            log_dir,
            state_dir,
            sidecar_config: None,
        });

        let target = SourceTarget {
            language: "python".to_string(),
            version: None,
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: source_dir.clone(),
            requested_cwd: None,
            cmd: None,
            dev_mode: false,
            isolation: Some(IsolationPolicy {
                sandbox_enabled: true,
                read_only_paths: vec![],
                read_write_paths: vec![],
                network_enabled: false,
                egress_allow: vec![],
            }),
            ipc_socket_paths: vec![],
            injected_mounts: vec![],
            interactive: false,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_shell: None,
            terminal_env_filter: "safe".to_string(),
        };

        let request = LaunchRequest {
            workload_id: "macos-seatbelt-smoke",
            bundle_root: source_dir.clone(),
            env: None,
            args: None,
            source_target: Some(target.clone()),
            socket_manager: None,
        };

        let result = launch_native_macos(&runtime, &request, &target)
            .await
            .unwrap();
        assert!(result.pid.is_some());

        let mut child = runtime.take_child("macos-seatbelt-smoke").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "sandboxed process failed: {status:?}");
        assert_eq!(std::fs::read_to_string(output_path).unwrap(), "ok");
    }
}
