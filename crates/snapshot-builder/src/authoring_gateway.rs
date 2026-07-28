//! One registered builder slot serving both Authoring Preview and Terminal.
//!
//! Preview traffic is an opaque TCP relay to the held guest. Only the
//! session-bound Terminal path is terminated here as a WebSocket, and only
//! after the API-injected builder bearer is verified.

use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AuthoringGateway {
    listen: SocketAddr,
    stopping: Arc<AtomicBool>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl AuthoringGateway {
    pub fn start(
        listen: SocketAddr,
        preview_upstream: &str,
        builder_session_id: &str,
        expected_builder_token: &str,
        terminal_lines: Vec<String>,
    ) -> io::Result<Self> {
        let upstream: SocketAddr = preview_upstream.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Authoring Preview upstream is not an ip:port address",
            )
        })?;
        TcpStream::connect_timeout(&upstream, CONNECT_TIMEOUT)?;
        let listener = TcpListener::bind(listen)?;
        let bound = listener.local_addr()?;
        let stopping = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stopping);
        let terminal_path = format!("/authoring/{builder_session_id}/terminal");
        let expected_authorization = format!("Bearer {expected_builder_token}");
        let accept_thread = std::thread::Builder::new()
            .name("ato-authoring-gateway".to_string())
            .spawn(move || {
                accept_loop(
                    listener,
                    upstream,
                    terminal_path,
                    expected_authorization,
                    terminal_lines,
                    stop_flag,
                )
            })?;
        Ok(Self {
            listen: bound,
            stopping,
            accept_thread: Some(accept_thread),
        })
    }

    #[allow(dead_code)]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.listen, Duration::from_millis(500));
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AuthoringGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    listener: TcpListener,
    preview_upstream: SocketAddr,
    terminal_path: String,
    expected_authorization: String,
    terminal_lines: Vec<String>,
    stopping: Arc<AtomicBool>,
) {
    for incoming in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = incoming else { continue };
        let path = terminal_path.clone();
        let authorization = expected_authorization.clone();
        let lines = terminal_lines.clone();
        let _ = std::thread::Builder::new()
            .name("ato-authoring-gateway-conn".to_string())
            .spawn(move || {
                if is_terminal_request(&client, &path) {
                    serve_terminal(client, &path, &authorization, &lines);
                } else if let Err(error) = relay_preview(client, preview_upstream) {
                    eprintln!("[builder] Authoring Preview connection ended: {error}");
                }
            });
    }
}

fn is_terminal_request(stream: &TcpStream, path: &str) -> bool {
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let mut prefix = [0_u8; 8192];
    let Ok(length) = stream.peek(&mut prefix) else {
        return false;
    };
    let request = String::from_utf8_lossy(&prefix[..length]);
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        == Some(path)
}

#[allow(clippy::result_large_err)]
fn serve_terminal(
    stream: TcpStream,
    expected_path: &str,
    expected_authorization: &str,
    lines: &[String],
) {
    let accepted = tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
        authorize_terminal_handshake(request, response, expected_path, expected_authorization)
    });
    let Ok(mut socket) = accepted else {
        return;
    };
    for line in lines {
        if socket
            .send(Message::Text(format!("{line}\r\n").into()))
            .is_err()
        {
            return;
        }
    }
    let notice =
        "Suggested setup is reproducible and read-only. Select manual setup to run commands.";
    if socket
        .send(Message::Text(format!("{notice}\r\n").into()))
        .is_err()
    {
        return;
    }
    while let Ok(message) = socket.read() {
        match message {
            Message::Ping(bytes) => {
                if socket.send(Message::Pong(bytes)).is_err() {
                    break;
                }
            }
            Message::Text(_) | Message::Binary(_) => {
                if socket
                    .send(Message::Text(format!("{notice}\r\n").into()))
                    .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

#[allow(clippy::result_large_err)]
fn authorize_terminal_handshake(
    request: &Request,
    response: Response,
    expected_path: &str,
    expected_authorization: &str,
) -> Result<Response, ErrorResponse> {
    let path_matches = request.uri().path() == expected_path;
    let authorization_matches = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(expected_authorization);
    if path_matches && authorization_matches {
        return Ok(response);
    }
    let mut denied = ErrorResponse::new(Some(
        "Authoring Terminal authentication failed.".to_string(),
    ));
    *denied.status_mut() = StatusCode::UNAUTHORIZED;
    Err(denied)
}

fn relay_preview(mut client: TcpStream, upstream: SocketAddr) -> io::Result<()> {
    let mut server = TcpStream::connect_timeout(&upstream, CONNECT_TIMEOUT)?;
    let mut client_read = client.try_clone()?;
    let mut server_write = server.try_clone()?;
    let upstream_half = std::thread::spawn(move || {
        let copied = io::copy(&mut client_read, &mut server_write);
        let _ = server_write.shutdown(Shutdown::Write);
        copied
    });
    let downstream = io::copy(&mut server, &mut client);
    let _ = client.shutdown(Shutdown::Both);
    let _ = server.shutdown(Shutdown::Both);
    let _ = upstream_half.join();
    downstream.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn preview_requests_are_relayed_without_http_rewriting() {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream");
        let upstream_address = upstream.local_addr().expect("address");
        let echo = std::thread::spawn(move || {
            let (probe, _) = upstream.accept().expect("probe");
            drop(probe);
            let (mut stream, _) = upstream.accept().expect("accept");
            let mut request = [0_u8; 128];
            let read = stream.read(&mut request).expect("read");
            stream.write_all(&request[..read]).expect("echo");
        });
        let gateway = AuthoringGateway::start(
            "127.0.0.1:0".parse().unwrap(),
            &upstream_address.to_string(),
            "setup_1",
            "builder-secret",
            Vec::new(),
        )
        .expect("gateway");
        let mut client = TcpStream::connect(gateway.listen_addr()).expect("connect");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: preview.example\r\n\r\n")
            .expect("write");
        let mut echoed = [0_u8; 128];
        let read = client.read(&mut echoed).expect("read");
        assert!(String::from_utf8_lossy(&echoed[..read]).starts_with("GET / HTTP/1.1"));
        drop(gateway);
        echo.join().expect("echo thread");
    }
}
