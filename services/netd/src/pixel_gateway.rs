//! Authenticated RFB-over-WebSocket adapter for pixel-stream surfaces.
//!
//! The gateway is deliberately separate from the regular HTTP ingress. It
//! accepts a browser-facing WebSocket only after a session-bound assertion has
//! been authorized, then relays binary messages to a private RFB TCP endpoint.

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub use crate::surface_authorization::{
    AuthorizedSurfaceAccess, SurfaceAccessAuthorizer, SurfaceAuthorizationError,
    SurfaceGatewayScope as PixelGatewayScope,
};
pub use crate::surface_websocket_auth::SURFACE_ASSERTION_HEADER;
use crate::surface_websocket_auth::{
    SurfaceHandshakeAuthorizer, is_normalized_allowed_origin, new_consumed_surface_grants,
};
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, mpsc, watch},
    task::JoinHandle,
    time::sleep,
};
use tokio_tungstenite::{accept_hdr_async_with_config, tungstenite::Message};

/// Header carrying the API-to-runner assertion. Browser credentials are never
/// forwarded to the private RFB endpoint.
const RFB_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CLIENT_MESSAGE_BYTES: usize = 1024 * 1024;
const OUTBOUND_QUEUE_DEPTH: usize = 8;
const RFB_CLIENT_HANDSHAKE_BYTES: usize = 14;
const MAX_TRACKED_CLIENT_MESSAGE_BYTES: usize = MAX_CLIENT_MESSAGE_BYTES;
const PRIVATE_RFB_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Configuration for one session-owned pixel gateway.
#[derive(Debug, Clone)]
pub struct PixelGatewayConfig {
    pub listen_addr: SocketAddr,
    pub private_rfb_addr: SocketAddr,
    pub private_rfb_connect_timeout: Duration,
    pub scope: PixelGatewayScope,
    pub allowed_origins: BTreeSet<String>,
}

fn is_private_rfb_address(address: SocketAddr) -> bool {
    match address.ip() {
        std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
    }
}

impl PixelGatewayConfig {
    fn validate(&self) -> Result<(), PixelGatewayError> {
        if self.scope.session_id.trim().is_empty() {
            return Err(PixelGatewayError::InvalidConfig(
                "session_id must not be empty",
            ));
        }
        if self.scope.surface_id.trim().is_empty() {
            return Err(PixelGatewayError::InvalidConfig(
                "surface_id must not be empty",
            ));
        }
        if self.allowed_origins.is_empty() {
            return Err(PixelGatewayError::InvalidConfig(
                "at least one allowed origin is required",
            ));
        }
        if self
            .allowed_origins
            .iter()
            .any(|origin| !is_normalized_allowed_origin(origin))
        {
            return Err(PixelGatewayError::InvalidConfig(
                "origins must be normalized HTTPS origins, except exact loopback development origins",
            ));
        }
        if !is_private_rfb_address(self.private_rfb_addr) {
            return Err(PixelGatewayError::InvalidConfig(
                "private RFB endpoint must use a private or loopback address",
            ));
        }
        if self.private_rfb_connect_timeout.is_zero() {
            return Err(PixelGatewayError::InvalidConfig(
                "private RFB connect timeout must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PixelGatewayError {
    #[error("invalid pixel gateway configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to bind pixel gateway: {0}")]
    Bind(#[source] std::io::Error),
    #[error("pixel gateway listener failed: {0}")]
    Accept(#[source] std::io::Error),
    #[error("failed to connect private RFB endpoint: {0}")]
    RfbConnect(#[source] std::io::Error),
    #[error("pixel gateway WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("pixel gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("pixel gateway task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Running session-owned gateway. [`stop`](Self::stop) is idempotent and waits
/// until the listener and all active relays are gone.
pub struct PixelGatewayHandle {
    local_addr: SocketAddr,
    cancel_tx: watch::Sender<bool>,
    task: AsyncMutex<Option<JoinHandle<Result<(), PixelGatewayError>>>>,
    last_input_activity_unix_millis: Arc<AtomicU64>,
}

impl PixelGatewayHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Timestamp of the latest parsed RFB keyboard or pointer input. Routine
    /// framebuffer requests do not extend a preview session. It starts at
    /// gateway creation so an untouched session can still time out.
    pub fn last_input_activity_unix_millis(&self) -> u64 {
        self.last_input_activity_unix_millis.load(Ordering::Relaxed)
    }

    pub async fn stop(&self) -> Result<(), PixelGatewayError> {
        let _ = self.cancel_tx.send(true);
        let mut task = self.task.lock().await;
        if let Some(task) = task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for PixelGatewayHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(true);
    }
}

/// Starts an authenticated WebSocket-to-RFB adapter.
pub async fn start_pixel_gateway(
    config: PixelGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
) -> Result<PixelGatewayHandle, PixelGatewayError> {
    config.validate()?;
    let listener = TcpListener::bind(config.listen_addr)
        .await
        .map_err(PixelGatewayError::Bind)?;
    let local_addr = listener.local_addr().map_err(PixelGatewayError::Bind)?;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let last_input_activity_unix_millis = Arc::new(AtomicU64::new(now_unix_millis()));
    let task = tokio::spawn(run_gateway(
        listener,
        config,
        authorizer,
        cancel_rx,
        Arc::clone(&last_input_activity_unix_millis),
    ));

    Ok(PixelGatewayHandle {
        local_addr,
        cancel_tx,
        task: AsyncMutex::new(Some(task)),
        last_input_activity_unix_millis,
    })
}

async fn run_gateway(
    listener: TcpListener,
    config: PixelGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    mut cancel_rx: watch::Receiver<bool>,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), PixelGatewayError> {
    let consumed_grants = new_consumed_surface_grants();
    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_err() || *cancel_rx.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(PixelGatewayError::Accept)?;
                let config = config.clone();
                let authorizer = Arc::clone(&authorizer);
                let consumed_grants = Arc::clone(&consumed_grants);
                let last_input_activity_unix_millis =
                    Arc::clone(&last_input_activity_unix_millis);
                connections.spawn(async move {
                    if let Err(error) = serve_pixel_connection(
                        stream,
                        config,
                        authorizer,
                        consumed_grants,
                        last_input_activity_unix_millis,
                    ).await {
                        tracing::debug!(%peer, %error, "pixel gateway connection closed");
                    }
                });
            }
        }
    }

    connections.shutdown().await;
    Ok(())
}

async fn serve_pixel_connection(
    stream: TcpStream,
    config: PixelGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    consumed_grants: crate::surface_websocket_auth::ConsumedSurfaceGrants,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), PixelGatewayError> {
    let callback = SurfaceHandshakeAuthorizer::new(
        config.allowed_origins.clone(),
        config.scope.clone(),
        authorizer,
        consumed_grants,
        "binary",
        false,
    );
    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_CLIENT_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_CLIENT_MESSAGE_BYTES));
    let websocket = accept_hdr_async_with_config(stream, callback, Some(ws_config)).await?;
    let rfb = connect_private_rfb(config.private_rfb_addr, config.private_rfb_connect_timeout)
        .await
        .map_err(PixelGatewayError::RfbConnect)?;

    relay_rfb(websocket, rfb, last_input_activity_unix_millis).await
}

async fn connect_private_rfb(
    addr: SocketAddr,
    connect_timeout: Duration,
) -> Result<TcpStream, std::io::Error> {
    let started = Instant::now();
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::ConnectionRefused
                    && started.elapsed() < connect_timeout =>
            {
                sleep(PRIVATE_RFB_CONNECT_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Parses the pinned RFB 3.8 client stream only far enough to distinguish
/// keyboard/pointer input from protocol housekeeping. Parsing is observational:
/// every byte is still relayed unchanged, and an unknown extension disables
/// activity tracking for that connection instead of rewriting the stream.
struct RfbClientInputTracker {
    handshake_remaining: usize,
    pending: Vec<u8>,
    supported: bool,
}

impl RfbClientInputTracker {
    fn new() -> Self {
        Self {
            handshake_remaining: RFB_CLIENT_HANDSHAKE_BYTES,
            pending: Vec::new(),
            supported: true,
        }
    }

    fn observe(&mut self, bytes: &[u8]) -> bool {
        if !self.supported
            || self
                .pending
                .len()
                .checked_add(bytes.len())
                .is_none_or(|size| size > MAX_TRACKED_CLIENT_MESSAGE_BYTES)
        {
            self.supported = false;
            self.pending.clear();
            return false;
        }
        self.pending.extend_from_slice(bytes);

        if self.handshake_remaining > 0 {
            let consumed = self.handshake_remaining.min(self.pending.len());
            self.pending.drain(..consumed);
            self.handshake_remaining -= consumed;
            if self.handshake_remaining > 0 {
                return false;
            }
        }

        let mut input_observed = false;
        loop {
            match tracked_rfb_client_message(&self.pending) {
                Ok(Some((length, is_input))) => {
                    self.pending.drain(..length);
                    input_observed |= is_input;
                }
                Ok(None) => break,
                Err(()) => {
                    self.supported = false;
                    self.pending.clear();
                    break;
                }
            }
        }
        input_observed
    }
}

fn tracked_rfb_client_message(bytes: &[u8]) -> Result<Option<(usize, bool)>, ()> {
    let Some(message_type) = bytes.first().copied() else {
        return Ok(None);
    };
    let fixed = |length: usize, is_input: bool| {
        if bytes.len() < length {
            Ok(None)
        } else {
            Ok(Some((length, is_input)))
        }
    };
    match message_type {
        0 => fixed(20, false), // SetPixelFormat
        2 => {
            if bytes.len() < 4 {
                return Ok(None);
            }
            let count = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
            let length = count
                .checked_mul(4)
                .and_then(|payload| payload.checked_add(4))
                .filter(|length| *length <= MAX_TRACKED_CLIENT_MESSAGE_BYTES)
                .ok_or(())?;
            fixed(length, false) // SetEncodings
        }
        3 => fixed(10, false), // FramebufferUpdateRequest
        4 => fixed(8, true),   // KeyEvent
        5 => fixed(6, true),   // PointerEvent
        6 => {
            if bytes.len() < 8 {
                return Ok(None);
            }
            let signed_length = i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            let payload = signed_length.checked_abs().ok_or(())? as usize;
            let length = payload
                .checked_add(8)
                .filter(|length| *length <= MAX_TRACKED_CLIENT_MESSAGE_BYTES)
                .ok_or(())?;
            fixed(length, false) // ClientCutText (disabled by the v1 profile)
        }
        150 => fixed(10, false), // EnableContinuousUpdates
        248 => {
            if bytes.len() < 9 {
                return Ok(None);
            }
            fixed(9 + usize::from(bytes[8]), false) // ClientFence
        }
        250 => fixed(4, true),   // XvpOp
        251 => fixed(24, false), // SetDesktopSize
        255 => fixed(12, true),  // QEMUExtendedKeyEvent
        _ => Err(()),
    }
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

async fn relay_rfb<S>(
    websocket: tokio_tungstenite::WebSocketStream<S>,
    rfb: TcpStream,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), PixelGatewayError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_stream) = websocket.split();
    let (mut rfb_read, mut rfb_write) = rfb.into_split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_DEPTH);

    let writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            ws_sink.send(message).await?;
        }
        ws_sink.close().await
    });

    let client_to_rfb = async {
        let mut input_tracker = RfbClientInputTracker::new();
        while let Some(message) = ws_stream.next().await {
            match message? {
                Message::Binary(bytes) => {
                    rfb_write.write_all(&bytes).await?;
                    if input_tracker.observe(&bytes) {
                        last_input_activity_unix_millis.store(now_unix_millis(), Ordering::Relaxed);
                    }
                }
                Message::Ping(bytes) => {
                    if outbound_tx.send(Message::Pong(bytes)).await.is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {}
                Message::Close(_) => break,
                Message::Text(_) | Message::Frame(_) => {
                    return Err(PixelGatewayError::InvalidConfig(
                        "RFB transport accepts binary WebSocket messages only",
                    ));
                }
            }
        }
        Ok::<(), PixelGatewayError>(())
    };

    let server_to_client = async {
        let mut buffer = vec![0_u8; RFB_BUFFER_BYTES];
        loop {
            let read = rfb_read.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            if outbound_tx
                .send(Message::binary(buffer[..read].to_vec()))
                .await
                .is_err()
            {
                break;
            }
        }
        Ok::<(), PixelGatewayError>(())
    };

    let result = tokio::select! {
        result = client_to_rfb => result,
        result = server_to_client => result,
    };
    drop(outbound_tx);
    let writer_result = writer.await?;
    result?;
    writer_result.map_err(PixelGatewayError::WebSocket)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use http::{
        HeaderValue, StatusCode,
        header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
    };
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{
            Error as WebSocketError, Message, client::IntoClientRequest, handshake::client::Request,
        },
    };

    use super::*;

    const ORIGIN_VALUE: &str = "https://app.ato.run";

    struct TestAuthorizer;

    impl SurfaceAccessAuthorizer for TestAuthorizer {
        fn authorize(
            &self,
            assertion: &str,
            scope: &PixelGatewayScope,
        ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError> {
            if assertion != "valid"
                || scope.session_id != "session-1"
                || scope.surface_id != "surface-1"
            {
                return Err(SurfaceAuthorizationError);
            }
            Ok(AuthorizedSurfaceAccess {
                principal: "user:1".to_string(),
                grant_id: "grant-1".to_string(),
            })
        }
    }

    async fn gateway_for(upstream: SocketAddr) -> PixelGatewayHandle {
        start_pixel_gateway(
            PixelGatewayConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                private_rfb_addr: upstream,
                private_rfb_connect_timeout: Duration::from_secs(1),
                scope: PixelGatewayScope {
                    session_id: "session-1".to_string(),
                    surface_id: "surface-1".to_string(),
                },
                allowed_origins: BTreeSet::from([ORIGIN_VALUE.to_string()]),
            },
            Arc::new(TestAuthorizer),
        )
        .await
        .unwrap()
    }

    fn request(addr: SocketAddr, origin: &str, assertion: Option<&str>) -> Request {
        let mut request = format!("ws://{addr}/surface")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
        request
            .headers_mut()
            .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("binary"));
        if let Some(assertion) = assertion {
            request.headers_mut().insert(
                SURFACE_ASSERTION_HEADER,
                HeaderValue::from_str(assertion).unwrap(),
            );
        }
        request
    }

    async fn unused_upstream() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    }

    #[test]
    fn config_rejects_lookalike_origins_and_public_rfb_addresses() {
        assert!(is_normalized_allowed_origin("https://app.ato.run"));
        assert!(is_normalized_allowed_origin("http://localhost:5173"));
        assert!(!is_normalized_allowed_origin(
            "http://localhost.evil.example"
        ));
        assert!(!is_normalized_allowed_origin("https://app.ato.run/path"));
        assert!(is_private_rfb_address("127.0.0.1:5900".parse().unwrap()));
        assert!(is_private_rfb_address("172.16.0.2:5900".parse().unwrap()));
        assert!(!is_private_rfb_address("8.8.8.8:5900".parse().unwrap()));
    }

    #[test]
    fn input_tracker_counts_pointer_and_keyboard_but_not_frame_requests() {
        let mut tracker = RfbClientInputTracker::new();
        assert!(!tracker.observe(b"RFB 003.008\n"));
        assert!(!tracker.observe(&[1, 1])); // security selection + ClientInit
        assert!(!tracker.observe(&[3, 1, 0, 0, 0, 0, 0, 1, 0, 1]));

        assert!(!tracker.observe(&[5, 0]));
        assert!(tracker.observe(&[0, 1, 0, 1]));
        assert!(tracker.observe(&[4, 1, 0, 0, 0, 0, 0, 65]));
        assert!(!tracker.observe(&[150, 1, 0, 0, 0, 0, 0, 1, 0, 1]));
    }

    #[tokio::test]
    async fn gateway_relays_binary_rfb_bytes_after_authorization() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = [0_u8; 5];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"hello");
            stream.write_all(b"world").await.unwrap();
        });
        let gateway = gateway_for(upstream_addr).await;
        let initial_activity = gateway.last_input_activity_unix_millis();

        let (mut websocket, response) =
            connect_async(request(gateway.local_addr(), ORIGIN_VALUE, Some("valid")))
                .await
                .unwrap();
        assert_eq!(
            response.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            "binary"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        websocket
            .send(Message::binary(b"hello".to_vec()))
            .await
            .unwrap();
        let reply = websocket.next().await.unwrap().unwrap();
        assert_eq!(reply.into_data(), b"world".as_slice());
        assert_eq!(gateway.last_input_activity_unix_millis(), initial_activity);

        gateway.stop().await.unwrap();
        upstream_task.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_rejects_missing_assertion() {
        let gateway = gateway_for(unused_upstream().await).await;
        let error = connect_async(request(gateway.local_addr(), ORIGIN_VALUE, None))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WebSocketError::Http(response) if response.status() == StatusCode::UNAUTHORIZED
        ));
        gateway.stop().await.unwrap();
    }

    #[tokio::test]
    async fn gateway_rejects_unlisted_origin() {
        let gateway = gateway_for(unused_upstream().await).await;
        let error = connect_async(request(
            gateway.local_addr(),
            "https://attacker.example",
            Some("valid"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            WebSocketError::Http(response) if response.status() == StatusCode::FORBIDDEN
        ));
        gateway.stop().await.unwrap();
    }

    #[tokio::test]
    async fn gateway_consumes_each_grant_once() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = upstream.accept().await.unwrap();
        });
        let gateway = gateway_for(upstream_addr).await;
        let (mut first, _) =
            connect_async(request(gateway.local_addr(), ORIGIN_VALUE, Some("valid")))
                .await
                .unwrap();
        first.close(None).await.unwrap();

        let error = connect_async(request(gateway.local_addr(), ORIGIN_VALUE, Some("valid")))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WebSocketError::Http(response) if response.status() == StatusCode::UNAUTHORIZED
        ));

        gateway.stop().await.unwrap();
        upstream_task.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_stop_is_idempotent_and_closes_listener() {
        let gateway = gateway_for(unused_upstream().await).await;
        let addr = gateway.local_addr();
        gateway.stop().await.unwrap();
        gateway.stop().await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(addr))
            .await
            .unwrap();
        assert!(result.is_err());
    }
}
