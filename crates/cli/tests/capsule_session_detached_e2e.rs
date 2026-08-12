//! Detached Capsule Session Supervisor + PTY acceptance.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use capsule_session_runtime::{SessionWal, WalEntry};

fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create hermetic test scratch root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create hermetic test directory")
}

fn ato(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato"));
    command.env("ATO_HOME", home.join("ato-home"));
    command.env("HOME", home.join("user-home"));
    command
}

fn make_bundle(root: &Path) -> PathBuf {
    let workspace = root.join("source");
    fs::create_dir_all(workspace.join("subdir")).expect("create source workspace");
    fs::write(workspace.join("subdir/fixture.txt"), b"fixture\n").expect("write fixture");
    let bundle = root.join("terminal.capsule");
    let output = ato(root)
        .args(["internal", "capsule-protocol", "capture", "--workspace"])
        .arg(&workspace)
        .arg("--output")
        .arg(&bundle)
        .args(["--", "/bin/sh", "-c", "printf seed-output"])
        .output()
        .expect("capture bundle");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    bundle
}

fn start_session(root: &Path, bundle: &Path, restore_name: &str) -> String {
    let output = ato(root)
        .args(["internal", "capsule-session", "start"])
        .arg(bundle)
        .arg("--into")
        .arg(root.join(restore_name))
        .arg("--no-attach")
        .output()
        .expect("start detached Session");
    assert!(
        output.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("SessionId UTF-8")
        .trim()
        .to_owned()
}

fn attach_with_input(root: &Path, session_id: &str, input: &[u8]) -> std::process::Output {
    let mut child = ato(root)
        .args(["internal", "capsule-session", "attach", session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn attach client");
    child
        .stdin
        .as_mut()
        .expect("attach stdin")
        .write_all(input)
        .expect("write attach input");
    thread::sleep(Duration::from_millis(700));
    child
        .stdin
        .as_mut()
        .expect("attach stdin")
        .write_all(&[0x1c, 0x04])
        .expect("send detach escape");
    drop(child.stdin.take());
    child.wait_with_output().expect("wait attach client")
}

fn status(root: &Path, session_id: &str) -> serde_json::Value {
    let output = ato(root)
        .args(["internal", "capsule-session", "status", session_id])
        .output()
        .expect("query Session status");
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

fn kill_session(root: &Path, session_id: &str) {
    let output = ato(root)
        .args(["internal", "capsule-session", "kill", session_id])
        .output()
        .expect("kill Session");
    assert!(
        output.status.success(),
        "kill failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn session_directory(root: &Path, session_id: &str) -> PathBuf {
    root.join("ato-home/capsule-protocol-sessions")
        .join(session_id)
}

#[test]
fn detached_session_survives_cli_and_preserves_one_shell_across_reattach() {
    let root = scratch_dir("detached-capsule-session-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "restored");

    let initial = status(root.path(), &session_id);
    assert_eq!(initial["lifecycle"], "running");
    assert!(initial["pid"].as_u64().is_some_and(|pid| pid > 1));

    let first = attach_with_input(
        root.path(),
        &session_id,
        b"export ATO_TEST_SESSION_VALUE=42\ncd subdir\n",
    );
    assert!(first.status.success());
    let second = attach_with_input(
        root.path(),
        &session_id,
        b"echo VALUE=$ATO_TEST_SESSION_VALUE\npwd\n",
    );
    assert!(second.status.success());
    let continued = String::from_utf8_lossy(&second.stdout);
    assert!(continued.contains("VALUE=42"), "{continued}");
    assert!(continued.contains("restored/subdir"), "{continued}");

    // Detach immediately after starting output. With no clients, the
    // Supervisor must keep draining and durably journaling the PTY.
    let detached = attach_with_input(
        root.path(),
        &session_id,
        b"yes x | head -c 200000; printf '\\nDRAINED\\n'\n",
    );
    assert!(detached.status.success());
    thread::sleep(Duration::from_secs(1));
    let after_drain = attach_with_input(root.path(), &session_id, b"echo AFTER-DRAIN\n");
    assert!(
        String::from_utf8_lossy(&after_drain.stdout).contains("AFTER-DRAIN"),
        "shell stopped after detached output: {}",
        String::from_utf8_lossy(&after_drain.stdout)
    );

    let observer_a_path = root.path().join("observer-a.out");
    let observer_b_path = root.path().join("observer-b.out");
    let mut observer_a = ato(root.path())
        .args([
            "internal",
            "capsule-session",
            "attach",
            &session_id,
            "--observe",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            fs::File::create(&observer_a_path).expect("observer A output"),
        ))
        .spawn()
        .expect("spawn observer A");
    let mut observer_b = ato(root.path())
        .args([
            "internal",
            "capsule-session",
            "attach",
            &session_id,
            "--observe",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            fs::File::create(&observer_b_path).expect("observer B output"),
        ))
        .spawn()
        .expect("spawn observer B");
    thread::sleep(Duration::from_millis(300));
    let observed = attach_with_input(root.path(), &session_id, b"echo MULTI-OBSERVER\n");
    assert!(observed.status.success());
    thread::sleep(Duration::from_millis(300));
    let _ = observer_a.kill();
    let _ = observer_b.kill();
    let _ = observer_a.wait();
    let _ = observer_b.wait();
    for path in [&observer_a_path, &observer_b_path] {
        assert!(
            fs::read_to_string(path)
                .expect("observer output UTF-8")
                .contains("MULTI-OBSERVER"),
            "observer did not receive PTY output: {}",
            path.display()
        );
    }

    // Hold one writer connection open and verify a second writer fails closed.
    let mut writer = ato(root.path())
        .args(["internal", "capsule-session", "attach", &session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn writer");
    thread::sleep(Duration::from_millis(300));
    let rejected = ato(root.path())
        .args(["internal", "capsule-session", "attach", &session_id])
        .stdin(Stdio::null())
        .output()
        .expect("attempt second writer");
    assert!(!rejected.status.success(), "second writer must be rejected");
    writer
        .stdin
        .as_mut()
        .expect("writer stdin")
        .write_all(&[0x1c, 0x04])
        .expect("detach writer");
    drop(writer.stdin.take());
    assert!(writer.wait().expect("wait writer").success());

    // A wrong token is rejected without mutating logical Session identity.
    let directory = session_directory(root.path(), &session_id);
    let token_path = directory.join("control/token");
    let token = fs::read(&token_path).expect("read token");
    fs::write(&token_path, b"wrong-token").expect("replace token");
    let unauthorized = ato(root.path())
        .args(["internal", "capsule-session", "status", &session_id])
        .output()
        .expect("wrong-token status");
    assert!(!unauthorized.status.success());
    fs::write(&token_path, token).expect("restore token");
    assert_eq!(status(root.path(), &session_id)["lifecycle"], "running");

    let session_path = directory.join("session.json");
    let original_session = fs::read(&session_path).expect("read Session identity");
    let mut stale: serde_json::Value =
        serde_json::from_slice(&original_session).expect("Session identity JSON");
    stale["supervisor"]["generation"] = serde_json::json!(2);
    fs::write(
        &session_path,
        serde_json::to_vec_pretty(&stale).expect("stale identity JSON"),
    )
    .expect("write stale identity");
    let stale_generation = ato(root.path())
        .args(["internal", "capsule-session", "status", &session_id])
        .output()
        .expect("stale-generation status");
    assert!(!stale_generation.status.success());
    fs::write(&session_path, &original_session).expect("restore Session identity");
    assert_eq!(status(root.path(), &session_id)["lifecycle"], "running");

    let mut stale_nonce: serde_json::Value =
        serde_json::from_slice(&original_session).expect("Session identity JSON");
    stale_nonce["supervisor"]["incarnation_nonce"] = serde_json::json!("stale-incarnation");
    fs::write(
        &session_path,
        serde_json::to_vec_pretty(&stale_nonce).expect("stale nonce JSON"),
    )
    .expect("write stale nonce");
    let stale_incarnation = ato(root.path())
        .args(["internal", "capsule-session", "status", &session_id])
        .output()
        .expect("stale-incarnation status");
    assert!(!stale_incarnation.status.success());
    fs::write(&session_path, &original_session).expect("restore Session identity");

    kill_session(root.path(), &session_id);
    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.join("session.json")).expect("read stopped Session"),
    )
    .expect("stored Session JSON");
    assert_eq!(stored["lifecycle"], "stopped");

    let recovered = SessionWal::open(directory.join("journal/wal-000001"))
        .expect("open WAL")
        .recover()
        .expect("recover WAL");
    let kinds: Vec<_> = recovered
        .entries
        .iter()
        .filter_map(|entry| match entry {
            WalEntry::RecordCandidate { record, .. } => Some(record.kind.as_str()),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&"stdin"));
    assert!(kinds.contains(&"output"));
    assert!(kinds.contains(&"resize"));
    assert!(kinds.contains(&"exit"));
    assert!(
        kinds
            .iter()
            .all(|kind| matches!(*kind, "stdin" | "output" | "resize" | "exit")),
        "Control Plane event leaked into WAL: {kinds:?}"
    );
}

#[test]
fn supervisor_sigkill_revokes_workload_lease() {
    let root = scratch_dir("capsule-session-containment-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "restored");
    let supervisor_pid = status(root.path(), &session_id)["pid"]
        .as_u64()
        .expect("Supervisor pid") as u32;
    let shell_pid = wait_for_shell_child(supervisor_pid);

    unsafe { libc::kill(supervisor_pid as libc::pid_t, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_alive(shell_pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(shell_pid),
        "workload {shell_pid} survived Supervisor lease loss"
    );
}

fn wait_for_shell_child(supervisor_pid: u32) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = Command::new("pgrep")
            .args(["-P", &supervisor_pid.to_string()])
            .output()
            .expect("pgrep children");
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Ok(pid) = line.trim().parse::<u32>() else {
                continue;
            };
            let command = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "command="])
                .output()
                .expect("inspect child");
            let command = String::from_utf8_lossy(&command.stdout);
            if command.contains("/bin/sh") && !command.contains("watchdog") {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "shell child did not appear");
        thread::sleep(Duration::from_millis(50));
    }
}

fn process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
