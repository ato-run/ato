//! Minimal HTTP healthcheck used by the fast path to validate that a
//! stored session is still serving requests before reuse. Lives here
//! (rather than in `capsule-wire`) because pure-DTO crates must not
//! own network code (RFC §3.2 design boundary).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, Result};

/// Returns `Ok(true)` when `url` answers HTTP 200 within `timeout`.
///
/// Treats every transient I/O failure (EAGAIN/ECONNRESET/timeout) as
/// "not ready" rather than propagating an error, mirroring the CLI's
/// `http_get_ok` behaviour. Only `Err` is returned when the URL is
/// itself malformed (parse error) — call sites can map that to "fall
/// through to spawn" identically to a `false` answer if they don't
/// care about the distinction.
pub fn http_get_ok(url: &str, timeout: Duration) -> Result<bool> {
    let parsed = parse_http_url(url)?;
    let address = (parsed.host.as_str(), parsed.port);
    let socket_addr = match address.to_socket_addrs() {
        Ok(mut iter) => match iter.next() {
            Some(addr) => addr,
            None => return Ok(false),
        },
        Err(_) => return Ok(false),
    };

    let Ok(mut stream) = TcpStream::connect_timeout(&socket_addr, timeout) else {
        return Ok(false);
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return Ok(false);
    }
    if write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        parsed.path, parsed.host
    )
    .is_err()
        || stream.flush().is_err()
    {
        return Ok(false);
    }

    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return Ok(false);
    }
    Ok(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
}

struct ParsedHttpUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedHttpUrl> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("expected http:// URL: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_str)) => {
            let port: u16 = port_str
                .parse()
                .map_err(|_| anyhow!("invalid port in {url}"))?;
            (host.to_string(), port)
        }
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(anyhow!("empty host in {url}"));
    }
    Ok(ParsedHttpUrl {
        host,
        port,
        path: path.to_string(),
    })
}

use std::net::ToSocketAddrs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_url_accepts_host_port_path() {
        let parsed = parse_http_url("http://127.0.0.1:5173/health").expect("parse");
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 5173);
        assert_eq!(parsed.path, "/health");
    }

    #[test]
    fn parse_http_url_defaults_path_to_slash() {
        let parsed = parse_http_url("http://127.0.0.1:5173").expect("parse");
        assert_eq!(parsed.path, "/");
    }

    #[test]
    fn parse_http_url_defaults_port_to_80() {
        let parsed = parse_http_url("http://example.com/").expect("parse");
        assert_eq!(parsed.port, 80);
    }

    #[test]
    fn parse_http_url_rejects_https() {
        assert!(parse_http_url("https://example.com").is_err());
    }

    #[test]
    fn http_get_ok_returns_false_when_nothing_listens() {
        // Use an unbound high port; expect no HTTP 200 within the
        // short timeout. Caller must handle false (not Err).
        let answer = http_get_ok("http://127.0.0.1:1/health", Duration::from_millis(50))
            .expect("not malformed");
        assert!(!answer);
    }
}
