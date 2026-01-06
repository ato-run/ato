//! Capsule CLI - UARC V1.1.0 Compliant Command Line Tool
//!
//! This CLI implements the Launcher Mode architecture:
//! - Acts as gRPC client to capsuled daemon
//! - Handles CAS hashing and canonical bytes signing
//! - Provides developer-friendly workflow (keygen, pack, sign, dev, run)

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod engine_client;

#[derive(Parser)]
#[command(name = "capsule")]
#[command(version = "0.1.0")]
#[command(about = "Capsule CLI - UARC V1.1.0 Compliant Management Tool")]
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
    /// Generate a new Ed25519 keypair for signing
    Keygen {
        /// Name for the key (default: timestamp-based)
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Pack a capsule.toml into deployable .capsule format
    Pack {
        /// Path to capsule.toml
        #[arg(short, long, default_value = "capsule.toml")]
        manifest: PathBuf,

        /// Output path for .capsule file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Sign a capsule with Ed25519 over canonical bytes
    Sign {
        /// Path to .capsule or capsule.toml
        #[arg(short, long)]
        manifest: PathBuf,

        /// Path to private key (.secret file)
        #[arg(short, long)]
        key: PathBuf,

        /// Output path for .sig file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Run capsule in development mode (auto-pack, dev_mode: true)
    Dev {
        /// Path to capsule.toml (default: current directory)
        #[arg(short, long)]
        manifest: Option<PathBuf>,
    },

    /// Deploy a signed capsule in production mode
    Run {
        /// Path to .capsule file
        capsule: PathBuf,
    },

    /// Stop a running capsule
    Stop {
        /// Capsule ID
        capsule_id: String,
    },

    /// Stream logs from a running capsule
    Logs {
        /// Capsule ID
        capsule_id: String,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },

    /// Run system diagnostics
    Doctor {
        /// Show detailed diagnostic information
        #[arg(short, long)]
        verbose: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();

    // Override engine_url from environment variable if set
    if let Ok(env_url) = std::env::var("CAPSULE_ENGINE_URL") {
        cli.engine_url = env_url;
    }

    if cli.verbose {
        println!("🔍 Verbose mode enabled");
        println!("Engine URL: {}\n", cli.engine_url);
    }

    match cli.command {
        Commands::Keygen { name } => {
            commands::keygen::execute(commands::keygen::KeygenArgs { name })
        }
        Commands::Pack { manifest, output } => {
            commands::pack::execute(commands::pack::PackArgs { manifest_path: manifest, output })
        }
        Commands::Sign { manifest, key, output } => {
            commands::sign::execute(commands::sign::SignArgs {
                manifest_path: manifest,
                key_path: key,
                output,
            })
        }
        Commands::Dev { manifest } => {
            commands::dev::execute(commands::dev::DevArgs {
                manifest_path: manifest,
                engine_url: Some(cli.engine_url),
            })
            .await
        }
        Commands::Run { capsule } => {
            commands::run::execute(commands::run::RunArgs {
                capsule_path: capsule,
                engine_url: Some(cli.engine_url),
            })
            .await
        }
        Commands::Stop { capsule_id } => {
            commands::stop::execute(commands::stop::StopArgs {
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
        Commands::Doctor { verbose } => {
            commands::doctor::execute(commands::doctor::DoctorArgs { verbose }).await
        }
    }
}
