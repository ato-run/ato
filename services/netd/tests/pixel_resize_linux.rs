//! Actual isolated TigerVNC resize proof; explicit ignored CPU fixture only.
#![cfg(target_os = "linux")]
use ato_ipc::session_surface::{
    SURFACE_GATEWAY_ASSERTION_AUDIENCE, SurfaceAssertionClaims, SurfaceAssertionPrincipal,
    SurfaceExecutionScope, SurfacePrincipalKind,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use netd::{
    pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring},
    pixel_gateway::{
        PixelGatewayConfig, PixelGatewayControls, PixelGatewayScope,
        start_interactive_pixel_gateway,
    },
    rfb_probe::{
        PixelGatewayProbeRequest, capture_private_rfb_frame, wait_for_authenticated_gateway_frame,
    },
};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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
const KEY: &str = "resize-fixture-key-not-an-operator-credential-0001";
fn command(program: &str, args: &[&str]) -> String {
    let result = Command::new(program).args(args).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}
struct Container(String);
impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .output();
    }
}
struct Task(Option<tokio::task::JoinHandle<()>>);
impl Task {
    async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}
fn scope() -> SurfaceExecutionScope {
    SurfaceExecutionScope {
        run_id: "run-resize".into(),
        lease_id: "lease-resize".into(),
        runner_id: "runner-resize".into(),
    }
}
fn signed(permission: bool, jti: &str) -> String {
    let claims = SurfaceAssertionClaims {
        clipboard_paste: false,
        framebuffer_resize: permission,
        execution: Some(scope()),
        aud: SURFACE_GATEWAY_ASSERTION_AUDIENCE.into(),
        session_id: "resize-test".into(),
        surface_id: "pixel-test".into(),
        principal: SurfaceAssertionPrincipal {
            kind: SurfacePrincipalKind::User,
            id: "fixture-owner".into(),
        },
        exp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120,
        jti: jti.into(),
        kid: "fixture".into(),
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
}
async fn exact(ws: &mut Ws, pending: &mut VecDeque<u8>, count: usize) -> Vec<u8> {
    assert!(count <= 64 * 1024 * 1024);
    while pending.len() < count {
        let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match next {
            Message::Binary(bytes) => {
                assert!(pending.len() + bytes.len() <= 64 * 1024 * 1024);
                pending.extend(bytes);
            }
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
            _ => panic!("unexpected RFB message"),
        }
    }
    pending.drain(..count).collect()
}
async fn connect(
    address: std::net::SocketAddr,
    permission: bool,
    jti: &str,
) -> (Ws, VecDeque<u8>, (u16, u16)) {
    let mut request = format!("ws://{address}/").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:5173".parse().unwrap());
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "binary".parse().unwrap());
    request.headers_mut().insert(
        "x-ato-surface-assertion",
        signed(permission, jti).parse().unwrap(),
    );
    let (mut ws, _) = connect_async(request).await.unwrap();
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
    assert!(name <= 4096);
    exact(&mut ws, &mut pending, name).await;
    (
        ws,
        pending,
        (
            u16::from_be_bytes([init[0], init[1]]),
            u16::from_be_bytes([init[2], init[3]]),
        ),
    )
}
fn resize(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![251, 0];
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([1, 0]);
    bytes.extend([0; 8]);
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([0; 4]);
    bytes
}
async fn request_frame(ws: &mut Ws, width: u16, height: u16) {
    // Match the existing noVNC acceptance client: incremental requests allow
    // ExtendedDesktopSize metadata to drain before framebuffer delivery.
    let mut request = vec![3, 1, 0, 0, 0, 0];
    request.extend(width.to_be_bytes());
    request.extend(height.to_be_bytes());
    ws.send(Message::binary(request)).await.unwrap();
}
async fn update(ws: &mut Ws, pending: &mut VecDeque<u8>) -> (usize, Vec<(u16, u16, u16, u16)>) {
    for _ in 0..16 {
        let kind = exact(ws, pending, 1).await[0];
        if kind == 2 {
            continue;
        }
        assert_eq!(kind, 0);
        let header = exact(ws, pending, 3).await;
        let rectangles = u16::from_be_bytes([header[1], header[2]]);
        assert!(rectangles <= 4096);
        let mut pixels = 0usize;
        let mut sizes = Vec::new();
        for _ in 0..rectangles {
            let rect = exact(ws, pending, 12).await;
            let x = u16::from_be_bytes([rect[0], rect[1]]);
            let y = u16::from_be_bytes([rect[2], rect[3]]);
            let rw = u16::from_be_bytes([rect[4], rect[5]]);
            let rh = u16::from_be_bytes([rect[6], rect[7]]);
            match i32::from_be_bytes(rect[8..12].try_into().unwrap()) {
                -308 => {
                    let screens = exact(ws, pending, 4).await;
                    assert_eq!(screens[0], 1);
                    exact(ws, pending, 16).await;
                    sizes.push((x, y, rw, rh));
                }
                0 => {
                    assert!(rw <= 4096 && rh <= 2160);
                    let bytes = usize::from(rw) * usize::from(rh) * 4;
                    pixels += bytes;
                    assert!(pixels <= 4096 * 2160 * 4);
                    exact(ws, pending, bytes).await;
                }
                other => panic!("unexpected encoding {other}"),
            }
        }
        return (pixels, sizes);
    }
    panic!("no bounded framebuffer update");
}
async fn resize_ack(ws: &mut Ws, pending: &mut VecDeque<u8>, width: u16, height: u16) {
    // Establish the actual initial Surface before input. Otherwise TigerVNC's
    // initial reason=0 metadata may coalesce with an early resize notification.
    ws.send(Message::binary(vec![
        0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
    ]))
    .await
    .unwrap();
    let mut encodings = vec![2, 0, 0, 2];
    encodings.extend(0i32.to_be_bytes());
    encodings.extend((-308i32).to_be_bytes());
    ws.send(Message::binary(encodings)).await.unwrap();
    let mut initial_pixels = 0;
    for _ in 0..16 {
        request_frame(ws, 640, 480).await;
        let (pixels, sizes) = update(ws, pending).await;
        eprintln!("initial_frame_bytes={pixels} sizes={sizes:?}");
        if pixels == 640 * 480 * 4 {
            initial_pixels = pixels;
            break;
        }
    }
    assert_eq!(initial_pixels, 640 * 480 * 4);
    ws.send(Message::binary(resize(width, height)))
        .await
        .unwrap();
    for _ in 0..16 {
        request_frame(ws, width, height).await;
        let (_, sizes) = update(ws, pending).await;
        if sizes.contains(&(1, 0, width, height)) {
            return;
        }
    }
    panic!("no request-specific successful resize acknowledgement");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires existing pinned TigerVNC image and explicit owned fixture root"]
async fn owned_unix_tigervnc_resize_has_real_ack_frame_reconnect_and_denied_zero_bytes() {
    let image = std::env::var("ATO_RESIZE_TEST_IMAGE").expect("explicit existing pinned image");
    let digest = image.split_once("@sha256:").unwrap().1;
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let uid = command("id", &["-u"]);
    let gid = command("id", &["-g"]);
    assert_ne!(uid, "0");
    let user = format!("{uid}:{gid}");
    let root = std::env::var("ATO_EXPLORATION_TEST_ROOT").expect("own workplace .tmp");
    let temp = tempfile::Builder::new()
        .prefix("resize-")
        .tempdir_in(root)
        .unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mount = format!("type=bind,src={},dst=/rfb", temp.path().display());
    let label = format!(
        "ato.test.native-resize={}",
        temp.path().file_name().unwrap().to_str().unwrap()
    );
    eprintln!("resize_fixture_label={label}");
    let id = command(
        "docker",
        &[
            "create",
            "--pull=never",
            "--label",
            &label,
            "--user",
            &user,
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=268435456",
            "--cpus=0.5",
            "--pids-limit=32",
            "--tmpfs=/tmp:rw,noexec,nosuid,size=16m",
            "--mount",
            &mount,
            "--entrypoint=/usr/bin/Xtigervnc",
            &image,
            ":64",
            "-geometry",
            "640x480",
            "-depth",
            "24",
            "-rfbport",
            "-1",
            "-rfbunixpath",
            "/rfb/display.sock",
            "-rfbunixmode",
            "0600",
            "-SecurityTypes",
            "None",
            "-AcceptCutText=0",
            "-SendCutText=0",
            "-AcceptSetDesktopSize=1",
            "-AlwaysShared",
            "-nolisten",
            "tcp",
            "-ac",
        ],
    );
    assert!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    let container = Container(id);
    command("docker", &["start", &container.0]);
    let socket = temp.path().join("display.sock");
    let started = std::time::Instant::now();
    while !socket.exists() {
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = relay.local_addr().unwrap();
    let bytes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&bytes);
    // The host path can exceed sockaddr_un; the owned fd keeps this short
    // path attached to exactly this fixture directory throughout the relay.
    let directory = std::fs::File::open(temp.path()).unwrap();
    let socket = std::path::PathBuf::from(format!(
        "/proc/self/fd/{}/display.sock",
        directory.as_raw_fd()
    ));
    let relay_task = Task(Some(tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! { accepted = relay.accept() => {
                let (mut client,_) = accepted.unwrap(); let path = socket.clone(); let count = Arc::clone(&count);
                connections.spawn(async move {
                    let mut guest = UnixStream::connect(path).await.unwrap(); let (mut cr, mut cw) = client.split(); let (mut gr, mut gw) = guest.split();
                    let input = async { let mut buffer = [0; 4096]; loop { let n = cr.read(&mut buffer).await?; if n == 0 { break; } count.fetch_add(n, Ordering::SeqCst); gw.write_all(&buffer[..n]).await?; } Ok::<_,std::io::Error>(()) };
                    let output = tokio::io::copy(&mut gr, &mut cw); tokio::select! { _ = input => {}, _ = output => {} }
                });
            }, _ = connections.join_next(), if !connections.is_empty() => {} }
        }
    })));
    let authorizer = HmacSurfaceAccessAuthorizer::new(Arc::new(
        SurfaceAssertionKeyring::new(BTreeMap::from([("fixture".into(), KEY.into())])).unwrap(),
    ))
    .with_execution_scope(scope());
    let gateway = start_interactive_pixel_gateway(
        PixelGatewayConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            private_rfb_addr: upstream,
            private_rfb_connect_timeout: Duration::from_secs(2),
            scope: PixelGatewayScope {
                session_id: "resize-test".into(),
                surface_id: "pixel-test".into(),
            },
            allowed_origins: BTreeSet::from(["http://localhost:5173".into()]),
        },
        Arc::new(authorizer),
        PixelGatewayControls {
            text_paste: false,
            framebuffer_resize: true,
        },
    )
    .await
    .unwrap();
    let (mut denied, _, initial) = connect(gateway.local_addr(), false, "denied-resize").await;
    assert_eq!(initial, (640, 480));
    let before = bytes.load(Ordering::SeqCst);
    denied
        .send(Message::binary(resize(1024, 768)))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), denied.next())
        .await
        .unwrap();
    assert_eq!(bytes.load(Ordering::SeqCst), before);
    let (mut allowed, mut pending, initial) =
        connect(gateway.local_addr(), true, "allowed-resize").await;
    assert_eq!(initial, (640, 480));
    resize_ack(&mut allowed, &mut pending, 1024, 768).await;
    allowed.close(None).await.unwrap();
    drop(allowed);
    let frame = capture_private_rfb_frame(
        upstream,
        1024,
        768,
        &[],
        1024 * 768 * 4,
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(frame.len(), 1024 * 768 * 4);
    let url = format!("http://{}/", gateway.local_addr());
    let token = signed(false, "reconnect");
    let connected = wait_for_authenticated_gateway_frame(
        PixelGatewayProbeRequest::new(&url, "http://localhost:5173", &token),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(
        (connected.width, connected.height, connected.bytes),
        (1024, 768, 1024 * 768 * 4)
    );
    let address = gateway.local_addr();
    gateway.stop().await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    relay_task.stop().await;
    assert!(TcpStream::connect(upstream).await.is_err());
    let id = container.0.clone();
    drop(container);
    assert!(
        !Command::new("docker")
            .args(["inspect", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    eprintln!(
        "resize_image={image} framebuffer=1024x768 bytes={} permission_denied_upstream_bytes=0 cleanup=confirmed",
        frame.len()
    );
}
