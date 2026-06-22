use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum IpcCommands {
    #[command(about = "Show status of running IPC services")]
    Status {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Start an IPC service")]
    Start {
        /// Capsule path or directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Stop a running IPC service")]
    Stop {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Validate and send a JSON-RPC invoke request")]
    Invoke {
        /// Capsule path or directory
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        method: String,
        #[arg(long)]
        args: String,
        #[arg(long, default_value = "invoke-1")]
        id: String,
        #[arg(long)]
        max_message_size: Option<usize>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}
