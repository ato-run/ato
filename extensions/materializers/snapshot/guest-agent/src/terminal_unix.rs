//! Private non-VM transport for the SAME guest terminal broker and frame protocol.
//! The Runner owns the sandbox, directory and lifetime; no listener is public.

use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::terminal::TerminalBrokerState;
use crate::terminal_vsock::{DEFAULT_TERMINAL_VSOCK_PORT, serve_terminal_stream};

pub struct TerminalUnixServer {
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<JoinHandle<()>>,
    socket: Option<OwnedSocket>,
}

struct OwnedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
impl TerminalUnixServer {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(active) = self.active.lock()
            && let Some(stream) = active.as_ref()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.socket.take();
    }
}
impl Drop for TerminalUnixServer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn start_terminal_unix(
    path: &Path,
    state: Arc<TerminalBrokerState>,
) -> io::Result<TerminalUnixServer> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("terminal socket parent is missing"))?;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            "terminal socket path must be absolute and normalized",
        ));
    }
    let directory = std::fs::symlink_metadata(parent)?;
    if !directory.is_dir()
        || directory.file_type().is_symlink()
        || directory.uid() != unsafe { libc::geteuid() }
        || directory.permissions().mode() & 0o077 != 0
        || std::fs::canonicalize(parent)? != parent
    {
        return Err(invalid(
            "terminal socket requires a private, owned canonical directory",
        ));
    }
    // Never unlink a prior socket/file: its owner must settle it first.
    let listener = UnixListener::bind(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    let socket = OwnedSocket {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let active: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let stopped = Arc::clone(&stop);
    let attached = Arc::clone(&active);
    let thread = std::thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(_) => break,
            };
            let Ok(copy) = stream.try_clone() else {
                continue;
            };
            let Ok(mut slot) = attached.lock() else {
                break;
            };
            *slot = Some(copy);
            drop(slot);
            if stopped.load(Ordering::Acquire) {
                break;
            }
            let connection = (|| -> io::Result<()> {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let expected = format!("CONNECT {DEFAULT_TERMINAL_VSOCK_PORT}\n");
                let mut actual = vec![0; expected.len()];
                stream.read_exact(&mut actual)?;
                if actual != expected.as_bytes() {
                    return Err(invalid("invalid terminal connection preface"));
                }
                stream.write_all(format!("OK {DEFAULT_TERMINAL_VSOCK_PORT}\n").as_bytes())?;
                stream.set_read_timeout(None)?;
                // File is only a transport handle. All PTY operations remain
                // in terminal_vsock's existing bounded protocol implementation.
                let file = File::from(std::os::fd::OwnedFd::from(stream));
                serve_terminal_stream(file, Arc::clone(&state))
            })();
            if let Ok(mut slot) = attached.lock() {
                slot.take();
            }
            // Disconnect/invalid frames are session-local, not process-fatal.
            let _ = connection;
        }
    });
    Ok(TerminalUnixServer {
        stop,
        active,
        thread: Some(thread),
        socket: Some(socket),
    })
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::{SpawnPlan, Workload};
    use crate::terminal::PtyChildWorkload;
    use ato_ipc::terminal_surface::{TerminalClientControl, TerminalServerControl};
    use std::collections::BTreeMap;

    fn connect(path: &Path) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"CONNECT 1026\n").unwrap();
        let mut ready = [0; 8];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"OK 1026\n");
        stream
    }
    fn frame(stream: &mut UnixStream) -> (u8, Vec<u8>) {
        let mut header = [0; 5];
        stream.read_exact(&mut header).unwrap();
        let size = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        assert!(size <= 65536);
        let mut payload = vec![0; size];
        stream.read_exact(&mut payload).unwrap();
        (header[0], payload)
    }
    fn send(stream: &mut UnixStream, kind: u8, payload: &[u8]) {
        stream.write_all(&[kind]).unwrap();
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(payload).unwrap();
    }
    fn read_output(stream: &mut UnixStream, expected: &str) {
        let mut output = Vec::new();
        for _ in 0..64 {
            let (kind, data) = frame(stream);
            if kind == 2 {
                output.extend(data);
            }
            if String::from_utf8_lossy(&output).contains(expected) {
                return;
            }
        }
        panic!("expected PTY output was absent");
    }

    #[test]
    fn refuses_public_or_symlinked_directory_and_existing_path() {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        let state = Arc::new(TerminalBrokerState::default());
        assert!(start_terminal_unix(&dir.join("terminal.sock"), Arc::clone(&state)).is_err());
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("alias")).unwrap();
        assert!(start_terminal_unix(&dir.join("alias/terminal.sock"), Arc::clone(&state)).is_err());
        let socket = dir.join("terminal.sock");
        std::fs::write(&socket, "owned by another attempt").unwrap();
        assert!(start_terminal_unix(&socket, state).is_err());
        assert_eq!(
            std::fs::read_to_string(socket).unwrap(),
            "owned by another attempt"
        );
    }

    #[test]
    fn reuses_real_pty_input_resize_reconnect_and_owned_teardown() {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("terminal.sock");
        let state = Arc::new(TerminalBrokerState::default());
        let mut workload = PtyChildWorkload::new(Arc::clone(&state));
        workload
            .start(&SpawnPlan {
                cmd: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"printf 'initial-prompt\n'; while IFS= read -r line; do printf 'got:%s\n' "$line"; stty size; done"#
                        .into(),
                ],
                cwd: "/".into(),
                base_env: BTreeMap::new(),
                secret_env: vec![],
                stdout_log: "/dev/null".into(),
                stderr_log: "/dev/null".into(),
                rootfs: None,
            })
            .unwrap();
        let mut server = start_terminal_unix(&path, Arc::clone(&state)).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // The gateway's first readiness probe must not steal the initial screen.
        let mut probe = connect(&path);
        assert_eq!(frame(&mut probe).0, 3);
        drop(probe);
        let mut stream = connect(&path);
        let (kind, ready) = frame(&mut stream);
        assert_eq!(kind, 3);
        assert_eq!(
            serde_json::from_slice::<TerminalServerControl>(&ready).unwrap(),
            TerminalServerControl::Ready { cols: 80, rows: 24 }
        );
        send(
            &mut stream,
            3,
            &serde_json::to_vec(&TerminalClientControl::Resize { cols: 80, rows: 24 }).unwrap(),
        );
        read_output(&mut stream, "initial-prompt");
        send(&mut stream, 1, b"hello\n");
        read_output(&mut stream, "got:hello");
        send(
            &mut stream,
            3,
            &serde_json::to_vec(&TerminalClientControl::Resize {
                cols: 120,
                rows: 40,
            })
            .unwrap(),
        );
        send(&mut stream, 1, b"again\n");
        read_output(&mut stream, "40 120");
        drop(stream);
        let mut reconnected = connect(&path);
        let (_, ready) = frame(&mut reconnected);
        assert_eq!(
            serde_json::from_slice::<TerminalServerControl>(&ready).unwrap(),
            TerminalServerControl::Ready {
                cols: 120,
                rows: 40
            }
        );
        server.stop();
        assert!(!path.exists());
        let mut byte = [0];
        assert!(matches!(reconnected.read(&mut byte), Ok(0) | Err(_)));
        assert!(state.is_running());
        drop(workload);
        assert!(!state.is_running());
    }

    #[test]
    fn stop_does_not_unlink_a_replacement_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("terminal.sock");
        let mut server =
            start_terminal_unix(&path, Arc::new(TerminalBrokerState::default())).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        server.stop();
        drop(server);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "replacement");
    }
}
