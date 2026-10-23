#![cfg(unix)]

//! Runner-owned native Adapter observations, distinct from Surface readiness.
//!
//! Authored bounded actions run once before publication. HTTP only retrieves
//! frozen measured bytes; neither expected digests nor K verdicts enter here.

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use ato_formation::native_surface::{
    NativePresentation, NativeProbeOperation, NativeSurfaceAdapter, pixel_probe_result,
};
use ato_ipc::terminal_surface::{TerminalClientControl, TerminalServerControl};
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinSet, time::timeout};

use netd::terminal_gateway::{
    FRAME_CONTROL, FRAME_INPUT, FRAME_OUTPUT, connect_guest_terminal, read_guest_frame,
    write_guest_frame,
};

/// Resolved from owned workload handles. Never serialized into a D or K.
pub enum NativeProbeTarget {
    Terminal(PathBuf),
    Pixel(SocketAddr),
}

pub struct NativeProbeHandle {
    local_addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl NativeProbeHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take() {
            match timeout(Duration::from_secs(3), &mut task).await {
                Ok(result) => result?,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    anyhow::bail!("native observation listener cleanup exceeded its bound");
                }
            }
        }
        Ok(())
    }
}

impl Drop for NativeProbeHandle {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(task) = self.task.take() {
            // The listener task owns a JoinSet: cancellation drops it and
            // aborts every in-flight response as well as closing its socket.
            task.abort();
        }
    }
}

/// The caller must expose this endpoint ONLY through its existing scoped
/// observation ingress. It is never the public Surface or a readiness port.
pub async fn capture_and_start_native_probe(
    listen_addr: SocketAddr,
    target: NativeProbeTarget,
    adapter: &NativeSurfaceAdapter,
) -> anyhow::Result<NativeProbeHandle> {
    adapter.validate_catalog().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        listen_addr.ip().is_loopback(),
        "native observation listener must be private loopback"
    );
    let compatible = matches!(
        (&target, &adapter.surface),
        (
            NativeProbeTarget::Terminal(_),
            NativePresentation::Terminal { .. }
        ) | (
            NativeProbeTarget::Pixel(_),
            NativePresentation::PixelStream { .. }
        )
    );
    anyhow::ensure!(
        compatible,
        "native observation target does not match declared Adapter"
    );
    if let NativeProbeTarget::Terminal(path) = &target {
        anyhow::ensure!(
            path.is_absolute(),
            "native terminal endpoint must be absolute"
        );
    }
    if let NativeProbeTarget::Pixel(addr) = &target {
        anyhow::ensure!(
            addr.ip().is_loopback() && addr.port() != 0,
            "native pixel endpoint must be private loopback"
        );
    }
    let mut measured = BTreeMap::new();
    for operation in &adapter.operations {
        let wait = Duration::from_millis(operation.timeout_ms());
        let bytes = match (&target, &adapter.surface, operation) {
            (
                NativeProbeTarget::Terminal(path),
                NativePresentation::Terminal { cols, rows, .. },
                NativeProbeOperation::TerminalExchange {
                    input,
                    output_delimiter,
                    max_output_bytes,
                    ..
                },
            ) => timeout(
                wait,
                capture_terminal(
                    path,
                    *cols,
                    *rows,
                    input,
                    output_delimiter,
                    *max_output_bytes as usize,
                    wait,
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("native terminal observation timed out"))??,
            (
                NativeProbeTarget::Pixel(addr),
                NativePresentation::PixelStream { width, height, .. },
                NativeProbeOperation::PixelKeyFrame {
                    keysyms,
                    max_frame_bytes,
                    ..
                },
            ) => {
                let frame = netd::rfb_probe::capture_private_rfb_frame(
                    *addr,
                    *width,
                    *height,
                    keysyms,
                    *max_frame_bytes as usize,
                    wait,
                )
                .await?;
                // A measured full-frame fingerprint keeps the existing HTTP
                // observation limits intact even for large private displays.
                // The expected fingerprint remains exclusively in K.
                pixel_probe_result(&frame).map_err(anyhow::Error::msg)?
            }
            _ => anyhow::bail!("native observation operation does not match its target"),
        };
        measured.insert(operation.path().to_owned(), Bytes::from(bytes));
    }
    // No listener is published when even one authored operation fails.
    start_frozen_listener(listen_addr, measured).await
}

async fn capture_terminal(
    path: &std::path::Path,
    cols: u16,
    rows: u16,
    input: &[u8],
    delimiter: &[u8],
    max_bytes: usize,
    wait: Duration,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = connect_guest_terminal(path, wait).await?;
    let (kind, bytes) = read_guest_frame(&mut stream).await?;
    anyhow::ensure!(
        kind == FRAME_CONTROL,
        "native terminal observation requires Ready control"
    );
    let ready: TerminalServerControl = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid native terminal observation Ready control"))?;
    ready.validate()?;
    anyhow::ensure!(
        matches!(ready, TerminalServerControl::Ready { .. }),
        "native terminal observation is not ready"
    );
    let resize = serde_json::to_vec(&TerminalClientControl::Resize { cols, rows })?;
    write_guest_frame(&mut stream, FRAME_CONTROL, &resize).await?;
    // Retain startup output as evidence, but never let a completion marker
    // already waiting before input stand in for this operation's response.
    // Wait for readability, rather than cancelling a partially read frame.
    let mut result = Vec::new();
    while timeout(Duration::from_millis(20), stream.readable())
        .await
        .is_ok()
    {
        // Clear stale readiness after the Ready frame without blocking on a
        // whole new frame before input is sent. Once a byte is consumed, keep
        // the frame read intact; only the outer operation deadline cancels it.
        let mut first = [0_u8; 1];
        match stream.try_read(&mut first) {
            Ok(0) => anyhow::bail!("native terminal closed before input"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        }
        let mut framed = std::io::Cursor::new(first).chain(&mut stream);
        let (kind, bytes) = read_guest_frame(&mut framed).await?;
        anyhow::ensure!(
            kind == FRAME_OUTPUT && !bytes.is_empty(),
            "native terminal observation ended before input"
        );
        anyhow::ensure!(
            result.len().saturating_add(bytes.len()) <= max_bytes,
            "native terminal observation exceeded output limit"
        );
        result.extend_from_slice(&bytes);
        anyhow::ensure!(
            !result
                .windows(delimiter.len())
                .any(|window| window == delimiter),
            "native terminal completion was already present before input"
        );
    }
    write_guest_frame(&mut stream, FRAME_INPUT, input).await?;
    loop {
        let (kind, bytes) = read_guest_frame(&mut stream).await?;
        anyhow::ensure!(
            kind == FRAME_OUTPUT && !bytes.is_empty(),
            "native terminal observation ended before completion"
        );
        anyhow::ensure!(
            result.len().saturating_add(bytes.len()) <= max_bytes,
            "native terminal observation exceeded output limit"
        );
        result.extend_from_slice(&bytes);
        if let Some(position) = result
            .windows(delimiter.len())
            .position(|window| window == delimiter)
        {
            anyhow::ensure!(
                position + delimiter.len() == result.len(),
                "native terminal observation has trailing output"
            );
            return Ok(result);
        }
    }
}

async fn start_frozen_listener(
    listen_addr: SocketAddr,
    measured: BTreeMap<String, Bytes>,
) -> anyhow::Result<NativeProbeHandle> {
    let listener = TcpListener::bind(listen_addr).await?;
    let local_addr = listener.local_addr()?;
    let measured = Arc::new(measured);
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                Some(_) = clients.join_next(), if !clients.is_empty() => {},
                result = listener.accept(), if clients.len() < 32 => {
                    let Ok((stream, _)) = result else { break; };
                    let measured = Arc::clone(&measured);
                    clients.spawn(async move {
                        let service = service_fn(move |request| {
                            let response = frozen_response(request, &measured);
                            async move { Ok::<_, std::convert::Infallible>(response) }
                        });
                        let mut builder = http1::Builder::new();
                        builder.keep_alive(false).max_headers(32).max_buf_size(8192);
                        let _ = timeout(Duration::from_secs(2), builder.serve_connection(TokioIo::new(stream), service)).await;
                    });
                }
            }
        }
        drop(listener);
        clients.abort_all();
        while clients.join_next().await.is_some() {}
    });
    Ok(NativeProbeHandle {
        local_addr,
        stop: Some(stop),
        task: Some(task),
    })
}

fn frozen_response(
    request: Request<Incoming>,
    measured: &BTreeMap<String, Bytes>,
) -> Response<Full<Bytes>> {
    let forbidden_body = request
        .headers()
        .contains_key(http::header::TRANSFER_ENCODING)
        || request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .is_some_and(|value| value != "0");
    let (status, body) = if request.method() != Method::GET {
        (StatusCode::METHOD_NOT_ALLOWED, Bytes::new())
    } else if request.uri().query().is_some() || forbidden_body {
        (StatusCode::BAD_REQUEST, Bytes::new())
    } else if let Some(bytes) = measured.get(request.uri().path()) {
        (StatusCode::OK, bytes.clone())
    } else {
        (StatusCode::NOT_FOUND, Bytes::new())
    };
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .header(http::header::CACHE_CONTROL, "no-store")
        .header(http::header::CONNECTION, "close")
        .body(Full::new(body))
        .expect("fixed native observation response headers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpStream, UnixListener},
    };

    fn adapter() -> NativeSurfaceAdapter {
        NativeSurfaceAdapter {
            surface: NativePresentation::Terminal {
                port: "terminal".into(),
                cols: 100,
                rows: 30,
            },
            probe_port: "native-results".into(),
            operations: vec![NativeProbeOperation::TerminalExchange {
                path: "/result".into(),
                input: b"compute\n".to_vec(),
                output_delimiter: b"END\r\n".to_vec(),
                max_output_bytes: 128,
                timeout_ms: 1000,
            }],
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        path: PathBuf,
        task: tokio::task::JoinHandle<()>,
        actions: Arc<AtomicUsize>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn fixture(bytes: Vec<u8>, kind: u8, delay: Duration) -> Fixture {
        fixture_with_early_output(bytes, kind, delay, false)
    }

    fn fixture_with_early_output(
        bytes: Vec<u8>,
        kind: u8,
        delay: Duration,
        early: bool,
    ) -> Fixture {
        let cwd = std::env::current_dir().unwrap();
        let parent = cwd
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == ".tmp"))
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join(".tmp"));
        std::fs::create_dir_all(&parent).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("np-")
            .tempdir_in(parent)
            .unwrap();
        let path = directory.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        let actions = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&actions);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handshake = [0_u8; 13];
            stream.read_exact(&mut handshake).await.unwrap();
            assert_eq!(&handshake, b"CONNECT 1026\n");
            stream.write_all(b"OK 1026\n").await.unwrap();
            write_guest_frame(
                &mut stream,
                FRAME_CONTROL,
                br#"{"type":"ready","cols":80,"rows":24}"#,
            )
            .await
            .unwrap();
            if early {
                write_guest_frame(&mut stream, FRAME_OUTPUT, &bytes)
                    .await
                    .unwrap();
            }
            let (resize_kind, resize) = read_guest_frame(&mut stream).await.unwrap();
            assert_eq!(resize_kind, FRAME_CONTROL);
            assert_eq!(
                serde_json::from_slice::<TerminalClientControl>(&resize).unwrap(),
                TerminalClientControl::Resize {
                    cols: 100,
                    rows: 30
                }
            );
            if early {
                assert_eq!(
                    stream.read_u8().await.unwrap_err().kind(),
                    std::io::ErrorKind::UnexpectedEof
                );
                return;
            }
            let (input_kind, input) = read_guest_frame(&mut stream).await.unwrap();
            assert_eq!(input_kind, FRAME_INPUT);
            assert_eq!(&input, b"compute\n");
            count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            let _ = write_guest_frame(&mut stream, kind, &bytes).await;
        });
        Fixture {
            _directory: directory,
            path,
            task,
            actions,
        }
    }

    async fn request(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        response
    }

    #[tokio::test]
    async fn actual_bytes_are_frozen_and_repeated_gets_never_replay_actions() {
        let fixture = fixture(
            b"START\r\nRESULT:42\r\nEND\r\n".to_vec(),
            FRAME_OUTPUT,
            Duration::ZERO,
        );
        let mut handle = capture_and_start_native_probe(
            "127.0.0.1:0".parse().unwrap(),
            NativeProbeTarget::Terminal(fixture.path.clone()),
            &adapter(),
        )
        .await
        .unwrap();
        for _ in 0..2 {
            let response = request(
                handle.local_addr(),
                b"GET /result HTTP/1.1\r\nHost: private\r\n\r\n",
            )
            .await;
            assert!(response.starts_with(b"HTTP/1.1 200"));
            assert!(response.ends_with(b"START\r\nRESULT:42\r\nEND\r\n"));
            let body_start = response
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap()
                + 4;
            use ato_formation::authoring::{
                BOUND_CONTRACT_SCHEMA, BoundContract, HTTP_CONTRACT_VERIFIER,
            };
            use ato_formation::verify::{
                RuntimeHttpObservation, RuntimeObservation, verify_runtime,
            };
            let expected = RuntimeHttpObservation::from_response(
                "native-results",
                "GET",
                "/result",
                200,
                b"START\r\nRESULT:42\r\nEND\r\n",
            );
            let contract: BoundContract = serde_json::from_value(serde_json::json!({
                "schema": BOUND_CONTRACT_SCHEMA, "requirements": [{ "id": "actual-application-answer",
                "verifier": HTTP_CONTRACT_VERIFIER, "port": "native-results", "method": "GET",
                "path": "/result", "status": 200, "body_digest": expected.body_digest }]
            })).unwrap();
            let observed = RuntimeObservation {
                http: vec![RuntimeHttpObservation::from_response(
                    "native-results",
                    "GET",
                    "/result",
                    200,
                    &response[body_start..],
                )],
                ..Default::default()
            };
            assert!(verify_runtime(&contract, &observed).fully_satisfied());
            let ready_only = RuntimeObservation {
                http: vec![RuntimeHttpObservation::from_response(
                    "native-results",
                    "GET",
                    "/result",
                    200,
                    b"surface_ready",
                )],
                ..Default::default()
            };
            assert!(!verify_runtime(&contract, &ready_only).fully_satisfied());
            let mut wrong_port = observed;
            wrong_port.http[0].port = "terminal".into();
            assert!(!verify_runtime(&contract, &wrong_port).fully_satisfied());
            assert!(!verify_runtime(&contract, &RuntimeObservation::default()).fully_satisfied());
        }
        assert_eq!(fixture.actions.load(Ordering::SeqCst), 1);
        for (wire, expected) in [
            (
                &b"POST /result HTTP/1.1\r\nHost: private\r\nContent-Length: 0\r\n\r\n"[..],
                "405",
            ),
            (
                &b"GET /result?again=1 HTTP/1.1\r\nHost: private\r\n\r\n"[..],
                "400",
            ),
            (
                &b"GET /result HTTP/1.1\r\nHost: private\r\nContent-Length: 1\r\n\r\nx"[..],
                "400",
            ),
            (&b"GET /other HTTP/1.1\r\nHost: private\r\n\r\n"[..], "404"),
        ] {
            let response = request(handle.local_addr(), wire).await;
            assert!(response.starts_with(format!("HTTP/1.1 {expected}").as_bytes()));
            assert!(
                !response
                    .windows(b"RESULT:42".len())
                    .any(|part| part == b"RESULT:42")
            );
        }
        let addr = handle.local_addr();
        let _pending = TcpStream::connect(addr).await.unwrap();
        handle.stop().await.unwrap();
        handle.stop().await.unwrap();
        assert!(TcpStream::connect(addr).await.is_err());
    }

    #[tokio::test]
    async fn echo_only_trailing_oversize_exit_and_timeout_do_not_publish_results() {
        for (bytes, kind, delay, limit) in [
            (b"compute\r\n".to_vec(), FRAME_OUTPUT, Duration::ZERO, 128),
            (
                b"RESULT\r\nEND\r\nextra".to_vec(),
                FRAME_OUTPUT,
                Duration::ZERO,
                128,
            ),
            (
                b"TOO-LONG-RESULT-END\r\n".to_vec(),
                FRAME_OUTPUT,
                Duration::ZERO,
                8,
            ),
            (
                br#"{"type":"exit","code":1,"signal":null}"#.to_vec(),
                FRAME_CONTROL,
                Duration::ZERO,
                128,
            ),
            (
                b"END\r\n".to_vec(),
                FRAME_OUTPUT,
                Duration::from_millis(150),
                128,
            ),
        ] {
            let fixture = fixture(bytes, kind, delay);
            let mut declaration = adapter();
            if let NativeProbeOperation::TerminalExchange {
                max_output_bytes,
                timeout_ms,
                ..
            } = &mut declaration.operations[0]
            {
                *max_output_bytes = limit;
                *timeout_ms = 50;
            }
            let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = reserved.local_addr().unwrap();
            drop(reserved);
            assert!(
                capture_and_start_native_probe(
                    addr,
                    NativeProbeTarget::Terminal(fixture.path.clone()),
                    &declaration
                )
                .await
                .is_err()
            );
            assert!(TcpStream::connect(addr).await.is_err());
        }
    }

    #[tokio::test]
    async fn a_completion_queued_before_input_is_not_an_application_answer() {
        let fixture = fixture_with_early_output(
            b"OLD-RESULT\r\nEND\r\n".to_vec(),
            FRAME_OUTPUT,
            Duration::ZERO,
            true,
        );
        let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);
        assert!(
            capture_and_start_native_probe(
                addr,
                NativeProbeTarget::Terminal(fixture.path.clone()),
                &adapter()
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.actions.load(Ordering::SeqCst), 0);
        assert!(TcpStream::connect(addr).await.is_err());
    }

    #[tokio::test]
    async fn a_later_failed_operation_prevents_the_whole_observation_port() {
        let fixture = fixture(
            b"RESULT:42\r\nEND\r\n".to_vec(),
            FRAME_OUTPUT,
            Duration::ZERO,
        );
        let mut declaration = adapter();
        let mut second = declaration.operations[0].clone();
        if let NativeProbeOperation::TerminalExchange { path, .. } = &mut second {
            *path = "/second".into();
        }
        declaration.operations.push(second);
        let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);
        assert!(
            capture_and_start_native_probe(
                addr,
                NativeProbeTarget::Terminal(fixture.path.clone()),
                &declaration
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.actions.load(Ordering::SeqCst), 1);
        assert!(TcpStream::connect(addr).await.is_err());
    }

    #[tokio::test]
    async fn invalid_catalog_and_public_listener_are_rejected_before_any_native_action() {
        let fixture = fixture(b"END\r\n".to_vec(), FRAME_OUTPUT, Duration::ZERO);
        let target = || NativeProbeTarget::Terminal(fixture.path.clone());
        assert!(
            capture_and_start_native_probe("0.0.0.0:0".parse().unwrap(), target(), &adapter())
                .await
                .is_err()
        );
        let mut invalid = adapter();
        if let NativeProbeOperation::TerminalExchange { input, .. } = &mut invalid.operations[0] {
            *input = b"END\r\n".to_vec();
        }
        assert!(
            capture_and_start_native_probe("127.0.0.1:0".parse().unwrap(), target(), &invalid)
                .await
                .is_err()
        );
        assert_eq!(fixture.actions.load(Ordering::SeqCst), 0);
    }
}
