//! PR 7 binary smoke: drive the guest-agent binary over JSON-lines stdin/stdout — the
//! same framing the vsock transport uses. Proves the binary delivers to tmpfs, reports
//! bound-ready, scrubs on stop, and never echoes the secret.

// Linux-only runtime component — see the unix gate rationale in src/*.rs tests.
#![cfg(unix)]
use std::io::Write;
use std::process::{Command, Stdio};

use ato_ipc::binding_control::HostToAgent;
use ato_ipc::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

#[test]
fn agent_binary_delivers_to_tmpfs_reports_ready_and_scrubs_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("bindings");
    let secret = "PG-PASSWORD-XYZ";

    // Far-future expiry so the binding is active against the binary's real clock.
    let lease = BindingLease::issue(
        BindingLeaseId::new("l1"),
        BindingName::parse("db_url").unwrap(),
        SecretValue::new(secret),
        0,
        10_000_000_000_000,
    );
    let mut input = String::new();
    input.push_str(&serde_json::to_string(&HostToAgent::Deliver(lease.to_delivery())).unwrap());
    input.push('\n');
    input.push_str(&serde_json::to_string(&HostToAgent::QueryBoundReady).unwrap());
    input.push('\n');
    input.push_str(&serde_json::to_string(&HostToAgent::Stop).unwrap());
    input.push('\n');

    let mut child = Command::new(env!("CARGO_BIN_EXE_guest-agent"))
        .arg("db_url")
        .env("ATO_BINDINGS_ROOT", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("\"kind\":\"ack\""),
        "expected Ack: {stdout}"
    );
    assert!(
        stdout.contains("\"ready\":true"),
        "expected bound-ready: {stdout}"
    );
    assert!(
        !root.join("db_url").exists(),
        "stop must scrub tmpfs; stdout={stdout}"
    );
    assert!(
        !stdout.contains(secret),
        "agent stdout leaked the secret: {stdout}"
    );
}

/// Exercise owner EOF through the actual Agent binary, with no Stop RPC.
#[cfg(target_os = "linux")]
#[test]
fn private_terminal_owner_eof_closes_connection_socket_and_workload() {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.join("term.sock");
    let config = root.join("supervisor.json");
    std::fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "stdio_mode":"pty", "cwd":root,
        "cmd":["/bin/sh","-c","echo $$ > child.pid; while IFS= read -r line; do printf '%s\\n' \"$line\"; done"]
    })).unwrap()).unwrap();
    struct AgentOwner {
        child: std::process::Child,
        pid_file: std::path::PathBuf,
    }
    impl Drop for AgentOwner {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Ok(pid) = std::fs::read_to_string(&self.pid_file)
                .unwrap_or_default()
                .trim()
                .parse::<i32>()
                && pid > 1
            {
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
    }
    let mut agent = AgentOwner {
        child: Command::new(env!("CARGO_BIN_EXE_guest-agent"))
            .env("ATO_GUEST_AGENT_MODE", "stdio")
            .env("ATO_SUPERVISOR_CONFIG", &config)
            .env("ATO_BINDINGS_ROOT", root.join("bindings"))
            .env("ATO_GUEST_AGENT_TERMINAL_UNIX_SOCKET", &socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        pid_file: root.join("child.pid"),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(socket.exists() && agent.pid_file.exists()) {
        assert!(Instant::now() < deadline, "private Terminal did not start");
        assert!(
            agent.child.try_wait().unwrap().is_none(),
            "Agent exited before ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(&agent.pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let mut connection = UnixStream::connect(&socket).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    connection.write_all(b"CONNECT 1026\n").unwrap();
    let mut response = [0; 8];
    connection.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"OK 1026\n");
    drop(agent.child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(status) = agent.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "owner EOF did not stop Agent");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!socket.exists());
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned workload survived EOF"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    // Prevent cleanup from signalling a PID after successful reap/verification.
    std::fs::remove_file(&agent.pid_file).unwrap();
}
