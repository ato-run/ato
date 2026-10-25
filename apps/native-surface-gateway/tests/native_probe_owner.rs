#![cfg(unix)]

use std::{net::SocketAddr, path::PathBuf, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
    process::{Child, Command},
    time::timeout,
};

const BOUND: Duration = Duration::from_secs(2);
const ACTUAL_OUTPUT: &[u8] = b"START\r\nRESULT:42\r\nEND\r\n";

struct AbortFixture(tokio::task::JoinHandle<()>);

impl Drop for AbortFixture {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn private_root() -> tempfile::TempDir {
    // Keep socket names short on BSD and write only below this workspace.
    let cwd = std::env::current_dir().unwrap();
    let parent = cwd
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == ".tmp"))
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.join(".tmp"));
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("npo-")
        .tempdir_in(parent)
        .unwrap()
}

async fn unused_addrs() -> (SocketAddr, SocketAddr) {
    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let observation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    (
        gateway.local_addr().unwrap(),
        observation.local_addr().unwrap(),
    )
}

fn config(path: &std::path::Path, gateway: SocketAddr, observation: SocketAddr) -> Value {
    json!({
        "execution": {"run_id":"run_native", "lease_id":"lease_native", "runner_id":"runner_native"},
        "session_id":"session-native", "surface_id":"surface-native",
        "listen_addr":gateway, "connect_url":format!("http://{gateway}/"),
        "origin":"http://localhost:5173",
        "assertion_keys":{"ato.runner-surface@1":"0123456789abcdef0123456789abcdef"},
        "idle_timeout_secs":30, "readiness_timeout_secs":5,
        "target":{"kind":"terminal", "firecracker_vsock_uds":path},
        "application_probe":{
            "listen_addr":observation,
            "adapter":{
                "surface":{"kind":"terminal", "port":"terminal", "cols":100, "rows":30},
                "probe_port":"native-results",
                "operations":[{
                    "kind":"terminal_exchange", "path":"/result", "input":b"compute\n",
                    "output_delimiter":b"END\r\n", "max_output_bytes":128, "timeout_ms":30000
                }]
            }
        }
    })
}

async fn start_sidecar(config: &Value) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ato-native-surface-gateway"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut bytes = serde_json::to_vec(config).unwrap();
    bytes.push(b'\n');
    let input = child.stdin.as_mut().unwrap();
    input.write_all(&bytes).await.unwrap();
    input.flush().await.unwrap();
    child
}

async fn frame(stream: &mut UnixStream) -> (u8, Vec<u8>) {
    let kind = stream.read_u8().await.unwrap();
    let length = stream.read_u32().await.unwrap() as usize;
    assert!(length <= 65_536);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await.unwrap();
    (kind, payload)
}

async fn send_frame(stream: &mut UnixStream, kind: u8, payload: &[u8]) {
    stream.write_u8(kind).await.unwrap();
    stream.write_u32(payload.len() as u32).await.unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

async fn terminal_ready(stream: &mut UnixStream) {
    let mut handshake = [0; 13];
    stream.read_exact(&mut handshake).await.unwrap();
    assert_eq!(&handshake, b"CONNECT 1026\n");
    stream.write_all(b"OK 1026\n").await.unwrap();
    send_frame(stream, 3, br#"{"type":"ready","cols":80,"rows":24}"#).await;
}

async fn terminal_action(stream: &mut UnixStream) {
    let (kind, resize) = frame(stream).await;
    assert_eq!(kind, 3);
    assert_eq!(
        serde_json::from_slice::<Value>(&resize).unwrap(),
        json!({"type":"resize", "cols":100, "rows":30})
    );
    let (kind, input) = frame(stream).await;
    assert_eq!(kind, 1);
    assert_eq!(&input, b"compute\n");
}

async fn assert_stream_closed(stream: &mut UnixStream) {
    let mut byte = [0];
    assert_eq!(
        timeout(BOUND, stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

async fn assert_no_listener(addr: SocketAddr) {
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn invalid_configuration_exits_before_connecting_to_the_native_workload() {
    let root = private_root();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    for invalid in ["key", "idle", "scope", "missing_scope", "origin"] {
        let (gateway, observation) = unused_addrs().await;
        let mut declaration = config(&path, gateway, observation);
        match invalid {
            "key" => declaration["assertion_keys"] = json!({"wrong-kid":"k".repeat(32)}),
            "idle" => declaration["idle_timeout_secs"] = json!(0),
            "scope" => declaration["execution"]["run_id"] = json!("invalid run"),
            "missing_scope" => declaration["execution"] = Value::Null,
            "origin" => declaration["origin"] = json!("https://example.test/path"),
            _ => unreachable!(),
        }
        let mut child = start_sidecar(&declaration).await;
        let status = timeout(BOUND, child.wait()).await.unwrap().unwrap();
        assert!(!status.success(), "invalid {invalid} was accepted");
        assert!(
            timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "invalid {invalid} contacted the workload"
        );
        assert_no_listener(gateway).await;
        assert_no_listener(observation).await;
    }
}

#[tokio::test]
async fn owner_eof_cancels_an_actual_inflight_exchange_before_its_thirty_second_timeout() {
    let root = private_root();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let (gateway, observation) = unused_addrs().await;
    let (accepted, action_received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        terminal_ready(&mut stream).await;
        terminal_action(&mut stream).await;
        accepted.send(()).unwrap();
        // No output: cancellation, rather than a successful probe or its 30s
        // timeout, must close this live private connection.
        assert_stream_closed(&mut stream).await;
    });
    let mut server = AbortFixture(server);
    let mut child = start_sidecar(&config(&path, gateway, observation)).await;
    timeout(BOUND, action_received).await.unwrap().unwrap();
    drop(child.stdin.take());
    let status = timeout(BOUND, child.wait()).await.unwrap().unwrap();
    assert!(
        status.success(),
        "owner EOF did not cancel cleanly: {status}"
    );
    timeout(BOUND, &mut server.0).await.unwrap().unwrap();
    assert_no_listener(gateway).await;
    assert_no_listener(observation).await;
}

#[tokio::test]
async fn completed_action_is_frozen_for_repeated_gets_and_owner_eof_closes_both_listeners() {
    let root = private_root();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let (gateway, observation) = unused_addrs().await;
    let actions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&actions);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        terminal_ready(&mut stream).await;
        terminal_action(&mut stream).await;
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        send_frame(&mut stream, 2, ACTUAL_OUTPUT).await;
        assert_stream_closed(&mut stream).await;
        // The distinct Surface readiness probe must stop after Ready. It must
        // neither repeat the application action nor steal PTY output.
        let (mut stream, _) = listener.accept().await.unwrap();
        terminal_ready(&mut stream).await;
        assert_stream_closed(&mut stream).await;
    });
    let mut server = AbortFixture(server);
    let mut child = start_sidecar(&config(&path, gateway, observation)).await;
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    timeout(BOUND, output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["surface_ready"], true);
    assert_eq!(ready["contract_verified"], false);
    assert_eq!(ready["application_probe_addr"], observation.to_string());
    for _ in 0..2 {
        let mut client = TcpStream::connect(observation).await.unwrap();
        client
            .write_all(b"GET /result HTTP/1.1\r\nHost: private\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        timeout(BOUND, client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let body = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert_eq!(&response[body..], ACTUAL_OUTPUT);
    }
    assert_eq!(actions.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Pending clients must be owned by the listeners and also close on EOF.
    let mut pending_observation = TcpStream::connect(observation).await.unwrap();
    let mut pending_gateway = TcpStream::connect(gateway).await.unwrap();
    drop(child.stdin.take());
    let status = timeout(BOUND, child.wait()).await.unwrap().unwrap();
    assert!(status.success(), "owner cleanup failed: {status}");
    assert_eq!(
        timeout(BOUND, pending_observation.read_u8())
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        timeout(BOUND, pending_gateway.read_u8())
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    timeout(BOUND, &mut server.0).await.unwrap().unwrap();
    assert_no_listener(gateway).await;
    assert_no_listener(observation).await;
}
