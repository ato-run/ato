mod fail_closed_support;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use fail_closed_support::prepare_fixture_workspace;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
fn chmod_executable(path: &std::path::Path) {
    let mut perms = fs::metadata(path)
        .expect("stat fixture entrypoint")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod fixture entrypoint");
}

#[cfg(unix)]
fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(unix)]
fn host_limited(stderr: &str) -> bool {
    stderr.contains("No compatible native sandbox backend is available")
        || stderr.contains("Sandbox unavailable")
        || stderr.contains("pfctl failed to load anchor")
        || stderr.contains("KeyError: 'CAPSULE_IPC_ECHO_SOCKET'")
}

#[cfg(unix)]
fn maybe_resolve_test_nacelle_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("NACELLE_PATH") {
        let nacelle = PathBuf::from(path);
        if nacelle.exists() {
            return Some(nacelle);
        }
    }

    let candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../nacelle/target/debug/nacelle");
    candidate.exists().then_some(candidate)
}

#[cfg(unix)]
fn spawn_unix_echo_server(
    socket_path: &std::path::Path,
) -> (mpsc::Receiver<String>, thread::JoinHandle<()>) {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).expect("create socket dir");
    }
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path).expect("bind unix echo server");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking unix echo server");

    let (tx, rx) = mpsc::channel();
    let path = socket_path.to_path_buf();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 256];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(err)
                                if err.kind() == std::io::ErrorKind::WouldBlock
                                    || err.kind() == std::io::ErrorKind::TimedOut =>
                            {
                                break;
                            }
                            Err(_) => return,
                        }
                    }
                    let message = String::from_utf8_lossy(&buf).to_string();
                    let _ = tx.send(message.clone());
                    let reply = format!("ACK:{}", message);
                    let _ = stream.write_all(reply.as_bytes());
                    let _ = stream.flush();
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        let _ = fs::remove_file(path);
    });

    (rx, handle)
}

#[cfg(unix)]
#[test]
fn ipc_socket_path_survives_native_sandbox_and_reaches_host_listener() {
    let (_workspace, fixture) = prepare_fixture_workspace("ipc-socket-passthrough");
    chmod_executable(&fixture.join("run.sh"));

    let socket_path = std::env::temp_dir().join("capsule-ipc").join("echo.sock");
    let (rx, handle) = spawn_unix_echo_server(&socket_path);
    let home = TempDir::new().expect("create temp HOME");
    let Some(nacelle) = maybe_resolve_test_nacelle_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires nacelle to be available for ipc_socket_e2e"
        );
        let _ = handle.join();
        return;
    };

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&fixture)
        .arg("run")
        .arg("--yes")
        .arg("--sandbox")
        .arg("--nacelle")
        .arg(&nacelle)
        .arg(".")
        .env("HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run ipc socket passthrough fixture");

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && host_limited(&stderr) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={stderr}"
        );
        let _ = handle.join();
        return;
    }

    assert!(output.status.success(), "stderr={stderr}");

    let received = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("host unix listener should receive sandbox payload");
    assert_eq!(received, "Hello from Sandbox");
    handle.join().expect("unix echo thread join");
}
