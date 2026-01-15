//! OCI container fallback for source execution
//!
//! When native sandbox is not available or fails, we fall back to running
//! source code inside OCI containers with the appropriate runtime images.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

/// OCI image mappings for various languages
const PYTHON_IMAGE: &str = "python:3.11-slim";
const NODE_IMAGE: &str = "node:20-slim";
const DENO_IMAGE: &str = "denoland/deno:latest";
const RUBY_IMAGE: &str = "ruby:3.2-slim";
const PERL_IMAGE: &str = "perl:5.38-slim";

/// Launch source workload using OCI container
pub async fn launch_with_oci(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    // Verify OCI fallback is configured (used for future YoukiRuntimeAdapter integration)
    let _oci_runtime = runtime.oci_fallback.as_ref().ok_or_else(|| {
        RuntimeError::SandboxSetupFailed("OCI runtime not configured".to_string())
    })?;

    info!(
        "Launching with OCI fallback: {} {}",
        target.language, target.entrypoint
    );

    // Select the appropriate image
    let image = select_image_for_language(&target.language, target.version.as_deref());

    // Ensure log directory exists
    std::fs::create_dir_all(&runtime.config.log_dir).map_err(|e| RuntimeError::Io {
        path: runtime.config.log_dir.clone(),
        source: e,
    })?;

    // Build container command
    let entrypoint = build_entrypoint(&target.language, &target.entrypoint, &target.args);

    // Create container spec
    let container_name = format!("nacelle-source-{}", request.workload_id);

    // For OCI fallback, we use docker/podman CLI directly
    // This is simpler than going through the full OCI spec for dev scenarios
    let result = launch_with_container_cli(
        runtime,
        request,
        target,
        &image,
        &container_name,
        &entrypoint,
    )
    .await?;

    Ok(result)
}

/// Select the appropriate OCI image for a language
fn select_image_for_language(language: &str, version: Option<&str>) -> String {
    match language.to_lowercase().as_str() {
        "python" | "python3" => {
            if let Some(v) = version {
                if v.starts_with("3.") {
                    format!("python:{}-slim", v)
                } else {
                    PYTHON_IMAGE.to_string()
                }
            } else {
                PYTHON_IMAGE.to_string()
            }
        }
        "node" | "nodejs" => {
            if let Some(v) = version {
                format!("node:{}-slim", v.split('.').next().unwrap_or("20"))
            } else {
                NODE_IMAGE.to_string()
            }
        }
        "deno" => DENO_IMAGE.to_string(),
        "ruby" => {
            if let Some(v) = version {
                format!("ruby:{}-slim", v)
            } else {
                RUBY_IMAGE.to_string()
            }
        }
        "perl" => PERL_IMAGE.to_string(),
        other => {
            warn!("Unknown language '{}', using alpine with shell", other);
            "alpine:latest".to_string()
        }
    }
}

/// Build the entrypoint command for a container
fn build_entrypoint(language: &str, entrypoint: &str, args: &[String]) -> Vec<String> {
    let mut cmd = Vec::new();

    match language.to_lowercase().as_str() {
        "python" | "python3" => {
            cmd.push("python3".to_string());
            cmd.push("-u".to_string()); // Unbuffered output
            cmd.push(entrypoint.to_string());
        }
        "node" | "nodejs" => {
            cmd.push("node".to_string());
            cmd.push(entrypoint.to_string());
        }
        "deno" => {
            cmd.push("deno".to_string());
            cmd.push("run".to_string());
            cmd.push("--allow-read".to_string());
            cmd.push(entrypoint.to_string());
        }
        "ruby" => {
            cmd.push("ruby".to_string());
            cmd.push(entrypoint.to_string());
        }
        "perl" => {
            cmd.push("perl".to_string());
            cmd.push(entrypoint.to_string());
        }
        _ => {
            // Try to run as shell script
            cmd.push("/bin/sh".to_string());
            cmd.push(entrypoint.to_string());
        }
    }

    cmd.extend(args.iter().cloned());
    cmd
}

/// Launch using docker or podman CLI
async fn launch_with_container_cli(
    runtime: &SourceRuntime,
    request: &LaunchRequest<'_>,
    target: &SourceTarget,
    image: &str,
    container_name: &str,
    entrypoint: &[String],
) -> Result<LaunchResult, RuntimeError> {
    // Detect container runtime (prefer podman for rootless)
    let container_cmd = detect_container_cli()?;

    let mut cmd = Command::new(&container_cmd);

    // Run in detached mode
    cmd.args(["run", "-d"]);

    // Container name for tracking
    cmd.args(["--name", container_name]);

    // Remove container when done
    cmd.arg("--rm");

    // Mount source directory
    let source_dir_str = target.source_dir.to_string_lossy();
    cmd.arg("-v");
    cmd.arg(format!("{}:/app:ro", source_dir_str));

    // Set working directory
    cmd.args(["-w", "/app"]);

    // Resource limits for safety
    cmd.args(["--memory", "512m"]);
    cmd.args(["--cpus", "1.0"]);

    // Security: drop all capabilities, no new privileges
    cmd.args(["--cap-drop", "ALL"]);
    cmd.arg("--security-opt=no-new-privileges");

    // Image
    cmd.arg(image);

    // Entrypoint command
    cmd.args(entrypoint);

    debug!("Executing container command: {:?}", cmd);

    // Execute and get container ID
    let output = cmd.output().map_err(|e| RuntimeError::CommandExecution {
        operation: format!("{} run", container_cmd),
        source: e,
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RuntimeError::SandboxSetupFailed(format!(
            "Container launch failed: {}",
            stderr
        )));
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    info!(
        "Started container {} for workload {}",
        &container_id[..12.min(container_id.len())],
        request.workload_id
    );

    // Setup log streaming
    let log_path = runtime.workload_log_path(request.workload_id);
    spawn_log_collector(&container_cmd, container_name, &log_path)?;

    // Get container PID (for tracking)
    let pid = get_container_pid(&container_cmd, container_name);

    // Track the container
    {
        let mut workloads = runtime.active_workloads.lock().unwrap();
        workloads.insert(request.workload_id.to_string(), pid.unwrap_or(0));
    }

    Ok(LaunchResult {
        pid,
        bundle_path: None,
        log_path: Some(log_path),
        port: None,
    })
}

/// Detect available container CLI (docker or podman)
fn detect_container_cli() -> Result<String, RuntimeError> {
    // Prefer podman for rootless operation
    if which::which("podman").is_ok() {
        return Ok("podman".to_string());
    }

    if which::which("docker").is_ok() {
        return Ok("docker".to_string());
    }

    Err(RuntimeError::SandboxSetupFailed(
        "No container runtime found (docker or podman)".to_string(),
    ))
}

/// Spawn a background process to collect container logs
fn spawn_log_collector(
    container_cmd: &str,
    container_name: &str,
    log_path: &PathBuf,
) -> Result<(), RuntimeError> {
    let log_file = std::fs::File::create(log_path).map_err(|e| RuntimeError::Io {
        path: log_path.clone(),
        source: e,
    })?;

    let mut cmd = Command::new(container_cmd);
    cmd.args(["logs", "-f", container_name]);
    cmd.stdout(Stdio::from(log_file.try_clone().map_err(|e| {
        RuntimeError::Io {
            path: log_path.clone(),
            source: e,
        }
    })?));
    cmd.stderr(Stdio::from(log_file));

    // Spawn detached - this will run until container exits
    std::thread::spawn(move || {
        let _ = cmd.spawn().map(|mut c| c.wait());
    });

    Ok(())
}

/// Get the PID of a container's main process
fn get_container_pid(container_cmd: &str, container_name: &str) -> Option<u32> {
    let output = Command::new(container_cmd)
        .args(["inspect", "--format", "{{.State.Pid}}", container_name])
        .output()
        .ok()?;

    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok()
    } else {
        None
    }
}

/// Stop a running container
#[allow(dead_code)]
pub fn stop_container(container_name: &str) -> Result<(), RuntimeError> {
    let container_cmd = detect_container_cli()?;

    let output = Command::new(&container_cmd)
        .args(["stop", "-t", "10", container_name])
        .output()
        .map_err(|e| RuntimeError::CommandExecution {
            operation: "container stop".to_string(),
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Failed to stop container {}: {}", container_name, stderr);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_selection() {
        assert_eq!(select_image_for_language("python", None), PYTHON_IMAGE);
        assert_eq!(
            select_image_for_language("python", Some("3.10")),
            "python:3.10-slim"
        );
        assert_eq!(select_image_for_language("node", None), NODE_IMAGE);
        assert_eq!(
            select_image_for_language("node", Some("18.17.0")),
            "node:18-slim"
        );
        assert_eq!(select_image_for_language("deno", None), DENO_IMAGE);
    }

    #[test]
    fn test_entrypoint_construction() {
        let entry = build_entrypoint("python", "main.py", &["--debug".to_string()]);
        assert_eq!(entry, vec!["python3", "-u", "main.py", "--debug"]);

        let entry = build_entrypoint("node", "app.js", &[]);
        assert_eq!(entry, vec!["node", "app.js"]);

        let entry = build_entrypoint("deno", "server.ts", &[]);
        assert_eq!(entry, vec!["deno", "run", "--allow-read", "server.ts"]);
    }
}
