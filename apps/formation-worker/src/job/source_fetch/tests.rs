use super::*;
use crate::job::{PinnedSourceFetcher, SourceFetcher};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn scratch() -> tempfile::TempDir {
    std::fs::create_dir_all(".tmp").unwrap();
    tempfile::tempdir_in(".tmp").unwrap()
}

fn fixture(serve: impl FnOnce(TcpStream) + Send + 'static) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() <= 8192);
        }
        serve(stream);
        request
    });
    (url, thread)
}

fn fetch(url: &str, root: &Path, maximum: u64, timeout: Duration) -> Result<(File, String)> {
    download_file(client()?.get(url), root, maximum, Instant::now() + timeout)
}

fn assert_clean(root: &Path) {
    let path = root.join(".tmp");
    if path.exists() {
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    }
}

fn is_limit(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<SourceError>(),
        Some(SourceError::LimitExceeded {
            limit: "archive_bytes"
        })
    )
}

#[test]
fn github_url_is_public_pinned_and_cannot_inherit_control_plane_credentials() {
    let source = serde_json::json!({"kind":"git_hub","owner":"ato-run","repository":"ato",
        "resolved_commit_sha":"0123456789abcdef0123456789abcdef01234567", "requested_ref":"main"});
    let fetcher = PinnedSourceFetcher::new(
        "https://private-api.invalid".to_owned(),
        "operator-key-canary".to_owned(),
    )
    .unwrap();
    let request = fetcher.request("job", &source).unwrap().build().unwrap();
    assert_eq!(
        request.url().as_str(),
        "https://codeload.github.com/ato-run/ato/tar.gz/0123456789abcdef0123456789abcdef01234567"
    );
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none()
    );
    assert!(request.url().query().is_none());
    for (key, invalid) in [
        ("owner", "user@127.0.0.1"),
        ("owner", "../private"),
        ("owner", "name?credential=canary"),
        ("repository", ".."),
        ("repository", "repo#fragment"),
        ("repository", "repo/extra"),
        ("resolved_commit_sha", "main"),
        (
            "resolved_commit_sha",
            "0123456789abcdef0123456789abcdef01234567?key=canary",
        ),
    ] {
        let mut value = source.clone();
        value[key] = invalid.into();
        let error = github_url(&value).unwrap_err().to_string();
        assert!(!error.contains("canary"));
    }
}

#[test]
fn excessive_content_length_is_rejected_before_waiting_for_or_allocating_the_body() {
    let root = scratch();
    let (release, wait_for_release) = std::sync::mpsc::channel();
    let (url, server) = fixture(move |mut stream| {
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1073741824\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        // Never send a body. The client must reject the header while this
        // server remains blocked, rather than waiting for EOF or its deadline.
        wait_for_release
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    });
    let error = fetch(&url, root.path(), 32, Duration::from_secs(1)).unwrap_err();
    assert!(is_limit(&error), "{error:#}");
    release.send(()).unwrap();
    assert_clean(root.path());
    server.join().unwrap();
}

#[test]
fn chunked_and_connection_close_bodies_stop_at_the_actual_stream_cap() {
    for chunked in [false, true] {
        let root = scratch();
        let (url, server) = fixture(move |mut stream| {
            let headers = if chunked {
                "Transfer-Encoding: chunked\r\n"
            } else {
                ""
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\n{headers}Connection: close\r\n\r\n"
            )
            .unwrap();
            let chunk = [b'x'; 4096];
            for _ in 0..1024 {
                let result = if chunked {
                    stream
                        .write_all(b"1000\r\n")
                        .and_then(|_| stream.write_all(&chunk))
                        .and_then(|_| stream.write_all(b"\r\n"))
                } else {
                    stream.write_all(&chunk)
                };
                if result.is_err() {
                    break;
                }
            }
            if chunked {
                let _ = stream.write_all(b"0\r\n\r\n");
            }
        });
        let error = fetch(&url, root.path(), 4096, Duration::from_secs(1)).unwrap_err();
        assert!(is_limit(&error), "{error:#}");
        assert_clean(root.path());
        server.join().unwrap();
    }
}

#[test]
fn exact_length_archive_is_preserved_and_anonymous_file_is_removed_on_drop() {
    let root = scratch();
    let (url, server) = fixture(|mut stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    let (mut file, digest) = fetch(&url, root.path(), 3, Duration::from_secs(1)).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 3);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abc");
    assert_eq!(
        digest,
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    drop(file);
    assert_clean(root.path());
    server.join().unwrap();
}

#[test]
fn every_redirect_is_refused_without_contacting_its_destination_or_echoing_location() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    for status in [301, 302, 303, 307, 308] {
        let target = destination.local_addr().unwrap();
        let root = scratch();
        let (url, server) = fixture(move |mut stream| {
            write!(stream,"HTTP/1.1 {status} Redirect\r\nLocation: http://{target}/private?key=credential-canary\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let error = fetch(&url, root.path(), 128, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("HTTP {status}")), "{error}");
        assert!(!error.contains("credential-canary"));
        assert!(
            matches!(destination.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
        );
        assert_clean(root.path());
        server.join().unwrap();
    }
}

#[test]
fn total_deadline_covers_headers_body_stalls_and_slow_drip_without_resetting() {
    for mode in [0, 1, 2] {
        let root = scratch();
        let (url, server) = fixture(move |mut stream| {
            if mode == 0 {
                thread::sleep(Duration::from_millis(160));
                return;
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            if mode == 1 {
                thread::sleep(Duration::from_millis(160));
                return;
            }
            for _ in 0..20 {
                if stream.write_all(b"1\r\nx\r\n").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let error = fetch(&url, root.path(), 128, Duration::from_millis(70)).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SourceError>(),
                Some(SourceError::DeadlineExceeded)
            ),
            "{error:#}"
        );
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_clean(root.path());
        server.join().unwrap();
    }
}

#[test]
fn http_content_encoding_is_not_transparently_expanded_before_accounting() {
    use flate2::{Compression, write::GzEncoder};
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    let compressed = gzip.finish().unwrap();
    let expected = compressed.clone();
    let maximum = compressed.len() as u64;
    let root = scratch();
    let (url, server) = fixture(move |mut stream| {
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",compressed.len()).unwrap();
        stream.write_all(&compressed).unwrap();
    });
    let (mut file, _) = fetch(&url, root.path(), maximum, Duration::from_secs(1)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
    drop(file);
    assert_clean(root.path());
    server.join().unwrap();
}

struct Endless {
    consumed: Arc<AtomicUsize>,
    largest_read: Arc<AtomicUsize>,
}
impl Read for Endless {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.consumed.fetch_add(buffer.len(), Ordering::SeqCst);
        self.largest_read.fetch_max(buffer.len(), Ordering::SeqCst);
        buffer.fill(b'x');
        Ok(buffer.len())
    }
}

#[test]
fn infinite_input_cannot_read_allocate_or_write_more_than_its_frozen_cap() {
    let consumed = Arc::new(AtomicUsize::new(0));
    let largest_read = Arc::new(AtomicUsize::new(0));
    let input = Endless {
        consumed: consumed.clone(),
        largest_read: largest_read.clone(),
    };
    let mut output = Vec::new();
    let error = copy_bounded(
        input,
        &mut output,
        128,
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(is_limit(&error));
    assert_eq!(consumed.load(Ordering::SeqCst), 129);
    assert!(largest_read.load(Ordering::SeqCst) <= 64 * 1024);
    assert!(output.len() <= 128);
    assert!(output.capacity() <= 128);
}

struct Legacy(Vec<u8>);
impl SourceFetcher for Legacy {
    fn fetch(&self, _: &str, _: &serde_json::Value) -> Result<Vec<u8>> {
        Ok(self.0.clone())
    }
}

#[test]
fn legacy_fixture_default_refuses_oversize_and_expired_inputs_and_cleans_owned_scratch() {
    let root = scratch();
    let source = serde_json::json!({});
    assert!(is_limit(
        &Legacy(vec![0; 17])
            .fetch_file(
                "job",
                &source,
                root.path(),
                16,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err()
    ));
    assert_clean(root.path());
    assert!(
        Legacy(vec![0; 16])
            .fetch_file(
                "job",
                &source,
                root.path(),
                16,
                Instant::now() - Duration::from_secs(1)
            )
            .is_err()
    );
    assert_clean(root.path());
    let (file, _) = Legacy(vec![0; 16])
        .fetch_file(
            "job",
            &source,
            root.path(),
            16,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(file.metadata().unwrap().len(), 16);
    drop(file);
    assert_clean(root.path());
}

#[test]
fn actual_pinned_fetcher_uses_the_bounded_file_path_for_job_owned_uploads() {
    let (url, server) = fixture(|mut stream| {
        stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n21\r\nxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n0\r\n\r\n").unwrap();
    });
    let fetcher = PinnedSourceFetcher::new(url, "test-job-owned-token".to_owned()).unwrap();
    let root = scratch();
    let error = fetcher
        .fetch_file(
            "job_1",
            &serde_json::json!({"kind":"uploaded_archive"}),
            root.path(),
            32,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
    assert!(is_limit(&error));
    assert_clean(root.path());
    let request = String::from_utf8(server.join().unwrap()).unwrap();
    assert!(request.starts_with("GET /v1/internal/formation/jobs/job_1/source-archive HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-job-owned-token")
    );
}

#[cfg(unix)]
#[test]
fn source_transport_scratch_cannot_follow_a_symlink_outside_the_job_root() {
    let root = scratch();
    let outside = scratch();
    std::os::unix::fs::symlink(outside.path(), root.path().join(".tmp")).unwrap();
    let error =
        temporary_file(root.path(), 16, Instant::now() + Duration::from_secs(1)).unwrap_err();
    assert!(error.to_string().contains("owned work root"));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn streamed_compressed_archive_reuses_the_same_tree_closure_and_strict_subdirectory_rules() {
    use ato_formation::source::{DownloadedArchive, FileVerifiedArchive, SourceLimits};
    use flate2::{Compression, write::GzEncoder};
    let mut tar = tar::Builder::new(Vec::new());
    for (path, body) in [
        ("repository/README.md", b"readme".as_slice()),
        ("repository/app/main.py", b"print(4)\n".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(123456);
        header.set_cksum();
        tar.append_data(&mut header, path, body).unwrap();
    }
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    let bytes = gzip.finish().unwrap();
    let size = bytes.len() as u64;
    let legacy = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&crate::job::digest(&bytes))
        .unwrap()
        .verify_tree_digest(None, SourceLimits::default())
        .unwrap();
    let root = scratch();
    let (url, server) = fixture(move |mut stream| {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        stream.write_all(&bytes).unwrap();
    });
    let (file, digest) = fetch(&url, root.path(), size, Duration::from_secs(2)).unwrap();
    let mut streamed =
        FileVerifiedArchive::verify(file, &digest, size, SourceLimits::default()).unwrap();
    assert_eq!(streamed.tree_digest(), legacy.tree_digest());
    assert_eq!(streamed.resolver_contract(), legacy.resolver_contract());
    assert_eq!(
        streamed.closure_ref("app").unwrap(),
        legacy.closure_ref("app").unwrap()
    );
    for invalid in ["../outside", "app/../../outside", "/app", "app\\outside"] {
        assert!(streamed.closure_ref(invalid).is_err());
    }
    let source = streamed
        .materialize(&root.path().join("source"), "app", SourceLimits::default())
        .unwrap();
    assert_eq!(
        std::fs::read(source.join("main.py")).unwrap(),
        b"print(4)\n"
    );
    drop(streamed);
    assert_clean(root.path());
    server.join().unwrap();
}
