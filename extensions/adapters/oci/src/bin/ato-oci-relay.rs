//! The relay container's half of the protocol.
//!
//! It runs *inside* the Run's internal container network, so the service
//! containers' addresses are reachable from it even when they are not
//! reachable from the Run's own process (a VM-backed or rootless daemon). It
//! speaks [`relay_protocol`]'s framing over stdin/stdout and connects to nothing
//! the Run did not list on its command line.
//!
//! ```text
//! ato-oci-relay --allow web:8080 --allow backend:8081
//! ```
//!
//! The process exits when its stdin closes, so a Run that goes away takes the
//! relay with it, and it never daemonizes.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// The framing source is included directly rather than through the adapter
// crate: the relay is a static binary that must not link the adapter (and the
// adapter embeds this binary, so linking it would embed the relay in itself).
#[path = "../relay_protocol.rs"]
#[allow(dead_code)] // the adapter's end uses the rest of the module
mod relay_protocol;

use relay_protocol::{AllowedDestination, Frame, PROTOCOL, resolve_ipv4};

fn main() {
    let mut allowed = Vec::new();
    let mut connect_timeout = Duration::from_secs(10);
    let mut resolve_only: Option<String> = None;
    let mut log_frames = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--allow" => match arguments.next() {
                Some(raw) => match AllowedDestination::parse(&raw) {
                    Ok(destination) => allowed.push(destination),
                    Err(error) => fail(&error, 2),
                },
                None => fail("--allow needs a service:port value", 2),
            },
            "--connect-timeout-ms" => match arguments.next().and_then(|raw| raw.parse().ok()) {
                Some(millis) => connect_timeout = Duration::from_millis(millis),
                None => fail("--connect-timeout-ms needs a number", 2),
            },
            // Diagnostics: resolve one name the same way a connection would,
            // and print what this binary's own DNS client saw.
            "--log-frames" => log_frames = true,
            "--resolve" => match arguments.next() {
                Some(name) => resolve_only = Some(name),
                None => fail("--resolve needs a name", 2),
            },
            other => fail(&format!("unknown argument {other:?}"), 2),
        }
    }
    if let Some(name) = resolve_only {
        match destination_addresses(&name, 1) {
            Ok(addresses) => {
                for address in addresses {
                    println!("{address}");
                }
                return;
            }
            Err(error) => fail(&format!("cannot resolve {name}: {error}"), 1),
        }
    }
    if allowed.is_empty() {
        fail("the relay was given no destination to proxy", 2);
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let writer = Arc::new(Mutex::new(stdout));

    if let Err(error) = write_frame(
        &writer,
        &Frame::Hello {
            version: PROTOCOL.to_owned(),
        },
    ) {
        fail(&format!("cannot write the relay handshake: {error}"), 1);
    }

    // Only this loop writes to a connection, so the write side is stored
    // directly. The read side is a clone owned by its own thread: a lock held
    // across a blocking read would deadlock the whole relay the first time a
    // peer stayed quiet (which is every idle keep-alive connection).
    let mut connections: BTreeMap<u32, TcpStream> = BTreeMap::new();
    let mut reader = stdin.lock();
    loop {
        let frame = match Frame::decode(&mut reader) {
            Ok(Some(frame)) => frame,
            // The Run's end closed: take every connection down with it.
            Ok(None) => break,
            Err(error) => {
                eprintln!("ato-oci-relay: stream error: {error}");
                break;
            }
        };
        if log_frames {
            eprintln!("ato-oci-relay: frame {frame:?}");
        }
        match frame {
            Frame::Open {
                conn,
                service,
                port,
            } => {
                if !allowed
                    .iter()
                    .any(|destination| destination.admits(&service, port))
                {
                    let _ = write_frame(
                        &writer,
                        &Frame::OpenFailed {
                            conn,
                            reason: format!("{service}:{port} is not a declared Port of this Run"),
                        },
                    );
                    continue;
                }
                match connect(&service, port, connect_timeout) {
                    Ok((stream, peer)) => {
                        if write_frame(&writer, &Frame::Opened { conn }).is_err() {
                            break;
                        }
                        match stream.try_clone() {
                            Ok(read_side) => {
                                connections.insert(conn, stream);
                                spawn_reader(conn, read_side, Arc::clone(&writer), log_frames);
                                eprintln!("ato-oci-relay: {service}:{port} connected ({peer})");
                            }
                            Err(error) => {
                                let _ = write_frame(&writer, &Frame::Close { conn });
                                eprintln!(
                                    "ato-oci-relay: {service}:{port} connected but cannot be read: {error}"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        let _ = write_frame(
                            &writer,
                            &Frame::OpenFailed {
                                conn,
                                reason: format!("{service}:{port}: {error}"),
                            },
                        );
                    }
                }
            }
            Frame::Data { conn, bytes } => {
                let write_failed = match connections.get_mut(&conn) {
                    Some(stream) => stream.write_all(&bytes).is_err(),
                    None => {
                        let _ = write_frame(&writer, &Frame::Close { conn });
                        continue;
                    }
                };
                if write_failed {
                    let _ = write_frame(&writer, &Frame::Close { conn });
                    connections.remove(&conn);
                }
            }
            Frame::HalfClose { conn } => {
                if let Some(stream) = connections.get_mut(&conn) {
                    let _ = stream.shutdown(Shutdown::Write);
                }
            }
            Frame::Close { conn } => {
                if let Some(stream) = connections.remove(&conn) {
                    let _ = stream.shutdown(Shutdown::Both);
                }
            }
            Frame::Opened { .. } | Frame::OpenFailed { .. } | Frame::Hello { .. } => {
                // The relay never receives these; ignoring them is not a
                // reason to drop a working Run's other connections.
            }
        }
    }
    for (_, stream) in connections {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn connect(service: &str, port: u16, timeout: Duration) -> io::Result<(TcpStream, String)> {
    let targets = destination_addresses(service, port)?;
    let mut last = None;
    for target in targets {
        match TcpStream::connect_timeout(&target, timeout) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok((stream, target.to_string()));
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("{service}:{port} did not resolve"),
        )
    }))
}

/// The addresses one destination resolves to.
///
/// An IP literal is used as-is. A container name is resolved by this binary's
/// own UDP query to the nameserver in `/etc/resolv.conf`, because a static
/// musl `getaddrinfo` times out against Docker's embedded resolver while the
/// same query from a libc-linked container succeeds. The system resolver is
/// still used as a fallback, so a `/etc/hosts` entry or a working libc keeps
/// working.
fn destination_addresses(service: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    if let Ok(ip) = service.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let resolv_conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    // Docker's embedded DNS may not have the record yet on the same instant a
    // service starts, and a Run retries readiness anyway; a short retry here
    // keeps one slow answer from looking like a refusal.
    for attempt in 0..3 {
        if let Ok(addresses) = resolve_ipv4(service, &resolv_conf)
            && !addresses.is_empty()
        {
            return Ok(addresses
                .into_iter()
                .map(|address| SocketAddr::new(IpAddr::V4(address), port))
                .collect());
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok((service, port).to_socket_addrs()?.collect())
}

fn spawn_reader(
    conn: u32,
    mut stream: TcpStream,
    writer: Arc<Mutex<io::Stdout>>,
    log_frames: bool,
) {
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => 0,
            };
            if read == 0 {
                if log_frames {
                    eprintln!("ato-oci-relay: conn {conn} peer closed");
                }
                let _ = write_frame(&writer, &Frame::HalfClose { conn });
                return;
            }
            if log_frames {
                eprintln!("ato-oci-relay: conn {conn} -> {read} bytes");
            }
            if write_frame(
                &writer,
                &Frame::Data {
                    conn,
                    bytes: buffer[..read].to_vec(),
                },
            )
            .is_err()
            {
                return;
            }
        }
    });
}

fn write_frame(writer: &Arc<Mutex<io::Stdout>>, frame: &Frame) -> io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| io::Error::other("relay stdout lock was poisoned"))?;
    frame.write_to(&mut *writer)
}

fn fail(message: &str, code: i32) -> ! {
    eprintln!("ato-oci-relay: {message}");
    std::process::exit(code)
}
