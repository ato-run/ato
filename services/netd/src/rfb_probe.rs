//! RFB protocol probes used to gate pixel-surface readiness.
//!
//! The public probe traverses the authenticated WebSocket gateway, negotiates
//! RFB 3.8, sends a harmless pointer event followed by a framebuffer request,
//! and waits for a complete raw frame. A session is not ready until that full
//! ordered path succeeds.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use http::{
    HeaderValue,
    header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    time::timeout,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;

use crate::pixel_gateway::{
    PixelGatewayConfig, PixelGatewayError, PixelGatewayHandle, SURFACE_ASSERTION_HEADER,
    SurfaceAccessAuthorizer, start_pixel_gateway,
};

const RFB_3_8_VERSION: &[u8; 12] = b"RFB 003.008\n";
const SECURITY_TYPE_NONE: u8 = 1;
const ENCODING_RAW: i32 = 0;
const MAX_DESKTOP_NAME_BYTES: usize = 4 * 1024;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const WEBSOCKET_BRIDGE_BYTES: usize = 64 * 1024;

struct AbortTaskOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfbFrameInfo {
    pub width: u16,
    pub height: u16,
    pub bytes: usize,
}

#[derive(Debug, Error)]
pub enum RfbProbeError {
    #[error("RFB readiness probe timed out")]
    Timeout,
    #[error("RFB readiness probe I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("RFB server protocol is unsupported")]
    UnsupportedProtocol,
    #[error("RFB private endpoint did not offer security type None")]
    UnsupportedSecurity,
    #[error("RFB private endpoint rejected initialization")]
    SecurityRejected,
    #[error("RFB framebuffer metadata is invalid")]
    InvalidFramebuffer,
    #[error("pixel gateway readiness URL is invalid or contains credential material")]
    InvalidGatewayUrl,
    #[error("pixel gateway readiness request headers are invalid")]
    InvalidGatewayHeaders,
    #[error("pixel gateway WebSocket connection failed: {0}")]
    WebSocket(#[source] tokio_tungstenite::tungstenite::Error),
    #[error("pixel gateway did not negotiate the binary RFB subprotocol")]
    MissingBinarySubprotocol,
}

/// Credential-bearing probe request. It intentionally has no `Debug` or
/// `Display` implementation so the assertion cannot enter ordinary logs.
pub struct PixelGatewayProbeRequest<'a> {
    connect_url: &'a str,
    origin: &'a str,
    assertion: &'a str,
}

impl<'a> PixelGatewayProbeRequest<'a> {
    pub fn new(connect_url: &'a str, origin: &'a str, assertion: &'a str) -> Self {
        Self {
            connect_url,
            origin,
            assertion,
        }
    }
}

#[derive(Debug, Error)]
pub enum PixelGatewayReadyError {
    #[error("pixel gateway failed to start: {0}")]
    Start(#[source] PixelGatewayError),
    #[error("authenticated pixel gateway readiness probe failed: {0}")]
    Probe(#[source] RfbProbeError),
    #[error("pixel gateway readiness probe failed and cleanup was not confirmed")]
    ProbeCleanup {
        #[source]
        probe: RfbProbeError,
        cleanup: PixelGatewayError,
    },
}

/// Converts the token-free public ready URL to the canonical browser-facing
/// WebSocket endpoint. Production requires HTTPS/WSS; plain HTTP/WS is allowed
/// only for exact loopback development and focused tests.
pub fn pixel_gateway_probe_url(public_ready_url: &str) -> Result<String, RfbProbeError> {
    let mut url = Url::parse(public_ready_url).map_err(|_| RfbProbeError::InvalidGatewayUrl)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(RfbProbeError::InvalidGatewayUrl);
    }
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    let websocket_scheme = match url.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" if loopback => "ws",
        _ => return Err(RfbProbeError::InvalidGatewayUrl),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| RfbProbeError::InvalidGatewayUrl)?;
    url.set_path("/surface");
    Ok(url.into())
}

/// Starts the session gateway and probes the authenticated public path before
/// returning its handle. Any probe failure stops the listener before returning,
/// so callers cannot accidentally report ready with an unproved gateway.
pub async fn start_ready_pixel_gateway(
    config: PixelGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    request: PixelGatewayProbeRequest<'_>,
    wait: Duration,
) -> Result<(PixelGatewayHandle, RfbFrameInfo), PixelGatewayReadyError> {
    let handle = start_pixel_gateway(config, authorizer)
        .await
        .map_err(PixelGatewayReadyError::Start)?;
    match wait_for_authenticated_gateway_frame(request, wait).await {
        Ok(frame) => Ok((handle, frame)),
        Err(probe) => match handle.stop().await {
            Ok(()) => Err(PixelGatewayReadyError::Probe(probe)),
            Err(cleanup) => Err(PixelGatewayReadyError::ProbeCleanup { probe, cleanup }),
        },
    }
}

/// Connects to a private RFB endpoint and waits for its first complete raw
/// framebuffer update. The timeout covers connect, handshake, and frame.
pub async fn wait_for_first_rfb_frame(
    addr: SocketAddr,
    wait: Duration,
) -> Result<RfbFrameInfo, RfbProbeError> {
    timeout(wait, probe(addr))
        .await
        .map_err(|_| RfbProbeError::Timeout)?
}

async fn probe(addr: SocketAddr) -> Result<RfbFrameInfo, RfbProbeError> {
    probe_stream(TcpStream::connect(addr).await?, false).await
}

/// Traverses the browser-facing authenticated WebSocket gateway and proves a
/// client input message and a complete first frame can cross the same ordered
/// RFB stream. The assertion is sent only in the dedicated header.
pub async fn wait_for_authenticated_gateway_frame(
    request: PixelGatewayProbeRequest<'_>,
    wait: Duration,
) -> Result<RfbFrameInfo, RfbProbeError> {
    timeout(wait, probe_gateway(request))
        .await
        .map_err(|_| RfbProbeError::Timeout)?
}

async fn probe_gateway(
    request: PixelGatewayProbeRequest<'_>,
) -> Result<RfbFrameInfo, RfbProbeError> {
    // Parse independently before IntoClientRequest so query/userinfo can never
    // become a hidden credential transport.
    let connect_url = pixel_gateway_probe_url(request.connect_url)?;
    let mut websocket_request = connect_url
        .as_str()
        .into_client_request()
        .map_err(|_| RfbProbeError::InvalidGatewayUrl)?;
    websocket_request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(request.origin).map_err(|_| RfbProbeError::InvalidGatewayHeaders)?,
    );
    websocket_request
        .headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("binary"));
    websocket_request.headers_mut().insert(
        SURFACE_ASSERTION_HEADER,
        HeaderValue::from_str(request.assertion)
            .map_err(|_| RfbProbeError::InvalidGatewayHeaders)?,
    );

    let (websocket, response) = connect_async(websocket_request)
        .await
        .map_err(RfbProbeError::WebSocket)?;
    if response
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some("binary")
    {
        return Err(RfbProbeError::MissingBinarySubprotocol);
    }

    let (client_stream, bridge_stream) = tokio::io::duplex(WEBSOCKET_BRIDGE_BYTES);
    // The timeout wrapper can cancel this future at any await point. Keep an
    // abort-on-drop owner so cancellation never detaches a credential-bearing
    // WebSocket bridge task.
    let bridge = AbortTaskOnDrop(tokio::spawn(async move {
        let _ = bridge_websocket(websocket, bridge_stream).await;
    }));
    let result = probe_stream(client_stream, true).await;
    drop(bridge);
    result
}

async fn bridge_websocket(
    websocket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    bridge: DuplexStream,
) -> Result<(), RfbProbeError> {
    let (mut websocket_sink, mut websocket_stream) = websocket.split();
    let (mut bridge_read, mut bridge_write) = tokio::io::split(bridge);
    let client_to_gateway = async {
        let mut buffer = vec![0_u8; WEBSOCKET_BRIDGE_BYTES];
        loop {
            let read = bridge_read.read(&mut buffer).await?;
            if read == 0 {
                websocket_sink
                    .close()
                    .await
                    .map_err(RfbProbeError::WebSocket)?;
                break;
            }
            websocket_sink
                .send(Message::binary(buffer[..read].to_vec()))
                .await
                .map_err(RfbProbeError::WebSocket)?;
        }
        Ok::<(), RfbProbeError>(())
    };
    let gateway_to_client = async {
        while let Some(message) = websocket_stream.next().await {
            match message.map_err(RfbProbeError::WebSocket)? {
                Message::Binary(bytes) => bridge_write.write_all(&bytes).await?,
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => break,
                Message::Text(_) | Message::Frame(_) => {
                    return Err(RfbProbeError::UnsupportedProtocol);
                }
            }
        }
        Ok::<(), RfbProbeError>(())
    };
    tokio::try_join!(client_to_gateway, gateway_to_client)?;
    Ok(())
}

async fn probe_stream<S>(
    mut stream: S,
    prove_input_path: bool,
) -> Result<RfbFrameInfo, RfbProbeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut version = [0_u8; 12];
    stream.read_exact(&mut version).await?;
    if &version != RFB_3_8_VERSION {
        return Err(RfbProbeError::UnsupportedProtocol);
    }
    stream.write_all(RFB_3_8_VERSION).await?;

    let security_count = stream.read_u8().await? as usize;
    if security_count == 0 || security_count > u8::MAX as usize {
        return Err(RfbProbeError::UnsupportedSecurity);
    }
    let mut security_types = vec![0_u8; security_count];
    stream.read_exact(&mut security_types).await?;
    if !security_types.contains(&SECURITY_TYPE_NONE) {
        return Err(RfbProbeError::UnsupportedSecurity);
    }
    stream.write_u8(SECURITY_TYPE_NONE).await?;
    if stream.read_u32().await? != 0 {
        return Err(RfbProbeError::SecurityRejected);
    }

    // Shared flag keeps the long-lived x11vnc server available for the actual
    // browser gateway connection after this short readiness probe exits.
    stream.write_u8(1).await?;
    let mut server_init = [0_u8; 24];
    stream.read_exact(&mut server_init).await?;
    let width = u16::from_be_bytes([server_init[0], server_init[1]]);
    let height = u16::from_be_bytes([server_init[2], server_init[3]]);
    let bytes_per_pixel = usize::from(server_init[4]).checked_div(8).unwrap_or(0);
    let name_len = u32::from_be_bytes([
        server_init[20],
        server_init[21],
        server_init[22],
        server_init[23],
    ]) as usize;
    if width == 0
        || height == 0
        || !matches!(bytes_per_pixel, 1 | 2 | 4)
        || name_len > MAX_DESKTOP_NAME_BYTES
    {
        return Err(RfbProbeError::InvalidFramebuffer);
    }
    let mut desktop_name = vec![0_u8; name_len];
    stream.read_exact(&mut desktop_name).await?;

    // A no-button pointer move is harmless but exercises the actual browser
    // input lane. The subsequent frame response proves the ordered RFB stream
    // carried both this message and the later update request to the server.
    if prove_input_path {
        let [x_hi, x_lo] = (width / 2).to_be_bytes();
        let [y_hi, y_lo] = (height / 2).to_be_bytes();
        stream.write_all(&[5, 0, x_hi, x_lo, y_hi, y_lo]).await?;
    }

    // SetEncodings(raw), then request one full non-incremental update.
    stream.write_all(&[2, 0, 0, 1]).await?;
    stream.write_all(&ENCODING_RAW.to_be_bytes()).await?;
    let [width_hi, width_lo] = width.to_be_bytes();
    let [height_hi, height_lo] = height.to_be_bytes();
    stream
        .write_all(&[3, 0, 0, 0, 0, 0, width_hi, width_lo, height_hi, height_lo])
        .await?;

    loop {
        match stream.read_u8().await? {
            0 => {
                let _padding = stream.read_u8().await?;
                let rectangle_count = stream.read_u16().await?;
                if rectangle_count == 0 {
                    continue;
                }
                let mut frame_bytes = 0_usize;
                for _ in 0..rectangle_count {
                    let mut rectangle = [0_u8; 12];
                    stream.read_exact(&mut rectangle).await?;
                    let x = u16::from_be_bytes([rectangle[0], rectangle[1]]);
                    let y = u16::from_be_bytes([rectangle[2], rectangle[3]]);
                    let rect_width = u16::from_be_bytes([rectangle[4], rectangle[5]]);
                    let rect_height = u16::from_be_bytes([rectangle[6], rectangle[7]]);
                    let encoding = i32::from_be_bytes([
                        rectangle[8],
                        rectangle[9],
                        rectangle[10],
                        rectangle[11],
                    ]);
                    if encoding != ENCODING_RAW {
                        return Err(RfbProbeError::UnsupportedProtocol);
                    }
                    if rect_width == 0
                        || rect_height == 0
                        || u32::from(x) + u32::from(rect_width) > u32::from(width)
                        || u32::from(y) + u32::from(rect_height) > u32::from(height)
                    {
                        return Err(RfbProbeError::InvalidFramebuffer);
                    }
                    let rectangle_bytes = usize::from(rect_width)
                        .checked_mul(usize::from(rect_height))
                        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
                        .ok_or(RfbProbeError::InvalidFramebuffer)?;
                    frame_bytes = frame_bytes
                        .checked_add(rectangle_bytes)
                        .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
                        .ok_or(RfbProbeError::InvalidFramebuffer)?;
                    let mut rectangle_pixels = vec![0_u8; rectangle_bytes];
                    stream.read_exact(&mut rectangle_pixels).await?;
                }
                return Ok(RfbFrameInfo {
                    width,
                    height,
                    bytes: frame_bytes,
                });
            }
            2 => {}
            3 => {
                let mut header = [0_u8; 7];
                stream.read_exact(&mut header).await?;
                let length =
                    u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
                if length > MAX_DESKTOP_NAME_BYTES {
                    return Err(RfbProbeError::UnsupportedProtocol);
                }
                let mut text = vec![0_u8; length];
                stream.read_exact(&mut text).await?;
            }
            _ => return Err(RfbProbeError::UnsupportedProtocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{net::TcpListener, time::sleep};

    async fn fixture_server(security_types: Vec<u8>) -> SocketAddr {
        fixture_server_with_input(security_types, false).await
    }

    async fn fixture_server_with_input(
        security_types: Vec<u8>,
        expect_pointer_input: bool,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        spawn_fixture_server(listener, security_types, expect_pointer_input);
        addr
    }

    fn spawn_fixture_server(
        listener: TcpListener,
        security_types: Vec<u8>,
        expect_pointer_input: bool,
    ) {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(RFB_3_8_VERSION).await.unwrap();
            let mut version = [0_u8; 12];
            stream.read_exact(&mut version).await.unwrap();
            stream.write_u8(security_types.len() as u8).await.unwrap();
            stream.write_all(&security_types).await.unwrap();
            if !security_types.contains(&SECURITY_TYPE_NONE) {
                return;
            }
            assert_eq!(stream.read_u8().await.unwrap(), SECURITY_TYPE_NONE);
            stream.write_u32(0).await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 1);

            let mut server_init = Vec::new();
            server_init.extend_from_slice(&2_u16.to_be_bytes());
            server_init.extend_from_slice(&1_u16.to_be_bytes());
            server_init
                .extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            server_init.extend_from_slice(&7_u32.to_be_bytes());
            server_init.extend_from_slice(b"fixture");
            stream.write_all(&server_init).await.unwrap();

            if expect_pointer_input {
                let mut pointer = [0_u8; 6];
                stream.read_exact(&mut pointer).await.unwrap();
                assert_eq!(pointer, [5, 0, 0, 1, 0, 0]);
            }
            let mut client_requests = [0_u8; 18];
            stream.read_exact(&mut client_requests).await.unwrap();
            assert_eq!(&client_requests[..8], &[2, 0, 0, 1, 0, 0, 0, 0]);
            assert_eq!(&client_requests[8..], &[3, 0, 0, 0, 0, 0, 0, 2, 0, 1]);

            stream.write_all(&[0, 0, 0, 2]).await.unwrap();
            stream
                .write_all(&[0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0])
                .await
                .unwrap();
            stream.write_all(&[0_u8; 4]).await.unwrap();
            stream
                .write_all(&[0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0])
                .await
                .unwrap();
            stream.write_all(&[0_u8; 4]).await.unwrap();
        });
    }

    struct TestAuthorizer;

    impl SurfaceAccessAuthorizer for TestAuthorizer {
        fn authorize(
            &self,
            assertion: &str,
            scope: &crate::pixel_gateway::PixelGatewayScope,
        ) -> Result<
            crate::pixel_gateway::AuthorizedSurfaceAccess,
            crate::pixel_gateway::SurfaceAuthorizationError,
        > {
            if assertion != "valid"
                || scope.session_id != "session-1"
                || scope.surface_id != "surface-1"
            {
                return Err(crate::pixel_gateway::SurfaceAuthorizationError);
            }
            Ok(crate::pixel_gateway::AuthorizedSurfaceAccess {
                principal: "user-1".to_string(),
                grant_id: "probe-grant-1".to_string(),
            })
        }
    }

    async fn reserved_listen_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    }

    fn gateway_config(listen_addr: SocketAddr, private_rfb_addr: SocketAddr) -> PixelGatewayConfig {
        PixelGatewayConfig {
            listen_addr,
            private_rfb_addr,
            private_rfb_connect_timeout: Duration::from_secs(2),
            scope: crate::pixel_gateway::PixelGatewayScope {
                session_id: "session-1".to_string(),
                surface_id: "surface-1".to_string(),
            },
            allowed_origins: std::collections::BTreeSet::from(
                ["http://localhost:5173".to_string()],
            ),
        }
    }

    #[tokio::test]
    async fn waits_for_a_complete_raw_frame() {
        let addr = fixture_server(vec![SECURITY_TYPE_NONE]).await;

        let frame = wait_for_first_rfb_frame(addr, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(
            frame,
            RfbFrameInfo {
                width: 2,
                height: 1,
                bytes: 8,
            }
        );
    }

    #[tokio::test]
    async fn rejects_password_only_private_endpoints() {
        let addr = fixture_server(vec![2]).await;

        assert!(matches!(
            wait_for_first_rfb_frame(addr, Duration::from_secs(1)).await,
            Err(RfbProbeError::UnsupportedSecurity)
        ));
    }

    #[test]
    fn public_probe_url_is_token_free_and_secure_except_on_loopback() {
        assert_eq!(
            pixel_gateway_probe_url("https://session.example/").unwrap(),
            "wss://session.example/surface"
        );
        assert_eq!(
            pixel_gateway_probe_url("http://127.0.0.1:8420/").unwrap(),
            "ws://127.0.0.1:8420/surface"
        );
        assert!(pixel_gateway_probe_url("http://session.example/").is_err());
        assert!(pixel_gateway_probe_url("https://user@session.example/").is_err());
        assert!(pixel_gateway_probe_url("https://session.example/?token=secret").is_err());
    }

    #[tokio::test]
    async fn authenticated_gateway_probe_proves_input_then_first_frame() {
        let private_rfb_addr = fixture_server_with_input(vec![SECURITY_TYPE_NONE], true).await;
        let listen_addr = reserved_listen_addr().await;
        let ready_url = format!("http://{listen_addr}/");
        let config = gateway_config(listen_addr, private_rfb_addr);
        let keyring = Arc::new(
            crate::pixel_authorization::SurfaceAssertionKeyring::new(
                std::collections::BTreeMap::from([(
                    "staging-v1".to_string(),
                    "0123456789abcdef0123456789abcdef".to_string(),
                )]),
            )
            .expect("test keyring"),
        );
        let assertion = keyring
            .issue_readiness_assertion(&config.scope)
            .expect("readiness assertion");
        let request =
            PixelGatewayProbeRequest::new(&ready_url, "http://localhost:5173", assertion.as_str());

        let (gateway, frame) = start_ready_pixel_gateway(
            config,
            Arc::new(crate::pixel_authorization::HmacSurfaceAccessAuthorizer::new(keyring)),
            request,
            Duration::from_secs(2),
        )
        .await
        .expect("authenticated public gateway path is ready");

        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.bytes, 8);
        gateway.stop().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_gateway_probe_retries_until_private_rfb_starts() {
        let private_rfb_addr = reserved_listen_addr().await;
        let listen_addr = reserved_listen_addr().await;
        let ready_url = format!("http://{listen_addr}/");
        let config = gateway_config(listen_addr, private_rfb_addr);
        let delayed_server = tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            let listener = TcpListener::bind(private_rfb_addr).await.unwrap();
            spawn_fixture_server(listener, vec![SECURITY_TYPE_NONE], true);
        });
        let request = PixelGatewayProbeRequest::new(&ready_url, "http://localhost:5173", "valid");

        let (gateway, frame) = start_ready_pixel_gateway(
            config,
            Arc::new(TestAuthorizer),
            request,
            Duration::from_secs(2),
        )
        .await
        .expect("gateway probe should retry while the private RFB endpoint starts");

        delayed_server.await.unwrap();
        assert_eq!(frame.width, 2);
        gateway.stop().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_gateway_probe_failure_stops_listener_and_prevents_ready() {
        // Authorization fails before the gateway may contact this deliberately
        // unused private endpoint.
        let private_rfb_addr = reserved_listen_addr().await;
        let listen_addr = reserved_listen_addr().await;
        let ready_url = format!("http://{listen_addr}/");
        let request = PixelGatewayProbeRequest::new(&ready_url, "http://localhost:5173", "invalid");

        let result = start_ready_pixel_gateway(
            gateway_config(listen_addr, private_rfb_addr),
            Arc::new(TestAuthorizer),
            request,
            Duration::from_secs(1),
        )
        .await;

        assert!(matches!(result, Err(PixelGatewayReadyError::Probe(_))));
        assert!(TcpStream::connect(listen_addr).await.is_err());
    }
}
