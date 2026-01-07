//! Capsule CLI - Universal Application Runtime Contract Manager
//!
//! Clean CLI for managing UARC-compliant applications:
//! - Lifecycle: new, init, open, close, logs, ps
//! - Packaging: pack, keygen  
//! - System: doctor

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        } => commands::pack::execute(commands::pack::PackArgs {
            manifest_path: manifest,
            output,
            key,
        }),
        Commands::Keygen { name } => {
            commands::keygen::execute(commands::keygen::KeygenArgs { name })
        }

        // System
        Commands::Doctor { verbose } => {
            commands::doctor::execute(commands::doctor::DoctorArgs { verbose }).await
        }
    }
}
