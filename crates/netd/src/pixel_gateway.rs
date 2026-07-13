//! Authenticated RFB-over-WebSocket adapter for pixel-stream surfaces.
//!
//! The gateway is deliberately separate from the regular HTTP ingress. It
//! accepts a browser-facing WebSocket only after a session-bound assertion has
//! been authorized, then relays binary messages to a private RFB TCP endpoint.

use std::{
    collections::{BTreeSet, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use futures_util::{SinkExt, StreamExt};
use http::{
    HeaderValue, StatusCode,
    header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{Callback, ErrorResponse, Request, Response},
    },
};

/// Header carrying the API-to-runner assertion. Browser credentials are never
/// forwarded to the private RFB endpoint.
pub const SURFACE_ASSERTION_HEADER: &str = "x-ato-surface-assertion";

const RFB_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CLIENT_MESSAGE_BYTES: usize = 1024 * 1024;
const OUTBOUND_QUEUE_DEPTH: usize = 8;

/// Immutable identity used to scope every access grant accepted by a gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelGatewayScope {
    pub session_id: String,
    pub surface_id: String,
}

/// Result of validating a session-bound internal assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSurfaceAccess {
    pub principal: String,
    /// Unique assertion identifier (`jti`). A gateway consumes it once.
    pub grant_id: String,
}

/// Intentionally opaque authorization failure so assertion contents never
/// appear in logs or client responses.
#[derive(Debug, Clone, Copy, Error)]
#[error("surface assertion rejected")]
pub struct SurfaceAuthorizationError;

/// Boundary implemented by the runner's assertion verifier.
pub trait SurfaceAccessAuthorizer: Send + Sync + 'static {
    fn authorize(
        &self,
        assertion: &str,
        scope: &PixelGatewayScope,
    ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError>;
}

/// Configuration for one session-owned pixel gateway.
#[derive(Debug, Clone)]
pub struct PixelGatewayConfig {
    pub listen_addr: SocketAddr,
    pub private_rfb_addr: SocketAddr,
    pub scope: PixelGatewayScope,
    pub allowed_origins: BTreeSet<String>,
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
        if self.allowed_origins.iter().any(|origin| {
            !(origin.starts_with("https://") || origin.starts_with("http://localhost"))
        }) {
            return Err(PixelGatewayError::InvalidConfig(
                "origins must use https, except localhost development origins",
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
}

impl PixelGatewayHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
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
    let task = tokio::spawn(run_gateway(listener, config, authorizer, cancel_rx));

    Ok(PixelGatewayHandle {
        local_addr,
        cancel_tx,
        task: AsyncMutex::new(Some(task)),
    })
}

async fn run_gateway(
    listener: TcpListener,
    config: PixelGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<(), PixelGatewayError> {
    let consumed_grants = Arc::new(Mutex::new(HashSet::<String>::new()));
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
                connections.spawn(async move {
                    if let Err(error) = serve_pixel_connection(
                        stream,
                        config,
                        authorizer,
                        consumed_grants,
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
    consumed_grants: Arc<Mutex<HashSet<String>>>,
) -> Result<(), PixelGatewayError> {
    let callback = HandshakeAuthorizer {
        allowed_origins: config.allowed_origins.clone(),
        scope: config.scope.clone(),
        authorizer,
        consumed_grants,
    };
    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_CLIENT_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_CLIENT_MESSAGE_BYTES));
    let websocket = accept_hdr_async_with_config(stream, callback, Some(ws_config)).await?;
    let rfb = TcpStream::connect(config.private_rfb_addr)
        .await
        .map_err(PixelGatewayError::RfbConnect)?;

    relay_rfb(websocket, rfb).await
}

struct HandshakeAuthorizer {
    allowed_origins: BTreeSet<String>,
    scope: PixelGatewayScope,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    consumed_grants: Arc<Mutex<HashSet<String>>>,
}

impl Callback for HandshakeAuthorizer {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        authorize_upgrade(
            request,
            &self.allowed_origins,
            &self.scope,
            self.authorizer.as_ref(),
            &self.consumed_grants,
        )
        .map_err(UpgradeRejection::into_response)?;

        if offered_binary_subprotocol(request) {
            response
                .headers_mut()
                .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("binary"));
        }
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy)]
enum UpgradeRejection {
    Unauthorized,
    Forbidden,
    Internal,
}

impl UpgradeRejection {
    fn into_response(self) -> ErrorResponse {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        rejection(status)
    }
}

fn authorize_upgrade(
    request: &Request,
    allowed_origins: &BTreeSet<String>,
    scope: &PixelGatewayScope,
    authorizer: &dyn SurfaceAccessAuthorizer,
    consumed_grants: &Mutex<HashSet<String>>,
) -> Result<(), UpgradeRejection> {
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(UpgradeRejection::Forbidden)?;
    if !allowed_origins.contains(origin) {
        return Err(UpgradeRejection::Forbidden);
    }

    let assertion = request
        .headers()
        .get(SURFACE_ASSERTION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(UpgradeRejection::Unauthorized)?;
    let access = authorizer
        .authorize(assertion, scope)
        .map_err(|_| UpgradeRejection::Unauthorized)?;
    if access.grant_id.trim().is_empty() || access.principal.trim().is_empty() {
        return Err(UpgradeRejection::Unauthorized);
    }

    let mut consumed = consumed_grants
        .lock()
        .map_err(|_| UpgradeRejection::Internal)?;
    if !consumed.insert(access.grant_id) {
        return Err(UpgradeRejection::Unauthorized);
    }
    Ok(())
}

fn offered_binary_subprotocol(request: &Request) -> bool {
    request
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|part| part.trim() == "binary"))
}

fn rejection(status: StatusCode) -> ErrorResponse {
    http::Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .body(None)
        .unwrap_or_else(|_| http::Response::new(None))
}

async fn relay_rfb<S>(
    websocket: tokio_tungstenite::WebSocketStream<S>,
    rfb: TcpStream,
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
        while let Some(message) = ws_stream.next().await {
            match message? {
                Message::Binary(bytes) => rfb_write.write_all(&bytes).await?,
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
    use http::HeaderValue;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Error as WebSocketError, Message, client::IntoClientRequest},
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

        let (mut websocket, response) =
            connect_async(request(gateway.local_addr(), ORIGIN_VALUE, Some("valid")))
                .await
                .unwrap();
        assert_eq!(
            response.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            "binary"
        );
        websocket
            .send(Message::binary(b"hello".to_vec()))
            .await
            .unwrap();
        let reply = websocket.next().await.unwrap().unwrap();
        assert_eq!(reply.into_data(), b"world".as_slice());

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
