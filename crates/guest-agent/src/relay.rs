//! ato#1026: an app-agnostic TCP relay for imported apps that bind
//! `127.0.0.1:PORT` inside the guest.
//!
//! The Firecracker readiness probe and the runtime app-proxy both dial the
//! guest's ROUTABLE IP (`reachable_host():port`), so an app listening only on
//! loopback is invisible to them and never passes readiness — even though it
//! is running (the mycorrhiza-wiki case on ato#1024). This relay listens on
//! the guest's own configured IP:PORT (a DIFFERENT address than the app's
//! `127.0.0.1:PORT`, so no bind conflict) and forwards each connection to
//! `127.0.0.1:PORT`.
//!
//! It is a subcommand of the already-injected, musl-static guest-agent so it
//! needs no tooling in the arbitrary app image. Invoked by the generated init
//! ONLY when the import opted in (`host_bind_relay`), never by default.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Parsed `tcp-relay` invocation.
struct RelayArgs {
    listen: SocketAddr,
    target: SocketAddr,
}

/// The guest's own configured IPv4, parsed from the kernel `ip=<ip>::…` cmdline
/// param (`crates/snapshot/src/firecracker.rs` sets `ip={guest_ip}::{host_ip}:…`).
/// `/proc/cmdline` is always present in the guest and needs no `iproute2` in the
/// (arbitrary) app image. Returns None if the param is absent/malformed.
fn guest_ipv4_from_cmdline(cmdline: &str) -> Option<std::net::Ipv4Addr> {
    for tok in cmdline.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("ip=") {
            // Format: <client-ip>::<gw>:<mask>:… — the client IP is field 0.
            let ip = rest.split(':').next().unwrap_or("");
            if let Ok(parsed) = ip.parse::<std::net::Ipv4Addr>() {
                return Some(parsed);
            }
        }
    }
    None
}

/// Parse `tcp-relay --listen <ip:port> --target <ip:port>` OR
/// `tcp-relay --listen-guest-port <port> --target <ip:port>` (resolve the
/// guest's own IPv4 from `/proc/cmdline`, so init needs no shell IP-parsing).
/// Strict: exactly one listen form + a target; values must parse. Returns an
/// error string rather than launching a half-configured relay.
fn parse_args(args: &[String]) -> Result<RelayArgs, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut listen_guest_port: Option<u16> = None;
    let mut target: Option<SocketAddr> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--listen" => {
                let v = it.next().ok_or("--listen requires a value")?;
                listen = Some(v.parse().map_err(|e| format!("bad --listen {v:?}: {e}"))?);
            }
            "--listen-guest-port" => {
                let v = it.next().ok_or("--listen-guest-port requires a value")?;
                listen_guest_port = Some(v.parse().map_err(|e| format!("bad --listen-guest-port {v:?}: {e}"))?);
            }
            "--target" => {
                let v = it.next().ok_or("--target requires a value")?;
                target = Some(v.parse().map_err(|e| format!("bad --target {v:?}: {e}"))?);
            }
            other => return Err(format!("unknown tcp-relay arg {other:?}")),
        }
    }
    let listen = match (listen, listen_guest_port) {
        (Some(_), Some(_)) => return Err("--listen and --listen-guest-port are mutually exclusive".into()),
        (Some(l), None) => l,
        (None, Some(port)) => {
            let cmdline = std::fs::read_to_string("/proc/cmdline")
                .map_err(|e| format!("read /proc/cmdline: {e}"))?;
            let ip = guest_ipv4_from_cmdline(&cmdline)
                .ok_or("could not resolve the guest IPv4 from /proc/cmdline (no ip= param)")?;
            SocketAddr::from((ip, port))
        }
        (None, None) => return Err("one of --listen / --listen-guest-port is required".into()),
    };
    Ok(RelayArgs {
        listen,
        target: target.ok_or("--target is required")?,
    })
}

/// Copy `from` → `to` until EOF, then half-close `to` for writing so the peer
/// sees the end of stream. Errors are swallowed: a relayed connection dying is
/// normal (client hangup), never fatal to the relay.
fn pump(mut from: TcpStream, to: TcpStream) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if (&to).write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}

/// Forward one accepted connection to the target, retrying the target connect
/// briefly so a relay started just before the app finishes binding loopback
/// still lands (the app's own listen races the first client). Bidirectional:
/// one thread each way.
fn handle(client: TcpStream, target: SocketAddr) {
    // The app may not be listening on loopback yet when the first probe
    // arrives; retry the target connect for a bounded window before giving up.
    let mut upstream = None;
    for _ in 0..40 {
        if let Ok(s) = TcpStream::connect_timeout(&target, Duration::from_millis(250)) {
            upstream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let Some(upstream) = upstream else { return };
    let (c2, u2) = match (client.try_clone(), upstream.try_clone()) {
        (Ok(c2), Ok(u2)) => (c2, u2),
        _ => return,
    };
    std::thread::spawn(move || pump(client, upstream));
    pump(u2, c2);
}

/// Run the relay: accept on `listen`, forward each connection to `target`.
/// Blocks forever (the init backgrounds it). Returns only on a fatal listen
/// error.
pub fn run(args: &[String]) -> Result<(), String> {
    let cfg = parse_args(args)?;
    let listener = TcpListener::bind(cfg.listen)
        .map_err(|e| format!("tcp-relay: bind {} failed: {e}", cfg.listen))?;
    eprintln!("ato-guest-agent: tcp-relay {} -> {}", cfg.listen, cfg.target);
    for stream in listener.incoming() {
        match stream {
            Ok(client) => {
                let target = cfg.target;
                std::thread::spawn(move || handle(client, target));
            }
            // Transient accept errors (e.g. EMFILE) must not kill the relay.
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parse_args_requires_both_flags() {
        assert!(parse_args(&["--listen".into(), "127.0.0.1:1".into()]).is_err());
        assert!(parse_args(&["--target".into(), "127.0.0.1:1".into()]).is_err());
        assert!(parse_args(&["--bogus".into()]).is_err());
        let ok = parse_args(&[
            "--listen".into(),
            "172.16.0.2:1737".into(),
            "--target".into(),
            "127.0.0.1:1737".into(),
        ])
        .unwrap();
        assert_eq!(ok.listen.port(), 1737);
        assert_eq!(ok.target.ip().to_string(), "127.0.0.1");
    }

    #[test]
    fn parse_args_rejects_bad_socket_addr() {
        assert!(parse_args(&["--listen".into(), "not-an-addr".into(), "--target".into(), "127.0.0.1:1".into()]).is_err());
    }

    #[test]
    fn parse_args_rejects_both_listen_forms() {
        assert!(parse_args(&[
            "--listen".into(), "127.0.0.1:1".into(),
            "--listen-guest-port".into(), "1".into(),
            "--target".into(), "127.0.0.1:1".into(),
        ])
        .is_err());
    }

    #[test]
    fn guest_ip_parses_the_kernel_ip_param() {
        let cmdline = "console=ttyS0 reboot=k panic=1 pci=off ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off";
        assert_eq!(
            guest_ipv4_from_cmdline(cmdline).unwrap().to_string(),
            "172.16.0.2"
        );
        assert!(guest_ipv4_from_cmdline("console=ttyS0 quiet").is_none());
        assert!(guest_ipv4_from_cmdline("ip=garbage::x").is_none());
    }

    #[test]
    fn relay_forwards_bytes_end_to_end() {
        // A stand-in "loopback app": echo the first line back uppercased.
        let app = TcpListener::bind("127.0.0.1:0").unwrap();
        let app_addr = app.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = app.accept() {
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).unwrap_or(0);
                let up = String::from_utf8_lossy(&buf[..n]).to_uppercase();
                let _ = s.write_all(up.as_bytes());
            }
        });

        // Relay listens on another loopback port (stands in for guest_ip:PORT)
        // and forwards to the app. Binding a different addr than the app's is
        // exactly the no-conflict property the design relies on.
        let relay_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        drop(relay_listener); // free the port for run() to bind
        let target = app_addr;
        std::thread::spawn(move || {
            let _ = run(&[
                "--listen".into(),
                relay_addr.to_string(),
                "--target".into(),
                target.to_string(),
            ]);
        });

        // Give the relay a moment to bind.
        std::thread::sleep(Duration::from_millis(200));
        let mut c = TcpStream::connect(relay_addr).unwrap();
        c.write_all(b"hello").unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert_eq!(resp, "HELLO");
    }
}
