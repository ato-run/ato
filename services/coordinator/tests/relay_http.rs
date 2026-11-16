//! The relay HTTP client must not depend on the server closing the socket.
//!
//! A keep-alive response (or a 204 with no body) never reaches EOF, so a
//! client that reads to EOF turns every poll into a read timeout and the relay
//! loop stalls after its first step. These tests frame responses exactly like a
//! keep-alive server and assert the client returns promptly.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use ato_coordinator::relay::{HttpRelayTransport, RelayTransport};

fn serve_once(response: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let (socket, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(socket.try_clone().expect("clone"));
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read line") == 0 || line.trim().is_empty() {
                break;
            }
        }
        let mut writer = socket;
        writer.write_all(&response).expect("write response");
        writer.flush().ok();
        std::thread::sleep(Duration::from_secs(3));
    });
    port
}

fn transport(port: u16) -> HttpRelayTransport {
    HttpRelayTransport::new(
        &format!("http://127.0.0.1:{port}"),
        "relay_test",
        "origin-token",
    )
}

fn next_body(request_id: &str) -> String {
    let envelope = format!(
        "{{\"schema\":\"ato.relay-envelope/1\",\"request_id\":\"{request_id}\",\"origin_epoch\":1,\"method\":\"POST\",\"path\":\"/v1/data-resources\",\"body\":\"{{}}\",\"authorization\":\"Bearer data-token\"}}"
    );
    format!(
        "{{\"request_id\":\"{request_id}\",\"origin_epoch\":1,\"envelope\":{}}}",
        serde_json::to_string(&envelope).expect("serialize")
    )
}

#[test]
fn a_204_with_keep_alive_returns_idle_without_waiting_for_eof() {
    let port = serve_once(b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n".to_vec());
    let mut transport = transport(port);
    let started = Instant::now();
    let polled = transport.poll().expect("poll");
    assert!(polled.is_none(), "204 means no envelope");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a bodyless response must not wait for the read timeout (took {:?})",
        started.elapsed()
    );
}

#[test]
fn a_content_length_body_is_returned_without_eof() {
    let body = next_body("req-cl");
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    let port = serve_once(response);
    let mut transport = transport(port);
    let started = Instant::now();
    let polled = transport.poll().expect("poll").expect("envelope");
    assert_eq!(polled.request_id, "req-cl");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a framed body must not wait for eof (took {:?})",
        started.elapsed()
    );
}

#[test]
fn a_chunked_body_is_decoded_without_eof() {
    let body = next_body("req-chunk");
    let half = body.len() / 2;
    let response = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        half,
        &body[..half],
        body.len() - half,
        &body[half..]
    )
    .into_bytes();
    let port = serve_once(response);
    let mut transport = transport(port);
    let polled = transport.poll().expect("poll").expect("envelope");
    assert_eq!(polled.request_id, "req-chunk");
}
