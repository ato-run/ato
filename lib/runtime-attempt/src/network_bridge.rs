//! A namespace-local proxy reaches one owner-bound Unix socket. The namespace
//! has no external interface. The socket's host peer forwards ONLY to netd's
//! fixed policy gate; it never interprets a workload destination itself.
use anyhow::{Context, Result, ensure};

pub const GUEST_SOCKET: &str = "/.ato/egress.sock";
pub const PROXY_PORT: u16 = 32189;

#[cfg(unix)]
pub struct HostBridge {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    streams: std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<u64, std::os::unix::net::UnixStream>>,
    >,
    listener: Option<std::thread::JoinHandle<()>>,
    socket: std::path::PathBuf,
}

#[cfg(unix)]
impl HostBridge {
    pub fn start(socket: &std::path::Path, target: std::net::SocketAddr) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        ensure!(
            target.ip().is_loopback(),
            "bridge peer must be an owner-local gate"
        );
        let parent = socket
            .parent()
            .context("bridge control directory missing")?;
        ensure!(
            std::fs::canonicalize(parent)? == std::path::absolute(parent)?,
            "bridge control directory must not be a symlink"
        );
        ensure!(
            std::fs::symlink_metadata(socket).is_err(),
            "bridge socket already exists"
        );
        let listener = UnixListener::bind(socket)?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let stop = stopped.clone();
        let open = streams.clone();
        let task = std::thread::spawn(move || {
            let mut children: Vec<std::thread::JoinHandle<()>> = Vec::new();
            let mut seq = 0_u64;
            while !stop.load(Ordering::Acquire) {
                children.retain(|t| !t.is_finished());
                match listener.accept() {
                    Ok((stream, _)) if children.len() < 16 => {
                        let Ok(peer) = std::net::TcpStream::connect_timeout(
                            &target,
                            std::time::Duration::from_secs(2),
                        ) else {
                            continue;
                        };
                        if let Ok(mut active) = open.lock()
                            && let Ok(clone) = stream.try_clone()
                        {
                            active.insert(seq, clone);
                        }
                        let counter = seq;
                        seq = seq.saturating_add(1);
                        let active = open.clone();
                        children.push(std::thread::spawn(move || {
                            let _ = relay(stream, peer);
                            if let Ok(mut sockets) = active.lock() {
                                sockets.remove(&counter);
                            }
                        }));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
            for child in children {
                let _ = child.join();
            }
        });
        Ok(Self {
            stopped,
            streams,
            listener: Some(task),
            socket: socket.to_path_buf(),
        })
    }
}

#[cfg(unix)]
impl Drop for HostBridge {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(streams) = self.streams.lock() {
            for stream in streams.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Some(task) = self.listener.take() {
            let _ = task.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(unix)]
fn relay(mut unix: std::os::unix::net::UnixStream, mut tcp: std::net::TcpStream) -> Result<()> {
    let mut unix_copy = unix.try_clone()?;
    let mut tcp_copy = tcp.try_clone()?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    tcp.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    unix.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    unix.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    let task = std::thread::spawn(move || {
        let _ = std::io::copy(&mut unix_copy, &mut tcp_copy);
        let _ = tcp_copy.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut tcp, &mut unix);
    let _ = unix.shutdown(std::net::Shutdown::Both);
    let _ = tcp.shutdown(std::net::Shutdown::Both);
    let _ = task.join();
    Ok(())
}

/// Called in the already isolated namespace, before any source-controlled
/// workload runs. No argv/env can choose a host address or socket here.
#[cfg(unix)]
pub fn namespace_relay() -> Result<()> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PROXY_PORT))?;
    let mut children: Vec<std::thread::JoinHandle<()>> = Vec::new();
    println!("ready");
    for stream in listener.incoming() {
        children.retain(|t| !t.is_finished());
        let tcp = stream?;
        if children.len() >= 16 {
            continue;
        }
        let unix = std::os::unix::net::UnixStream::connect(GUEST_SOCKET)?;
        children.push(std::thread::spawn(move || {
            let _ = relay(unix, tcp);
        }));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn namespace_relay() -> Result<()> {
    anyhow::bail!("scoped network namespaces require Unix")
}

pub fn start_namespace_relay() -> Result<std::process::Child> {
    use std::io::BufRead;
    let mut child = std::process::Command::new("/.ato/formation")
        .args(["sandbox-exec", "--network-relay"])
        .env_clear()
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("relay readiness missing")?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .map(|_| line);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(line)) if line == "ready\n" => Ok(child),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("namespace proxy failed readiness")
        }
    }
}
