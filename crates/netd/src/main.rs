//! `ato-netd` — session-scoped ato network broker (slice A skeleton).
//!
//! See #294 for the overall architecture and #296 for this slice's
//! scope. Slice A intentionally ships **no** ingress proxy, **no** DNS
//! resolver, **no** egress CONNECT proxy. It exposes only the control
//! plane (`status`, `shutdown`) so subsequent slices can build on a
//! stable client / server boundary defined in `netd::net::control`.
//!
//! **Platform note.** The daemon uses a Unix domain socket on Unix
//! and a Windows named pipe on Windows. Both transports share the
//! same newline-delimited JSON control protocol from `netd::net::control`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Command-line surface. Same binary handles two callers:
///
/// - `ato-netd` (no flags) → run as the daemon. Binds the control
///   socket and serves requests until `shutdown` arrives, the parent
///   sends SIGTERM, or stdin / the controlling terminal closes.
/// - `ato-netd --status` → act as a *client*. Connect to whichever
///   daemon owns the canonical socket and print its JSON status
///   envelope. If no daemon is running, print `{"status":"not_running"}`
///   and exit non-zero. This is the user-facing diagnostic surface.
#[derive(Debug, Parser)]
#[command(
    name = "ato-netd",
    about = "ato network broker (skeleton; see #294 / #296)",
    version
)]
struct Cli {
    /// Print the running daemon's status envelope as JSON and exit.
    /// Exits non-zero with `{"status":"not_running"}` if no daemon
    /// owns the control socket.
    #[arg(long, conflicts_with = "shutdown")]
    status: bool,

    /// Send a `Shutdown` request to the running daemon and exit.
    /// Mirrors `Client::shutdown`; useful for shell smoke tests.
    #[arg(long)]
    shutdown: bool,

    /// Override the control endpoint path. Defaults to the canonical
    /// Unix socket path or Windows named-pipe path.
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("ato-netd: failed to build tokio runtime: {err}");
            return ExitCode::from(2);
        }
    };

    runtime.block_on(async move {
        if cli.status {
            return run_status_client(cli.socket.clone()).await;
        }
        if cli.shutdown {
            return run_shutdown_client(cli.socket.clone()).await;
        }
        run_daemon(cli.socket.clone()).await
    })
}

fn init_tracing() {
    // Pick up RUST_LOG=netd=debug etc. Default is `info` so a
    // freshly-spawned daemon doesn't drown the parent's stderr.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init()
        .ok();
}

fn resolve_socket_path(override_path: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    netd::net::control::default_socket_path().map_err(|err| err.to_string())
}

/// `--status` codepath. Connects, prints, exits. Never starts a daemon.
async fn run_status_client(override_path: Option<PathBuf>) -> ExitCode {
    let socket = match resolve_socket_path(override_path) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("ato-netd: cannot resolve control socket path: {err}");
            return ExitCode::from(2);
        }
    };
    match netd::net::control::Client::connect(&socket).await {
        Ok(mut client) => match client.status().await {
            Ok(report) => match serde_json::to_string(&report) {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("ato-netd: failed to serialize status: {err}");
                    ExitCode::from(2)
                }
            },
            Err(err) => {
                eprintln!("ato-netd: status request failed: {err}");
                ExitCode::from(2)
            }
        },
        Err(netd::net::control::Error::NotRunning { .. }) => {
            println!(r#"{{"status":"not_running"}}"#);
            // Exit code 3 distinguishes "daemon not up" from generic
            // failure (exit 2). Stable for shell consumers.
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!("ato-netd: control socket error: {err}");
            ExitCode::from(2)
        }
    }
}

/// `--shutdown` codepath. Mirrors `Client::shutdown`. Useful for shell
/// smoke tests; not part of the production lifecycle (the daemon is
/// session-scoped and exits when the parent's cleanup scope drops it).
async fn run_shutdown_client(override_path: Option<PathBuf>) -> ExitCode {
    let socket = match resolve_socket_path(override_path) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("ato-netd: cannot resolve control socket path: {err}");
            return ExitCode::from(2);
        }
    };
    match netd::net::control::Client::connect(&socket).await {
        Ok(client) => match client.shutdown().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("ato-netd: shutdown failed: {err}");
                ExitCode::from(2)
            }
        },
        Err(netd::net::control::Error::NotRunning { .. }) => {
            // Idempotent: shutting down a daemon that isn't running is
            // a no-op success from the caller's perspective.
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("ato-netd: control socket error: {err}");
            ExitCode::from(2)
        }
    }
}

/// Daemon codepath. Binds the control socket and serves requests until
/// `shutdown` arrives or the runtime is cancelled.
async fn run_daemon(override_path: Option<PathBuf>) -> ExitCode {
    let socket = match resolve_socket_path(override_path) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("ato-netd: cannot resolve control socket path: {err}");
            return ExitCode::from(2);
        }
    };

    // Derive ATO_HOME from the shared wire-crate path resolver so that
    // `ato-netd` and every other ato binary agree on the root.
    let ato_home = match netd::net::control::ato_home_dir() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("ato-netd: cannot resolve ATO_HOME: {err}");
            return ExitCode::from(2);
        }
    };

    let daemon = match netd::server::Daemon::start(socket, ato_home).await {
        Ok(d) => d,
        Err(netd::server::StartError::AlreadyRunning { pid, path }) => {
            // Typed failure so a wrapping process can observe the
            // distinct "another daemon already owns this socket"
            // condition without parsing stderr.
            eprintln!(
                r#"ato-netd: daemon already running (pid {pid}) at {path}"#,
                path = path.display()
            );
            return ExitCode::from(4);
        }
        Err(err) => {
            eprintln!("ato-netd: failed to start: {err}");
            return ExitCode::from(2);
        }
    };

    match daemon.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ato-netd: daemon loop exited with error: {err}");
            ExitCode::from(2)
        }
    }
}
