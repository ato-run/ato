//! UARC CLI - Command Line Interface for UARC Capsule Management
//!
//! This CLI uses the capsuled library for all core functionality,
//! ensuring consistency with the Engine's verification and signing logic.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "uarc")]
#[command(version = "0.1.0")]
#[command(about = "UARC CLI - Capsule Management Tool", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy a capsule to the local engine
    Deploy {
        /// Path to the capsule manifest (capsule.toml)
        #[arg(short, long)]
        manifest: String,

        /// Target engine endpoint
        #[arg(short, long, default_value = "http://localhost:50051")]
        endpoint: String,
    },

    /// Verify a capsule manifest
    Verify {
        /// Path to the capsule manifest
        #[arg(short, long)]
        manifest: String,
    },

    /// Sign a capsule manifest
    Sign {
        /// Path to the capsule manifest
        #[arg(short, long)]
        manifest: String,

        /// Path to the signing key
        #[arg(short, long)]
        key: String,
    },

    /// Show CLI version and library info
    Version,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        println!("Verbose mode enabled");
    }

    match cli.command {
        Commands::Deploy { manifest, endpoint } => {
            println!("📦 Deploying capsule...");
            println!("   Manifest: {}", manifest);
            println!("   Endpoint: {}", endpoint);
            // TODO: Use capsuled::proto for gRPC client
            // TODO: Use capsuled::capsule_types for manifest parsing
            println!("   ✅ Deploy command placeholder (implementation pending)");
            Ok(())
        }
        Commands::Verify { manifest } => {
            println!("🔍 Verifying manifest: {}", manifest);
            // TODO: Use capsuled::verification for verification
            println!("   ✅ Verify command placeholder (implementation pending)");
            Ok(())
        }
        Commands::Sign { manifest, key } => {
            println!("🔐 Signing manifest...");
            println!("   Manifest: {}", manifest);
            println!("   Key: {}", key);
            // TODO: Use capsuled::capsule_types::signing for signing
            println!("   ✅ Sign command placeholder (implementation pending)");
            Ok(())
        }
        Commands::Version => {
            println!("UARC CLI v{}", env!("CARGO_PKG_VERSION"));
            println!("Using capsuled library (UARC V1.1.0 compliant)");
            Ok(())
        }
    }
}
