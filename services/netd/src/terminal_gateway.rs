//! Authenticated Terminal WebSocket gateway to the selected Firecracker guest.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ato_ipc::terminal_surface::{
    MAX_TERMINAL_CONTROL_FRAME_BYTES, MAX_TERMINAL_INPUT_FRAME_BYTES,
    MAX_TERMINAL_OUTPUT_CHUNK_BYTES, MAX_UNACKED_TERMINAL_OUTPUT_BYTES,
    TERMINAL_WEBSOCKET_SUBPROTOCOL, TerminalClientControl,
};
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::surface_authorization::{SurfaceAccessAuthorizer, SurfaceGatewayScope};
use crate::surface_websocket_auth::{
    ConsumedSurfaceGrants, SurfaceHandshakeAuthorizer, is_normalized_allowed_origin,
    new_consumed_surface_grants,
};

pub const GUEST_TERMINAL_VSOCK_PORT: u32 = 1026;
const FRAME_INPUT: u8 = 1;
const FRAME_OUTPUT: u8 = 2;
const FRAME_CONTROL: u8 = 3;
const OUTBOUND_QUEUE_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub struct TerminalGatewayConfig {
    pub listen_addr: std::net::SocketAddr,
    pub firecracker_vsock_uds: PathBuf,
    pub guest_connect_timeout: Duration,
    pub scope: SurfaceGatewayScope,
    pub allowed_origins: BTreeSet<String>,
}

impl TerminalGatewayConfig {
    fn validate(&self) -> Result<(), TerminalGatewayError> {
        if self.scope.session_id.trim().is_empty() || self.scope.surface_id.trim().is_empty() {
            return Err(TerminalGatewayError::InvalidConfig(
                "session_id and surface_id must not be empty",
            ));
        }
        if self.allowed_origins.is_empty()
            || self
                .allowed_origins
                .iter()
                .any(|origin| !is_normalized_allowed_origin(origin))
        {
            return Err(TerminalGatewayError::InvalidConfig(
                "an exact normalized HTTPS origin is required",
            ));
        }
        if !self.firecracker_vsock_uds.is_absolute() {
            return Err(TerminalGatewayError::InvalidConfig(
                "Firecracker vsock UDS path must be absolute",
            ));
        }
        if self.guest_connect_timeout.is_zero() {
            return Err(TerminalGatewayError::InvalidConfig(
                "guest connect timeout must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TerminalGatewayError {
    #[error("invalid terminal gateway configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to bind terminal gateway: {0}")]
    Bind(#[source] std::io::Error),
    #[error("terminal gateway listener failed: {0}")]
    Accept(#[source] std::io::Error),
    #[error("terminal gateway WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("terminal guest connection failed: {0}")]
    GuestConnect(#[source] std::io::Error),
    #[error("terminal gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("terminal gateway task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub struct TerminalGatewayHandle {
    local_addr: std::net::SocketAddr,
    cancel_tx: watch::Sender<bool>,
    task: AsyncMutex<Option<JoinHandle<Result<(), TerminalGatewayError>>>>,
    last_input_activity_unix_millis: Arc<AtomicU64>,
}

impl TerminalGatewayHandle {
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.local_addr
    }

    pub fn last_input_activity_unix_millis(&self) -> u64 {
        self.last_input_activity_unix_millis.load(Ordering::Relaxed)
    }

    pub async fn stop(&self) -> Result<(), TerminalGatewayError> {
        let _ = self.cancel_tx.send(true);
        let mut task = self.task.lock().await;
        if let Some(task) = task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for TerminalGatewayHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(true);
    }
}

pub async fn start_terminal_gateway(
    config: TerminalGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
) -> Result<TerminalGatewayHandle, TerminalGatewayError> {
    config.validate()?;
    let listener = TcpListener::bind(config.listen_addr)
        .await
        .map_err(TerminalGatewayError::Bind)?;
    let local_addr = listener.local_addr().map_err(TerminalGatewayError::Bind)?;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let last_input_activity_unix_millis = Arc::new(AtomicU64::new(now_unix_millis()));
    let task = tokio::spawn(run_gateway(
        listener,
        config,
        authorizer,
        cancel_rx,
        Arc::clone(&last_input_activity_unix_millis),
    ));
    Ok(TerminalGatewayHandle {
        local_addr,
        cancel_tx,
        task: AsyncMutex::new(Some(task)),
        last_input_activity_unix_millis,
    })
}

async fn run_gateway(
    listener: TcpListener,
    config: TerminalGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    mut cancel_rx: watch::Receiver<bool>,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), TerminalGatewayError> {
    let consumed_grants = new_consumed_surface_grants();
    let active_viewer = Arc::new(AtomicBool::new(false));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_err() || *cancel_rx.borrow() { break; }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(TerminalGatewayError::Accept)?;
                let config = config.clone();
                let authorizer = Arc::clone(&authorizer);
                let consumed_grants = Arc::clone(&consumed_grants);
                let active_viewer = Arc::clone(&active_viewer);
                let activity = Arc::clone(&last_input_activity_unix_millis);
                connections.spawn(async move {
                    if let Err(error) = serve_terminal_connection(
                        stream, config, authorizer, consumed_grants, active_viewer, activity,
                    ).await {
                        tracing::debug!(%peer, %error, "terminal gateway connection closed");
                    }
                });
            }
        }
    }
    connections.shutdown().await;
    Ok(())
}

struct ActiveViewerGuard(Arc<AtomicBool>);

impl ActiveViewerGuard {
    fn acquire(active: Arc<AtomicBool>) -> Result<Self, TerminalGatewayError> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                TerminalGatewayError::InvalidConfig("terminal viewer already connected")
            })?;
        Ok(Self(active))
    }
}

impl Drop for ActiveViewerGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

async fn serve_terminal_connection(
    stream: TcpStream,
    config: TerminalGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    consumed_grants: ConsumedSurfaceGrants,
    active_viewer: Arc<AtomicBool>,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), TerminalGatewayError> {
    let callback = SurfaceHandshakeAuthorizer::new(
        config.allowed_origins.clone(),
        config.scope.clone(),
        authorizer,
        consumed_grants,
        TERMINAL_WEBSOCKET_SUBPROTOCOL,
        true,
    );
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_TERMINAL_INPUT_FRAME_BYTES))
        .max_frame_size(Some(MAX_TERMINAL_INPUT_FRAME_BYTES));
    let websocket = accept_hdr_async_with_config(stream, callback, Some(ws_config)).await?;
    let _viewer = ActiveViewerGuard::acquire(active_viewer)?;
    let guest = connect_guest_terminal(&config.firecracker_vsock_uds, config.guest_connect_timeout)
        .await
        .map_err(TerminalGatewayError::GuestConnect)?;
    relay_terminal(websocket, guest, last_input_activity_unix_millis).await
}

async fn connect_guest_terminal(
    path: &std::path::Path,
    budget: Duration,
) -> std::io::Result<UnixStream> {
    timeout(budget, async {
        let mut stream = UnixStream::connect(path).await?;
        stream
            .write_all(format!("CONNECT {GUEST_TERMINAL_VSOCK_PORT}\n").as_bytes())
            .await?;
        stream.flush().await?;
        let mut line = Vec::with_capacity(16);
        while line.len() <= 128 {
            let byte = stream.read_u8().await?;
            line.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        if line.len() > 128 || !line.starts_with(b"OK") || !line.ends_with(b"\n") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "Firecracker rejected terminal vsock connection",
            ));
        }
        Ok(stream)
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "terminal guest connect timed out",
        )
    })?
}

async fn relay_terminal<S>(
    websocket: tokio_tungstenite::WebSocketStream<S>,
    guest: UnixStream,
    last_input_activity_unix_millis: Arc<AtomicU64>,
) -> Result<(), TerminalGatewayError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_stream) = websocket.split();
    let (mut guest_read, mut guest_write) = guest.into_split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_DEPTH);
    let outstanding = Arc::new(AtomicUsize::new(0));
    let acked = Arc::new(Notify::new());

    let writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            ws_sink.send(message).await?;
        }
        ws_sink.close().await
    });

    let client_outstanding = Arc::clone(&outstanding);
    let client_acked = Arc::clone(&acked);
    let client_to_guest = async {
        while let Some(message) = ws_stream.next().await {
            match message? {
                Message::Binary(bytes) if bytes.len() <= MAX_TERMINAL_INPUT_FRAME_BYTES => {
                    write_guest_frame(&mut guest_write, FRAME_INPUT, &bytes).await?;
                    last_input_activity_unix_millis.store(now_unix_millis(), Ordering::Relaxed);
                }
                Message::Text(text) if text.len() <= MAX_TERMINAL_CONTROL_FRAME_BYTES => {
                    let control: TerminalClientControl = serde_json::from_str(text.as_ref())
                        .map_err(|_| {
                            TerminalGatewayError::InvalidConfig("invalid terminal control frame")
                        })?;
                    control.validate().map_err(|_| {
                        TerminalGatewayError::InvalidConfig("invalid terminal control frame")
                    })?;
                    match control {
                        TerminalClientControl::Ack { bytes } => {
                            let bytes = usize::try_from(bytes).map_err(|_| {
                                TerminalGatewayError::InvalidConfig(
                                    "terminal ack exceeds platform size",
                                )
                            })?;
                            client_outstanding
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                                    (bytes <= current).then_some(current - bytes)
                                })
                                .map_err(|_| {
                                    TerminalGatewayError::InvalidConfig(
                                        "terminal ack exceeds outstanding output",
                                    )
                                })?;
                            client_acked.notify_waiters();
                        }
                        resize @ TerminalClientControl::Resize { .. } => {
                            let payload =
                                serde_json::to_vec(&resize).map_err(std::io::Error::other)?;
                            write_guest_frame(&mut guest_write, FRAME_CONTROL, &payload).await?;
                        }
                    }
                }
                Message::Ping(bytes) => {
                    if outbound_tx.send(Message::Pong(bytes)).await.is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => {
                    return Err(TerminalGatewayError::InvalidConfig(
                        "invalid terminal WebSocket frame",
                    ));
                }
            }
        }
        Ok::<(), TerminalGatewayError>(())
    };

    let server_outstanding = Arc::clone(&outstanding);
    let server_acked = Arc::clone(&acked);
    let server_to_client = async {
        loop {
            let (kind, payload) = read_guest_frame(&mut guest_read).await?;
            let message = match kind {
                FRAME_OUTPUT if payload.len() <= MAX_TERMINAL_OUTPUT_CHUNK_BYTES => {
                    while server_outstanding.load(Ordering::Acquire) + payload.len()
                        > MAX_UNACKED_TERMINAL_OUTPUT_BYTES
                    {
                        server_acked.notified().await;
                    }
                    server_outstanding.fetch_add(payload.len(), Ordering::AcqRel);
                    Message::binary(payload)
                }
                FRAME_CONTROL if payload.len() <= MAX_TERMINAL_CONTROL_FRAME_BYTES => {
                    let text = String::from_utf8(payload).map_err(|_| {
                        TerminalGatewayError::InvalidConfig("guest terminal control is not UTF-8")
                    })?;
                    Message::text(text)
                }
                _ => {
                    return Err(TerminalGatewayError::InvalidConfig(
                        "invalid guest terminal frame",
                    ));
                }
            };
            if outbound_tx.send(message).await.is_err() {
                break;
            }
        }
        Ok::<(), TerminalGatewayError>(())
    };

    let result = tokio::select! {
        result = client_to_guest => result,
        result = server_to_client => result,
    };
    drop(outbound_tx);
    let writer_result = writer.await?;
    result?;
    writer_result.map_err(TerminalGatewayError::WebSocket)
}

async fn write_guest_frame(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    kind: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    writer.write_u8(kind).await?;
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

async fn read_guest_frame(
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> Result<(u8, Vec<u8>), TerminalGatewayError> {
    let kind = reader.read_u8().await?;
    let length = reader.read_u32().await? as usize;
    if length > MAX_TERMINAL_OUTPUT_CHUNK_BYTES.max(MAX_TERMINAL_CONTROL_FRAME_BYTES) {
        return Err(TerminalGatewayError::InvalidConfig(
            "guest terminal frame exceeds v1 limit",
        ));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    Ok((kind, payload))
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use http::{
        HeaderValue,
        header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
    };
    use tokio::net::UnixListener;
    use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

    use super::*;
    use crate::surface_authorization::{AuthorizedSurfaceAccess, SurfaceAuthorizationError};
    use crate::surface_websocket_auth::SURFACE_ASSERTION_HEADER;

    const ORIGIN_VALUE: &str = "https://app.ato.run";

    struct TestAuthorizer {
        seen: Mutex<Vec<String>>,
    }

    impl SurfaceAccessAuthorizer for TestAuthorizer {
        fn authorize(
            &self,
            assertion: &str,
            scope: &SurfaceGatewayScope,
        ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError> {
            if scope.session_id != "session-1" || scope.surface_id != "surface-1" {
                return Err(SurfaceAuthorizationError);
            }
            self.seen.lock().unwrap().push(assertion.to_string());
            Ok(AuthorizedSurfaceAccess {
                principal: "user-1".into(),
                grant_id: assertion.to_string(),
            })
        }
    }

    fn request(
        addr: std::net::SocketAddr,
        assertion: &str,
        subprotocol: &str,
    ) -> http::Request<()> {
        let mut request = format!("ws://{addr}/surface")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_static(ORIGIN_VALUE));
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(subprotocol).unwrap(),
        );
        request.headers_mut().insert(
            SURFACE_ASSERTION_HEADER,
            HeaderValue::from_str(assertion).unwrap(),
        );
        request
    }

    async fn write_fake_frame(stream: &mut UnixStream, kind: u8, payload: &[u8]) {
        stream.write_u8(kind).await.unwrap();
        stream.write_u32(payload.len() as u32).await.unwrap();
        stream.write_all(payload).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_fake_frame(stream: &mut UnixStream) -> (u8, Vec<u8>) {
        let kind = stream.read_u8().await.unwrap();
        let length = stream.read_u32().await.unwrap() as usize;
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).await.unwrap();
        (kind, payload)
    }

    #[tokio::test]
    async fn gateway_authenticates_and_relays_only_the_terminal_vsock_protocol() {
        std::fs::create_dir_all(".tmp").unwrap();
        let temp = tempfile::Builder::new()
            .prefix("tg-")
            .tempdir_in(".tmp")
            .unwrap();
        let uds_path = temp.path().join("vsock.sock");
        let listener = UnixListener::bind(&uds_path).unwrap();
        let guest = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = stream;
            let mut connect = Vec::new();
            loop {
                let byte = stream.read_u8().await.unwrap();
                connect.push(byte);
                if byte == b'\n' {
                    break;
                }
            }
            assert_eq!(connect, b"CONNECT 1026\n");
            stream.write_all(b"OK 1234\n").await.unwrap();
            write_fake_frame(
                &mut stream,
                FRAME_CONTROL,
                br#"{"type":"ready","cols":80,"rows":24}"#,
            )
            .await;
            let input = read_fake_frame(&mut stream).await;
            assert_eq!(input, (FRAME_INPUT, b"hello".to_vec()));
            let resize = read_fake_frame(&mut stream).await;
            assert_eq!(resize.0, FRAME_CONTROL);
            let resize: TerminalClientControl = serde_json::from_slice(&resize.1).unwrap();
            assert_eq!(
                resize,
                TerminalClientControl::Resize {
                    cols: 120,
                    rows: 40
                }
            );
            write_fake_frame(&mut stream, FRAME_OUTPUT, b"world").await;
        });

        let gateway = start_terminal_gateway(
            TerminalGatewayConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                firecracker_vsock_uds: std::fs::canonicalize(&uds_path).unwrap(),
                guest_connect_timeout: Duration::from_secs(1),
                scope: SurfaceGatewayScope {
                    session_id: "session-1".into(),
                    surface_id: "surface-1".into(),
                },
                allowed_origins: BTreeSet::from([ORIGIN_VALUE.into()]),
            },
            Arc::new(TestAuthorizer {
                seen: Mutex::new(Vec::new()),
            }),
        )
        .await
        .unwrap();

        let (mut websocket, response) = connect_async(request(
            gateway.local_addr(),
            "grant-1",
            TERMINAL_WEBSOCKET_SUBPROTOCOL,
        ))
        .await
        .unwrap();
        assert_eq!(
            response.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            TERMINAL_WEBSOCKET_SUBPROTOCOL
        );
        let ready = tokio::time::timeout(Duration::from_secs(2), websocket.next())
            .await
            .expect("ready frame timeout")
            .unwrap()
            .unwrap();
        assert!(matches!(ready, Message::Text(text) if text.contains("\"ready\"")));
        websocket
            .send(Message::binary(b"hello".as_slice()))
            .await
            .unwrap();
        websocket
            .send(Message::text(r#"{"type":"resize","cols":120,"rows":40}"#))
            .await
            .unwrap();
        let output = tokio::time::timeout(Duration::from_secs(2), websocket.next())
            .await
            .expect("output frame timeout")
            .unwrap()
            .unwrap();
        assert_eq!(output.into_data().as_ref(), b"world");
        websocket
            .send(Message::text(r#"{"type":"ack","bytes":5}"#))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), guest)
            .await
            .expect("fake guest timeout")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), gateway.stop())
            .await
            .expect("gateway stop timeout")
            .unwrap();
    }

    #[tokio::test]
    async fn gateway_requires_the_terminal_subprotocol_before_guest_connect() {
        std::fs::create_dir_all(".tmp").unwrap();
        let temp = tempfile::Builder::new()
            .prefix("tgs-")
            .tempdir_in(".tmp")
            .unwrap();
        let uds_path = temp.path().join("vsock.sock");
        let _listener = UnixListener::bind(&uds_path).unwrap();
        let gateway = start_terminal_gateway(
            TerminalGatewayConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                firecracker_vsock_uds: std::fs::canonicalize(&uds_path).unwrap(),
                guest_connect_timeout: Duration::from_millis(100),
                scope: SurfaceGatewayScope {
                    session_id: "session-1".into(),
                    surface_id: "surface-1".into(),
                },
                allowed_origins: BTreeSet::from([ORIGIN_VALUE.into()]),
            },
            Arc::new(TestAuthorizer {
                seen: Mutex::new(Vec::new()),
            }),
        )
        .await
        .unwrap();
        assert!(
            connect_async(request(gateway.local_addr(), "grant-2", "binary"))
                .await
                .is_err()
        );
        gateway.stop().await.unwrap();
    }
}
