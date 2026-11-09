//! HTTP surface: `PUT` / `GET` / `HEAD` / `DELETE` object routes and
//! `/healthz`.
//!
//! Request bodies stream straight into [`BlobStore`] without ever being
//! buffered whole, and responses stream objects back from disk. The handler
//! never decodes an object, never inspects Capsule content, and never touches
//! key material.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, ETAG, RETRY_AFTER, WWW_AUTHENTICATE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::auth::authorized;
use crate::config::{PutAuth, ServerConfig};
use crate::rate_limit::RateLimiter;
use crate::storage::{BlobStore, ObjectKey, StorageError};
use crate::sweep;

/// Base path of the object routes.
pub const OBJECTS_PATH: &str = "/v1/capsulehub/objects/";
/// Liveness path.
pub const HEALTH_PATH: &str = "/healthz";

const READ_CHUNK_BYTES: usize = 64 * 1024;
/// Bound on how much of a request body is drained before an early error
/// response (401/400/405/413/507). Draining keeps the connection from being
/// torn down mid-request, so clients observe the HTTP error instead of a
/// connection reset. Larger bodies are not drained and the connection closes.
const DRAIN_LIMIT_BYTES: u64 = 1024 * 1024;

type HubBody = BoxBody<Bytes, std::io::Error>;

struct AppState {
    store: Arc<BlobStore>,
    config: ServerConfig,
    limiter: RateLimiter,
}

impl AppState {
    fn new(config: ServerConfig) -> Result<Self> {
        let store = BlobStore::open(
            config.storage.clone(),
            config.max_object_bytes,
            config.max_storage_bytes,
        )
        .with_context(|| format!("failed to open storage root {}", config.storage.display()))?;
        let limiter = RateLimiter::new(config.rate_limit_per_minute);
        Ok(Self {
            store: Arc::new(store),
            config,
            limiter,
        })
    }
}

/// A running coordinator.
pub struct RunningHub {
    local_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    accept_loop: tokio::task::JoinHandle<()>,
    sweeper: Option<tokio::task::JoinHandle<()>>,
}

impl RunningHub {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops accepting connections and ends the retention task.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.accept_loop.await;
        if let Some(sweeper) = self.sweeper {
            let _ = sweeper.await;
        }
    }
}

/// Binds the configured address and serves in the background.
///
/// The address must be explicit; the coordinator never chooses a bind address
/// on its own.
pub async fn start_in_background(config: ServerConfig) -> Result<RunningHub> {
    let address = config.bind;
    let state = Arc::new(AppState::new(config)?);
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    let local_addr = listener.local_addr()?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let accept_loop = tokio::spawn(run(listener, state.clone(), shutdown_rx.clone()));
    let sweeper = state.config.retention.map(|retention| {
        let store = state.store.clone();
        tokio::spawn(sweep::run_sweeper(store, retention, shutdown_rx))
    });
    info!(address = %local_addr, "ato-capsulehub listening");
    Ok(RunningHub {
        local_addr,
        shutdown,
        accept_loop,
        sweeper,
    })
}

/// Serves until SIGINT/Ctrl-C.
pub async fn serve(config: ServerConfig) -> Result<()> {
    let hub = start_in_background(config).await?;
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for shutdown signal")?;
    info!("ato-capsulehub shutting down");
    hub.shutdown().await;
    Ok(())
}

async fn run(listener: TcpListener, state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let state = state.clone();
                    let mut connection_shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |request| {
                            handle(state.clone(), peer.ip(), request)
                        });
                        let connection = http1::Builder::new().serve_connection(io, service);
                        tokio::pin!(connection);
                        tokio::select! {
                            result = &mut connection => {
                                if let Err(error) = result {
                                    debug!(%error, "capsulehub connection ended with an error");
                                }
                            }
                            _ = connection_shutdown.changed() => {}
                        }
                    });
                }
                Err(error) => {
                    warn!(%error, "capsulehub accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
    debug!("capsulehub accept loop stopped");
}

async fn handle(
    state: Arc<AppState>,
    client: IpAddr,
    request: Request<Incoming>,
) -> Result<Response<HubBody>, Infallible> {
    Ok(route(state, client, request).await)
}

async fn route(
    state: Arc<AppState>,
    client: IpAddr,
    request: Request<Incoming>,
) -> Response<HubBody> {
    let method = request.method().clone();
    let path = request.uri().path();

    if path == HEALTH_PATH {
        return match method {
            Method::GET | Method::HEAD => text(StatusCode::OK, "ok"),
            _ => {
                drain_body(request.into_body()).await;
                method_not_allowed(&[Method::GET, Method::HEAD])
            }
        };
    }

    if !state.limiter.allow(client) {
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header(RETRY_AFTER, "60")
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(full(Bytes::from_static(b"rate limit exceeded")))
            .expect("static response is valid");
    }

    let Some(segment) = object_segment(path) else {
        drain_body(request.into_body()).await;
        return not_found();
    };
    let key = match ObjectKey::parse(segment) {
        Ok(key) => key,
        Err(error) => {
            drain_body(request.into_body()).await;
            return storage_failure(StatusCode::BAD_REQUEST, &error);
        }
    };

    match method {
        Method::PUT => put(&state, request, &key).await,
        Method::GET => get(&state, request, &key, true).await,
        Method::HEAD => get(&state, request, &key, false).await,
        Method::DELETE => delete(&state, request, &key).await,
        _ => {
            drain_body(request.into_body()).await;
            method_not_allowed(&[Method::PUT, Method::GET, Method::HEAD, Method::DELETE])
        }
    }
}

/// Consumes (a bounded amount of) an unwanted request body so the client sees
/// the HTTP error rather than a connection reset.
async fn drain_body(mut body: Incoming) {
    let mut total = 0u64;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        total = total.saturating_add(data.len() as u64);
        if total >= DRAIN_LIMIT_BYTES {
            break;
        }
    }
}

fn object_segment(path: &str) -> Option<&str> {
    let segment = path.strip_prefix(OBJECTS_PATH)?;
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    Some(segment)
}

async fn put(state: &AppState, request: Request<Incoming>, key: &ObjectKey) -> Response<HubBody> {
    let upload_allowed = state.config.put_auth != PutAuth::Token
        || authorized(state.config.put_token.as_ref(), request.headers());
    if !upload_allowed {
        drain_body(request.into_body()).await;
        return unauthorized();
    }

    let body = request.into_body();
    if let Some(exact) = body.size_hint().exact() {
        if exact > state.store.max_object_bytes() {
            drain_body(body).await;
            return storage_failure(StatusCode::PAYLOAD_TOO_LARGE, &StorageError::TooLarge);
        }
        if let Some(maximum) = state.store.max_storage_bytes() {
            let (used, reserved) = state.store.usage();
            if used.saturating_add(reserved).saturating_add(exact) > maximum {
                drain_body(body).await;
                return storage_failure(
                    StatusCode::INSUFFICIENT_STORAGE,
                    &StorageError::QuotaExceeded,
                );
            }
        }
    }

    let mut upload = match state.store.begin_upload(key).await {
        Ok(upload) => upload,
        Err(error) => {
            drain_body(body).await;
            return storage_failure(status_of(&error), &error);
        }
    };

    let mut body = body;
    loop {
        match body.frame().await {
            None => break,
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data()
                    && let Err(error) = upload.write(&data).await
                {
                    return storage_failure(status_of(&error), &error);
                }
            }
            Some(Err(error)) => {
                // The client went away mid-upload; dropping `upload` removes
                // the partial file and releases its reservation.
                warn!(%error, "capsulehub upload aborted");
                return text(StatusCode::BAD_REQUEST, "upload aborted");
            }
        }
    }

    match upload.finish().await {
        Ok(stored) => {
            debug!(
                object = %key.id(),
                size = stored.size,
                created = stored.created,
                "capsulehub object stored"
            );
            let status = if stored.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            Response::builder()
                .status(status)
                .body(empty())
                .expect("static response is valid")
        }
        Err(error) => storage_failure(status_of(&error), &error),
    }
}

async fn get(
    state: &AppState,
    request: Request<Incoming>,
    key: &ObjectKey,
    include_body: bool,
) -> Response<HubBody> {
    if !state.config.allow_anonymous_read
        && !authorized(state.config.put_token.as_ref(), request.headers())
    {
        drain_body(request.into_body()).await;
        return unauthorized();
    }

    let size = match state.store.metadata(key).await {
        Ok(Some(size)) => size,
        Ok(None) => return not_found(),
        Err(error) => return storage_failure(status_of(&error), &error),
    };

    let etag = format!("\"{}\"", key.id());
    if include_body {
        let file = match state.store.open_object(key).await {
            Ok(Some(file)) => file,
            Ok(None) => return not_found(),
            Err(error) => return storage_failure(status_of(&error), &error),
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_LENGTH, size.to_string())
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(ETAG, etag)
            .body(FileBody::new(file, size).boxed())
            .expect("valid response")
    } else {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_LENGTH, size.to_string())
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(ETAG, etag)
            .body(empty())
            .expect("valid response")
    }
}

async fn delete(
    state: &AppState,
    request: Request<Incoming>,
    key: &ObjectKey,
) -> Response<HubBody> {
    if !authorized(state.config.delete_token.as_ref(), request.headers()) {
        drain_body(request.into_body()).await;
        return text(StatusCode::FORBIDDEN, "operator delete token required");
    }
    match state.store.remove(key).await {
        Ok(true) => {
            debug!(object = %key.id(), "capsulehub object removed");
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(empty())
                .expect("static response is valid")
        }
        Ok(false) => not_found(),
        Err(error) => storage_failure(status_of(&error), &error),
    }
}

fn status_of(error: &StorageError) -> StatusCode {
    match error {
        StorageError::InvalidKey | StorageError::DigestMismatch { .. } => StatusCode::BAD_REQUEST,
        StorageError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        StorageError::QuotaExceeded => StatusCode::INSUFFICIENT_STORAGE,
        StorageError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn storage_failure(status: StatusCode, error: &StorageError) -> Response<HubBody> {
    let message = match error {
        StorageError::InvalidKey => "object id must be `sha256:<64 lowercase hex>`",
        StorageError::TooLarge => "object exceeds the configured maximum size",
        StorageError::QuotaExceeded => "storage quota exceeded",
        StorageError::DigestMismatch { .. } => "uploaded bytes do not match the object id",
        StorageError::Io(io_error) => {
            // I/O errors never carry object bytes; log the kind only.
            warn!(%io_error, "capsulehub storage I/O failed");
            "storage failure"
        }
    };
    text(status, message)
}

fn not_found() -> Response<HubBody> {
    text(StatusCode::NOT_FOUND, "not found")
}

fn unauthorized() -> Response<HubBody> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(WWW_AUTHENTICATE, "Bearer")
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(Bytes::from_static(b"authentication required")))
        .expect("static response is valid")
}

fn method_not_allowed(allowed: &[Method]) -> Response<HubBody> {
    let allow = allowed
        .iter()
        .map(Method::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(ALLOW, allow)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(Bytes::from_static(b"method not allowed")))
        .expect("static response is valid")
}

fn text(status: StatusCode, message: &'static str) -> Response<HubBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(Bytes::from_static(message.as_bytes())))
        .expect("static response is valid")
}

fn empty() -> HubBody {
    full(Bytes::new())
}

fn full(bytes: Bytes) -> HubBody {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Streams an object file to the client without buffering it whole.
struct FileBody {
    file: tokio::fs::File,
    remaining: u64,
    finished: bool,
}

impl FileBody {
    fn new(file: tokio::fs::File, len: u64) -> Self {
        Self {
            file,
            remaining: len,
            finished: false,
        }
    }
}

impl Body for FileBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        let this = self.get_mut();
        if this.finished || this.remaining == 0 {
            return Poll::Ready(None);
        }
        let chunk = usize::try_from(this.remaining.min(READ_CHUNK_BYTES as u64))
            .unwrap_or(READ_CHUNK_BYTES);
        let mut buffer = vec![0u8; chunk];
        let mut read_buffer = ReadBuf::new(&mut buffer);
        match Pin::new(&mut this.file).poll_read(cx, &mut read_buffer) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                this.finished = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Ok(())) => {
                let filled = read_buffer.filled().len();
                if filled == 0 {
                    // The file is shorter than the recorded metadata; end the
                    // stream rather than serving uninitialized bytes.
                    this.finished = true;
                    Poll::Ready(None)
                } else {
                    this.remaining = this.remaining.saturating_sub(filled as u64);
                    buffer.truncate(filled);
                    Poll::Ready(Some(Ok(Frame::data(Bytes::from(buffer)))))
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        if self.finished {
            SizeHint::default()
        } else {
            SizeHint::with_exact(self.remaining)
        }
    }
}
