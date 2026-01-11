//! Nacelle CLI - Unified Runtime for Capsules
//!
//! Engine entrypoint.
//!
//! User-facing commands live in `capsule` (meta-CLI). This binary exposes a
//! machine-oriented interface via `nacelle internal ...` and also supports
//! self-extracting bundle execution (v2.0).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

mod commands;

#[derive(Parser)]
#[command(name = "nacelle")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Nacelle CLI - Unified Runtime for Capsules")]
#[command(after_help = "\
EXAMPLES:
        # Engine features (JSON over stdio)
        nacelle internal --input - features

        # Build bundle (JSON over stdio)
        nacelle internal --input - pack

        # Execute workload (streaming; exit status is propagated)
        nacelle internal --input - exec
" )]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ═══════════════════════════════════════════════════════════════════
    // INTERNAL (Process Boundary)
    // ═══════════════════════════════════════════════════════════════════
    /// Machine-oriented engine interface (JSON over stdio)
    Internal {
        /// JSON input file path, or '-' for stdin
        #[arg(long, default_value = "-")]
        input: String,

        #[command(subcommand)]
        command: InternalCommands,
    },
}

#[derive(Subcommand)]
enum InternalCommands {
    /// Report engine capabilities for dispatch (JSON)
    Features,
    /// Build artifacts (JSON)
    Pack,
    /// Execute workload (JSON)
    Exec,
}

/// Check if this binary contains a self-extracting bundle
fn is_self_extracting_bundle() -> Result<bool> {
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    let file_data = std::fs::read(&exe_path)?;
    let len = file_data.len();

    // Bundle format: [Binary][Compressed Bundle][MAGIC (18 bytes)][Size (8 bytes)]
    const BUNDLE_MAGIC: &[u8] = b"NACELLE_V2_BUNDLE";

    if len < BUNDLE_MAGIC.len() + 8 {
        return Ok(false);
    }

    let magic_start = len - BUNDLE_MAGIC.len() - 8;
    let magic = &file_data[magic_start..magic_start + BUNDLE_MAGIC.len()];

    Ok(magic == BUNDLE_MAGIC)
}

/// Extract and run the bundled application
async fn run_bundled_application() -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;

    println!("🚀 Starting nacelle bundle...");

    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    // Extract bundle
    let decompressed = commands::pack_v2::extract_bundle(&exe_path)?;

    // Create temporary directory for extraction
    let pid = std::process::id();
    let extract_dir = std::env::temp_dir().join(format!("nacelle-{}", pid));
    std::fs::create_dir_all(&extract_dir)?;

    println!("📦 Extracting to {:?}...", extract_dir);

    // Extract tar archive
    let cursor = std::io::Cursor::new(decompressed);
    let mut archive = tar::Archive::new(cursor);
    archive.unpack(&extract_dir)?;

    // Find Python binary
    let runtime_dir = extract_dir.join("runtime");
    let python_path = find_python_binary(&runtime_dir)?;

    println!("🐍 Found Python: {:?}", python_path);

    // Find entrypoint (capsule.toml or default to main.py)
    let source_dir = extract_dir.join("source");
    let entrypoint = find_entrypoint(&source_dir)?;

    println!("📄 Running: {:?}", entrypoint);

    // Socket Activation (Phase 2): Bind socket before spawning child process
    // This ensures the parent process owns the port and passes it to the child
    let port: u16 = std::env::var("CAPSULE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);

    let socket_manager = match create_socket_manager(port) {
        Ok(sm) => {
            println!(
                "🔌 Socket Activation: Bound to port {} (FD {})",
                port,
                sm.raw_fd()
            );
            Some(sm)
        }
        Err(e) => {
            eprintln!("⚠️  Socket Activation: Failed to bind port {}: {}", port, e);
            eprintln!("    Child process will attempt to bind socket directly.");
            None
        }
    };

    // Build command
    let mut cmd = Command::new(&python_path);
    cmd.arg(&entrypoint)
        .current_dir(&source_dir)
        .env("PYTHONHOME", &runtime_dir.join("python"))
        .env("PYTHONPATH", &source_dir);

    // Apply socket activation if available
    #[cfg(unix)]
    if let Some(ref sm) = socket_manager {
        let socket_fd = sm.raw_fd();

        // Set environment variables for socket activation
        cmd.env("LISTEN_FDS", "1");
        cmd.env("LISTEN_PID", std::process::id().to_string());

        // Use pre_exec to duplicate socket FD to position 3 (SD_LISTEN_FDS_START)
        unsafe {
            cmd.pre_exec(move || {
                const SD_LISTEN_FDS_START: i32 = 3;

                if socket_fd != SD_LISTEN_FDS_START {
                    if libc::dup2(socket_fd, SD_LISTEN_FDS_START) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }

                // Clear FD_CLOEXEC so the socket survives exec
                let flags = libc::fcntl(SD_LISTEN_FDS_START, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(
                        SD_LISTEN_FDS_START,
                        libc::F_SETFD,
                        flags & !libc::FD_CLOEXEC,
                    );
                }

                Ok(())
            });
        }

        println!("🔗 Socket Activation: Passing FD {} to child process", 3);
    }

    // Run the application
    let status = cmd.status().context("Failed to execute Python")?;

    // Cleanup
    if let Err(e) = std::fs::remove_dir_all(&extract_dir) {
        eprintln!("⚠️ Warning: Failed to cleanup temp dir: {}", e);
    }

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// Create a socket manager for socket activation
fn create_socket_manager(port: u16) -> Result<SocketManager> {
    let addr: std::net::SocketAddr = format!("0.0.0.0:{}", port)
        .parse()
        .context("Invalid socket address")?;

    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("Failed to bind socket on port {}", port))?;

    Ok(SocketManager { listener })
}

/// Simple socket manager for CLI bundle execution
struct SocketManager {
    listener: std::net::TcpListener,
}

impl SocketManager {
    #[cfg(unix)]
    fn raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.listener.as_raw_fd()
    }

    #[cfg(not(unix))]
    fn raw_fd(&self) -> i32 {
        -1 // Not supported on Windows
    }
}

/// Find Python binary in the extracted runtime directory
fn find_python_binary(runtime_dir: &PathBuf) -> Result<PathBuf> {
    // Try common locations
    let candidates = [
        runtime_dir.join("python/bin/python3"),
        runtime_dir.join("python/bin/python"),
        runtime_dir.join("bin/python3"),
        runtime_dir.join("bin/python"),
        runtime_dir.join("python.exe"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }

    // Search recursively for python binary
    for entry in walkdir::WalkDir::new(runtime_dir).max_depth(5) {
        if let Ok(e) = entry {
            let name = e.file_name().to_string_lossy();
            if name == "python3" || name == "python" || name == "python.exe" {
                if e.file_type().is_file() {
                    return Ok(e.path().to_path_buf());
                }
            }
        }
    }

    anyhow::bail!("Python binary not found in runtime directory")
}

/// Find entrypoint script in the source directory
fn find_entrypoint(source_dir: &PathBuf) -> Result<PathBuf> {
    // Try to read capsule.toml for entrypoint
    let manifest_path = source_dir.join("capsule.toml");
    if manifest_path.exists() {
        let content = std::fs::read_to_string(&manifest_path)?;
        if let Ok(manifest) = toml::from_str::<toml::Value>(&content) {
            if let Some(entrypoint) = manifest
                .get("execution")
                .and_then(|e| e.get("entrypoint"))
                .and_then(|e| e.as_str())
            {
                let ep_path = source_dir.join(entrypoint);
                if ep_path.exists() {
                    return Ok(ep_path);
                }
            }
        }
    }

    // Default entrypoints
    let defaults = ["main.py", "app.py", "__main__.py", "server.py"];
    for default in &defaults {
        let path = source_dir.join(default);
        if path.exists() {
            return Ok(path);
        }
    }

    anyhow::bail!("No entrypoint found in source directory")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Check if this is a self-extracting bundle
    if is_self_extracting_bundle()? {
        return run_bundled_application().await;
    }

    // Normal CLI mode
    let cli = Cli::parse();

    if cli.verbose {
        println!("🔍 Verbose mode enabled");
    }

    match cli.command {
        // Internal
        Commands::Internal { input, command } => {
            let cmd = match command {
                InternalCommands::Features => commands::internal::InternalCommand::Features,
                InternalCommands::Pack => commands::internal::InternalCommand::Pack,
                InternalCommands::Exec => commands::internal::InternalCommand::Exec,
            };

            commands::internal::execute(commands::internal::InternalArgs {
                input,
                command: cmd,
            })
            .await
        }
    }
}
