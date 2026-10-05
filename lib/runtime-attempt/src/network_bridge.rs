//! A namespace-local proxy reaches one owner-bound Unix socket. The namespace
//! has no external interface. The socket's host peer forwards ONLY to netd's
//! fixed policy gate; it never interprets a workload destination itself.
#[cfg(unix)]
use anyhow::ensure;
use anyhow::{Context, Result};

pub const GUEST_SOCKET: &str = "/.ato/egress.sock";
pub const PROXY_PORT: u16 = 32189;
pub const INGRESS_ROOT: &str = "/.ato/ingress";

/// Ingress is a distinct owner endpoint. Its guest peer may dial only a
/// declared port on its OWN loopback, never a host resource. The handshake
/// proves the workload accepted a connection, not merely that our proxy did.
#[cfg(unix)]
pub struct IngressBridge {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    listener: Option<std::thread::JoinHandle<()>>,
    socket: std::path::PathBuf,
    streams: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<u64, std::net::TcpStream>>>,
}

#[cfg(unix)]
impl IngressBridge {
    pub fn start(socket: std::path::PathBuf, host_port: u16) -> Result<Self> {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, host_port))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let stop = stopped.clone();
        let active = streams.clone();
        let target = socket.clone();
        let task = std::thread::spawn(move || {
            let mut children: Vec<std::thread::JoinHandle<()>> = Vec::new();
            let mut seq = 0_u64;
            while !stop.load(Ordering::Acquire) {
                children.retain(|t| !t.is_finished());
                match listener.accept() {
                    Ok((tcp, _)) if children.len() < 16 => {
                        if let Ok(mut handles) = active.lock()
                            && let Ok(clone) = tcp.try_clone()
                        {
                            handles.insert(seq, clone);
                        }
                        let path = target.clone();
                        let counter = seq;
                        seq = seq.saturating_add(1);
                        let handles = active.clone();
                        children.push(std::thread::spawn(move || {
                            if let Ok(unix) = ingress_connect(&path) {
                                let _ = relay(unix, tcp);
                            }
                            if let Ok(mut handles) = handles.lock() {
                                handles.remove(&counter);
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
            for task in children {
                let _ = task.join();
            }
        });
        Ok(Self {
            stopped,
            listener: Some(task),
            socket,
            streams,
        })
    }

    pub fn ready(&self) -> Result<()> {
        ingress_connect(&self.socket).map(|_| ())
    }
}

#[cfg(unix)]
impl Drop for IngressBridge {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(handles) = self.streams.lock() {
            for stream in handles.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Some(task) = self.listener.take() {
            let _ = task.join();
        }
    }
}

#[cfg(unix)]
fn ingress_connect(socket: &std::path::Path) -> Result<std::os::unix::net::UnixStream> {
    use std::io::Read;
    let mut unix = connect_owner_socket(socket)?;
    unix.set_read_timeout(Some(std::time::Duration::from_millis(250)))?;
    let mut accepted = [0];
    unix.read_exact(&mut accepted)?;
    ensure!(accepted == [1], "guest endpoint did not accept");
    Ok(unix)
}

/// Linux resolves this reference through a held directory descriptor. The
/// private realization directory may be deeper than sockaddr_un.sun_path;
/// shortening its address must not change the endpoint or the process cwd.
#[cfg(target_os = "linux")]
fn connect_owner_socket(socket: &std::path::Path) -> Result<std::os::unix::net::UnixStream> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let parent = socket.parent().context("ingress directory missing")?;
    let name = socket.file_name().context("ingress socket name missing")?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent)?;
    let address =
        std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
    Ok(std::os::unix::net::UnixStream::connect(address)?)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn connect_owner_socket(socket: &std::path::Path) -> Result<std::os::unix::net::UnixStream> {
    Ok(std::os::unix::net::UnixStream::connect(socket)?)
}

#[cfg(all(test, target_os = "linux"))]
mod ingress_tests {
    use super::*;
    use std::io::Write;
    use std::os::{fd::AsRawFd, unix::net::UnixListener};

    #[test]
    fn ingress_handshake_reaches_a_deep_private_socket_without_chdir() {
        let root =
            tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| ".".into())).unwrap();
        let parent = root.path().join("owner-realization-".repeat(12));
        std::fs::create_dir(&parent).unwrap();
        let directory = std::fs::File::open(&parent).unwrap();
        let short = format!("/proc/self/fd/{}/8080.sock", directory.as_raw_fd());
        let listener = UnixListener::bind(short).unwrap();
        let socket = parent.join("8080.sock");
        assert!(socket.as_os_str().len() > 108);
        assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());
        let cwd = std::env::current_dir().unwrap();
        let task = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(&[1]).unwrap();
        });
        ingress_connect(&socket).unwrap();
        task.join().unwrap();
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }
}

#[cfg(unix)]
pub fn namespace_ingress(args: &[String]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    ensure!(
        !args.is_empty() && args.len() <= 16,
        "invalid ingress endpoints"
    );
    let mut tasks = Vec::new();
    for value in args {
        let port: u16 = value.parse()?;
        ensure!(port > 0, "invalid ingress port");
        let socket = format!("{INGRESS_ROOT}/{port}.sock");
        let listener = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        tasks.push(std::thread::spawn(move || {
            let mut children: Vec<std::thread::JoinHandle<()>> = Vec::new();
            for incoming in listener.incoming() {
                children.retain(|t| !t.is_finished());
                let Ok(mut unix) = incoming else {
                    break;
                };
                if children.len() >= 16 {
                    continue;
                }
                let Ok(tcp) = std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
                    std::time::Duration::from_millis(100),
                ) else {
                    continue;
                };
                if unix.write_all(&[1]).is_err() {
                    continue;
                }
                children.push(std::thread::spawn(move || {
                    let _ = relay(unix, tcp);
                }));
            }
        }));
    }
    println!("ready");
    for task in tasks {
        let _ = task.join();
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn namespace_ingress(_: &[String]) -> Result<()> {
    anyhow::bail!("scoped ingress requires Unix")
}

pub fn start_namespace_ingress(shim: &str, args: &[String]) -> Result<std::process::Child> {
    use std::io::BufRead;
    let ports: Vec<_> = args
        .windows(2)
        .filter(|v| v[0] == "--ingress-port")
        .map(|v| &v[1])
        .collect();
    let mut child = std::process::Command::new(shim)
        .args(["sandbox-exec", "--ingress-relay"])
        .args(ports)
        .env_clear()
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("ingress readiness missing")?;
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
            anyhow::bail!("namespace ingress failed readiness")
        }
    }
}

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

pub fn start_namespace_relay(shim: &str) -> Result<std::process::Child> {
    use std::io::BufRead;
    let mut child = std::process::Command::new(shim)
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
