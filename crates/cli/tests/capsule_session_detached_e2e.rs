//! Detached Capsule Session Supervisor + PTY acceptance.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use capsule::protocol_bundle::{PortableCapsule, capture_local_workspace_checkpoint};
use capsule_session_runtime::{SessionWal, WalEntry, WalPayload};

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

fn start_session_with_discovery_failure(root: &Path, bundle: &Path, restore_name: &str) -> String {
    let output = ato(root)
        .env("ATO_TEST_FAIL_PROCESS_DISCOVERY", "1")
        .args(["internal", "capsule-session", "start"])
        .arg(bundle)
        .arg("--into")
        .arg(root.join(restore_name))
        .arg("--no-attach")
        .output()
        .expect("start detached Session");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
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

fn branch_session(root: &Path, source_id: &str, restore_name: &str) -> String {
    let output = ato(root)
        .args(["internal", "capsule-session", "branch", source_id, "--into"])
        .arg(root.join(restore_name))
        .arg("--no-attach")
        .output()
        .expect("branch Session");
    assert!(
        output.status.success(),
        "branch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("child SessionId UTF-8")
        .trim()
        .to_owned()
}

fn suspend_session(root: &Path, session_id: &str) {
    let output = ato(root)
        .args(["internal", "capsule-session", "suspend", session_id])
        .output()
        .expect("suspend Session");
    assert!(
        output.status.success(),
        "suspend failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn resume_session(root: &Path, session_id: &str) -> std::process::Output {
    ato(root)
        .args(["internal", "capsule-session", "resume", session_id])
        .output()
        .expect("resume Session")
}

fn resume_session_with_failure(root: &Path, session_id: &str) -> std::process::Output {
    ato(root)
        .env("ATO_TEST_FAIL_RESUME_STARTUP", "1")
        .args(["internal", "capsule-session", "resume", session_id])
        .output()
        .expect("resume Session with injected failure")
}

fn stored_session(root: &Path, session_id: &str) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(session_directory(root, session_id).join("session.json"))
            .expect("read stored Session"),
    )
    .expect("stored Session JSON")
}

fn wal_record_kinds(root: &Path, session_id: &str) -> Vec<String> {
    let wal = SessionWal::open(session_directory(root, session_id).join("journal/wal-000001"))
        .expect("open Session WAL");
    wal.recover()
        .expect("recover Session WAL")
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            WalEntry::RecordCandidate { record, .. } => Some(record.kind),
            _ => None,
        })
        .collect()
}

fn wal_inline_payload(root: &Path, session_id: &str) -> Vec<u8> {
    let wal = SessionWal::open(session_directory(root, session_id).join("journal/wal-000001"))
        .expect("open Session WAL");
    wal.recover()
        .expect("recover Session WAL")
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            WalEntry::RecordCandidate {
                record:
                    capsule_session_runtime::WalRecord {
                        payload: WalPayload::Inline(bytes),
                        ..
                    },
                ..
            } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect()
}

fn send_control_frame(stream: &mut UnixStream, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).expect("encode control frame");
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .expect("write control length");
    stream.write_all(&body).expect("write control body");
}

fn read_control_frame(stream: &mut UnixStream) -> serde_json::Value {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).expect("read control length");
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut body).expect("read control body");
    serde_json::from_slice(&body).expect("decode control frame")
}

fn journal_resize(root: &Path, session_id: &str, rows: u16, cols: u16) {
    let directory = session_directory(root, session_id);
    let stored = stored_session(root, session_id);
    let socket = PathBuf::from(
        String::from_utf8(fs::read(directory.join("control/socket-address")).unwrap()).unwrap(),
    );
    let token = fs::read(directory.join("control/token")).unwrap();
    let auth = serde_json::json!({
        "session_id": session_id,
        "generation": stored["supervisor"]["generation"],
        "incarnation_nonce": stored["supervisor"]["incarnation_nonce"],
        "supervisor_pid": stored["supervisor"]["pid"],
        "process_start_identity": stored["supervisor"]["process_start_identity"],
        "token": token,
    });
    let mut stream = UnixStream::connect(socket).expect("connect raw control client");
    send_control_frame(
        &mut stream,
        &serde_json::json!({"auth": auth, "action": {"method": "attach", "params": {"observe": false}}}),
    );
    assert_eq!(read_control_frame(&mut stream)["event"], "attached");
    send_control_frame(
        &mut stream,
        &serde_json::json!({"auth": auth, "action": {"method": "resize", "params": {"rows": rows, "cols": cols}}}),
    );
    send_control_frame(
        &mut stream,
        &serde_json::json!({"auth": auth, "action": {"method": "detach"}}),
    );
    thread::sleep(Duration::from_millis(200));
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
    fs::remove_file(&bundle).expect("remove caller-owned input bundle");
    let seed = session_directory(root.path(), &session_id).join("seed/source.capsule.local");
    assert!(
        seed.is_file(),
        "Session must own an immutable recovery seed"
    );

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

    journal_resize(root.path(), &session_id, 30, 100);
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
    assert!(
        !kinds.contains(&"exit"),
        "Control Plane kill must not synthesize a PTY exit record"
    );
    assert!(
        kinds
            .iter()
            .all(|kind| matches!(*kind, "stdin" | "output" | "resize" | "exit")),
        "Control Plane event leaked into WAL: {kinds:?}"
    );
}

#[test]
fn branch_replays_to_a_verified_independent_workspace_and_shell() {
    let root = scratch_dir("capsule-session-branch-");
    let bundle = make_bundle(root.path());
    let source_id = start_session(root.path(), &bundle, "source-restored");
    let source_setup = attach_with_input(
        root.path(),
        &source_id,
        b"export VALUE=42\ncd subdir\nprintf source > branch-file.txt\n",
    );
    assert!(source_setup.status.success());
    journal_resize(root.path(), &source_id, 40, 132);

    let child_id = branch_session(root.path(), &source_id, "child-restored");
    assert!(
        stored_session(root.path(), &child_id)["historical_replay"].is_object(),
        "child must persist its verified historical replay range"
    );
    let source_state = capture_local_workspace_checkpoint(&root.path().join("source-restored"))
        .expect("hash source frontier")
        .0;
    let child_state = capture_local_workspace_checkpoint(&root.path().join("child-restored"))
        .expect("hash child replay result")
        .0;
    assert_eq!(
        source_state.state_ref, child_state.state_ref,
        "child replay State must equal the committed source checkpoint"
    );
    let child_observed = attach_with_input(
        root.path(),
        &child_id,
        b"echo VALUE=$VALUE\npwd\ncat branch-file.txt\nexport VALUE=99\nprintf child > branch-file.txt\nprintf CHILD_ONLY\n",
    );
    assert!(child_observed.status.success());
    let child_output = String::from_utf8_lossy(&child_observed.stdout);
    assert!(child_output.contains("VALUE=42"), "{child_output}");
    assert!(
        child_output.contains("child-restored/subdir"),
        "{child_output}"
    );
    assert!(child_output.contains("source"), "{child_output}");

    let source_observed = attach_with_input(
        root.path(),
        &source_id,
        b"echo VALUE=$VALUE\ncat branch-file.txt\nprintf SOURCE_ONLY\n",
    );
    let source_output = String::from_utf8_lossy(&source_observed.stdout);
    assert!(source_output.contains("VALUE=42"), "{source_output}");
    assert!(source_output.contains("source"), "{source_output}");
    assert_eq!(
        fs::read_to_string(root.path().join("child-restored/subdir/branch-file.txt")).unwrap(),
        "child"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("source-restored/subdir/branch-file.txt")).unwrap(),
        "source"
    );
    let source_wal = wal_inline_payload(root.path(), &source_id);
    let child_wal = wal_inline_payload(root.path(), &child_id);
    assert!(
        source_wal
            .windows(b"SOURCE_ONLY".len())
            .any(|w| w == b"SOURCE_ONLY")
    );
    assert!(
        !source_wal
            .windows(b"CHILD_ONLY".len())
            .any(|w| w == b"CHILD_ONLY")
    );
    assert!(
        child_wal
            .windows(b"CHILD_ONLY".len())
            .any(|w| w == b"CHILD_ONLY")
    );
    assert!(
        !child_wal
            .windows(b"SOURCE_ONLY".len())
            .any(|w| w == b"SOURCE_ONLY")
    );

    kill_session(root.path(), &child_id);
    kill_session(root.path(), &source_id);
}

#[test]
fn branch_rejects_workspace_mutation_missing_from_pty_history() {
    let root = scratch_dir("capsule-session-branch-diverged-");
    let bundle = make_bundle(root.path());
    let source_id = start_session(root.path(), &bundle, "source-diverged");
    fs::write(
        root.path().join("source-diverged/cat-in-the-room.txt"),
        b"not represented by boundary I/O",
    )
    .expect("mutate source outside PTY history");
    let child = ato(root.path())
        .args([
            "internal",
            "capsule-session",
            "branch",
            &source_id,
            "--into",
        ])
        .arg(root.path().join("rejected-child"))
        .arg("--no-attach")
        .output()
        .expect("attempt diverged branch");
    assert!(!child.status.success(), "diverged branch must fail closed");
    assert!(
        String::from_utf8_lossy(&child.stderr).contains("BranchDiverged"),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    kill_session(root.path(), &source_id);
}

#[test]
fn process_discovery_failure_does_not_commit_a_consistent_frontier() {
    let root = scratch_dir("capsule-session-discovery-failure-");
    let bundle = make_bundle(root.path());
    let session_id = start_session_with_discovery_failure(root.path(), &bundle, "discovery-fail");
    let before = stored_session(root.path(), &session_id)["latest_consistent_frontier"].clone();
    let branch = ato(root.path())
        .args([
            "internal",
            "capsule-session",
            "branch",
            &session_id,
            "--into",
        ])
        .arg(root.path().join("never-created"))
        .arg("--no-attach")
        .output()
        .expect("attempt branch with failed discovery");
    assert!(!branch.status.success());
    assert_eq!(
        stored_session(root.path(), &session_id)["latest_consistent_frontier"],
        before
    );
    kill_session(root.path(), &session_id);
}

#[test]
fn suspend_quiesces_fork_churn_and_leaves_no_descendant() {
    let root = scratch_dir("capsule-session-fork-churn-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "fork-churn");
    let started = attach_with_input(
        root.path(),
        &session_id,
        b"sh -c 'while :; do printf x >> churn.txt; sleep 0.01; done' & echo $! > worker.pid\n",
    );
    assert!(started.status.success());
    let worker_path = root.path().join("fork-churn/worker.pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker_path.is_file() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    let worker_pid: u32 = fs::read_to_string(worker_path)
        .expect("worker pid")
        .trim()
        .parse()
        .expect("numeric worker pid");
    suspend_session(root.path(), &session_id);
    let suspended = stored_session(root.path(), &session_id);
    assert_eq!(suspended["lifecycle"], "suspended");
    assert!(suspended["latest_consistent_frontier"].is_object());
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(worker_pid as libc::pid_t, 0) } == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_ne!(
        unsafe { libc::kill(worker_pid as libc::pid_t, 0) },
        0,
        "suspend left a churn descendant alive"
    );
}

#[test]
fn suspend_resume_rebases_at_filesystem_restart_without_synthetic_exit() {
    let root = scratch_dir("capsule-session-suspend-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "resumable");
    let setup = attach_with_input(
        root.path(),
        &session_id,
        b"export VALUE=42\nprintf durable > resume-file.txt\n",
    );
    assert!(setup.status.success());
    journal_resize(root.path(), &session_id, 40, 132);
    let before = stored_session(root.path(), &session_id);
    let old_token = fs::read(session_directory(root.path(), &session_id).join("control/token"))
        .expect("read initial token");
    let exits_before = wal_record_kinds(root.path(), &session_id)
        .into_iter()
        .filter(|kind| kind == "exit")
        .count();

    suspend_session(root.path(), &session_id);
    let suspended = stored_session(root.path(), &session_id);
    assert_eq!(suspended["lifecycle"], "suspended");
    assert!(suspended["active_checkpoint"].is_object());
    assert_eq!(
        wal_record_kinds(root.path(), &session_id)
            .into_iter()
            .filter(|kind| kind == "exit")
            .count(),
        exits_before,
        "Suspend is Control Plane and must not synthesize PTY exit"
    );

    let resumed = resume_session(root.path(), &session_id);
    assert!(
        resumed.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let after = stored_session(root.path(), &session_id);
    assert_eq!(after["lifecycle"], "running");
    assert_eq!(
        after["supervisor"]["generation"].as_u64(),
        before["supervisor"]["generation"]
            .as_u64()
            .map(|generation| generation + 1)
    );
    assert_eq!(after["base_computation"], before["base_computation"]);
    assert_eq!(after["base_computation"]["kind"], "native");
    assert_eq!(
        after["base_frontier"],
        suspended["active_checkpoint"]["captured_at"]["records_through"]
    );
    let token_path = session_directory(root.path(), &session_id).join("control/token");
    let current_token = fs::read(&token_path).expect("read resumed token");
    assert_ne!(
        current_token, old_token,
        "resume must rotate the control token"
    );
    fs::write(&token_path, &old_token).expect("present stale token");
    let stale = ato(root.path())
        .args(["internal", "capsule-session", "status", &session_id])
        .output()
        .expect("stale-token status");
    assert!(
        !stale.status.success(),
        "old token must not authorize new incarnation"
    );
    fs::write(&token_path, current_token).expect("restore current token");

    let observed = attach_with_input(
        root.path(),
        &session_id,
        b"echo VALUE=${VALUE-unset}\ncat resume-file.txt\nstty size\n",
    );
    let output = String::from_utf8_lossy(&observed.stdout);
    assert!(output.contains("VALUE=unset"), "{output}");
    assert!(output.contains("durable"), "{output}");
    assert!(output.contains("40 132"), "{output}");

    let child_id = branch_session(root.path(), &session_id, "resumed-child");
    let child_size = attach_with_input(root.path(), &child_id, b"stty size\n");
    assert!(
        String::from_utf8_lossy(&child_size.stdout).contains("40 132"),
        "{}",
        String::from_utf8_lossy(&child_size.stdout)
    );
    kill_session(root.path(), &child_id);
    kill_session(root.path(), &session_id);
}

#[test]
fn resume_fails_closed_when_suspended_workspace_drifted() {
    let root = scratch_dir("capsule-session-drift-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "drifted");
    suspend_session(root.path(), &session_id);
    fs::write(root.path().join("drifted/external.txt"), b"cat in the room")
        .expect("mutate suspended workspace");
    let resumed = resume_session(root.path(), &session_id);
    assert!(
        !resumed.status.success(),
        "workspace drift must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&resumed.stderr).contains("WorkspaceDrift"),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(
        stored_session(root.path(), &session_id)["lifecycle"],
        "suspended"
    );
}

#[test]
fn failed_resume_rolls_back_to_suspended_and_seed_rebase_prunes_objects() {
    let root = scratch_dir("capsule-session-resume-rollback-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "rollback");
    let changed = attach_with_input(root.path(), &session_id, b"printf one > state.txt\n");
    assert!(changed.status.success());
    suspend_session(root.path(), &session_id);

    let failed = resume_session_with_failure(root.path(), &session_id);
    assert!(!failed.status.success());
    assert_eq!(
        stored_session(root.path(), &session_id)["lifecycle"],
        "suspended"
    );
    let resumed = resume_session(root.path(), &session_id);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );

    suspend_session(root.path(), &session_id);
    let seed_path = session_directory(root.path(), &session_id).join("seed/source.capsule.local");
    let once = PortableCapsule::read(&seed_path).expect("read once-rebased seed");
    let once_size = fs::metadata(&seed_path).unwrap().len();
    assert_eq!(
        once.objects.len(),
        1,
        "unreachable seed objects must be pruned"
    );
    assert!(resume_session(root.path(), &session_id).status.success());
    suspend_session(root.path(), &session_id);
    let twice = PortableCapsule::read(&seed_path).expect("read twice-rebased seed");
    let twice_size = fs::metadata(&seed_path).unwrap().len();
    assert_eq!(twice.objects.len(), once.objects.len());
    assert!(
        twice_size <= once_size,
        "repeated rebase must not grow the seed"
    );
    assert!(resume_session(root.path(), &session_id).status.success());
    kill_session(root.path(), &session_id);
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
    let grandchild_file = root.path().join("restored/grandchild.pid");
    let started = attach_with_input(
        root.path(),
        &session_id,
        b"sleep 1000 & echo $! > grandchild.pid\n",
    );
    assert!(started.status.success());
    let grandchild_pid = wait_for_pid_file(&grandchild_file);

    unsafe { libc::kill(supervisor_pid as libc::pid_t, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while (process_alive(shell_pid) || process_alive(grandchild_pid)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(shell_pid),
        "workload {shell_pid} survived Supervisor lease loss"
    );
    assert!(
        !process_alive(grandchild_pid),
        "grandchild {grandchild_pid} survived Supervisor lease loss"
    );

    let listed = ato(root.path())
        .args(["internal", "capsule-session", "list"])
        .output()
        .expect("list orphaned Sessions");
    assert!(listed.status.success());
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.contains(&format!("{session_id}\torphaned\t{supervisor_pid}")),
        "dead Supervisor must not remain running in list output: {listed}"
    );
}

#[test]
fn public_stop_reconciles_orphaned_workspace_session_and_releases_alias() {
    let root = scratch_dir("capsule-session-public-orphan-");
    let bundle = make_bundle(root.path());
    let started = ato(root.path())
        .args(["decap", "start", "--detach"])
        .arg(&bundle)
        .output()
        .expect("start public workspace Session");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert_eq!(
        String::from_utf8(started.stdout).unwrap(),
        "Starting terminal...\nReady.\n"
    );

    let listed = ato(root.path())
        .args(["decap", "list", "--json"])
        .output()
        .expect("list public Session");
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
    let session_id = rows[0]["session_id"].as_str().unwrap();
    let directory = session_directory(root.path(), session_id);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("session.json")).unwrap()).unwrap();
    let supervisor_pid = stored["supervisor"]["pid"].as_u64().unwrap() as u32;
    let shell_pid = wait_for_shell_child(supervisor_pid);

    assert_eq!(
        unsafe { libc::kill(supervisor_pid as libc::pid_t, libc::SIGKILL) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while (!directory.join("control/containment-revoked").is_file() || process_alive(shell_pid))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(!process_alive(shell_pid));
    assert!(directory.join("control/containment-revoked").is_file());

    let orphaned = ato(root.path())
        .args(["decap", "list", "--json"])
        .output()
        .expect("list orphaned public Session");
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&orphaned.stdout).unwrap();
    assert_eq!(rows[0]["state"], "orphaned");

    let stopped = ato(root.path())
        .args(["decap", "stop", "terminal"])
        .output()
        .expect("reconcile public orphan");
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    wait_for_stored_lifecycle(&directory, "stopped");

    let reused = ato(root.path())
        .args(["decap", "start", "--detach"])
        .arg(&bundle)
        .output()
        .expect("reuse reconciled alias");
    assert!(reused.status.success());
    assert_eq!(
        String::from_utf8(reused.stdout).unwrap(),
        "Starting terminal...\nReady.\n"
    );
    assert!(
        ato(root.path())
            .args(["decap", "stop", "terminal"])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn natural_exit_is_committed_once_after_final_output_with_actual_status() {
    let root = scratch_dir("capsule-session-natural-exit-");
    let bundle = make_bundle(root.path());
    let session_id = start_session(root.path(), &bundle, "restored");
    let mut client = ato(root.path())
        .args(["internal", "capsule-session", "attach", &session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("attach for natural exit");
    client
        .stdin
        .as_mut()
        .expect("attach stdin")
        .write_all(b"printf FINAL-OUTPUT; exit 7\n")
        .expect("request natural shell exit");
    drop(client.stdin.take());
    let output = client.wait_with_output().expect("wait natural exit client");
    assert!(
        output.status.success(),
        "attach failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let directory = session_directory(root.path(), &session_id);
    wait_for_stored_lifecycle(&directory, "stopped");
    let recovered = SessionWal::open(directory.join("journal/wal-000001"))
        .expect("open natural-exit WAL")
        .recover()
        .expect("recover natural-exit WAL");
    let records: Vec<_> = recovered
        .entries
        .iter()
        .filter_map(|entry| match entry {
            WalEntry::RecordCandidate { record, .. } => Some(record),
            _ => None,
        })
        .collect();
    let exits: Vec<_> = records
        .iter()
        .filter(|record| record.kind.as_str() == "exit")
        .collect();
    assert_eq!(exits.len(), 1, "natural exit must be idempotent");
    assert_eq!(records.last().expect("last record").kind.as_str(), "exit");
    let exit_record: capsule_protocol::IoRecord = (*exits[0])
        .clone()
        .try_into()
        .expect("decode WAL exit record");
    let capsule_protocol::Payload::Inline(payload) = &exit_record.payload else {
        panic!("exit payload must be inline");
    };
    let exit: serde_json::Value = serde_json::from_slice(payload).expect("exit payload JSON");
    assert_eq!(exit["exit_code"], 7);
    assert_eq!(exit["reason"], "natural");
    assert_eq!(exit["signal"], serde_json::Value::Null);
    assert!(
        records
            .iter()
            .take(records.len() - 1)
            .any(|record| record.kind.as_str() == "output"),
        "final output must be durable before exit"
    );
}

fn wait_for_stored_lifecycle(directory: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stored: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.join("session.json")).expect("read stored Session"),
        )
        .expect("stored Session JSON");
        if stored["lifecycle"] == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Session did not reach {expected}: {stored}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(pid) = value.trim().parse()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "PID file did not appear");
        thread::sleep(Duration::from_millis(50));
    }
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
