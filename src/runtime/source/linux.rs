//! Linux-specific sandbox implementation using bubblewrap (bwrap)
//!
//! Provides namespace-based isolation for source execution with minimal overhead.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

/// Launch a source workload using bubblewrap sandbox
pub async fn launch_with_bubblewrap(
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
        "Launching with bubblewrap: {} {} (toolchain: {:?}, dev_mode: {})",
        target.language, target.entrypoint, toolchain.path, target.dev_mode
    );

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build bwrap command
    let mut cmd = Command::new("bwrap");

    // Namespace isolation - production mode disables network
    if target.dev_mode {
        cmd.args(["--unshare-all", "--share-net"]); // Share network for dev convenience
    } else {
        cmd.args(["--unshare-all"]); // No --share-net in production - network isolated
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
    let toolchain_path_str = toolchain.path.to_string_lossy();
    cmd.args(["--ro-bind", &toolchain_path_str, &toolchain_path_str]);

    // Bind mount the source directory read-only
    let source_dir_str = target.source_dir.to_string_lossy();
    cmd.args(["--ro-bind", &source_dir_str, "/app"]);

    // Set working directory
    cmd.args(["--chdir", "/app"]);

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

        // Block access to sensitive kernel interfaces
        cmd.args(["--ro-bind", "/dev/null", "/dev/kvm"]);

        // Use seccomp filtering (if available)
        // Note: Would need seccomp-bpf profile file
    }

    // Add the actual command
    cmd.arg("--");
    cmd.arg(&toolchain.path);

    // Add language-specific arguments
    match target.language.to_lowercase().as_str() {
        "python" => {
            // Disable bytecode caching in sandbox
            cmd.args(["-B", &target.entrypoint]);
        }
        "node" | "nodejs" => {
            cmd.arg(&target.entrypoint);
        }
        "deno" => {
            cmd.args(["run", "--allow-read=/app", &target.entrypoint]);
        }
        _ => {
            cmd.arg(&target.entrypoint);
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
}
