//! Runtime-owned peer broker and loopback shim for a native process group.
//!
//! A native process service group may declare that one service connects to a
//! Port another service serves. macOS Seatbelt and Linux Landlock can filter
//! TCP by port but not by address, so granting a consumer outbound access to a
//! peer's real host port would also grant the same port number on any host.
//!
//! The Runtime therefore owns the authority:
//!
//! ```text
//! consumer --TCP--> shim --unix socket--> broker --TCP--> declared peer
//! ```
//!
//! The broker holds a table `token -> 127.0.0.1:<peer port>` built from the
//! selected Derivation's declared edges. A connection presents
//! `ATO-PEER/1 <token>\n`; an unknown token is closed without relaying. The
//! shim is a runtime-owned helper, so its target and token never come from the
//! bundle or the workload.
//!
//! This module is a transport concern only. It decides nothing about K or D;
//! the caller builds the routes from the Derivation it already admitted.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Header a shim sends to the broker before any workload byte.
pub const PEER_BROKER_PROTOCOL: &str = "ATO-PEER/1";
/// File name of the broker socket inside the Runtime's peer root.
pub const BROKER_SOCKET_NAME: &str = "broker.sock";
/// Longest accepted header, so a hostile client cannot grow it without bound.
const MAX_HEADER_BYTES: usize = 256;

/// Conservative limit for `sockaddr_un.sun_path`, including a NUL byte. macOS
/// allows 104 bytes; a shorter directory name leaves room for the leaf.
const MAX_SOCKET_PATH: usize = 100;

/// The Runtime-owned Unix socket broker for one Run.
pub struct PeerBroker {
    socket: PathBuf,
    stopped: Arc<AtomicBool>,
    listener: Option<std::thread::JoinHandle<()>>,
    /// Present only when the Run root was too deep for `sun_path`; the socket
    /// then lives in the platform temp directory for the Run's lifetime.
    temporary_root: Option<PathBuf>,
}

impl PeerBroker {
    /// Bind the broker socket under `root` and start relaying.
    ///
    /// `routes` is the authority table: only the tokens it holds reach a
    /// target, and the socket is owner-only.
    pub fn start(root: &Path, routes: BTreeMap<String, SocketAddr>) -> Result<Self> {
        if routes.is_empty() {
            bail!("a peer broker needs at least one declared route");
        }
        std::fs::create_dir_all(root)
            .with_context(|| format!("create peer broker root {}", root.display()))?;
        set_owner_only(root)?;
        let (socket, temporary_root) = broker_socket_path(root)?;
        if socket.exists() {
            std::fs::remove_file(&socket)
                .with_context(|| format!("remove stale broker socket {}", socket.display()))?;
        }
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("bind peer broker {}", socket.display()))?;
        set_owner_only(&socket)?;
        // A sandboxed shim is allowed to connect to this path, and the seatbelt
        // filter matches the string it connects with. `temp_dir()` on macOS is
        // `/var/folders/...`, which is a symlink to `/private/var/...`: report
        // the canonical path so the policy and the connection name the same
        // file.
        let socket = socket.canonicalize().unwrap_or(socket);
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let routes = Arc::new(routes);
        let stop = stopped.clone();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // A socket accepted from a non-blocking listener
                        // inherits that flag on macOS, and every read/write
                        // below would fail with EAGAIN the moment the peer
                        // had not written yet. The listener polls for accepts;
                        // the accepted connection is blocking.
                        let _ = stream.set_nonblocking(false);
                        let routes = routes.clone();
                        std::thread::spawn(move || {
                            let _ = handle_broker_connection(stream, &routes);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            socket,
            stopped,
            listener: Some(thread),
            temporary_root,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for PeerBroker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.listener.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
        if let Some(root) = self.temporary_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// Where the broker socket actually lives.
///
/// A deep checkout or `ATO_HOME` can exceed `sockaddr_un.sun_path`. The
/// socket keeps a short, owner-only directory in the platform temp directory
/// instead of failing the Run; the caller always uses the returned path, so
/// the address change does not change the endpoint.
fn broker_socket_path(root: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let socket = root.join(BROKER_SOCKET_NAME);
    if socket.as_os_str().len() < MAX_SOCKET_PATH {
        return Ok((socket, None));
    }
    let fallback = std::env::temp_dir().join(format!(
        "ato-peer-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&fallback)
        .with_context(|| format!("create short peer broker root {}", fallback.display()))?;
    set_owner_only(&fallback)?;
    Ok((fallback.join(BROKER_SOCKET_NAME), Some(fallback)))
}

fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {}", path.display()))
}

/// Read one header, authorize it, and relay to the declared target.
fn handle_broker_connection(
    mut stream: UnixStream,
    routes: &BTreeMap<String, SocketAddr>,
) -> Result<()> {
    let header = read_header(&mut stream)?;
    let Some(token) = header
        .strip_prefix(PEER_BROKER_PROTOCOL)
        .and_then(|rest| rest.strip_prefix(' '))
        .filter(|token| !token.is_empty())
    else {
        return Ok(());
    };
    let Some(target) = routes.get(token) else {
        // Unknown token: close. The consumer sees its edge fail rather than a
        // connection to an undeclared peer.
        return Ok(());
    };
    let tcp = TcpStream::connect_timeout(target, Duration::from_secs(10))
        .with_context(|| format!("broker connect to declared peer {target}"))?;
    relay(stream, tcp)
}

fn read_header(stream: &mut UnixStream) -> Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while header.len() < MAX_HEADER_BYTES {
        match stream.read(&mut byte) {
            Ok(1) => {
                if byte[0] == b'\n' {
                    return Ok(String::from_utf8_lossy(&header).into_owned());
                }
                header.push(byte[0]);
            }
            Ok(_) => break,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("peer broker header is malformed or too long")
}

/// Serve one shim listener until `stopped` is set.
///
/// The shim is the only component on the workload side of the boundary: the
/// consumer speaks ordinary loopback TCP, and the shim turns that into an
/// authorized broker connection.
pub fn serve_shim(
    listener: TcpListener,
    broker: &Path,
    token: &str,
    stopped: Arc<AtomicBool>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    while !stopped.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Same inheritance as the broker's listener: macOS carries
                // the listener's non-blocking flag into the accepted socket.
                let _ = stream.set_nonblocking(false);
                let broker = broker.to_path_buf();
                let token = token.to_owned();
                std::thread::spawn(move || {
                    let _ = handle_shim_connection(stream, &broker, &token);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Bind `port` on loopback, report the chosen port, and serve until killed.
///
/// The CLI's hidden `__portable-peer-shim` entry calls this. `port = 0` asks
/// the OS for one; the selected port is printed as
/// `ATO-PEER-SHIM/1 <port>` so a caller that reads stdout knows it.
pub fn run_shim(broker: &Path, token: &str, port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .context("bind the peer shim's loopback port")?;
    let port = listener.local_addr()?.port();
    println!("ATO-PEER-SHIM/1 {port}");
    std::io::stdout().flush()?;
    serve_shim(listener, broker, token, Arc::new(AtomicBool::new(false)))
}

fn handle_shim_connection(tcp: TcpStream, broker: &Path, token: &str) -> Result<()> {
    // A shim is the only component between the consumer and the broker, so a
    // failure here must say what it was: the consumer sees a reset, not a
    // reason.
    let mut unix = match UnixStream::connect(broker) {
        Ok(unix) => unix,
        Err(error) => {
            eprintln!(
                "ato peer shim: cannot connect the broker {}: {error}",
                broker.display()
            );
            return Err(error).with_context(|| format!("connect peer broker {}", broker.display()));
        }
    };
    if let Err(error) = unix.write_all(format!("{PEER_BROKER_PROTOCOL} {token}\n").as_bytes()) {
        eprintln!("ato peer shim: cannot present its token to the broker: {error}");
        return Err(error.into());
    }
    if let Err(error) = relay(unix, tcp) {
        eprintln!("ato peer shim: relay failed: {error:#}");
        return Err(error);
    }
    Ok(())
}

fn relay(mut unix: UnixStream, mut tcp: TcpStream) -> Result<()> {
    let mut unix_copy = unix.try_clone()?;
    let mut tcp_copy = tcp.try_clone()?;
    let task = std::thread::spawn(move || {
        let _ = std::io::copy(&mut tcp_copy, &mut unix_copy);
        let _ = unix_copy.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut unix, &mut tcp);
    let _ = tcp.shutdown(std::net::Shutdown::Write);
    let _ = task.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn echo_server() -> (SocketAddr, Arc<AtomicBool>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        listener.set_nonblocking(true).unwrap();
        let stop = stopped.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        std::thread::spawn(move || {
                            let mut buffer = [0u8; 64];
                            if let Ok(read) = stream.read(&mut buffer) {
                                let _ = stream.write_all(&buffer[..read]);
                            }
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        (address, stopped)
    }

    #[test]
    fn the_broker_reports_the_canonical_socket_path() {
        // The shim's seatbelt rule is built from this path and the shim
        // connects with it; a `/var` vs `/private/var` mismatch makes the
        // connect fail with EPERM inside the sandbox.
        let (peer, peer_stop) = echo_server();
        let root = tempfile::tempdir().unwrap();
        // A path in the platform temp dir, as the short-path fallback uses.
        let short = std::env::temp_dir().join(format!("ato-peer-canonical-{}", std::process::id()));
        std::fs::create_dir_all(&short).unwrap();
        let broker = PeerBroker::start(&short, BTreeMap::from([("edge".to_owned(), peer)]))
            .expect("broker starts");
        let path = broker.socket_path();
        assert_eq!(
            path,
            path.canonicalize().expect("the socket exists"),
            "the broker must report the canonical socket path"
        );
        drop(broker);
        let _ = peer_stop;
        let _ = std::fs::remove_dir_all(&short);
        let _ = root;
    }

    #[test]
    fn broker_relays_only_declared_tokens() {
        let (peer, peer_stop) = echo_server();
        let root = tempfile::tempdir().unwrap();
        let broker = PeerBroker::start(root.path(), BTreeMap::from([("edge".to_owned(), peer)]))
            .expect("broker starts");
        let mut stream = UnixStream::connect(broker.socket_path()).unwrap();
        stream
            .write_all(format!("{PEER_BROKER_PROTOCOL} edge\nping").as_bytes())
            .unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");

        let mut refused = UnixStream::connect(broker.socket_path()).unwrap();
        refused
            .write_all(format!("{PEER_BROKER_PROTOCOL} undeclared\nping").as_bytes())
            .unwrap();
        let mut echoed = [0u8; 4];
        assert!(matches!(refused.read(&mut echoed), Ok(0) | Err(_)));
        peer_stop.store(true, Ordering::Release);
    }

    #[test]
    fn shim_turns_loopback_traffic_into_one_authorized_edge() {
        let (peer, peer_stop) = echo_server();
        let root = tempfile::tempdir().unwrap();
        let broker =
            PeerBroker::start(root.path(), BTreeMap::from([("edge".to_owned(), peer)])).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let shim_port = listener.local_addr().unwrap().port();
        let stopped = Arc::new(AtomicBool::new(false));
        let broker_path = broker.socket_path().to_path_buf();
        let stop = stopped.clone();
        std::thread::spawn(move || {
            serve_shim(listener, &broker_path, "edge", stop).unwrap();
        });
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, shim_port)).unwrap();
        stream.write_all(b"pong").unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"pong");
        stopped.store(true, Ordering::Release);
        peer_stop.store(true, Ordering::Release);
    }
}
