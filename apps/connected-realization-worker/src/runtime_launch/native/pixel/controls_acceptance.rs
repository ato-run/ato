//! Explicit actual Native fixture. All clipboard state belongs to this Run's
//! isolated X server, never the host. Protocol helpers mirror the bounded netd
//! CPU fixture, adding the actual Runner-generated Xauthority cookie.
use super::*;
use ato_ipc::session_surface::{
    SURFACE_GATEWAY_ASSERTION_AUDIENCE, SurfaceAssertionClaims, SurfaceAssertionPrincipal,
    SurfacePrincipalKind,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    collections::VecDeque,
    io,
    net::TcpStream,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::UnixStream;
use tungstenite::{Message, WebSocket, client::IntoClientRequest};

type Ws = WebSocket<TcpStream>;
fn signed(
    scope: &SurfaceGatewayScope,
    execution: &SurfaceExecutionScope,
    key: &str,
    allowed: bool,
    jti: &str,
) -> String {
    signed_until(
        scope,
        execution,
        key,
        allowed,
        jti,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60,
    )
}
fn signed_until(
    scope: &SurfaceGatewayScope,
    execution: &SurfaceExecutionScope,
    key: &str,
    allowed: bool,
    jti: &str,
    exp: u64,
) -> String {
    let claims = SurfaceAssertionClaims {
        clipboard_paste: allowed,
        framebuffer_resize: allowed,
        execution: Some(execution.clone()),
        aud: SURFACE_GATEWAY_ASSERTION_AUDIENCE.into(),
        session_id: scope.session_id.clone(),
        surface_id: scope.surface_id.clone(),
        principal: SurfaceAssertionPrincipal {
            kind: SurfacePrincipalKind::User,
            id: "fixture-owner".into(),
        },
        exp,
        jti: jti.into(),
        kid: "ato.runner-surface@1".into(),
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
}
fn exact(ws: &mut Ws, pending: &mut VecDeque<u8>, count: usize) -> Vec<u8> {
    while pending.len() < count {
        match ws.read().unwrap() {
            Message::Binary(bytes) => pending.extend(bytes),
            Message::Ping(_) => ws.flush().unwrap(),
            _ => panic!("unexpected RFB frame"),
        }
        assert!(pending.len() <= 4096 * 2160 * 4 + 4096);
    }
    pending.drain(..count).collect()
}
fn connect(addr: SocketAddr, origin: &str, token: &str) -> (Ws, (u16, u16)) {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut req = format!("ws://{addr}/__ato/surface")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("origin", origin.parse().unwrap());
    req.headers_mut()
        .insert("x-ato-surface-assertion", token.parse().unwrap());
    let (mut ws, _) = tungstenite::client(req, stream).unwrap();
    let mut pending = VecDeque::new();
    assert_eq!(exact(&mut ws, &mut pending, 12), b"RFB 003.008\n");
    ws.send(Message::binary(b"RFB 003.008\n".to_vec())).unwrap();
    let count = exact(&mut ws, &mut pending, 1)[0] as usize;
    assert!(exact(&mut ws, &mut pending, count).contains(&1));
    ws.send(Message::binary(vec![1])).unwrap();
    assert_eq!(exact(&mut ws, &mut pending, 4), [0; 4]);
    ws.send(Message::binary(vec![1])).unwrap();
    let init = exact(&mut ws, &mut pending, 24);
    let name = u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize;
    assert!(name <= 4096);
    exact(&mut ws, &mut pending, name);
    (
        ws,
        (
            u16::from_be_bytes(init[..2].try_into().unwrap()),
            u16::from_be_bytes(init[2..4].try_into().unwrap()),
        ),
    )
}
fn cut(text: &[u8]) -> Vec<u8> {
    let mut bytes = vec![6, 0, 0, 0];
    bytes.extend((text.len() as u32).to_be_bytes());
    bytes.extend(text);
    bytes
}
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
    async fn open(path: &std::path::Path, cookie: &[u8]) -> Self {
        let mut stream = UnixStream::connect(path).await.unwrap();
        let name = b"MIT-MAGIC-COOKIE-1";
        let mut setup = vec![b'l', 0, 11, 0, 0, 0];
        setup.extend((name.len() as u16).to_le_bytes());
        setup.extend((cookie.len() as u16).to_le_bytes());
        setup.extend([0; 2]);
        setup.extend(name);
        setup.resize(setup.len().div_ceil(4) * 4, 0);
        setup.extend(cookie);
        setup.resize(setup.len().div_ceil(4) * 4, 0);
        stream.write_all(&setup).await.unwrap();
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

pub(crate) async fn exercise(
    surface: SocketAddr,
    root: &Path,
    scope: &SurfaceGatewayScope,
    key: &str,
    origin: &str,
    execution: &SurfaceExecutionScope,
) {
    let authority = std::fs::read(root.join("oci-runtime/display/auth/Xauthority")).unwrap();
    let cookie = &authority[authority.len() - 16..];
    use std::os::fd::AsRawFd;
    let directory = std::fs::File::open(root.join("oci-runtime/display/x11")).unwrap();
    let socket = PathBuf::from(format!("/proc/self/fd/{}/X64", directory.as_raw_fd()));
    let mut selection = SelectionClient::open(&socket, cookie).await;
    assert_eq!(selection.read().await, None);
    let denied = signed(scope, execution, key, false, "native-controls-denied");
    let origin_owned = origin.to_string();
    tokio::task::spawn_blocking(move || {
        let (mut ws, size) = connect(surface, &origin_owned, &denied);
        assert_eq!(size, (640, 480));
        let _ = ws.send(Message::binary(cut(b"denied")));
        assert!(matches!(ws.read(), Err(_) | Ok(Message::Close(_))));
    })
    .await
    .unwrap();
    assert_eq!(selection.read().await, None);
    let allowed = signed(scope, execution, key, true, "native-controls-allowed");
    let origin_owned = origin.to_string();
    let mut ws = tokio::task::spawn_blocking(move || {
        let (mut ws, size) = connect(surface, &origin_owned, &allowed);
        assert_eq!(size, (640, 480));
        let paste = cut(b"Ato caf\xe9\n");
        ws.send(Message::binary(paste[..9].to_vec())).unwrap();
        ws
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        selection.read().await,
        None,
        "partial paste must not leave prefix"
    );
    ws = tokio::task::spawn_blocking(move || {
        ws.send(Message::binary(cut(b"Ato caf\xe9\n")[9..].to_vec()))
            .unwrap();
        ws
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(selection.read().await, Some(b"Ato caf\xe9\n".to_vec()));
    tokio::task::spawn_blocking(move || {
        let mut encodings = vec![2, 0, 0, 2];
        encodings.extend(0i32.to_be_bytes());
        encodings.extend((-308i32).to_be_bytes());
        ws.send(Message::binary(encodings)).unwrap();
        let mut resize = vec![251, 0];
        resize.extend(800u16.to_be_bytes());
        resize.extend(600u16.to_be_bytes());
        resize.extend([1, 0]);
        resize.extend([0; 8]);
        resize.extend(800u16.to_be_bytes());
        resize.extend(600u16.to_be_bytes());
        resize.extend([0; 4]);
        ws.send(Message::binary(resize)).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let _ = ws.close(None);
    })
    .await
    .unwrap();
    let allowed = signed(scope, execution, key, true, "native-controls-reconnect");
    let origin_owned = origin.to_string();
    tokio::task::spawn_blocking(move || {
        let (mut ws, size) = connect(surface, &origin_owned, &allowed);
        assert_eq!(size, (800,600), "resize changes only the owned display");
        // SendCutText=0: reconnect does not receive server clipboard data.
        ws.get_mut().set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        assert!(matches!(ws.read(), Err(tungstenite::Error::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)));
        let _ = ws.close(None);
    }).await.unwrap();
}

/// Independent state observation after the browser has finished. This never
/// injects a paste or resize and cannot make a browser failure look successful.
pub(crate) async fn verify_browser_result(
    surface: SocketAddr,
    root: &Path,
    scope: &SurfaceGatewayScope,
    key: &str,
    origin: &str,
    execution: &SurfaceExecutionScope,
    expiry: u64,
) {
    use std::os::fd::AsRawFd;
    let authority = std::fs::read(root.join("oci-runtime/display/auth/Xauthority")).unwrap();
    let directory = std::fs::File::open(root.join("oci-runtime/display/x11")).unwrap();
    let socket = PathBuf::from(format!("/proc/self/fd/{}/X64", directory.as_raw_fd()));
    let mut selection = SelectionClient::open(&socket, &authority[authority.len() - 16..]).await;
    assert_eq!(
        selection.read().await,
        Some(b"caf\xe9\t\n".to_vec()),
        "browser paste must reach only the owned display"
    );
    for (suffix, message) in [
        ("paste", cut(b"denied replacement")),
        ("resize", {
            let mut bytes = vec![251, 0];
            bytes.extend(800u16.to_be_bytes());
            bytes.extend(600u16.to_be_bytes());
            bytes.extend([1, 0]);
            bytes.extend([0; 8]);
            bytes.extend(800u16.to_be_bytes());
            bytes.extend(600u16.to_be_bytes());
            bytes.extend([0; 4]);
            bytes
        }),
    ] {
        let denied = signed_until(
            scope,
            execution,
            key,
            false,
            &format!("native-browser-denied-{suffix}"),
            expiry,
        );
        let origin = origin.to_owned();
        tokio::task::spawn_blocking(move || {
            let (mut ws, size) = connect(surface, &origin, &denied);
            assert_eq!(size, (1024, 768));
            ws.send(Message::binary(message)).unwrap();
            assert!(
                matches!(ws.read(), Err(_) | Ok(Message::Close(_))),
                "permission=false must close without forwarding input"
            );
        })
        .await
        .unwrap();
        assert_eq!(
            selection.read().await,
            Some(b"caf\xe9\t\n".to_vec()),
            "denied input must leave the actual clipboard unchanged"
        );
    }
    let token = signed_until(
        scope,
        execution,
        key,
        true,
        "native-browser-state-verifier",
        expiry,
    );
    let origin_owned = origin.to_owned();
    let token_for_init = token.clone();
    tokio::task::spawn_blocking(move || {
        let (mut ws,size)=connect(surface,&origin_owned,&token_for_init);
        assert_eq!(size,(1024,768));
        ws.get_mut().set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        assert!(matches!(ws.read(),Err(tungstenite::Error::Io(e)) if matches!(e.kind(),io::ErrorKind::WouldBlock|io::ErrorKind::TimedOut)));
        let _=ws.close(None);
    }).await.unwrap();
    let frame_token = signed_until(
        scope,
        execution,
        key,
        true,
        "native-browser-full-frame-verifier",
        expiry,
    );
    let frame = netd::rfb_probe::wait_for_authenticated_gateway_frame(
        netd::rfb_probe::PixelGatewayProbeRequest::new(
            &format!("http://{surface}/__ato/surface"),
            origin,
            &frame_token,
        ),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.bytes),
        (1024, 768, 1024 * 768 * 4)
    );
}
