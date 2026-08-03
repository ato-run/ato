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
//! The relay is owned by the hold and torn down by `Drop`, so it cannot outlive
//! the hold that owns it and keep a port bound against the next one — including
//! on the hold's early-return paths, which is why teardown is `Drop` rather than
//! an explicit call somebody has to reach.
//!
//! [`HoldIngress::gate_for_verification`] is the one explicit transition: it
//! stops relaying and answers 503 instead, at the point the held guest is
//! released for acceptance. See its doc for why continuing to relay there would
//! be actively wrong rather than merely useless.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a connect to the guest may take before the relay gives up on it.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a gated client is told to wait before retrying.
///
/// Advisory only. It is sized to one cold disposable restore plus the author's
/// `seal_at` command, which is the window the gate is open for.
const VERIFICATION_RETRY_AFTER_SECS: u32 = 15;

/// Bound the bytes and time spent draining a gated request after sending 503.
/// Draining prevents an unread request from turning the graceful response into
/// a TCP reset on platforms such as macOS.
const GATE_REQUEST_DRAIN_LIMIT: u64 = 64 * 1024;
const GATE_REQUEST_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

/// A running relay: `listen` -> the held guest.
pub struct HoldIngress {
    listen: SocketAddr,
    stopping: Arc<AtomicBool>,
    gated: Arc<AtomicBool>,
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
        let gated = Arc::new(AtomicBool::new(false));
        let gate_flag = Arc::clone(&gated);
        let upstream_owned = upstream.to_string();
        let accept_thread = std::thread::Builder::new()
            .name("ato-hold-ingress".to_string())
            .spawn(move || {
                accept_loop(
                    listener,
                    upstream_addr,
                    upstream_owned,
                    stop_flag,
                    gate_flag,
                )
            })?;

        Ok(Self {
            listen: bound,
            stopping,
            gated,
            accept_thread: Some(accept_thread),
        })
    }

    /// Stop relaying to the guest and answer 503 instead, permanently.
    ///
    /// Called immediately before the held guest is released for verification.
    /// Relaying past that point is not merely useless, it is WRONG: the
    /// upstream is a fixed guest address, so once the hold's guest is gone the
    /// relay would carry the author's browser into whatever occupies that
    /// address next — during acceptance, the disposable guest being verified.
    /// That would feed real input to the guest whose behaviour `seal_at` is
    /// judging.
    ///
    /// A 503 rather than a closed port because the author is watching: a
    /// refused connection renders as a broken preview, while a 503 with
    /// `Retry-After` is a state the wizard can explain alongside
    /// `WizardStage::Accepting`.
    pub fn gate_for_verification(&self) {
        self.gated.store(true, Ordering::SeqCst);
    }

    /// The address actually bound (useful when `listen` asked for port 0).
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// Stop accepting and wait for the accept loop to end.
    ///
    /// In-flight connections are not force-closed: they end when either side
    /// does. The hold's teardown kills the guest, which closes them.
    ///
    /// Production no longer calls this — the relay is owned by the hold and
    /// `Drop` runs the same teardown on every exit path, which is what makes the
    /// early returns safe. It is kept as the explicit counterpart to `Drop` and
    /// is what this module's tests drive, since a test needs teardown to have
    /// finished before it asserts. (`snapshot-builder` is a binary crate, so a
    /// `pub` item reached only from tests still reads as dead to the bin target.)
    #[allow(dead_code)]
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
    gated: Arc<AtomicBool>,
) {
    for incoming in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = incoming else { continue };
        // Gated: answer here and never dial upstream. Checked per connection
        // rather than once, because the gate closes while the loop is parked in
        // `accept()` and every connection after that point must be refused.
        if gated.load(Ordering::SeqCst) {
            // Keep the accept loop responsive while the connection drains. A
            // peer can otherwise hold every later preview request behind this
            // bounded graceful-close window.
            let _ = std::thread::Builder::new()
                .name("ato-hold-ingress-gate".to_string())
                .spawn(move || {
                    if let Err(error) = write_verification_gate_response(client) {
                        eprintln!("[builder] hold ingress gate response ended: {error}");
                    }
                });
            continue;
        }
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

/// The canned answer while the hold is being verified.
///
/// Deliberately a complete, self-contained HTTP/1.1 response with
/// `Connection: close` and a fixed `Content-Length`: the client is a browser
/// that may be mid-keepalive, and a partial or unframed reply would render as a
/// network error — the exact thing the gate exists to avoid.
fn write_verification_gate_response(mut client: TcpStream) -> io::Result<()> {
    const BODY: &str = "Verifying this capsule's snapshot. The preview returns when it finishes.";
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Retry-After: {VERIFICATION_RETRY_AFTER_SECS}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {BODY}",
        BODY.len()
    );
    client.write_all(response.as_bytes())?;
    client.flush()?;
    client.shutdown(Shutdown::Write)?;

    // Closing a socket with unread peer data may emit RST and discard the 503
    // the browser is supposed to see. Drain only a small bounded window: this
    // is connection hygiene, not HTTP parsing, and never reaches the guest.
    client.set_read_timeout(Some(GATE_REQUEST_DRAIN_TIMEOUT))?;
    let mut request = (&mut client).take(GATE_REQUEST_DRAIN_LIMIT);
    match io::copy(&mut request, &mut io::sink()) {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
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

    /// Once gated, the relay answers 503 and never dials the guest.
    ///
    /// The "never dials" half is the correctness one. The relay's upstream is a
    /// fixed guest address, so after the hold's guest is released that address
    /// belongs to whatever occupies the slot next — during acceptance, the
    /// disposable guest under verification. Relaying there would feed the
    /// author's browser into the guest whose behaviour `seal_at` is judging.
    #[test]
    fn a_gated_relay_answers_503_without_reaching_the_upstream() {
        let (upstream, _up) = spawn_echo_upstream();
        let ingress = HoldIngress::start("127.0.0.1:0".parse().unwrap(), &upstream.to_string())
            .expect("start ingress");

        ingress.gate_for_verification();

        let mut client = TcpStream::connect(ingress.listen_addr()).expect("connect via ingress");
        client.write_all(b"GET / HTTP/1.0\r\n\r\n").expect("write");
        let mut got = String::new();
        client.read_to_string(&mut got).expect("read");

        assert!(
            got.starts_with("HTTP/1.1 503 "),
            "expected a 503 status line, got {got:?}"
        );
        assert!(
            got.contains("Retry-After:"),
            "a gated response must tell the client when to come back: {got:?}"
        );
        assert!(
            !got.contains("hello"),
            "the gate must not reach the upstream at all: {got:?}"
        );
        ingress.stop();
    }

    /// The gate applies to connections that arrive AFTER it closes.
    ///
    /// It is checked per accepted connection rather than once, because the gate
    /// closes while the accept loop is parked in `accept()`.
    #[test]
    fn the_gate_applies_to_connections_accepted_after_it_closes() {
        let (upstream, _up) = spawn_echo_upstream();
        let ingress = HoldIngress::start("127.0.0.1:0".parse().unwrap(), &upstream.to_string())
            .expect("start ingress");

        // Before the gate: a normal relayed response.
        let mut first = TcpStream::connect(ingress.listen_addr()).expect("connect");
        first.write_all(b"GET / HTTP/1.0\r\n\r\n").expect("write");
        let mut before = String::new();
        first.read_to_string(&mut before).expect("read");
        assert!(before.contains("hello"), "pre-gate body: {before:?}");

        ingress.gate_for_verification();

        // After: refused, on a connection the loop accepted later.
        let mut second = TcpStream::connect(ingress.listen_addr()).expect("connect");
        second.write_all(b"GET / HTTP/1.0\r\n\r\n").expect("write");
        let mut after = String::new();
        second.read_to_string(&mut after).expect("read");
        assert!(
            after.starts_with("HTTP/1.1 503 "),
            "post-gate response: {after:?}"
        );
        ingress.stop();
    }
}
