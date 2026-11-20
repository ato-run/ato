//! A private HTTP observation of the window owned by this Run's process.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::process_group::ProcessServiceGroup;
use anyhow::{Result, bail};
use ato_host_facts::window::visible_window;
use ato_runtime_attempt::control::{AttemptPhase, ExecutionControl};

pub(super) struct WindowProbe {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl WindowProbe {
    pub fn observe(
        group: &mut ProcessServiceGroup,
        pid: u32,
        control: Option<&ExecutionControl>,
    ) -> Result<Self> {
        // Reserve before observing. Another process can neither impersonate this
        // endpoint nor replace it between readiness and Contract observation.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let pause = match control {
                Some(control) => control.cap(AttemptPhase::Launch, Duration::from_millis(100))?,
                None => Duration::from_millis(100),
            };
            if let Some(exit) = group.exited()? {
                bail!("native_window_process_exited: {exit}");
            }
            if visible_window(pid) {
                break;
            }
            if Instant::now() >= deadline {
                bail!("native_window_not_visible: no on-screen window for pid {pid}");
            }
            std::thread::sleep(pause);
        }
        Ok(Self::serve(listener, port))
    }

    fn serve(listener: TcpListener, port: u16) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS accept inherits O_NONBLOCK from the listener.
                        // Wait for the bounded complete header, not an empty read.
                        let _ = stream.set_nonblocking(false);
                        let deadline = Instant::now() + Duration::from_secs(2);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
                        let mut bytes = [0_u8; 4096];
                        let mut used = 0;
                        while used < bytes.len()
                            && Instant::now() < deadline
                            && !done.load(Ordering::Relaxed)
                        {
                            match stream.read(&mut bytes[used..]) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => {
                                    used += count;
                                    if bytes[..used].windows(4).any(|s| s == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                            }
                        }
                        let body = if bytes[..used].starts_with(b"GET /ready HTTP/1.") {
                            Some(b"{\"window_visible\":true}".as_slice())
                        } else {
                            None
                        };
                        let (status, body) = body
                            .map(|b| ("200 OK", b))
                            .unwrap_or(("404 Not Found", b""));
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for WindowProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_endpoint_waits_for_a_fragmented_request_and_refuses_other_paths() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let _probe = WindowProbe::serve(listener, port);
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client.write_all(b"GET /re").unwrap();
        std::thread::sleep(Duration::from_millis(40));
        client
            .write_all(b"ady HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.ends_with("{\"window_visible\":true}"));
        let response = reqwest::blocking::get(format!("http://127.0.0.1:{port}/other")).unwrap();
        assert_eq!(response.status(), 404);
    }
}
