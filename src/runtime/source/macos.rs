//! macOS-specific sandbox implementation
//!
//! Provides two sandbox approaches:
//! 1. Alcoholless (alcless) - lightweight user-separation sandbox (preferred)
//! 2. sandbox-exec - built-in Seatbelt sandbox with dynamic profile generation (fallback)
//!
//! Reference:
//! - Alcoholless: https://github.com/AkihiroSuda/alcless
//! - sandbox-exec: https://igorstechnoclub.com/sandbox-exec/

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

/// Alcoholless installation guide URL
const ALCOHOLLESS_INSTALL_URL: &str = "https://github.com/AkihiroSuda/alcless#install";

/// Launch with native macOS sandbox (Alcoholless preferred, sandbox-exec fallback)
pub async fn launch_native_macos(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Try Alcoholless first (preferred)
    if is_alcoholless_available() {
        info!("Using Alcoholless sandbox for macOS");
        return launch_with_alcoholless(runtime, request, target).await;
    }

    // Fallback to sandbox-exec
    warn!(
        "Alcoholless not found. Install via: {}. Falling back to sandbox-exec.",
        ALCOHOLLESS_INSTALL_URL
    );
    launch_with_sandbox_exec(runtime, request, target).await
}

/// Check if Alcoholless (alcless) is installed
pub fn is_alcoholless_available() -> bool {
    which::which("alcless").is_ok() || which::which("alclessctl").is_ok()
}

/// Check if native sandbox is available on macOS
/// Returns true if either Alcoholless or sandbox-exec is available
pub fn is_native_available() -> bool {
    // sandbox-exec is always available on macOS (though deprecated)
    // Alcoholless is preferred but optional
    true
}

/// Launch using Alcoholless (alcless) sandbox
///
/// Alcoholless creates a separate user environment using sudo/su/pam_launchd
/// and syncs files back via rsync. Very low overhead, no VM/container.
///
/// Usage: `alcless <command>` or `alclessctl shell default -- <command>`
async fn launch_with_alcoholless(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Find toolchain binary
    let toolchain = runtime
        .toolchain_manager
        .find_toolchain(&target.language, target.version.as_deref())
        .ok_or_else(|| RuntimeError::ToolchainNotFound {
            language: target.language.clone(),
            version: target.version.clone(),
        })?;

    info!(
        "Launching with Alcoholless: {} {} (toolchain: {:?})",
        target.language, target.entrypoint, toolchain.path
    );

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build alcless command
    // alcless runs the command in a sandboxed user environment
    // Files in current directory are synced back on exit
    let mut cmd = Command::new("alcless");

    // Use --plain to skip directory rsync if source_dir is not writable
    if !target.source_dir.join(".alcless_writable").exists() {
        cmd.arg("--plain");
    }

    // Set working directory to source
    cmd.current_dir(&target.source_dir);

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
                toolchain.path.clone()
            } else {
                which::which(binary).unwrap_or_else(|_| PathBuf::from(binary))
            };
            cmd.arg(&binary_path);

            // For Python, add -B to disable bytecode caching
            if target.language == "python" {
                cmd.arg("-B");
            }

            cmd.args(args);
        }
    } else {
        // Legacy path: use toolchain + language-specific arguments
        cmd.arg(&toolchain.path);

        // Add language-specific arguments
        match target.language.to_lowercase().as_str() {
            "python" | "python3" => {
                cmd.args(["-B", &target.entrypoint]); // Disable bytecode caching
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

    debug!("Executing alcless command: {:?}", cmd);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "alcless spawn".to_string(),
        source: e,
    })?;

    let pid = child.id();
    info!(
        "Started source workload {} with Alcoholless, PID {}",
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
    // Find toolchain binary
    let toolchain = runtime
        .toolchain_manager
        .find_toolchain(&target.language, target.version.as_deref())
        .ok_or_else(|| RuntimeError::ToolchainNotFound {
            language: target.language.clone(),
            version: target.version.clone(),
        })?;

    info!(
        "Launching with sandbox-exec: {} {} (toolchain: {:?})",
        target.language, target.entrypoint, toolchain.path
    );

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Generate dynamic Seatbelt profile
    let profile = generate_seatbelt_profile(target, &toolchain.path);

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
                toolchain.path.clone()
            } else {
                which::which(binary).unwrap_or_else(|_| PathBuf::from(binary))
            };
            cmd.arg(&binary_path);

            // For Python, add -B to disable bytecode caching
            if target.language == "python" {
                cmd.arg("-B");
            }

            cmd.args(args);
        }
    } else {
        // Legacy path: use toolchain + language-specific arguments
        cmd.arg(&toolchain.path);

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

    // Set working directory
    cmd.current_dir(&target.source_dir);

    // Set environment variables (inherit from manifest/runplan)
    // Note: sandbox-exec inherits parent env, but we explicitly set user-defined vars
    if let Some(manifest_json) = request.manifest_json {
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(manifest_json) {
            if let Some(env_obj) = manifest
                .get("execution")
                .and_then(|e| e.get("env"))
                .and_then(|e| e.as_object())
            {
                for (k, v) in env_obj {
                    if let Some(v_str) = v.as_str() {
                        cmd.env(k, v_str);
                    }
                }
            }
        }
    }
    // Explicitly set PORT if allocated (from HOST_PORT in manifest env)
    if let Some(manifest_json) = request.manifest_json {
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(manifest_json) {
            if let Some(port) = manifest
                .get("execution")
                .and_then(|e| e.get("env"))
                .and_then(|e| e.get("HOST_PORT"))
                .and_then(|p| p.as_str())
            {
                cmd.env("PORT", port);
            }
        }
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

/// Generate a dynamic Seatbelt profile for sandbox-exec
///
/// Profile structure:
/// - (version 1) - Required version declaration
/// - For dev mode: allow default for simplicity
/// - For production mode: deny default with explicit allowlist
fn generate_seatbelt_profile(target: &SourceTarget, toolchain_path: &std::path::Path) -> String {
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
        // Production mode: strict deny-default profile with minimal allowlist
        let source_dir = target.source_dir.to_string_lossy();
        let toolchain_dir = toolchain_path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/usr/bin".to_string());

        format!(
            r#"(version 1)
(deny default)

; Allow essential process operations
(allow process-fork)
(allow process-exec)
(allow signal (target self))

; Allow read access to the capsule source directory (read-only)
(allow file-read* (subpath "{source_dir}"))

; Allow read access to the toolchain binary and its directory
(allow file-read* (subpath "{toolchain_dir}"))
(allow file-read* (literal "{toolchain}"))

; Allow read access to essential system libraries
(allow file-read* (subpath "/usr/lib"))
(allow file-read* (subpath "/System/Library/Frameworks"))
(allow file-read* (subpath "/Library/Frameworks"))
(allow file-read* (subpath "/opt/homebrew"))
(allow file-read* (subpath "/usr/local"))
(allow file-read* (regex "^/usr/share/.*"))

; Allow read access to SSL certificates
(allow file-read* (subpath "/etc/ssl"))
(allow file-read* (subpath "/private/etc/ssl"))

; Allow read access to essential runtime files
(allow file-read* (literal "/dev/null"))
(allow file-read* (literal "/dev/urandom"))
(allow file-read* (literal "/dev/random"))
(allow file-write* (literal "/dev/null"))

; Allow tmp directory access (write)
(allow file-read* (subpath "/tmp"))
(allow file-read* (subpath "/private/tmp"))
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))

; Allow network access only if needed (commented out by default)
; To enable: uncomment the following line
; (allow network*)

; Allow mach ports for basic IPC
(allow mach-lookup)

; Allow sysctl read for basic system info
(allow sysctl-read)

; Deny all network by default in production
(deny network*)
"#,
            source_dir = source_dir,
            toolchain_dir = toolchain_dir,
            toolchain = toolchain_path.to_string_lossy()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_available() {
        // sandbox-exec is always available on macOS
        assert!(is_native_available());
    }

    #[test]
    fn test_alcoholless_check() {
        // This will be false on most dev machines unless alcless is installed
        let _available = is_alcoholless_available();
        // Just ensure the check doesn't panic
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
            cmd: None,
            dev_mode: true,
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
            cmd: None,
            dev_mode: false,
        };
        let toolchain = PathBuf::from("/usr/bin/python3");

        let profile = generate_seatbelt_profile(&target, &toolchain);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("/Users/test/project"));
        assert!(profile.contains("/usr/bin"));
        assert!(profile.contains("(deny network*)"));
    }
}
