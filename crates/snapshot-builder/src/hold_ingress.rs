//! A local TCP relay that fronts a held guest, so the wizard preview can reach it.
//!
//! # Why this exists, and why it is this small
//!
//! The app proxy resolves a wizard preview's upstream from the control plane's
//! own registry of builder ingress slots (ADR-004) — the builder never names its
//! own upstream, or it could point the proxy anywhere. What the builder DOES own
//! is the other end: the operator provisioned an https origin that terminates on
//! this box at a fixed local port per slot, and something has to carry bytes from
//! that port to the guest.
//!
//! That is all this is: an L4 pipe, the same shape the runner's slot proxy has.
//! It parses no HTTP, so WebSockets, SSE and streaming bodies pass through
//! untouched — and it cannot become an open relay, because the upstream address
//! is fixed at construction and never read from the wire.
//!
//! # Blocking on purpose
//!
//! `snapshot-builder` has no async runtime: its api client is `ureq`, its backoffs
//! are `std::thread::sleep`. A relay needs a listener, a thread per connection and
//! `std::io::copy` — nothing more — so it stays that way rather than pulling tokio
//! into a blocking daemon for one feature.
//!
//! # Lifetime
//!
//! [`HoldIngress::stop`] is explicit, and `Drop` runs the same teardown, so a
//! forgotten relay cannot outlive the hold that owns it and keep a port bound
//! against the next one.

use std::io::{self};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a connect to the guest may take before the relay gives up on it.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A running relay: `listen` -> the held guest.
pub struct HoldIngress {
    listen: SocketAddr,
    stopping: Arc<AtomicBool>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl HoldIngress {
    /// Bind `listen` and start relaying to `upstream` (`ip:port`).
    ///
    /// The upstream is probed once before the listener is announced: binding a
    /// port that answers but goes nowhere would let the control plane publish a
    /// preview URL for a guest that never accepts, which reads to the author as
    /// "Ato is broken" rather than "the app is not up". `boot_and_hold` has
    /// already health-probed the same address, so a failure here is a real
    /// regression, not an ordinary race.
    pub fn start(listen: SocketAddr, upstream: &str) -> io::Result<Self> {
        let upstream_addr: SocketAddr = upstream.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("upstream `{upstream}` is not an ip:port address"),
            )
        })?;
        TcpStream::connect_timeout(&upstream_addr, UPSTREAM_CONNECT_TIMEOUT)?;

        let listener = TcpListener::bind(listen)?;
        let bound = listener.local_addr()?;

        let stopping = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stopping);
        let upstream_owned = upstream.to_string();
        let accept_thread = std::thread::Builder::new()
            .name("ato-hold-ingress".to_string())
            .spawn(move || accept_loop(listener, upstream_addr, upstream_owned, stop_flag))?;

        Ok(Self {
            listen: bound,
            stopping,
            accept_thread: Some(accept_thread),
        })
    }

    /// The address actually bound (useful when `listen` asked for port 0).
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// Stop accepting and wait for the accept loop to end.
    ///
    /// In-flight connections are not force-closed: they end when either side
    /// does. The hold's teardown kills the guest, which closes them.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Unblock a thread parked in `accept()` by connecting to ourselves; the
        // loop then observes the flag and returns.
        let _ = TcpStream::connect_timeout(&self.listen, Duration::from_millis(500));
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HoldIngress {
    fn drop(&mut self) {
        // A forgotten relay must not keep the slot's port bound against the next
        // hold.
        self.shutdown();
    }
}

fn accept_loop(
    listener: TcpListener,
    upstream_addr: SocketAddr,
    upstream: String,
    stopping: Arc<AtomicBool>,
) {
    for incoming in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = incoming else { continue };
        let upstream = upstream.clone();
        // One thread per connection, detached: a stuck peer must never block the
        // accept loop (and therefore every other viewer of the preview).
        let spawned = std::thread::Builder::new()
            .name("ato-hold-ingress-conn".to_string())
            .spawn(move || {
                if let Err(error) = relay(client, upstream_addr) {
                    // Connection-scoped and expected in normal operation (a
                    // reloading browser resets constantly), so this is a note,
                    // not a failure of the hold.
                    eprintln!("[builder] hold ingress relay to {upstream} ended: {error}");
                }
            });
        if spawned.is_err() {
            // Out of threads: drop this connection rather than wedging the loop.
            continue;
        }
    }
}

/// Pipe one client connection to the guest, both directions, until either ends.
fn relay(client: TcpStream, upstream_addr: SocketAddr) -> io::Result<()> {
    let server = TcpStream::connect_timeout(&upstream_addr, UPSTREAM_CONNECT_TIMEOUT)?;
    // Nagle would add latency to the small request/response turns a preview is
    // made of, on both legs.
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);

    let client_read = client.try_clone()?;
    let server_write = server.try_clone()?;
    // Client -> guest on this thread's child, guest -> client on this one, so a
    // half-closed direction does not stall the other.
    let up = std::thread::Builder::new()
        .name("ato-hold-ingress-up".to_string())
        .spawn(move || pump(client_read, server_write))?;
    let _ = pump(server, client);
    let _ = up.join();
    Ok(())
}

/// Copy until EOF, then half-close the destination so the peer sees the end.
fn pump(mut from: TcpStream, mut to: TcpStream) -> io::Result<u64> {
    let copied = io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
    copied
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A trivial upstream that echoes one request line, standing in for a guest.
    fn spawn_echo_upstream() -> (SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut buf = [0u8; 64];
                let Ok(n) = s.read(&mut buf) else { continue };
                if n == 0 {
                    continue;
                }
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\nhello");
                let _ = s.shutdown(Shutdown::Write);
            }
        });
        (addr, handle)
    }

    /// Bytes reach the guest and come back — the whole job of this file.
    #[test]
    fn relays_a_request_to_the_upstream_and_back() {
        let (upstream, _up) = spawn_echo_upstream();
        let ingress = HoldIngress::start("127.0.0.1:0".parse().unwrap(), &upstream.to_string())
            .expect("start ingress");

        let mut client = TcpStream::connect(ingress.listen_addr()).expect("connect via ingress");
        client.write_all(b"GET / HTTP/1.0\r\n\r\n").expect("write");
        let mut got = Vec::new();
        client.read_to_end(&mut got).expect("read");
        assert!(
            String::from_utf8_lossy(&got).contains("hello"),
            "relayed body: {:?}",
            String::from_utf8_lossy(&got)
        );
        ingress.stop();
    }

    /// A guest that is not accepting is refused at START, not published as a
    /// working preview URL that then 502s for the author.
    #[test]
    fn refuses_to_start_when_the_upstream_is_not_accepting() {
        // Bind and immediately drop, so the port is (almost certainly) closed.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr")
        };
        let started = HoldIngress::start("127.0.0.1:0".parse().unwrap(), &dead.to_string());
        assert!(
            started.is_err(),
            "a relay must not announce a listener for a guest that is not there"
        );
    }

    /// `stop` releases the port, so the next hold on this slot can bind it.
    #[test]
    fn stopping_releases_the_listen_port() {
        let (upstream, _up) = spawn_echo_upstream();
        let ingress = HoldIngress::start("127.0.0.1:0".parse().unwrap(), &upstream.to_string())
            .expect("start");
        let addr = ingress.listen_addr();
        ingress.stop();

        // Rebinding the same port is the observable proof the listener is gone.
        let rebound = TcpListener::bind(addr);
        assert!(rebound.is_ok(), "stop() left {addr} bound");
    }

    /// An upstream that is not an `ip:port` is rejected rather than resolved —
    /// the relay must never take a hostname it could be pointed at.
    #[test]
    fn refuses_an_upstream_that_is_not_an_ip_port() {
        let started = HoldIngress::start("127.0.0.1:0".parse().unwrap(), "evil.example.com:80");
        assert!(started.is_err());
    }
}
