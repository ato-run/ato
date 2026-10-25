use std::path::PathBuf;

use anyhow::{Context, Result};
use ato_cli::activity_mcp::{ActivityMcpServer, run_stdio};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "ato-activity-mcp",
    version,
    about = "Control one ato.run Activity Actor through stdio MCP"
)]
struct Args {
    /// Path to a mode-0600 Activity connection JSON file.
    #[arg(long, env = "ATO_ACTIVITY_CONNECTION_FILE", value_name = "PATH")]
    connection_file: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        // stdout is reserved exclusively for MCP JSON-RPC frames.
        eprintln!("ato-activity-mcp: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let server = ActivityMcpServer::connect(&args.connection_file)
        .context("start Activity Controller session")?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_stdio(server, stdin.lock(), stdout.lock()).context("serve stdio MCP")
}
