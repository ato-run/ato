//! End-to-end tests against a real loopback listener on port 0.
//!
//! Every case in the PR2 acceptance list is exercised over real HTTP/1.1
//! sockets: idempotent re-PUT, digest mismatch, oversize rejection before
//! storage, upload auth, anonymous GET/HEAD, operator-only DELETE, concurrent
//! PUT, aborted upload cleanup, restart persistence, rate limiting, quota, and
//! the storage-level retention sweep (unit-tested in the library).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use capsulehub::{PutAuth, RunningHub, Secret, ServerConfig, start_in_background};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PUT_TOKEN: &str = "test-put-token";
const DELETE_TOKEN: &str = "test-delete-token";

struct Options {
    max_object_bytes: u64,
    max_storage_bytes: Option<u64>,
    put_auth: PutAuth,
    put_token: Option<&'static str>,
    delete_token: Option<&'static str>,
    rate_limit_per_minute: u32,
    allow_anonymous_read: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_object_bytes: 64 * 1024,
            max_storage_bytes: None,
            put_auth: PutAuth::Token,
            put_token: Some(PUT_TOKEN),
            delete_token: Some(DELETE_TOKEN),
            // Most tests would otherwise share one 60/minute budget.
            rate_limit_per_minute: 0,
            allow_anonymous_read: true,
        }
    }
}

struct TestHub {
    hub: RunningHub,
    storage: TempDir,
}

impl TestHub {
    async fn start(options: Options) -> Self {
        let storage = tempfile::tempdir().expect("temporary storage");
        let hub = start_hub(storage.path(), &options).await;
        Self { hub, storage }
    }

    fn addr(&self) -> SocketAddr {
        self.hub.local_addr()
    }

    fn storage(&self) -> &Path {
        self.storage.path()
    }

    async fn shutdown(self) {
        self.hub.shutdown().await;
    }
}

async fn start_hub(storage: &Path, options: &Options) -> RunningHub {
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("loopback address"),
        storage: storage.to_path_buf(),
        max_object_bytes: options.max_object_bytes,
        max_storage_bytes: options.max_storage_bytes,
        put_auth: options.put_auth,
        put_token: options.put_token.map(Secret::new),
        delete_token: options.delete_token.map(Secret::new),
        rate_limit_per_minute: options.rate_limit_per_minute,
        retention: None,
        allow_anonymous_read: options.allow_anonymous_read,
    };
    start_in_background(config).await.expect("hub starts")
}

fn digest(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

fn object_path(addr: SocketAddr, id: &str) -> (SocketAddr, String) {
    (addr, format!("/v1/capsulehub/objects/{id}"))
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn request(
    addr: SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect to hub");
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write request head");
    if !body.is_empty() {
        stream.write_all(body).await.expect("write request body");
    }
    stream.flush().await.expect("flush request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap_or_else(|error| {
        panic!("read response for {method} {target}: {error} (partial: {raw:?})")
    });
    parse_response(raw)
}

fn parse_response(raw: Vec<u8>) -> HttpResponse {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response has a header separator");
    let head = String::from_utf8(raw[..split].to_vec()).expect("utf-8 response head");
    let body = raw[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status code");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(": ")
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        })
        .collect();
    assert!(
        !headers.iter().any(|(name, _)| name == "transfer-encoding"),
        "hub responses must carry Content-Length"
    );
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn put_object(hub: &TestHub, payload: &[u8]) -> HttpResponse {
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);
    request(
        addr,
        "PUT",
        &target,
        &[("Authorization", &authorization)],
        payload,
    )
    .await
}

async fn get_object(hub: &TestHub, payload: &[u8]) -> HttpResponse {
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    request(addr, "GET", &target, &[], &[]).await
}

async fn head_object(hub: &TestHub, payload: &[u8]) -> HttpResponse {
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    request(addr, "HEAD", &target, &[], &[]).await
}

fn object_files(storage: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(storage.join("objects"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

fn partial_files(storage: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(storage.join("tmp"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition was not reached in time");
}

#[tokio::test]
async fn health_endpoint_is_alive_and_leaks_no_configuration() {
    let hub = TestHub::start(Options::default()).await;
    let response = request(hub.addr(), "GET", "/healthz", &[], &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.text(), "ok");
    let rendered = response.text();
    assert!(!rendered.contains(hub.storage().to_str().unwrap()));
    assert!(!rendered.contains(PUT_TOKEN));
    assert!(!rendered.contains(DELETE_TOKEN));
    hub.shutdown().await;
}

#[tokio::test]
async fn put_get_and_head_round_trip_opaque_bytes() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"\x00opaque ciphertext\xff not a bundle";

    let put = put_object(&hub, payload).await;
    assert_eq!(put.status, 201);

    let get = get_object(&hub, payload).await;
    assert_eq!(get.status, 200);
    assert_eq!(get.body, payload);
    assert_eq!(get.header("content-type"), Some("application/octet-stream"));
    assert_eq!(
        get.header("content-length"),
        Some(payload.len().to_string().as_str())
    );
    assert_eq!(
        get.header("etag"),
        Some(format!("\"{}\"", digest(payload)).as_str())
    );

    let head = head_object(&hub, payload).await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(payload.len().to_string().as_str())
    );
    assert_eq!(
        head.header("etag"),
        Some(format!("\"{}\"", digest(payload)).as_str())
    );

    assert_eq!(object_files(hub.storage()).len(), 1);
    hub.shutdown().await;
}

#[tokio::test]
async fn repeated_put_of_the_same_object_is_idempotent() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"same bytes";

    assert_eq!(put_object(&hub, payload).await.status, 201);
    assert_eq!(put_object(&hub, payload).await.status, 200);

    assert_eq!(object_files(hub.storage()).len(), 1);
    assert_eq!(get_object(&hub, payload).await.body, payload);
    hub.shutdown().await;
}

#[tokio::test]
async fn a_digest_mismatch_is_rejected_and_stores_nothing() {
    let hub = TestHub::start(Options::default()).await;
    let claimed = digest(b"the bytes the id claims");
    let (addr, target) = object_path(hub.addr(), &claimed);
    let authorization = bearer(PUT_TOKEN);

    let response = request(
        addr,
        "PUT",
        &target,
        &[("Authorization", &authorization)],
        b"different bytes",
    )
    .await;
    assert_eq!(response.status, 400);
    assert!(!response.text().contains(PUT_TOKEN));
    assert!(object_files(hub.storage()).is_empty());
    assert!(partial_files(hub.storage()).is_empty());
    hub.shutdown().await;
}

#[tokio::test]
async fn an_oversize_upload_is_rejected_before_storage() {
    let options = Options {
        max_object_bytes: 8,
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let payload = b"0123456789";
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);

    let response = request(
        addr,
        "PUT",
        &target,
        &[("Authorization", &authorization)],
        payload,
    )
    .await;
    assert_eq!(response.status, 413);
    assert!(object_files(hub.storage()).is_empty());
    assert!(partial_files(hub.storage()).is_empty());
    hub.shutdown().await;
}

#[tokio::test]
async fn uploads_require_the_configured_token() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"needs auth";
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);

    assert_eq!(
        request(addr, "PUT", &target, &[], payload).await.status,
        401
    );

    let wrong = bearer("wrong-token");
    assert_eq!(
        request(addr, "PUT", &target, &[("Authorization", &wrong)], payload)
            .await
            .status,
        401
    );

    assert!(object_files(hub.storage()).is_empty());
    assert_eq!(put_object(&hub, payload).await.status, 201);
    hub.shutdown().await;
}

#[tokio::test]
async fn open_upload_mode_accepts_anonymous_puts() {
    let options = Options {
        put_auth: PutAuth::Open,
        put_token: None,
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let payload = b"anonymous upload";
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);

    let response = request(addr, "PUT", &target, &[], payload).await;
    assert_eq!(response.status, 201);
    assert_eq!(get_object(&hub, payload).await.body, payload);
    hub.shutdown().await;
}

#[tokio::test]
async fn reads_can_require_authentication() {
    let options = Options {
        allow_anonymous_read: false,
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let payload = b"authenticated read";
    assert_eq!(put_object(&hub, payload).await.status, 201);

    assert_eq!(get_object(&hub, payload).await.status, 401);
    assert_eq!(head_object(&hub, payload).await.status, 401);

    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);
    assert_eq!(
        request(
            addr,
            "GET",
            &target,
            &[("Authorization", &authorization)],
            &[]
        )
        .await
        .status,
        200
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn delete_is_operator_only_and_takes_effect() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"takedown me";
    assert_eq!(put_object(&hub, payload).await.status, 201);
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);

    assert_eq!(request(addr, "DELETE", &target, &[], &[]).await.status, 403);

    let put_authorization = bearer(PUT_TOKEN);
    assert_eq!(
        request(
            addr,
            "DELETE",
            &target,
            &[("Authorization", &put_authorization)],
            &[],
        )
        .await
        .status,
        403
    );

    let wrong = bearer("wrong-delete-token");
    assert_eq!(
        request(addr, "DELETE", &target, &[("Authorization", &wrong)], &[])
            .await
            .status,
        403
    );

    let delete_authorization = bearer(DELETE_TOKEN);
    assert_eq!(
        request(
            addr,
            "DELETE",
            &target,
            &[("Authorization", &delete_authorization)],
            &[],
        )
        .await
        .status,
        204
    );
    assert_eq!(get_object(&hub, payload).await.status, 404);
    assert_eq!(
        request(
            addr,
            "DELETE",
            &target,
            &[("Authorization", &delete_authorization)],
            &[],
        )
        .await
        .status,
        404
    );
    assert!(object_files(hub.storage()).is_empty());
    hub.shutdown().await;
}

#[tokio::test]
async fn delete_is_forbidden_when_no_delete_token_is_configured() {
    let options = Options {
        delete_token: None,
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let payload = b"undeletable";
    assert_eq!(put_object(&hub, payload).await.status, 201);
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);

    for token in [PUT_TOKEN, DELETE_TOKEN, "anything"] {
        let authorization = bearer(token);
        assert_eq!(
            request(
                addr,
                "DELETE",
                &target,
                &[("Authorization", &authorization)],
                &[],
            )
            .await
            .status,
            403
        );
    }
    assert_eq!(get_object(&hub, payload).await.status, 200);
    hub.shutdown().await;
}

#[tokio::test]
async fn concurrent_puts_of_one_id_both_succeed() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"racing upload";
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);

    let headers = [("Authorization", authorization.as_str())];
    let first = request(addr, "PUT", &target, &headers, payload);
    let second = request(addr, "PUT", &target, &headers, payload);
    let (first, second) = tokio::join!(first, second);

    assert!(first.status == 200 || first.status == 201);
    assert!(second.status == 200 || second.status == 201);
    assert_eq!(object_files(hub.storage()).len(), 1);
    assert_eq!(get_object(&hub, payload).await.body, payload);
    hub.shutdown().await;
}

#[tokio::test]
async fn an_aborted_upload_leaves_no_object_or_partial() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"never completes";
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "PUT {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 4096\r\nAuthorization: {authorization}\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.expect("write head");
    stream
        .write_all(&payload[..payload.len().min(8)])
        .await
        .expect("write partial body");
    stream.flush().await.expect("flush");
    drop(stream);

    wait_until(|| partial_files(hub.storage()).is_empty()).await;
    assert!(object_files(hub.storage()).is_empty());
    assert_eq!(get_object(&hub, payload).await.status, 404);
    hub.shutdown().await;
}

#[tokio::test]
async fn stored_objects_survive_a_restart() {
    let storage = tempfile::tempdir().expect("temporary storage");
    let payload = b"persistent object";
    {
        let hub = start_hub(storage.path(), &Options::default()).await;
        let put = put_object_of(&hub, payload).await;
        // `put_object` takes a TestHub; this helper keeps the shared storage.
        assert_eq!(put.status, 201);
        let id = digest(payload);
        let (addr, target) = object_path(hub.local_addr(), &id);
        assert_eq!(request(addr, "GET", &target, &[], &[]).await.body, payload);
        hub.shutdown().await;
    }

    let restarted = start_hub(storage.path(), &Options::default()).await;
    let id = digest(payload);
    let (addr, target) = object_path(restarted.local_addr(), &id);
    let response = request(addr, "GET", &target, &[], &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, payload);
    restarted.shutdown().await;
}

/// PUT against a hub that is not wrapped in [`TestHub`] (restart test).
async fn put_object_of(hub: &RunningHub, payload: &[u8]) -> HttpResponse {
    let id = digest(payload);
    let (addr, target) = object_path(hub.local_addr(), &id);
    let authorization = bearer(PUT_TOKEN);
    request(
        addr,
        "PUT",
        &target,
        &[("Authorization", &authorization)],
        payload,
    )
    .await
}

#[tokio::test]
async fn rate_limiting_rejects_excess_requests_but_not_health() {
    let options = Options {
        rate_limit_per_minute: 2,
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let id = digest(b"never stored");
    let (addr, target) = object_path(hub.addr(), &id);

    assert_eq!(request(addr, "GET", &target, &[], &[]).await.status, 404);
    assert_eq!(request(addr, "GET", &target, &[], &[]).await.status, 404);
    let limited = request(addr, "GET", &target, &[], &[]).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.header("retry-after"), Some("60"));

    for _ in 0..5 {
        assert_eq!(request(addr, "GET", "/healthz", &[], &[]).await.status, 200);
    }
    hub.shutdown().await;
}

#[tokio::test]
async fn storage_quota_rejects_objects_that_do_not_fit() {
    let options = Options {
        max_storage_bytes: Some(10),
        ..Options::default()
    };
    let hub = TestHub::start(options).await;
    let first = b"123456";
    let second = b"abcdef";
    assert_eq!(put_object(&hub, first).await.status, 201);

    let id = digest(second);
    let (addr, target) = object_path(hub.addr(), &id);
    let authorization = bearer(PUT_TOKEN);
    let response = request(
        addr,
        "PUT",
        &target,
        &[("Authorization", &authorization)],
        second,
    )
    .await;
    assert_eq!(response.status, 507);

    assert_eq!(object_files(hub.storage()).len(), 1);
    assert_eq!(get_object(&hub, second).await.status, 404);
    assert_eq!(get_object(&hub, first).await.status, 200);
    hub.shutdown().await;
}

#[tokio::test]
async fn unknown_routes_and_methods_are_rejected() {
    let hub = TestHub::start(Options::default()).await;
    let addr = hub.addr();

    assert_eq!(request(addr, "GET", "/", &[], &[]).await.status, 404);
    assert_eq!(
        request(addr, "GET", "/v1/capsulehub/objects/", &[], &[])
            .await
            .status,
        404
    );

    let invalid_id = object_path(addr, "not-a-digest");
    assert_eq!(
        request(invalid_id.0, "GET", &invalid_id.1, &[], &[])
            .await
            .status,
        400
    );
    let uppercase = object_path(addr, &format!("sha256:{}", "A".repeat(64)));
    assert_eq!(
        request(uppercase.0, "GET", &uppercase.1, &[], &[])
            .await
            .status,
        400
    );

    let extra_segment = object_path(addr, &format!("{}/extra", digest(b"x")));
    assert_eq!(
        request(extra_segment.0, "GET", &extra_segment.1, &[], &[])
            .await
            .status,
        404
    );

    let payload = b"exists";
    assert_eq!(put_object(&hub, payload).await.status, 201);
    let id = digest(payload);
    let (addr, target) = object_path(hub.addr(), &id);
    let response = request(addr, "POST", &target, &[], &[]).await;
    assert_eq!(response.status, 405);
    assert_eq!(response.header("allow"), Some("PUT, GET, HEAD, DELETE"));

    let health_post = request(addr, "POST", "/healthz", &[], &[]).await;
    assert_eq!(health_post.status, 405);
    hub.shutdown().await;
}

#[tokio::test]
async fn a_deleted_object_is_removed_from_disk() {
    let hub = TestHub::start(Options::default()).await;
    let payload = b"remove from disk";
    assert_eq!(put_object(&hub, payload).await.status, 201);
    let stored = hub.storage().join("objects").join(&digest(payload)[7..]);
    assert!(stored.is_file());

    let (addr, target) = object_path(hub.addr(), &digest(payload));
    let authorization = bearer(DELETE_TOKEN);
    assert_eq!(
        request(
            addr,
            "DELETE",
            &target,
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .status,
        204
    );
    assert!(!stored.exists());
    hub.shutdown().await;
}

#[tokio::test]
async fn error_bodies_never_contain_credentials() {
    let hub = TestHub::start(Options::default()).await;
    let id = digest(b"auth probe");
    let (addr, target) = object_path(hub.addr(), &id);

    let responses = [
        request(addr, "PUT", &target, &[], b"payload").await,
        request(
            addr,
            "DELETE",
            &target,
            &[("Authorization", &bearer("wrong"))],
            &[],
        )
        .await,
        request(addr, "GET", &object_path(addr, "bogus").1, &[], &[]).await,
    ];
    for response in responses {
        let rendered = format!(
            "{} {} {}",
            response.text(),
            response
                .headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\r\n"),
            response
                .headers
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<String>()
        );
        assert!(!rendered.contains(PUT_TOKEN));
        assert!(!rendered.contains(DELETE_TOKEN));
    }
    hub.shutdown().await;
}
