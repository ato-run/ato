use std::path::PathBuf;

use anyhow::Result;
use ato_cli::formation_session_mcp::{FormationSessionMcpServer, relay, run_stdio};
use clap::{ArgGroup, Parser, error::ErrorKind};

#[derive(Debug, Parser)]
#[command(
    name = "ato-formation-session-mcp",
    version,
    about = "Expose one scoped Formation Session through four fixed MCP tools",
    group(ArgGroup::new("mode").required(true).multiple(false).args(["connection", "relay"]))
)]
struct Args {
    /// Owner-provided scoped Session connection; never an Ato credential file.
    #[arg(long, value_name = "PATH")]
    connection: Option<PathBuf>,

    /// Producer-only descriptor; the relay never reads the owner's connection.
    #[arg(long, value_name = "PATH")]
    relay: Option<PathBuf>,

    /// Publish a fresh scoped descriptor and serve as the external owner broker.
    #[arg(long, value_name = "PATH", requires = "connection")]
    publish_relay: Option<PathBuf>,

    /// Optional shorter expiry, never later than the saved Search deadline.
    #[arg(long, requires = "publish_relay")]
    relay_expiry_ms: Option<u64>,
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
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match (args.connection, args.relay, args.publish_relay) {
        (Some(connection), None, Some(descriptor)) => {
            relay::serve_owner(&connection, &descriptor, args.relay_expiry_ms)
        }
        (Some(connection), None, None) => {
            let server = FormationSessionMcpServer::connect(&connection)?;
            run_stdio(server, stdin.lock(), stdout.lock())
        }
        (None, Some(descriptor), None) => {
            relay::run_relay_stdio(&descriptor, stdin.lock(), stdout.lock())
        }
        _ => anyhow::bail!("invalid MCP mode"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_mutually_exclusive_and_publication_requires_owner_connection() {
        for arguments in [
            vec!["mcp"],
            vec!["mcp", "--connection", "owner", "--relay", "producer"],
            vec!["mcp", "--relay", "producer", "--publish-relay", "new"],
            vec!["mcp", "--relay", "producer", "--relay-expiry-ms", "1"],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
        for arguments in [
            vec!["mcp", "--connection", "owner"],
            vec!["mcp", "--relay", "producer"],
            vec!["mcp", "--connection", "owner", "--publish-relay", "new"],
        ] {
            assert!(Args::try_parse_from(arguments).is_ok());
        }
    }
}
