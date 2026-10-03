//! Agent-side CLI: a scoped connection, typed output on stdin, no owner token.
use anyhow::{Result, ensure};
use ato_formation_worker::runtime_network::proposal::reasoning::session::{self, Command};
use clap::{Args, Subcommand};
use std::{io::Read, path::PathBuf};

#[derive(Debug, Args)]
pub(super) struct SessionArgs {
    #[arg(long)]
    connection: PathBuf,
    #[command(subcommand)]
    command: SessionCommand,
}
#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Observe saved state without creating an exchange or executing anything.
    Status,
    /// Get the current saved public input; repeat reads consume no budget.
    Next,
    /// Atomically submit exactly one typed output read from stdin.
    Submit {
        #[arg(long)]
        exchange_id: String,
        #[arg(long)]
        input_sha256: String,
    },
    /// Request owner-authorized cancellation; unresolved execution stays blocked.
    Cancel,
}
pub(super) fn run(args: SessionArgs) -> Result<()> {
    let command = match args.command {
        SessionCommand::Status => Command::Status,
        SessionCommand::Next => Command::Next,
        SessionCommand::Cancel => Command::Cancel,
        SessionCommand::Submit {
            exchange_id,
            input_sha256,
        } => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(16 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 16 * 1024,
                "typed Session output exceeds limit"
            );
            Command::Submit {
                exchange_id,
                input_sha256,
                output: serde_json::from_slice(&bytes)?,
            }
        }
    };
    let view = session::request(&args.connection, command)?;
    println!("{}", serde_json::to_string_pretty(&view)?);
    Ok(())
}
