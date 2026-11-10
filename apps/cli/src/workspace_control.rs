//! A tiny loopback control channel for the workspace shell.
//!
//! The shell is a static page, so it cannot call the CLI directly. This server
//! accepts one same-origin-exempt POST (`/stop?token=…`) from the page and sets
//! a flag the CLI polls; the token is per run and the listener is loopback only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct ControlHandle {
    url: String,
    token: String,
    stop: Arc<AtomicBool>,
    checkpoint: Arc<AtomicBool>,
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_checkpoint = Arc::clone(&checkpoint);
        let thread_shutdown = Arc::clone(&shutdown);
        let stop_path = format!("/stop?token={token}");
        let checkpoint_path = format!("/checkpoint?token={token}");
        let thread = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = answer(
                            stream,
                            &stop_path,
                            &checkpoint_path,
                            &thread_stop,
                            &thread_checkpoint,
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

    /// Consume one checkpoint request, so a held page button saves once.
    pub fn take_checkpoint_requested(&self) -> bool {
        self.checkpoint.swap(false, Ordering::AcqRel)
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
    stop: &AtomicBool,
    checkpoint: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0u8; 4096];
    let read = stream.read(&mut request)?;
    let head = String::from_utf8_lossy(&request[..read]);
    let action = head.split_whitespace().nth(1).unwrap_or_default();
    let status = if action == stop_path {
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
            "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\naccess-control-allow-origin: *\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
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
