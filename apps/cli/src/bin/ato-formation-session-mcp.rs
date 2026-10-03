use std::path::PathBuf;

use anyhow::Result;
use ato_cli::formation_session_mcp::{FormationSessionMcpServer, run_stdio};
use clap::{Parser, error::ErrorKind};

#[derive(Debug, Parser)]
#[command(
    name = "ato-formation-session-mcp",
    version,
    about = "Expose one scoped Formation Session through four fixed stdio MCP tools"
)]
struct Args {
    /// Owner-provided scoped Session connection; never an Ato credential file.
    #[arg(long, value_name = "PATH")]
    connection: PathBuf,
}

fn main() {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return;
        }
        Err(_) => {
            eprintln!("ato-formation-session-mcp: invalid startup arguments");
            std::process::exit(2);
        }
    };
    if run(args).is_err() {
        // Both stdout and diagnostics exclude file paths, capability contents
        // and parser errors. stdout contains only MCP JSON-RPC frames.
        eprintln!("ato-formation-session-mcp: startup or transport failed");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let server = FormationSessionMcpServer::connect(&args.connection)?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_stdio(server, stdin.lock(), stdout.lock())
}
