//! Nacelle CLI module (unified entrypoint).

pub mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "nacelle")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Nacelle engine CLI (internal plumbing only)")]
#[command(
    long_about = "nacelle is the low-level execution engine for capsule bundles. It is intended to be invoked by ato-cli or as a self-extracting bundle, not directly by users.",
    after_help = "See `ato --help` for development and packaging commands."
)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Machine-oriented engine interface (JSON over stdio)
    /// Use ato-cli for human-facing workflows.
    Internal {
        /// JSON input file path, or '-' for stdin
        #[arg(long, default_value = "-")]
        input: String,

        #[command(subcommand)]
        command: InternalCommands,
    },

    /// (Hidden: legacy command, use ato dev instead)
    #[command(hide = true)]
    Dev,
}

#[derive(Subcommand)]
enum InternalCommands {
    /// Report engine capabilities for dispatch (JSON, internal use)
    Features,
    /// Execute a workload from JSON spec (internal use)
    Exec,
    /// Legacy placeholder: build/pack is owned by ato-cli, not nacelle
    #[command(hide = true)]
    Pack,
}

pub async fn execute() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        eprintln!("Verbose mode enabled");
    }

    match cli.command {
        Commands::Internal { input, command } => {
            let cmd = match command {
                InternalCommands::Features => commands::internal::InternalCommand::Features,
                InternalCommands::Exec => commands::internal::InternalCommand::Exec,
                InternalCommands::Pack => commands::internal::InternalCommand::Pack,
            };

            commands::internal::execute(commands::internal::InternalArgs {
                input,
                command: cmd,
            })
            .await
        }
        Commands::Dev => {
            anyhow::bail!("`nacelle dev` is deprecated. Use `ato dev` instead.");
        }
    }
}
