//! PR 7 binary smoke: drive the guest-agent binary over JSON-lines stdin/stdout — the
//! same framing the vsock transport uses. Proves the binary delivers to tmpfs, reports
//! bound-ready, scrubs on stop, and never echoes the secret.

use std::io::Write;
use std::process::{Command, Stdio};

use protocol::binding_control::HostToAgent;
use protocol::binding_lease::{BindingLease, BindingLeaseId, BindingName, SecretValue};

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
