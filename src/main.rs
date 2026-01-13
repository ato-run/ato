//! Nacelle Engine - Main Entry Point
//!
//! v2.0: Bundle Runtime Model
//! - Self-extracting bundle (embedded runtime)
//! - Direct execution with supervisor and sandbox

use std::path::PathBuf;
use tracing::warn;

use nacelle::verification::sandbox::SandboxPolicy;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // v2.0: Check if running as self-extracting bundle
    if is_self_extracting_bundle()? {
        return bootstrap_bundled_runtime().await;
    }

    // If not a bundle, show help message
    eprintln!("🔴 Nacelle v2.0: Not running as a bundle");
    eprintln!("This binary should be executed as a self-extracting bundle.");
    eprintln!("Use 'nacelle pack --bundle' to create executable bundles.");
    std::process::exit(1);
}

/// v2.0: Check if this binary contains an embedded bundle
fn is_self_extracting_bundle() -> anyhow::Result<bool> {
    let exe_path = std::env::current_exe()?;
    nacelle::bundle::is_self_extracting_bundle(&exe_path)
}

/// v2.0: Bootstrap and run embedded runtime
async fn bootstrap_bundled_runtime() -> anyhow::Result<()> {
    use nacelle::engine::socket::{SocketConfig, SocketManager};
    use nacelle::engine::supervisor::ProcessSupervisor;

    println!("🚀 Starting nacelle bundle...");

    // Extract bundle to temp directory
    let temp_dir = std::env::temp_dir().join(format!("nacelle-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;

    println!("📦 Extracting to {:?}...", temp_dir);

    let exe_path = std::env::current_exe()?;
    nacelle::bundle::extract_bundle_to_dir(&exe_path, &temp_dir)?;

    // Find entrypoint in source/
    let source_dir = temp_dir.join("source");
    let runtime_dir = temp_dir.join("runtime");

    // Look for capsule.toml to determine entrypoint
    let manifest_path = source_dir.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!("No capsule.toml found in bundle");
    }

    let entrypoint = nacelle::bundle::read_entrypoint_from_manifest(&manifest_path)?;

    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest: toml::Value = toml::from_str(&manifest_content)?;

    let targets_source_language_is_python = manifest
        .get("targets")
        .and_then(|t| t.get("source"))
        .and_then(|s| s.get("language"))
        .and_then(|l| l.as_str())
        .map(|l| l.eq_ignore_ascii_case("python") || l.eq_ignore_ascii_case("python3"))
        .unwrap_or(false);

    let enable_socket_activation =
        targets_source_language_is_python || looks_like_python_file(&entrypoint, &source_dir);

    // Resolve port: env (CAPSULE_PORT, PORT) > capsule.toml (execution.port) > default
    let port = std::env::var("CAPSULE_PORT")
        .ok()
        .or_else(|| std::env::var("PORT").ok())
        .and_then(|p| p.parse().ok())
        .or_else(|| {
            manifest
                .get("execution")
                .and_then(|e| e.get("port"))
                .and_then(|p| p.as_integer())
                .and_then(|p| u16::try_from(p).ok())
        })
        .unwrap_or(8000);

    // Setup Socket Activation (Python-only by default)
    let socket_manager = if enable_socket_activation {
        let socket_config = SocketConfig {
            port,
            host: "0.0.0.0".to_string(),
            enabled: true,
        };
        let socket_manager = SocketManager::new(socket_config)?;
        println!(
            "🔌 Socket Activation: Bound to port {} (FD {})",
            port,
            socket_manager.raw_fd()
        );
        Some(socket_manager)
    } else {
        println!("ℹ️  Socket Activation: Disabled (non-Python entrypoint)");
        None
    };

    // ⚠️ IMPORTANT: Setup signal handlers BEFORE spawning child process
    // This ensures signals are captured by our handler, not the default termination handler
    #[cfg(unix)]
    let (mut sig_term, mut sig_int) = {
        use tokio::signal::unix::{signal, SignalKind};
        let sig_term = signal(SignalKind::terminate())?;
        let sig_int = signal(SignalKind::interrupt())?;
        (sig_term, sig_int)
    };

    // Create the Supervisor (Actor-based)
    let supervisor = ProcessSupervisor::new();

    // Prepare command with Socket Activation
    let source_language = nacelle::bundle::read_source_language_from_manifest(&manifest_path)?;
    let mut cmd = nacelle::bundle::build_bundle_command(
        source_language.as_deref(),
        &entrypoint,
        &source_dir,
        &runtime_dir,
    )?;

    // Provide a consistent port signal to the child.
    cmd.env("PORT", port.to_string());
    cmd.env("CAPSULE_PORT", port.to_string());

    // Pass socket FD to child (if enabled)
    if let Some(sm) = socket_manager.as_ref() {
        sm.prepare_command(&mut cmd)?;
    }

    // Set process group and sandbox for signal propagation and isolation
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);

        // Phase 3: Apply sandbox for process isolation
        let sandbox_policy = SandboxPolicy::for_capsule(&source_dir).with_development_mode(true); // Use dev mode for now (less restrictive)

        let policy_clone = sandbox_policy.clone();
        unsafe {
            cmd.pre_exec(move || {
                match nacelle::verification::sandbox::apply_sandbox(&policy_clone) {
                    Ok(result) => {
                        if !result.fully_enforced && !result.partially_enforced {
                            eprintln!("⚠️  Sandbox not enforced: {}", result.message);
                        }
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("⚠️  Sandbox error (continuing): {}", e);
                        // Continue without sandbox in case of error
                        Ok(())
                    }
                }
            });
        }

        println!("🔒 Sandbox: Configured for source directory");
    }

    // Spawn the child process
    let child = cmd.spawn()?;
    let child_pid = child.id();
    if socket_manager.is_some() {
        println!("🔗 Socket Activation: Passing FD 3 to child process");
    }

    // Register with Supervisor
    supervisor.register("main-app".to_string(), child)?;

    // Wait for shutdown signal (SIGTERM or SIGINT)
    #[cfg(unix)]
    {
        tokio::select! {
            _ = sig_term.recv() => {
                println!("\n🛑 Received SIGTERM, shutting down gracefully...");
            }
            _ = sig_int.recv() => {
                println!("\n🛑 Received SIGINT (Ctrl+C), shutting down gracefully...");
            }
        }

        // Kill the process group directly to ensure child is terminated
        use nix::sys::signal::{self as nix_signal, Signal};
        use nix::unistd::Pid;

        println!("📤 Sending SIGTERM to process group (PID {})...", child_pid);

        // Send SIGTERM to child's process group
        let pgid = Pid::from_raw(child_pid as i32);
        if let Err(e) = nix_signal::killpg(pgid, Signal::SIGTERM) {
            warn!("Failed to send SIGTERM to process group: {}", e);
            // Fallback: try killing just the child
            if let Err(e) = nix_signal::kill(Pid::from_raw(child_pid as i32), Signal::SIGTERM) {
                warn!("Failed to send SIGTERM to child: {}", e);
            }
        }

        // Wait briefly for graceful exit
        println!("⏳ Waiting for processes to exit gracefully...");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Force kill if still running
        println!("🔨 Sending SIGKILL to ensure termination...");
        let _ = nix_signal::killpg(pgid, Signal::SIGKILL);
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        println!("\n🛑 Received shutdown signal, cleaning up...");
    }

    // Graceful shutdown via Supervisor (cleanup internal state)
    if let Err(e) = supervisor.shutdown_and_wait().await {
        // Ignore errors - the process may already be gone
        let _ = e;
    }

    // Cleanup temp directory
    if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
        warn!("Failed to cleanup temp directory: {}", e);
    }

    println!("✅ Shutdown complete");
    Ok(())
}

fn looks_like_python_file(entrypoint: &str, source_dir: &PathBuf) -> bool {
    let ep = entrypoint.trim();
    if ep.ends_with(".py") {
        return true;
    }

    let candidate = source_dir.join(ep);
    candidate.is_file()
        && candidate
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
}

// NOTE: Command-building and extraction helpers live in `nacelle::bundle` for reuse/testability.
