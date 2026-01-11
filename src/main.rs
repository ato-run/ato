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
    let file_data = std::fs::read(&exe_path)?;

    let len = file_data.len();
    let magic = b"NACELLE_V2_BUNDLE";

    if len < magic.len() + 8 {
        return Ok(false);
    }

    let magic_start = len - magic.len() - 8;
    let found_magic = &file_data[magic_start..magic_start + magic.len()];

    Ok(found_magic == magic)
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
    let file_data = std::fs::read(&exe_path)?;

    // Parse bundle
    let len = file_data.len();
    let magic = b"NACELLE_V2_BUNDLE";
    let magic_start = len - magic.len() - 8;
    let size_bytes = &file_data[len - 8..len];
    let bundle_size = u64::from_le_bytes(size_bytes.try_into()?) as usize;
    let bundle_start = magic_start - bundle_size;
    let compressed = &file_data[bundle_start..magic_start];

    // Decompress
    let decompressed = zstd::decode_all(compressed)?;

    // Extract tar
    use tar::Archive;
    let mut archive = Archive::new(decompressed.as_slice());
    archive.unpack(&temp_dir)?;

    // Find entrypoint in source/
    let source_dir = temp_dir.join("source");
    let runtime_dir = temp_dir.join("runtime");

    // Look for capsule.toml to determine entrypoint
    let manifest_path = source_dir.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!("No capsule.toml found in bundle");
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest: toml::Value = toml::from_str(&manifest_content)?;

    let entrypoint = manifest
        .get("execution")
        .and_then(|e| e.get("entrypoint"))
        .and_then(|e| e.as_str())
        .ok_or_else(|| anyhow::anyhow!("No entrypoint defined in capsule.toml"))?;

    // Find Python binary in runtime
    let python_bin = find_python_binary(&runtime_dir)?;
    println!("🐍 Found Python: {:?}", python_bin);
    println!("📄 Running: {:?}", source_dir.join(entrypoint));

    // Parse port from manifest (default 8000)
    let port = manifest
        .get("execution")
        .and_then(|e| e.get("port"))
        .and_then(|p| p.as_integer())
        .map(|p| p as u16)
        .unwrap_or(8000);

    // Setup Socket Activation
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
    let entrypoint_path = source_dir.join(entrypoint);
    let mut cmd = std::process::Command::new(&python_bin);
    cmd.arg(&entrypoint_path);
    cmd.current_dir(&source_dir);

    // Pass socket FD to child
    socket_manager.prepare_command(&mut cmd)?;

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
    println!("🔗 Socket Activation: Passing FD 3 to child process");

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

/// Find Python binary in extracted runtime
fn find_python_binary(runtime_dir: &PathBuf) -> anyhow::Result<PathBuf> {
    // Look for python3 or python binary
    for entry in std::fs::read_dir(runtime_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Check in bin/ subdirectory
            let bin_dir = path.join("bin");
            if bin_dir.exists() {
                for name in &["python3", "python"] {
                    let python_path = bin_dir.join(name);
                    if python_path.exists() {
                        return Ok(python_path);
                    }
                }
            }
        }
    }

    anyhow::bail!("Python binary not found in runtime directory")
}
