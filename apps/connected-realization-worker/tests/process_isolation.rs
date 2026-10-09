//! Dedicated-host security acceptance of the actual bwrap + Runner shim.
//! Only synthetic canaries are used; no real credentials or metadata service.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ato_connected_realization_worker::runtime_launch::resolved::{
    ResolvedRuntimeLaunchContext, ResolvedStateAttachment,
};
use ato_connected_realization_worker::runtime_launch::sandbox::sandboxed_command;
use ato_ipc::runtime_launch::StateAccessV1;
use serde_json::Value;

fn dedicated_host() {
    assert_eq!(
        std::env::var("ATO_RUNNER_ISOLATION_TEST_HOST").as_deref(),
        Ok("1"),
        "run only on a dedicated test host with synthetic canaries"
    );
    assert_eq!(
        std::env::var("ATO_TEST_PARENT_CREDENTIAL").as_deref(),
        Ok("synthetic-parent-credential-canary"),
        "seed the parent environment to make credential exclusion a positive test"
    );
}

fn probe(share_network: bool) -> Value {
    dedicated_host();
    let root = tempfile::tempdir_in(".tmp").expect("test root under .tmp");
    let workspace = root.path().join("workspace");
    let state = root.path().join("state-a");
    let other = root.path().join("state-b");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let credential = root.path().join("runner-token");
    std::fs::write(&credential, "synthetic-runner-credential-canary").unwrap();
    let other_file = other.join("private.txt");
    std::fs::write(&other_file, "synthetic-other-tenant-canary").unwrap();
    let host_credential = credential.canonicalize().unwrap();
    let host_other = other_file.canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopping) = mpsc::channel();
    let service = thread::spawn(move || {
        while stopping.try_recv().is_err() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    let body = "synthetic-management-canary";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopping.recv_timeout(Duration::from_millis(10)).is_ok() {
                        break;
                    }
                }
                Err(error) => panic!("canary listener: {error}"),
            }
        }
    });
    let python = r#"
import json, os, socket, urllib.request
def readable(path):
    try:
        with open(path, 'rb') as f: f.read(256)
        return True
    except OSError: return False
def writable(path):
    try:
        with open(path, 'w') as f: f.write('synthetic-own-state')
        return True
    except OSError: return False
def reachable():
    try:
        with urllib.request.urlopen('http://127.0.0.1:' + os.environ['CANARY_PORT'], timeout=1) as r:
            return b'synthetic-management-canary' in r.read(256)
    except (OSError, ValueError): return False
print(json.dumps({
    'host_credential': readable(os.environ['HOST_CREDENTIAL']),
    'other_state': readable(os.environ['HOST_OTHER']),
    'proc_host_root': readable('/proc/1/root' + os.environ['HOST_CREDENTIAL']),
    'ambient_credential': 'ATO_TEST_PARENT_CREDENTIAL' in os.environ,
    'docker_socket': os.path.exists('/var/run/docker.sock'),
    'workspace_write': writable('/app/injected.txt'),
    'own_state_write': writable('/data/own.txt'),
    'host_management_reachable': reachable(),
}))
"#;
    std::fs::write(workspace.join("probe.py"), python).unwrap();
    let context = ResolvedRuntimeLaunchContext::new(
        workspace.canonicalize().unwrap(),
        "",
        BTreeMap::from([
            ("CANARY_PORT".into(), port.to_string()),
            (
                "HOST_CREDENTIAL".into(),
                host_credential.display().to_string(),
            ),
            ("HOST_OTHER".into(), host_other.display().to_string()),
        ]),
        vec![],
        vec![ResolvedStateAttachment::new(
            "app_data",
            None,
            state.canonicalize().unwrap(),
            "/data",
            StateAccessV1::ReadWrite,
        )],
        vec![],
    )
    .unwrap();
    let policy_path = root.path().join("policy.json");
    let sandbox = sandboxed_command(
        &context,
        &["/usr/bin/python3".into(), "/app/probe.py".into()],
        std::path::Path::new(env!("CARGO_BIN_EXE_ato-connected-realization-worker")),
        &policy_path,
        share_network,
    )
    .unwrap();
    std::fs::write(&policy_path, serde_json::to_vec(&sandbox.policy).unwrap()).unwrap();
    let output = Command::new(&sandbox.argv[0])
        .args(&sandbox.argv[1..])
        .env_clear()
        .envs(context.environment_for_spawn())
        .output();
    let _ = stop.send(());
    service.join().unwrap();
    let output = output.expect("bwrap spawn");
    assert!(
        output.status.success(),
        "sandbox did not run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed: Value = serde_json::from_slice(&output.stdout).expect("probe JSON");
    eprintln!("share_network={share_network}: {observed}");
    for key in [
        "host_credential",
        "other_state",
        "proc_host_root",
        "ambient_credential",
        "docker_socket",
        "workspace_write",
    ] {
        assert_eq!(observed[key], false, "{key}: {observed}");
    }
    assert_eq!(
        observed["own_state_write"], true,
        "own state positive control: {observed}"
    );
    assert_eq!(
        std::fs::read_to_string(&credential).unwrap(),
        "synthetic-runner-credential-canary"
    );
    assert_eq!(
        std::fs::read_to_string(&other_file).unwrap(),
        "synthetic-other-tenant-canary"
    );
    observed
}

#[test]
#[ignore = "dedicated Linux isolation test host required"]
fn isolated_network_cannot_reach_host_management_canary() {
    std::fs::create_dir_all(".tmp").unwrap();
    let observed = probe(false);
    assert_eq!(observed["host_management_reachable"], false);
}

#[test]
#[ignore = "dedicated Linux isolation test host required"]
fn direct_process_host_network_requires_host_boundary_classification() {
    std::fs::create_dir_all(".tmp").unwrap();
    // Positive control: this is the mode launch_process currently uses.
    // It MUST NOT be advertised as suitable for sharing between tenants.
    let observed = probe(true);
    assert_eq!(observed["host_management_reachable"], true);
}
