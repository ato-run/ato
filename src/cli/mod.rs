//! Nacelle CLI module (unified entrypoint).

pub mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "nacelle")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Nacelle runtime CLI")]
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
    Internal {
        /// JSON input file path, or '-' for stdin
        #[arg(long, default_value = "-")]
        input: String,

        #[command(subcommand)]
        command: InternalCommands,
    },

    /// Run a command under the system sandbox (config.json driven)
    Run {
        /// Path to config.json
        #[arg(short, long, default_value = "config.json")]
        config: PathBuf,

        /// Command and arguments to run
        #[arg(last = true, required = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum InternalCommands {
    /// Report engine capabilities for dispatch (JSON)
    Features,
}

pub async fn execute() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        println!("🔍 Verbose mode enabled");
    }

    match cli.command {
        Commands::Internal { input, command } => {
            let cmd = match command {
                InternalCommands::Features => commands::internal::InternalCommand::Features,
            };

            commands::internal::execute(commands::internal::InternalArgs {
                input,
                command: cmd,
            })
            .await
        }
        Commands::Run { config, args } => {
            commands::run::execute(commands::run::RunArgs {
                config_path: config,
                args,
            })
            .await
        }
    }
}
