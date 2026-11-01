//! Real loopback TLS servers and SMTP STARTTLS conversations.
//!
//! Each test binds its own loopback listener, so the suite is offline and
//! parallel-safe. Fixtures are TEST-ONLY self-signed material; see
//! tests/fixtures/README.md for the exact openssl commands.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ato_tls_observe::{TlsStart, TrustAnchors, observe_tls};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const TIMEOUT: Duration = Duration::from_secs(10);

fn fixture(relative: &str) -> String {
    format!("{}/tests/fixtures/{relative}", env!("CARGO_MANIFEST_DIR"))
}

fn fixture_der(relative: &str, label: &str) -> Vec<u8> {
    let path = fixture(relative);
    let pem = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    pem_der(&pem, label)
}

fn pem_der(pem: &str, label: &str) -> Vec<u8> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body_start = pem.find(&begin).expect("PEM begin marker") + begin.len();
    let body_end = pem.find(&end).expect("PEM end marker");
    let encoded: String = pem[body_start..body_end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    base64_decode(&encoded)
}

fn base64_decode(encoded: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [u8::MAX; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        lookup[usize::from(*byte)] = index as u8;
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in encoded.bytes() {
        if byte == b'=' {
            break;
        }
        let value = lookup[usize::from(byte)];
        assert_ne!(value, u8::MAX, "invalid base64 byte");
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            decoded.push((accumulator >> bits) as u8);
            accumulator &= (1u32 << bits) - 1;
        }
    }
    decoded
}

fn test_anchors(certificate: &str) -> TrustAnchors {
    TrustAnchors::TestOnly(vec![CertificateDer::from(fixture_der(
        certificate,
        "CERTIFICATE",
    ))])
}

fn server_config(certificate: &str, key: &str) -> Arc<rustls::ServerConfig> {
    server_config_with_versions(certificate, key, rustls::DEFAULT_VERSIONS)
}

fn server_config_with_versions(
    certificate: &str,
    key: &str,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Arc<rustls::ServerConfig> {
    let certificate = CertificateDer::from(fixture_der(certificate, "CERTIFICATE"));
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture_der(key, "PRIVATE KEY")));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("server protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("server certificate and key");
    Arc::new(config)
}

fn spawn_tls_server(certificate: &str, key: &str) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    let config = server_config(certificate, key);
    thread::spawn(move || {
        let Ok((tcp, _)) = listener.accept() else {
            return;
        };
        let connection = rustls::ServerConnection::new(config).expect("server connection");
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        // The client may reject the certificate, so the handshake can fail
        // here; the observation under test already happened on the client.
        let _ = stream.conn.complete_io(&mut stream.sock);
    });
    address
}

fn spawn_smtp_server(certificate: &str, key: &str, offer_starttls: bool) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    let config = server_config(certificate, key);
    thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let _ = tcp.set_read_timeout(Some(TIMEOUT));
        let _ = tcp.write_all(b"220 mail.ato.test ESMTP\r\n");
        let _ = read_smtp_line(&mut tcp);
        if !offer_starttls {
            let _ = tcp.write_all(b"250-mail.ato.test\r\n250 HELP\r\n");
            let _ = read_smtp_line(&mut tcp);
            return;
        }
        let _ = tcp.write_all(b"250-mail.ato.test\r\n250 STARTTLS\r\n");
        let _ = read_smtp_line(&mut tcp);
        let _ = tcp.write_all(b"220 2.0.0 Ready to start TLS\r\n");
        let connection = rustls::ServerConnection::new(config).expect("server connection");
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        let _ = stream.conn.complete_io(&mut stream.sock);
    });
    address
}

fn read_smtp_line(stream: &mut TcpStream) -> Vec<u8> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
        }
    }
    line
}

#[test]
fn implicit_tls_reports_the_presented_certificate() {
    let address = spawn_tls_server("valid/cert.pem", "valid/key.pem");
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        TIMEOUT,
        test_anchors("valid/cert.pem"),
    )
    .expect("observation");

    assert!(facts.trusted);
    assert!(facts.server_name_matches);
    assert!(facts.within_validity);
    assert_eq!(facts.presented_serial, "1234abcd");
    assert_eq!(facts.san, vec!["mail.ato.test", "mail.example"]);
    assert_eq!(facts.not_before, "2026-01-01T00:00:00Z");
    assert_eq!(facts.not_after, "2046-01-01T00:00:00Z");
    assert!(facts.days_remaining > 0);
    assert!(facts.chain_length >= 1);
    assert_eq!(facts.trust_anchor, "test-only");
    assert_eq!(facts.server_name, "mail.ato.test");
    assert_eq!(facts.start, TlsStart::Implicit);
    assert!(facts.handshake_completed);
    assert!(facts.handshake_error.is_none());
}

#[test]
fn public_roots_report_an_untrusted_self_signed_chain() {
    let address = spawn_tls_server("valid/cert.pem", "valid/key.pem");
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        TIMEOUT,
        TrustAnchors::PublicRoots,
    )
    .expect("facts are reported even when the chain is untrusted");

    assert!(!facts.trusted);
    assert_eq!(facts.trust_anchor, "public-roots");
    assert_eq!(facts.presented_serial, "1234abcd");
    assert!(facts.server_name_matches);
    assert!(facts.within_validity);
    assert!(!facts.handshake_completed);
    assert!(facts.handshake_error.is_some());
}

#[test]
fn a_name_mismatch_is_reported_with_the_facts() {
    let address = spawn_tls_server("valid/cert.pem", "valid/key.pem");
    let facts = observe_tls(
        address,
        "wrong.example",
        TlsStart::Implicit,
        TIMEOUT,
        test_anchors("valid/cert.pem"),
    )
    .expect("facts are reported even when the name does not match");

    assert!(!facts.trusted);
    assert!(!facts.server_name_matches);
    assert!(facts.within_validity);
    assert_eq!(facts.san, vec!["mail.ato.test", "mail.example"]);
    assert!(!facts.handshake_completed);
    assert!(facts.handshake_error.is_some());
}

#[test]
fn an_expired_certificate_is_reported_outside_its_validity() {
    let address = spawn_tls_server("expired/cert.pem", "expired/key.pem");
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        TIMEOUT,
        test_anchors("expired/cert.pem"),
    )
    .expect("facts are reported even when the certificate has expired");

    assert!(!facts.within_validity);
    assert!(facts.days_remaining < 0);
    assert_eq!(facts.presented_serial, "5555");
    assert!(!facts.handshake_completed);
    assert!(facts.handshake_error.is_some());
}

#[test]
fn smtp_starttls_observes_the_same_certificate() {
    let address = spawn_smtp_server("valid/cert.pem", "valid/key.pem", true);
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::SmtpStarttls,
        TIMEOUT,
        test_anchors("valid/cert.pem"),
    )
    .expect("observation");

    assert_eq!(facts.start, TlsStart::SmtpStarttls);
    assert!(facts.trusted);
    assert!(facts.server_name_matches);
    assert_eq!(facts.presented_serial, "1234abcd");
    assert!(facts.handshake_completed);
    assert!(facts.handshake_error.is_none());
}

#[test]
fn smtp_without_starttls_is_an_error() {
    let address = spawn_smtp_server("valid/cert.pem", "valid/key.pem", false);
    let result = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::SmtpStarttls,
        TIMEOUT,
        test_anchors("valid/cert.pem"),
    );

    assert!(result.is_err());
}

#[test]
fn connection_refused_is_an_error() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    drop(listener);

    let result = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        Duration::from_secs(2),
        test_anchors("valid/cert.pem"),
    );

    assert!(result.is_err());
}

#[test]
fn an_unrelated_anchor_does_not_trust_the_presented_chain() {
    let address = spawn_tls_server("valid/cert.pem", "valid/key.pem");
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        TIMEOUT,
        test_anchors("wrong-name/cert.pem"),
    )
    .expect("facts are reported even when the chain does not anchor");

    assert!(!facts.trusted);
    assert!(facts.server_name_matches);
    assert!(facts.within_validity);
    assert_eq!(facts.presented_serial, "1234abcd");
}

// A TLS 1.2 server that queues its whole server flight (server hello,
// certificate, key exchange, hello done), sends it, and immediately closes the
// connection. The client validates and records the certificate, but the
// server's Finished never arrives, so the handshake cannot complete. That is
// the regression this guards: valid certificate facts from an unfinished
// session are not a completed observation.
fn spawn_tls12_server_that_dies_after_its_flight(certificate: &str, key: &str) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    let config = server_config_with_versions(certificate, key, &[&rustls::version::TLS12]);
    thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let _ = tcp.set_read_timeout(Some(TIMEOUT));
        let _ = tcp.set_write_timeout(Some(TIMEOUT));
        let Ok(mut connection) = rustls::ServerConnection::new(config) else {
            return;
        };
        // Read until the ClientHello has been processed and the server flight
        // is queued; a single read may carry only part of the ClientHello.
        let mut steps = 0;
        while !connection.wants_write() {
            steps += 1;
            if steps > 8 {
                return;
            }
            if connection.read_tls(&mut tcp).unwrap_or(0) == 0 {
                return;
            }
            if connection.process_new_packets().is_err() {
                return;
            }
        }
        let mut flight = Vec::new();
        if connection.write_tls(&mut flight).is_err() {
            return;
        }
        let _ = tcp.write_all(&flight);
        let _ = tcp.flush();
        let _ = tcp.shutdown(Shutdown::Both);
    });
    address
}

// A loopback listener that accepts and then says nothing at all.
fn spawn_silent_server() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    thread::spawn(move || {
        if let Ok((tcp, _)) = listener.accept() {
            thread::sleep(Duration::from_secs(2));
            drop(tcp);
        }
    });
    address
}

// Sends the whole SMTP STARTTLS preamble byte by byte, faster than the read
// timeout at each step but slower than the observation budget as a whole.
// Without a total deadline the conversation would finish; with one it must
// stop when the budget is spent.
fn spawn_trickling_smtp_server() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    let address = listener.local_addr().expect("listener address");
    thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let script =
            b"220 mail.ato.test ESMTP\r\n250-mail.ato.test\r\n250 STARTTLS\r\n220 2.0.0 Ready to start TLS\r\n";
        for byte in script {
            if tcp.write_all(&[*byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(40));
        }
        thread::sleep(Duration::from_secs(2));
    });
    address
}

#[test]
fn a_valid_certificate_is_not_a_completed_observation_when_the_session_dies() {
    let address = spawn_tls12_server_that_dies_after_its_flight("valid/cert.pem", "valid/key.pem");
    let facts = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        TIMEOUT,
        test_anchors("valid/cert.pem"),
    )
    .expect("the certificate that was presented is still reported");

    assert!(facts.trusted);
    assert!(facts.server_name_matches);
    assert!(facts.within_validity);
    assert_eq!(facts.presented_serial, "1234abcd");
    assert!(!facts.handshake_completed);
    assert!(facts.handshake_error.is_some());
}

#[test]
fn a_silent_peer_exhausts_the_observation_budget() {
    let address = spawn_silent_server();
    let started = Instant::now();
    let result = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::Implicit,
        Duration::from_millis(250),
        test_anchors("valid/cert.pem"),
    );

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_whole_observation_budget_bounds_a_trickling_peer() {
    let address = spawn_trickling_smtp_server();
    let started = Instant::now();
    let error = observe_tls(
        address,
        "mail.ato.test",
        TlsStart::SmtpStarttls,
        Duration::from_millis(250),
        test_anchors("valid/cert.pem"),
    )
    .expect_err("a trickling peer must not extend the observation past its budget");

    assert_eq!(error.reason(), "the observation exceeded its timeout");
    assert!(started.elapsed() < Duration::from_secs(5));
}
