//! Real CPU-only TigerVNC acceptance. Uses an already-present pinned image;
//! never builds/pulls an image, touches host DISPLAY or enables production.
#![cfg(target_os = "linux")]
use ato_ipc::session_surface::{
    SURFACE_GATEWAY_ASSERTION_AUDIENCE, SurfaceAssertionClaims, SurfaceAssertionPrincipal,
    SurfaceExecutionScope, SurfacePrincipalKind,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use netd::{
    pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring},
    pixel_gateway::{PixelGatewayConfig, start_text_paste_pixel_gateway},
    surface_authorization::SurfaceGatewayScope,
};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UnixStream},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
struct OwnedContainer(String);
impl Drop for OwnedContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .output();
    }
}
fn docker(args: &[&str]) -> String {
    let result = Command::new("docker")
        .args(args)
        .output()
        .expect("existing Docker");
    assert!(
        result.status.success(),
        "Docker fixture refused: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}
fn signed(permission: bool, jti: &str, key: &str, execution: SurfaceExecutionScope) -> String {
    let claims = SurfaceAssertionClaims {
        clipboard_paste: permission,
        execution: Some(execution),
        aud: SURFACE_GATEWAY_ASSERTION_AUDIENCE.into(),
        session_id: "paste-test".into(),
        surface_id: "pixel-test".into(),
        principal: SurfaceAssertionPrincipal {
            kind: SurfacePrincipalKind::User,
            id: "fixture-owner".into(),
        },
        exp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60,
        jti: jti.into(),
        kid: "fixture".into(),
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
}
async fn exact(ws: &mut Ws, pending: &mut VecDeque<u8>, count: usize) -> Vec<u8> {
    while pending.len() < count {
        match ws.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => pending.extend(bytes),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
            _ => panic!("unexpected RFB frame"),
        }
    }
    pending.drain(..count).collect()
}
async fn connect(address: std::net::SocketAddr, token: &str) -> Ws {
    let mut req = format!("ws://{address}/").into_client_request().unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:5173".parse().unwrap());
    req.headers_mut()
        .insert("x-ato-surface-assertion", token.parse().unwrap());
    let (mut ws, _) = connect_async(req).await.unwrap();
    let mut pending = VecDeque::new();
    assert_eq!(exact(&mut ws, &mut pending, 12).await, b"RFB 003.008\n");
    ws.send(Message::binary(b"RFB 003.008\n".to_vec()))
        .await
        .unwrap();
    let count = exact(&mut ws, &mut pending, 1).await[0] as usize;
    assert!(exact(&mut ws, &mut pending, count).await.contains(&1));
    ws.send(Message::binary(vec![1])).await.unwrap();
    assert_eq!(exact(&mut ws, &mut pending, 4).await, [0; 4]);
    ws.send(Message::binary(vec![1])).await.unwrap();
    let init = exact(&mut ws, &mut pending, 24).await;
    let name = u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize;
    assert!(name < 4096);
    exact(&mut ws, &mut pending, name).await;
    assert!(pending.is_empty());
    ws
}
fn cut(text: &[u8]) -> Vec<u8> {
    let mut bytes = vec![6, 0, 0, 0];
    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
    bytes.extend(text);
    bytes
}

// Minimal X11 selection client over the fixture's owned Unix socket. No host
// Xlib/DISPLAY/clipboard, xclip installation, arbitrary shell or credentials.
struct SelectionClient {
    stream: UnixStream,
    window: u32,
    selection: u32,
    property: u32,
}
fn u32le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
async fn packet(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = vec![0; 32];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut header))
        .await
        .unwrap()
        .unwrap();
    if header[0] == 1 {
        let length = u32le(&header[4..8]) as usize * 4;
        assert!(length <= 128 * 1024);
        let n = header.len();
        header.resize(n + length, 0);
        tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut header[n..]))
            .await
            .unwrap()
            .unwrap();
    }
    assert_ne!(header[0], 0, "X11 request error");
    header
}
async fn atom(stream: &mut UnixStream, name: &[u8]) -> u32 {
    let mut req = vec![16, 0];
    req.extend_from_slice(&((8 + name.len()).div_ceil(4) as u16).to_le_bytes());
    req.extend_from_slice(&(name.len() as u16).to_le_bytes());
    req.extend([0, 0]);
    req.extend(name);
    req.resize(req.len().div_ceil(4) * 4, 0);
    stream.write_all(&req).await.unwrap();
    let reply = packet(stream).await;
    assert_eq!(reply[0], 1);
    u32le(&reply[8..12])
}
impl SelectionClient {
    async fn open(path: &std::path::Path) -> Self {
        let mut stream = UnixStream::connect(path).await.unwrap();
        stream
            .write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut header = [0; 8];
        tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut header))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header[0], 1, "isolated X11 setup");
        let len = u16::from_le_bytes([header[6], header[7]]) as usize * 4;
        assert!(len < 128 * 1024);
        let mut body = vec![0; len];
        tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut body))
            .await
            .unwrap()
            .unwrap();
        let window = u32le(&body[4..8]) | 1;
        let vendor = u16::from_le_bytes([body[16], body[17]]) as usize;
        let root_offset = 32 + vendor.div_ceil(4) * 4 + 8 * body[21] as usize;
        let root = u32le(&body[root_offset..root_offset + 4]);
        let mut req = vec![1, 0, 8, 0];
        req.extend(window.to_le_bytes());
        req.extend(root.to_le_bytes());
        req.extend([0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(req.len(), 32);
        stream.write_all(&req).await.unwrap();
        let selection = atom(&mut stream, b"CLIPBOARD").await;
        let property = atom(&mut stream, b"ATO_TEST_PASTE").await;
        Self {
            stream,
            window,
            selection,
            property,
        }
    }
    async fn read(&mut self) -> Option<Vec<u8>> {
        let mut req = vec![24, 0, 6, 0];
        for value in [self.window, self.selection, 31, self.property, 0] {
            req.extend(value.to_le_bytes());
        }
        self.stream.write_all(&req).await.unwrap();
        let notify = packet(&mut self.stream).await;
        assert_eq!(notify[0] & 127, 31);
        if u32le(&notify[20..24]) == 0 {
            return None;
        }
        let mut req = vec![20, 1, 6, 0];
        for value in [self.window, self.property, 0, 0, 16385] {
            req.extend(value.to_le_bytes());
        }
        self.stream.write_all(&req).await.unwrap();
        let reply = packet(&mut self.stream).await;
        assert_eq!(reply[0], 1);
        assert_eq!(reply[1], 8);
        let length = u32le(&reply[16..20]) as usize;
        assert!(length <= 65536);
        Some(reply[32..32 + length].to_vec())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires existing pinned TigerVNC image and explicit isolated fixture root"]
async fn isolated_tigervnc_receives_only_complete_signed_latin1_paste() {
    let image = std::env::var("ATO_TEXT_PASTE_TEST_IMAGE").expect("existing pinned helper image");
    let digest = image.split_once("@sha256:").expect("digest-pinned image").1;
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let root = std::env::var("ATO_EXPLORATION_TEST_ROOT").expect("owned fixture root");
    let temp = tempfile::Builder::new()
        .prefix("paste-")
        .tempdir_in(root)
        .unwrap();
    let socket_dir = temp.path().join("x11");
    std::fs::create_dir(&socket_dir).unwrap();
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mount = format!("type=bind,src={},dst=/tmp/.X11-unix", socket_dir.display());
    let rfb_dir = temp.path().join("rfb");
    std::fs::create_dir(&rfb_dir).unwrap();
    std::fs::set_permissions(&rfb_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rfb_mount = format!(
        "type=bind,src={},dst=/run/ato-fixture-rfb",
        rfb_dir.display()
    );
    let metadata = std::fs::metadata(&socket_dir).unwrap();
    let user = format!("{}:{}", metadata.uid(), metadata.gid());
    let id = docker(&[
        "create",
        "--pull=never",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--memory=134217728",
        "--cpus=0.5",
        "--pids-limit=32",
        "--tmpfs=/tmp:rw,noexec,nosuid,size=16m",
        "--mount",
        &mount,
        "--mount",
        &rfb_mount,
        "--user",
        &user,
        "--network=none",
        "--entrypoint=/usr/bin/Xtigervnc",
        &image,
        ":64",
        "-geometry",
        "320x200",
        "-depth",
        "24",
        "-rfbport",
        "-1",
        "-rfbunixpath",
        "/run/ato-fixture-rfb/display.sock",
        "-rfbunixmode",
        "0600",
        "-SecurityTypes",
        "None",
        "-AcceptCutText=1",
        "-SendCutText=0",
        "-AcceptSetDesktopSize=0",
        "-nolisten",
        "tcp",
        "-ac",
    ]);
    assert!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    let container = OwnedContainer(id);
    docker(&["start", &container.0]);
    let started = std::time::Instant::now();
    let socket = socket_dir.join("X64");
    let rfb_socket = rfb_dir.join("display.sock");
    while !socket.exists() || !rfb_socket.exists() {
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut tcp, _) = relay.accept().await.unwrap();
            let socket = rfb_socket.clone();
            connections.spawn(async move {
                let mut unix = UnixStream::connect(socket).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
            });
        }
    });
    let mut selection = SelectionClient::open(&socket).await;
    assert_eq!(selection.read().await, None);
    let key = "fixture-key-not-an-operator-credential-0001";
    let execution = SurfaceExecutionScope {
        run_id: "run-paste".into(),
        lease_id: "lease-paste".into(),
        runner_id: "runner-paste".into(),
    };
    let authorizer = HmacSurfaceAccessAuthorizer::new(Arc::new(
        SurfaceAssertionKeyring::new(BTreeMap::from([("fixture".into(), key.into())])).unwrap(),
    ))
    .with_execution_scope(execution.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let gateway = start_text_paste_pixel_gateway(
        PixelGatewayConfig {
            listen_addr: addr,
            private_rfb_addr: upstream,
            private_rfb_connect_timeout: Duration::from_secs(2),
            scope: SurfaceGatewayScope {
                session_id: "paste-test".into(),
                surface_id: "pixel-test".into(),
            },
            allowed_origins: BTreeSet::from(["http://localhost:5173".into()]),
        },
        Arc::new(authorizer),
    )
    .await
    .unwrap();
    let mut denied = connect(addr, &signed(false, "denied", key, execution.clone())).await;
    denied.send(Message::binary(cut(b"DENIED"))).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), denied.next())
            .await
            .is_ok()
    );
    assert_eq!(selection.read().await, None);
    let mut allowed = connect(addr, &signed(true, "allowed", key, execution)).await;
    let text = b"caf\xe9 \xff\t\n";
    let bytes = cut(text);
    allowed
        .send(Message::binary(bytes[..bytes.len() - 1].to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(selection.read().await, None);
    allowed
        .send(Message::binary(bytes[bytes.len() - 1..].to_vec()))
        .await
        .unwrap();
    loop {
        if selection.read().await.as_deref() == Some(text) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(8));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), allowed.next())
            .await
            .is_err(),
        "server clipboard must remain disabled"
    );
    allowed.close(None).await.unwrap();
    gateway.stop().await.unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
    relay_task.abort();
    let _ = relay_task.await;
    let elapsed = started.elapsed().as_millis();
    let id = container.0.clone();
    drop(selection);
    drop(container);
    assert!(
        !Command::new("docker")
            .args(["inspect", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    println!(
        "real_tigervnc_latin1_paste_ms={elapsed}, denied_prefix=0, partial_prefix=0, server_clipboard=false, owned_container_removed=true"
    );
}
