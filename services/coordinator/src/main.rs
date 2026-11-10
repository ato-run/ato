//! `ato-coordinator` — the resident selfhost Coordinator process (G3).
//!
//! `init` creates/migrates the SQLite database, `serve` runs the HTTP API, and
//! `health` asks a running process for its status. The database file is the
//! authority; stopping the process does not lose a committed revision.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::watch;

use ato_coordinator::Coordinator;
use ato_coordinator::http::{ApiState, handle_request};
use ato_coordinator::service;

#[derive(Parser)]
#[command(
    name = "ato-coordinator",
    about = "Selfhost JSON-first SQLite Coordinator for DataResources"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or migrate the SQLite database and exit.
    Init {
        #[arg(long)]
        db: PathBuf,
    },
    /// Serve the HTTP API.
    Serve {
        #[arg(long)]
        db: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8788")]
        addr: String,
        #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
        token: String,
        /// Serve this static app directory at / together with the Data Ports
        /// bridge.
        #[arg(long)]
        apps_dir: Option<PathBuf>,
        /// The app instance the bridge answers for.
        #[arg(long)]
        app_instance: Option<String>,
        /// The Port the served app declares (defaults to tasks).
        #[arg(long, default_value = "tasks")]
        app_port: String,
        /// Allow binding the plaintext API to a non-loopback address. Without
        /// it the bearer token would cross the network in the clear.
        #[arg(long)]
        allow_plaintext_bind: bool,
    },
    /// Ask a running coordinator for its health.
    Health {
        #[arg(long, default_value = "http://127.0.0.1:8788")]
        url: String,
    },
    /// Start the Coordinator as a background service.
    Start {
        #[arg(long)]
        db: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8788")]
        addr: String,
        #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
        token: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Stop the background service.
    Stop {
        #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
        token: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Poll a hosted relay and answer the requests addressed to this origin.
    Relay {
        #[arg(long)]
        db: PathBuf,
        /// Relay base URL, e.g. http://127.0.0.1:8788.
        #[arg(long)]
        relay_url: String,
        /// Origin id returned by the relay registration.
        #[arg(long)]
        origin_id: String,
        /// Fencing epoch the origin was registered with.
        #[arg(long)]
        origin_epoch: i64,
        /// Coordinator token (the origin's own API bearer).
        #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
        token: String,
        /// Origin token issued by the relay registration.
        #[arg(long, env = "ATO_RELAY_TOKEN")]
        relay_token: String,
        /// Handle at most one step and exit (tests/one-shot use).
        #[arg(long)]
        once: bool,
    },
    /// Report whether the background service is running.
    Status {
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Restart the background service.
    Restart {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        addr: Option<String>,
        #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
        token: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Init { db } => {
            Coordinator::open(&db)?;
            println!("initialized {}", db.display());
            Ok(())
        }
        Command::Start {
            db,
            addr,
            token,
            state_dir,
        } => {
            let dir = state_dir.unwrap_or_else(service::default_state_dir);
            let state = service::start(&db, &addr, &token, &dir)?;
            println!(
                "started pid={} addr={} db={}",
                state.pid, state.addr, state.db
            );
            Ok(())
        }
        Command::Stop { token, state_dir } => {
            let dir = state_dir.unwrap_or_else(service::default_state_dir);
            let stopped = service::stop(&dir, token.as_deref())?;
            println!("{}", if stopped { "stopped" } else { "not running" });
            Ok(())
        }
        Command::Relay {
            db,
            relay_url,
            origin_id,
            origin_epoch,
            token,
            relay_token,
            once,
        } => {
            let coordinator = Coordinator::open(&db)?;
            let origin = ato_coordinator::relay::Origin::new(coordinator, origin_epoch, token);
            let mut transport = ato_coordinator::relay::HttpRelayTransport::new(
                &relay_url,
                &origin_id,
                &relay_token,
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let mut relay_loop = ato_coordinator::relay::RelayLoop::new();
            loop {
                let step = runtime.block_on(relay_loop.step(&origin, &mut transport));
                println!("{step:?}");
                if once {
                    // A one-shot run must fail loudly when the relay could not be
                    // reached, so a script or health check cannot read it as
                    // "nothing to do".
                    if matches!(step, ato_coordinator::relay::RelayStep::Unavailable) {
                        return Err("relay_unavailable".into());
                    }
                    break;
                }
                match step {
                    ato_coordinator::relay::RelayStep::Idle => {
                        std::thread::sleep(std::time::Duration::from_millis(500))
                    }
                    // An unreachable relay executes nothing; back off and retry.
                    ato_coordinator::relay::RelayStep::Unavailable => {
                        std::thread::sleep(ato_coordinator::relay::backoff_delay(
                            relay_loop.report().consecutive_failures,
                        ))
                    }
                    ato_coordinator::relay::RelayStep::Answered { .. } => {}
                }
            }
            Ok(())
        }
        Command::Status { state_dir } => {
            let dir = state_dir.unwrap_or_else(service::default_state_dir);
            match service::status(&dir)? {
                Some((state, health)) => {
                    println!(
                        "running pid={} addr={} db={}",
                        state.pid, state.addr, state.db
                    );
                    match health {
                        Some(body) => println!("health: {body}"),
                        None => println!("health: unreachable"),
                    }
                }
                None => println!("stopped"),
            }
            Ok(())
        }
        Command::Restart {
            db,
            addr,
            token,
            state_dir,
        } => {
            let dir = state_dir.unwrap_or_else(service::default_state_dir);
            let prior = service::read_state(&dir)?;
            let db = db
                .or_else(|| prior.as_ref().map(|state| PathBuf::from(&state.db)))
                .ok_or("restart needs --db (no previous service state)")?;
            let addr = addr
                .or_else(|| prior.as_ref().map(|state| state.addr.clone()))
                .unwrap_or_else(|| "127.0.0.1:8788".to_owned());
            service::stop(&dir, Some(&token))?;
            let state = service::start(&db, &addr, &token, &dir)?;
            println!(
                "restarted pid={} addr={} db={}",
                state.pid, state.addr, state.db
            );
            Ok(())
        }
        Command::Health { url } => {
            let address = url
                .strip_prefix("http://")
                .unwrap_or(&url)
                .trim_end_matches('/')
                .to_owned();
            let mut stream = TcpStream::connect(&address)?;
            write!(
                stream,
                "GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            let body = response
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .unwrap_or_default();
            println!("{body}");
            Ok(())
        }
        Command::Serve {
            db,
            addr,
            token,
            apps_dir,
            app_instance,
            app_port,
            allow_plaintext_bind,
        } => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(serve(
                db,
                addr,
                token,
                apps_dir,
                app_instance,
                app_port,
                allow_plaintext_bind,
            ))
        }
    }
}

async fn serve(
    db: PathBuf,
    addr: String,
    token: String,
    apps_dir: Option<PathBuf>,
    app_instance: Option<String>,
    app_port: String,
    allow_plaintext_bind: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| format!("invalid --addr {addr} (expected IP:port)"))?;
    if !socket.ip().is_loopback() && !allow_plaintext_bind {
        return Err(
            "refusing to serve the plaintext Coordinator API on a non-loopback address; \
             pass --allow-plaintext-bind only on a trusted network"
                .into(),
        );
    }
    let coordinator = Coordinator::open(&db)?;
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let state = Arc::new(ApiState {
        coordinator: Mutex::new(coordinator),
        token,
        shutdown: shutdown_tx,
        apps: apps_dir.map(|dir| ato_coordinator::http::AppsConfig {
            dir,
            instance_id: app_instance.unwrap_or_default(),
            port: app_port,
        }),
    });
    let listener = TcpListener::bind(&addr).await?;
    println!("listening on http://{}", listener.local_addr()?);
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |request: Request<Incoming>| {
                        let state = Arc::clone(&state);
                        async move {
                            let (parts, body) = request.into_parts();
                            let bytes = body
                                .collect()
                                .await
                                .map(|collected| collected.to_bytes())
                                .unwrap_or_default();
                            let authorization = parts
                                .headers
                                .get("authorization")
                                .and_then(|value| value.to_str().ok());
                            let response = handle_request(
                                &state,
                                &parts.method,
                                parts.uri.path(),
                                authorization,
                                &bytes,
                            )
                            .await;
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                    let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await;
                });
            }
        }
    }
    Ok(())
}
