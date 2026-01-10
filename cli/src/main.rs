//! Capsule CLI - Universal Application Runtime Contract Manager
//!
//! Clean CLI for managing UARC-compliant applications:
//! - Lifecycle: new, init, open, close, logs, ps
//! - Packaging: pack, keygen  
//! - System: doctor
//!
//! Also supports self-extracting bundle execution (v2.0)

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

mod commands;
mod engine_client;

#[derive(Parser)]
#[command(name = "capsule")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Capsule CLI - Universal Application Runtime Contract Manager")]
#[command(after_help = "\
LIFECYCLE:
  new      Create a new Capsule project
  init     Initialize existing project as Capsule  
  open     Open and launch a Capsule (--dev for development)
  close    Close a running Capsule
  logs     Stream logs from an open Capsule
  ps       List currently open Capsules

PACKAGING:
  pack     Build and sign a .capsule archive
  keygen   Generate developer signing keys

SYSTEM:
  doctor   Check Engine status and host requirements

EXAMPLES:
  capsule new my-app --template python
  capsule init
  capsule open --dev
  capsule pack --key ~/.capsule/keys/dev.secret
  capsule open my-app.capsule
")]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Engine gRPC endpoint (can be set via CAPSULE_ENGINE_URL)
    #[arg(long, global = true, default_value = "http://127.0.0.1:50051")]
    engine_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ═══════════════════════════════════════════════════════════════════
    // LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════
    /// Create a new Capsule project from a template
    New {
        /// Project name
        name: String,

        /// Template type: python, node, rust, shell
        #[arg(short, long, default_value = "python")]
        template: String,
    },

    /// Initialize existing project as a Capsule
    Init {
        /// Target directory (default: current directory)
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Skip prompts and use detected defaults
        #[arg(short, long)]
        yes: bool,
    },

    /// Open and launch a Capsule application
    Open {
        /// Path to capsule.toml or .capsule file
        path: Option<PathBuf>,

        /// Development mode (hot reload, loose security)
        #[arg(short, long)]
        dev: bool,
    },

    /// Close a running Capsule
    Close {
        /// Capsule ID to close
        capsule_id: String,
    },

    /// Stream logs from an open Capsule
    Logs {
        /// Capsule ID
        capsule_id: String,

        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
    },

    /// List currently open Capsules
    Ps {
        /// Show all (including stopped) Capsules
        #[arg(short, long)]
        all: bool,
    },

    // ═══════════════════════════════════════════════════════════════════
    // PACKAGING
    // ═══════════════════════════════════════════════════════════════════
    /// Build and sign a .capsule archive
    Pack {
        /// Path to capsule.toml
        #[arg(short, long, default_value = "capsule.toml")]
        manifest: PathBuf,

        /// Output path for .capsule file
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Signing key (.secret file) - if provided, signs the archive
        #[arg(short, long)]
        key: Option<PathBuf>,

        /// v2.0: Create self-extracting bundle instead of .capsule
        #[arg(long)]
        bundle: bool,

        /// v2.0: Path to runtime directory (optional, uses cache if not specified)
        #[arg(long)]
        runtime: Option<PathBuf>,
    },

    /// Generate a new Ed25519 keypair for signing
    Keygen {
        /// Name for the key (default: timestamp-based)
        #[arg(short, long)]
        name: Option<String>,
    },

    // ═══════════════════════════════════════════════════════════════════
    // SYSTEM
    // ═══════════════════════════════════════════════════════════════════
    /// Check Engine status and host requirements
    Doctor {
        /// Show detailed diagnostic information
        #[arg(short, long)]
        verbose: bool,
    },
}

/// Check if this binary contains a self-extracting bundle
fn is_self_extracting_bundle() -> Result<bool> {
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    let file_data = std::fs::read(&exe_path)?;
    let len = file_data.len();

    // Bundle format: [Binary][Compressed Bundle][MAGIC (18 bytes)][Size (8 bytes)]
    const BUNDLE_MAGIC: &[u8] = b"CAPSULED_V2_BUNDLE";

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

    println!("🚀 Starting capsuled bundle...");

    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    // Extract bundle
    let decompressed = commands::pack_v2::extract_bundle(&exe_path)?;

    // Create temporary directory for extraction
    let pid = std::process::id();
    let extract_dir = std::env::temp_dir().join(format!("capsuled-{}", pid));
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
        println!("Engine URL: {}\n", cli.engine_url);
    }

    match cli.command {
        // Lifecycle
        Commands::New { name, template } => commands::new::execute(commands::new::NewArgs {
            name,
            template: Some(template),
        }),
        Commands::Init { path, yes } => {
            commands::init::execute(commands::init::InitArgs { path, yes })
        }
        Commands::Open { path, dev } => {
            commands::open::execute(commands::open::OpenArgs {
                path,
                dev,
                engine_url: Some(cli.engine_url),
            })
            .await
        }
        Commands::Close { capsule_id } => {
            commands::close::execute(commands::close::CloseArgs {
                capsule_id,
                engine_url: Some(cli.engine_url),
            })
            .await
        }
        Commands::Logs { capsule_id, follow } => {
            commands::logs::execute(commands::logs::LogsArgs {
                capsule_id,
                follow,
                engine_url: Some(cli.engine_url),
            })
            .await
        }
        Commands::Ps { all } => {
            commands::ps::execute(commands::ps::PsArgs {
                all,
                engine_url: Some(cli.engine_url),
            })
            .await
        }

        // Packaging
        Commands::Pack {
            manifest,
            output,
            key,
            bundle,
            runtime,
        } => {
            if bundle {
                // v2.0: Create self-extracting bundle
                commands::pack_v2::execute(commands::pack_v2::PackV2Args {
                    manifest_path: manifest,
                    runtime_path: runtime,
                    output,
                })
                .await
            } else {
                // Legacy: Create .capsule archive
                commands::pack::execute(commands::pack::PackArgs {
                    manifest_path: manifest,
                    output,
                    key,
                })
            }
        }
        Commands::Keygen { name } => {
            commands::keygen::execute(commands::keygen::KeygenArgs { name })
        }

        // System
        Commands::Doctor { verbose } => {
            commands::doctor::execute(commands::doctor::DoctorArgs { verbose }).await
        }
    }
}
