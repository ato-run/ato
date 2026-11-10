//! A tiny loopback control channel for the workspace shell.
//!
//! The shell is a static page, so it cannot call the CLI directly. This server
//! accepts one same-origin-exempt POST (`/stop?token=…`) from the page and sets
//! a flag the CLI polls; the token is per run and the listener is loopback only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct ControlHandle {
    url: String,
    token: String,
    stop: Arc<AtomicBool>,
    checkpoint: Arc<AtomicBool>,
    status: Arc<Mutex<Option<(bool, String)>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlHandle {
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = random_token()?;
        let stop = Arc::new(AtomicBool::new(false));
        let checkpoint = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_checkpoint = Arc::clone(&checkpoint);
        let thread_status = Arc::clone(&status);
        let thread_shutdown = Arc::clone(&shutdown);
        let stop_path = format!("/stop?token={token}");
        let checkpoint_path = format!("/checkpoint?token={token}");
        let status_path = format!("/status?token={token}");
        let thread = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = answer(
                            stream,
                            &stop_path,
                            &checkpoint_path,
                            &status_path,
                            &thread_stop,
                            &thread_checkpoint,
                            &thread_status,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            }
        });
        Ok(Self {
            url: format!("http://{address}"),
            token,
            stop,
            checkpoint,
            status,
            shutdown,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Consume one stop request; a failed checkpoint keeps the workspace open
    /// so the request must not fire again on its own.
    pub fn take_stop_requested(&self) -> bool {
        self.stop.swap(false, Ordering::AcqRel)
    }

    /// Consume one checkpoint request, so a held page button saves once.
    pub fn take_checkpoint_requested(&self) -> bool {
        self.checkpoint.swap(false, Ordering::AcqRel)
    }

    /// Record the outcome of the last checkpoint write for the page to read.
    pub fn record_checkpoint_result(&self, ok: bool, message: impl Into<String>) {
        if let Ok(mut status) = self.status.lock() {
            *status = Some((ok, message.into()));
        }
    }
}

impl Drop for ControlHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Ok(mut stream) = TcpStream::connect_timeout(
            &self
                .url
                .trim_start_matches("http://")
                .parse()
                .expect("loopback address"),
            Duration::from_millis(200),
        ) {
            let _ = stream.write_all(b"GET /shutdown HTTP/1.1\r\n\r\n");
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(
    mut stream: TcpStream,
    stop_path: &str,
    checkpoint_path: &str,
    status_path: &str,
    stop: &AtomicBool,
    checkpoint: &AtomicBool,
    status: &Mutex<Option<(bool, String)>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0u8; 4096];
    let read = stream.read(&mut request)?;
    let head = String::from_utf8_lossy(&request[..read]);
    let action = head.split_whitespace().nth(1).unwrap_or_default();
    if action == status_path {
        let body = match status.lock().ok().and_then(|status| status.clone()) {
            Some((true, message)) => format!("ok: {message}"),
            Some((false, message)) => format!("error: {message}"),
            None => "pending".to_owned(),
        };
        stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\naccess-control-allow-origin: *\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )?;
        return Ok(());
    }
    let status_line = if action == stop_path {
        stop.store(true, Ordering::Release);
        "200 OK"
    } else if action == checkpoint_path {
        checkpoint.store(true, Ordering::Release);
        "200 OK"
    } else {
        "404 Not Found"
    };
    stream.write_all(
        format!(
            "HTTP/1.1 {status_line}\r\ncontent-type: text/plain\r\naccess-control-allow-origin: *\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("entropy unavailable: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::ControlHandle;

    fn post(control: &ControlHandle, action: &str) -> String {
        let address = control.url().trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                format!(
                    "POST /{action}?token={} HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    control.token()
                )
                .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn a_checkpoint_request_is_consumed_once_and_stop_sets_the_stop_flag() {
        let control = ControlHandle::start().unwrap();
        let response = post(&control, "checkpoint");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(control.take_checkpoint_requested());
        assert!(!control.take_checkpoint_requested());
        assert!(!control.stop_requested());
        let response = post(&control, "stop");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(control.stop_requested());
    }

    fn get_status(control: &ControlHandle) -> String {
        let address = control.url().trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                format!(
                    "GET /status?token={} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
                    control.token()
                )
                .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn the_checkpoint_outcome_is_readable_from_the_page() {
        let control = ControlHandle::start().unwrap();
        assert!(get_status(&control).ends_with("pending"));
        control.record_checkpoint_result(true, "3 panes");
        assert!(get_status(&control).ends_with("ok: 3 panes"));
        control.record_checkpoint_result(false, "disk full");
        assert!(get_status(&control).ends_with("error: disk full"));
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let control = ControlHandle::start().unwrap();
        let address = control.url().trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                b"POST /stop?token=wrong HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
        assert!(!control.stop_requested());
    }
}
