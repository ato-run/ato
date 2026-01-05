//! Windows-specific sandbox implementation
//!
//! Provides two sandbox approaches:
//! 1. Windows Sandbox - built-in ephemeral VM (Pro/Enterprise only)
//! 2. Sandboxie Plus - open-source sandbox (all editions, requires install)
//!
//! Reference:
//! - Windows Sandbox: https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/
//! - Sandboxie Plus: https://github.com/sandboxie-plus/Sandboxie

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

/// Sandboxie Plus installation guide URL
const SANDBOXIE_INSTALL_URL: &str = "https://github.com/sandboxie-plus/Sandboxie/releases/latest";

/// Windows Sandbox feature name
const WINDOWS_SANDBOX_FEATURE: &str = "Containers-DisposableClientVM";

/// Launch with native Windows sandbox
/// Priority: Windows Sandbox (if available) -> Sandboxie Plus -> Error
pub async fn launch_native_windows(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Try Windows Sandbox first (Pro/Enterprise)
    if is_windows_sandbox_available() {
        info!("Using Windows Sandbox for isolation");
        return launch_with_windows_sandbox(runtime, request, target).await;
    }

    // Try Sandboxie Plus (all editions)
    if is_sandboxie_available() {
        info!("Using Sandboxie Plus for isolation");
        return launch_with_sandboxie(runtime, request, target).await;
    }

    // No sandbox available
    Err(RuntimeError::SandboxSetupFailed(format!(
        "No Windows sandbox available. Options:\n\
         1. Enable Windows Sandbox (Pro/Enterprise): Settings > Apps > Optional Features\n\
         2. Install Sandboxie Plus (all editions): {}",
        SANDBOXIE_INSTALL_URL
    )))
}

/// Check if Windows Sandbox is enabled
/// Only available on Windows 10/11 Pro, Enterprise, Education
pub fn is_windows_sandbox_available() -> bool {
    // Check if WindowsSandbox.exe exists
    let sandbox_path = PathBuf::from(r"C:\Windows\System32\WindowsSandbox.exe");
    if !sandbox_path.exists() {
        return false;
    }

    // Verify the feature is enabled via PowerShell
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(Get-WindowsOptionalFeature -Online -FeatureName {}).State -eq 'Enabled'",
                WINDOWS_SANDBOX_FEATURE
            ),
        ])
        .output();

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            stdout.trim().eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

/// Check if Sandboxie Plus is installed
pub fn is_sandboxie_available() -> bool {
    // Check common installation paths
    let paths = [
        r"C:\Program Files\Sandboxie-Plus\Start.exe",
        r"C:\Program Files\Sandboxie\Start.exe",
    ];

    for path in paths {
        if PathBuf::from(path).exists() {
            return true;
        }
    }

    // Also check PATH
    which::which("Start.exe")
        .map(|p| p.to_string_lossy().contains("Sandboxie"))
        .unwrap_or(false)
}

/// Check if native sandbox is available on Windows
pub fn is_native_available() -> bool {
    is_windows_sandbox_available() || is_sandboxie_available()
}

/// Get Sandboxie Start.exe path
fn get_sandboxie_start_path() -> Option<PathBuf> {
    let paths = [
        r"C:\Program Files\Sandboxie-Plus\Start.exe",
        r"C:\Program Files\Sandboxie\Start.exe",
    ];

    for path in paths {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Launch using Windows Sandbox with .wsb configuration file
///
/// Creates an ephemeral sandbox VM with:
/// - Mapped folder (read-only source directory)
/// - LogonCommand to execute the script
/// - Network enabled (for dev mode)
async fn launch_with_windows_sandbox(
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
        "Launching with Windows Sandbox: {} {} (toolchain: {:?})",
        target.language, target.entrypoint, toolchain.path
    );

    // Ensure directories exist
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;
    std::fs::create_dir_all(&runtime.config.state_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.state_dir.clone(),
        source: e,
    })?;

    // Generate .wsb configuration file
    let wsb_content = generate_wsb_config(target, &toolchain.path);
    let wsb_path = runtime
        .config
        .state_dir
        .join(format!("{}.wsb", request.workload_id));

    std::fs::write(&wsb_path, &wsb_content).map_err(|e| RuntimeError::Io {
        path: wsb_path.clone(),
        source: e,
    })?;

    // Launch Windows Sandbox with the configuration
    let mut cmd = Command::new(&wsb_path);

    // Setup output redirection (limited for Windows Sandbox)
    let log_path = runtime.workload_log_path(request.workload_id);

    debug!("Launching Windows Sandbox with config: {:?}", wsb_path);

    // Spawn the process
    // Note: Windows Sandbox runs as a separate VM, PID tracking is limited
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "Windows Sandbox launch".to_string(),
        source: e,
    })?;

    let pid = child.id();
    info!(
        "Started Windows Sandbox for workload {}, host PID {}",
        request.workload_id, pid
    );

    // Track the workload
    {
        let mut workloads = runtime.active_workloads.lock().unwrap();
        workloads.insert(request.workload_id.to_string(), pid);
    }

    Ok(LaunchResult {
        pid: Some(pid),
        bundle_path: None,
        log_path: Some(log_path),
        port: None,
    })
}

/// Generate Windows Sandbox .wsb configuration file
///
/// Configuration options:
/// - MappedFolders: Share source directory read-only
/// - LogonCommand: Execute the script on sandbox startup
/// - Networking: Enable (for dev mode)
/// - vGPU: Disable (not needed for scripts)
fn generate_wsb_config(target: &SourceTarget, toolchain_path: &PathBuf) -> String {
    let source_dir = target.source_dir.to_string_lossy();
    let toolchain = toolchain_path.to_string_lossy();

    // Build the command to run inside sandbox
    let command = match target.language.to_lowercase().as_str() {
        "python" | "python3" => {
            format!("{} -B {}", toolchain, target.entrypoint)
        }
        "node" | "nodejs" => {
            format!("{} {}", toolchain, target.entrypoint)
        }
        "deno" => {
            format!("{} run --allow-read {}", toolchain, target.entrypoint)
        }
        _ => {
            format!("{} {}", toolchain, target.entrypoint)
        }
    };

    // Add user arguments
    let full_command = if target.args.is_empty() {
        command
    } else {
        format!("{} {}", command, target.args.join(" "))
    };

    format!(
        r#"<Configuration>
  <vGPU>Disable</vGPU>
  <Networking>Enable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{source_dir}</HostFolder>
      <SandboxFolder>C:\Source</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>cmd /c "cd C:\Source && {command}"</Command>
  </LogonCommand>
  <MemoryInMB>2048</MemoryInMB>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
</Configuration>"#,
        source_dir = source_dir,
        command = full_command.replace("\"", "&quot;")
    )
}

/// Launch using Sandboxie Plus
///
/// Uses Start.exe CLI:
/// - /box:BoxName - Specify sandbox name
/// - /wait - Wait for completion
/// - /hide_window - Hide sandbox window (optional)
async fn launch_with_sandboxie(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    let start_exe = get_sandboxie_start_path().ok_or_else(|| {
        RuntimeError::SandboxSetupFailed(format!(
            "Sandboxie Plus not found. Install from: {}",
            SANDBOXIE_INSTALL_URL
        ))
    })?;

    // Find toolchain binary
    let toolchain = runtime
        .toolchain_manager
        .find_toolchain(&target.language, target.version.as_deref())
        .ok_or_else(|| RuntimeError::ToolchainNotFound {
            language: target.language.clone(),
            version: target.version.clone(),
        })?;

    info!(
        "Launching with Sandboxie Plus: {} {} (toolchain: {:?})",
        target.language, target.entrypoint, toolchain.path
    );

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build Sandboxie command
    // Format: Start.exe /box:CapsuledBox /wait <program> <args>
    let box_name = format!("Capsuled_{}", &request.workload_id[..8.min(request.workload_id.len())]);

    let mut cmd = Command::new(&start_exe);
    cmd.arg(format!("/box:{}", box_name));
    cmd.arg("/silent"); // Suppress error dialogs

    // Add the toolchain
    cmd.arg(&toolchain.path);

    // Add language-specific arguments
    match target.language.to_lowercase().as_str() {
        "python" | "python3" => {
            cmd.args(["-B", &target.entrypoint]);
        }
        "deno" => {
            cmd.args(["run", "--allow-read", &target.entrypoint]);
        }
        _ => {
            cmd.arg(&target.entrypoint);
        }
    }

    // Add user-provided arguments
    cmd.args(&target.args);

    // Set working directory
    cmd.current_dir(&target.source_dir);

    // Setup output redirection
    let log_path = runtime.workload_log_path(request.workload_id);
    let log_file = std::fs::File::create(&log_path).map_err(|e| RuntimeError::Io {
        path: log_path.clone(),
        source: e,
    })?;

    cmd.stdout(Stdio::from(
        log_file.try_clone().map_err(|e| RuntimeError::Io {
            path: log_path.clone(),
            source: e,
        })?,
    ));
    cmd.stderr(Stdio::from(log_file));

    debug!("Executing Sandboxie command: {:?}", cmd);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| RuntimeError::CommandExecution {
        operation: "Sandboxie Start.exe spawn".to_string(),
        source: e,
    })?;

    let pid = child.id();
    info!(
        "Started source workload {} with Sandboxie Plus, PID {}",
        request.workload_id, pid
    );

    // Track the workload
    {
        let mut workloads = runtime.active_workloads.lock().unwrap();
        workloads.insert(request.workload_id.to_string(), pid);
    }

    Ok(LaunchResult {
        pid: Some(pid),
        bundle_path: None,
        log_path: Some(log_path),
        port: None,
    })
}

/// Stop a Sandboxie sandbox by name
pub fn stop_sandboxie_box(workload_id: &str) -> Result<(), RuntimeError> {
    let start_exe = match get_sandboxie_start_path() {
        Some(p) => p,
        None => return Ok(()), // Not installed, nothing to stop
    };

    let box_name = format!("Capsuled_{}", &workload_id[..8.min(workload_id.len())]);

    let output = Command::new(&start_exe)
        .args([&format!("/box:{}", box_name), "/terminate"])
        .output()
        .map_err(|e| RuntimeError::CommandExecution {
            operation: "Sandboxie terminate".to_string(),
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Failed to terminate Sandboxie box {}: {}", box_name, stderr);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wsb_config_generation() {
        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec!["--debug".to_string()],
            source_dir: PathBuf::from(r"C:\Users\test\project"),
        };
        let toolchain = PathBuf::from(r"C:\Python311\python.exe");

        let config = generate_wsb_config(&target, &toolchain);

        assert!(config.contains("<Configuration>"));
        assert!(config.contains("<vGPU>Disable</vGPU>"));
        assert!(config.contains(r"C:\Users\test\project"));
        assert!(config.contains("python.exe"));
        assert!(config.contains("--debug"));
    }

    #[test]
    fn test_sandboxie_path_check() {
        // This will likely be false on dev machines unless Sandboxie is installed
        let _available = is_sandboxie_available();
        // Just ensure the check doesn't panic
    }

    #[test]
    fn test_windows_sandbox_check() {
        // This will be false on non-Windows or Home editions
        let _available = is_windows_sandbox_available();
        // Just ensure the check doesn't panic
    }
}
