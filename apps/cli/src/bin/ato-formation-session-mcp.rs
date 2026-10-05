use std::path::PathBuf;

use anyhow::Result;
use ato_cli::formation_session_mcp::{FormationSessionMcpServer, relay, run_stdio};
use clap::{ArgGroup, Parser, error::ErrorKind};

#[derive(Debug, Parser)]
#[command(
    name = "ato-formation-session-mcp",
    version,
    about = "Expose one scoped Formation Session through three fixed MCP tools",
    group(ArgGroup::new("mode").required(true).multiple(false).args(["connection", "relay"]))
)]
struct Args {
    /// Owner-provided scoped Session connection; never an Ato credential file.
    #[arg(long, value_name = "PATH")]
    connection: Option<PathBuf>,

    /// Producer-only descriptor; the relay never reads the owner's connection.
    #[arg(long, value_name = "PATH")]
    relay: Option<PathBuf>,

    /// Owner-selected private relay authorization, outside the model region.
    #[arg(
        long,
        value_name = "PATH",
        requires = "relay",
        conflicts_with = "connection"
    )]
    capability_file: Option<PathBuf>,

    /// Publish private relay authorization separately from its public descriptor.
    #[arg(
        long,
        value_name = "PATH",
        requires = "publish_relay",
        conflicts_with = "relay"
    )]
    publish_capability_file: Option<PathBuf>,

    /// Publish a fresh scoped descriptor and serve as the external owner broker.
    #[arg(
        long,
        value_name = "PATH",
        requires = "connection",
        conflicts_with = "relay"
    )]
    publish_relay: Option<PathBuf>,

    /// Bind the owner broker at one fresh Unix socket instead of loopback TCP.
    #[arg(
        long,
        value_name = "PATH",
        requires = "publish_relay",
        conflicts_with = "relay"
    )]
    relay_socket: Option<PathBuf>,

    /// Expiry capped by Search deadline, or 120 seconds for reconcile-only reporting.
    #[arg(long, requires = "publish_relay", conflicts_with = "relay")]
    relay_expiry_ms: Option<u64>,

    /// Publish status/next only for a saved UNKNOWN; never renew Search authority.
    #[arg(long, requires_all = ["publish_relay", "relay_expiry_ms"], conflicts_with = "relay")]
    reconcile_only: bool,
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
            let capability = args
                .publish_capability_file
                .unwrap_or_else(|| descriptor.with_extension("capability"));
            if let Some(socket) = args.relay_socket {
                #[cfg(unix)]
                return relay::serve_owner_unix_with_scope(
                    &connection,
                    &descriptor,
                    &capability,
                    &socket,
                    args.relay_expiry_ms,
                    args.reconcile_only,
                );
                #[cfg(not(unix))]
                {
                    let _ = socket;
                    anyhow::bail!("Unix relay unavailable");
                }
            }
            relay::serve_owner_with_scope(
                &connection,
                &descriptor,
                &capability,
                args.relay_expiry_ms,
                args.reconcile_only,
            )
        }
        (Some(connection), None, None) => {
            let server = FormationSessionMcpServer::connect(&connection)?;
            run_stdio(server, stdin.lock(), stdout.lock())
        }
        (None, Some(descriptor), None) => {
            let capability = args
                .capability_file
                .unwrap_or_else(|| descriptor.with_extension("capability"));
            relay::run_relay_stdio_with_capability(
                &descriptor,
                &capability,
                stdin.lock(),
                stdout.lock(),
            )
        }
        _ => anyhow::bail!("invalid MCP mode"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_mutually_exclusive_and_publication_requires_owner_connection() {
        for (index, arguments) in [
            vec!["mcp"],
            vec!["mcp", "--connection", "owner", "--relay", "producer"],
            vec!["mcp", "--relay", "producer", "--publish-relay", "new"],
            vec!["mcp", "--relay", "producer", "--relay-expiry-ms", "1"],
            vec!["mcp", "--relay", "producer", "--reconcile-only"],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--publish-relay",
                "new",
                "--reconcile-only",
            ],
            vec!["mcp", "--connection", "owner", "--relay-socket", "socket"],
            vec!["mcp", "--relay", "producer", "--relay-socket", "socket"],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--capability-file",
                "private",
            ],
            vec![
                "mcp",
                "--relay",
                "producer",
                "--publish-capability-file",
                "private",
            ],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--publish-capability-file",
                "private",
            ],
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                Args::try_parse_from(arguments).is_err(),
                "invalid case {index}"
            );
        }
        for arguments in [
            vec!["mcp", "--connection", "owner"],
            vec!["mcp", "--relay", "producer"],
            vec!["mcp", "--relay", "producer", "--capability-file", "private"],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--publish-relay",
                "new",
                "--publish-capability-file",
                "private",
            ],
            vec!["mcp", "--connection", "owner", "--publish-relay", "new"],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--publish-relay",
                "new",
                "--reconcile-only",
                "--relay-expiry-ms",
                "1000",
            ],
            vec![
                "mcp",
                "--connection",
                "owner",
                "--publish-relay",
                "new",
                "--relay-socket",
                "socket",
            ],
        ] {
            assert!(Args::try_parse_from(arguments).is_ok());
        }
    }
}
