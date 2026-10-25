use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};

fn scratch() -> tempfile::TempDir {
    std::fs::create_dir_all(".tmp").unwrap();
    tempfile::tempdir_in(".tmp").unwrap()
}
fn claim(subdirectory: &str) -> ClaimedSourceArchive {
    let mut job: Value = serde_json::from_str(include_str!(
        "../../../../lib/ipc/tests/fixtures/formation-v1/python-process-job.json"
    ))
    .unwrap();
    job["requested_outputs"] = json!(["source_archive"]);
    job["policy"] = json!({"policy_version":"formation-source-archive-v1","network":"denied","publish_enabled":false});
    job.as_object_mut().unwrap().remove("authoring");
    job["source"]["subdirectory"] = subdirectory.into();
    let job: FormationJobV1 = serde_json::from_value(job).unwrap();
    ClaimedSourceArchive {
        job_id: job.job_id.clone(),
        attempt_id: "fattempt_source".into(),
        attempt_fence: 7,
        job_digest: job.canonical_digest().unwrap(),
        job,
    }
}
fn archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    // Application scripts are data during source-only intake, even when named
    // as conventional package/build entrypoints.
    for (name, data) in [
        ("repository/site/index.html", "hello"),
        (
            "repository/package.json",
            r#"{"scripts":{"postinstall":"touch SOURCE_ONLY_MUST_NOT_RUN"}}"#,
        ),
        (
            "repository/setup.py",
            "raise RuntimeError('SOURCE_ONLY_MUST_NOT_RUN')",
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, data.as_bytes())
            .unwrap();
    }
    builder.into_inner().unwrap()
}
struct Fetcher {
    bytes: Vec<u8>,
    calls: AtomicUsize,
}
impl SourceFetcher for Fetcher {
    fn fetch(&self, _: &str, _: &Value) -> Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.bytes.clone())
    }
}
#[derive(Default)]
struct Observed {
    staging: Vec<u8>,
    binding: Option<Value>,
    accepted: usize,
    aborted: usize,
    object_requests: usize,
    api_requests: usize,
    object_bearer_leaked: bool,
}
#[derive(Clone, Copy, Default, Debug)]
enum Fault {
    #[default]
    None,
    TicketBinding,
    TicketExpired,
    TicketUrl,
    PutFailure,
    ReadbackDigest,
    ReadbackSize,
    ReadbackShort,
    ReadbackSlow,
    ReadbackRedirect,
    StaleFence,
    WrongAckAttempt,
    CleanupFailure,
}
struct Server {
    url: String,
    state: Arc<Mutex<Observed>>,
    stopped: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}
fn request(stream: &mut TcpStream) -> Option<(String, String, String, Vec<u8>)> {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut header = Vec::new();
    let mut byte = [0];
    while !header.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() {
            return None;
        }
        header.push(byte[0]);
        assert!(header.len() <= 8192);
    }
    let header = String::from_utf8(header).unwrap();
    let mut first = header.lines().next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_owned();
    let path = first.next().unwrap().to_owned();
    let lower = header.to_ascii_lowercase();
    let length = lower
        .lines()
        .find_map(|s| s.strip_prefix("content-length:"))
        .map(|s| s.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    assert!(length <= 1024 * 1024);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    Some((method, path, lower, body))
}
fn respond(stream: &mut TcpStream, status: &str, body: &[u8]) {
    let header = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if let Err(error) = header {
        // Deadline tests deliberately close the readback socket before the
        // delayed response. Keep the server alive to observe owned cleanup.
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ));
        return;
    }
    let _ = stream.write_all(body);
}
impl Server {
    fn new(fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let base = url.clone();
        let state = Arc::new(Mutex::new(Observed::default()));
        let captured = state.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let task = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let Some((method, path, headers, body)) = request(&mut stream) else {
                    continue;
                };
                let mut state = captured.lock().unwrap();
                if path.starts_with("/object") {
                    state.object_requests += 1;
                    state.object_bearer_leaked |= headers.contains("authorization:")
                        || headers.contains("runner-token-canary");
                    if method == "PUT" {
                        assert!(!headers.contains("transfer-encoding:"));
                        let expected_size =
                            state.binding.as_ref().unwrap()["archive_descriptor"]["size_bytes"]
                                .as_u64()
                                .unwrap();
                        assert_eq!(body.len() as u64, expected_size);
                        assert!(headers.contains(&format!("content-length: {expected_size}\r\n")));
                        if matches!(fault, Fault::PutFailure | Fault::CleanupFailure) {
                            respond(&mut stream, "503 Unavailable", b"{}");
                            continue;
                        }
                        state.staging = body;
                        respond(&mut stream, "204 No Content", b"");
                    } else if matches!(fault, Fault::ReadbackRedirect) {
                        write!(stream,"HTTP/1.1 302 Found\r\nLocation: {base}/object/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    } else {
                        if matches!(fault, Fault::ReadbackSlow) {
                            thread::sleep(Duration::from_millis(200));
                        }
                        let mut stored = state.staging.clone();
                        if matches!(fault, Fault::ReadbackDigest) {
                            stored[700] ^= 1;
                        }
                        if matches!(fault, Fault::ReadbackSize) {
                            stored.push(0);
                        }
                        if matches!(fault, Fault::ReadbackShort) {
                            stored.pop();
                        }
                        respond(&mut stream, "200 OK", &stored);
                    }
                    continue;
                }
                state.api_requests += 1;
                assert!(headers.contains("authorization: bearer runner-token-canary"));
                let mut value: Value = serde_json::from_slice(&body).unwrap();
                if method == "DELETE" {
                    assert_eq!(
                        value["binding_digest"],
                        state.binding.as_ref().unwrap()["_digest"]
                    );
                    state.aborted += 1;
                    if matches!(fault, Fault::CleanupFailure) {
                        respond(&mut stream, "503 Unavailable", b"{}");
                    } else {
                        state.staging.clear();
                        respond(&mut stream, "200 OK", br#"{"cleanup_confirmed":true}"#);
                    }
                } else if path.ends_with("/uploads") {
                    if matches!(fault, Fault::StaleFence) {
                        respond(&mut stream, "409 Conflict", b"{}");
                        continue;
                    }
                    let binding: Binding = serde_json::from_value(value.clone()).unwrap();
                    assert_eq!(binding.attempt_fence, 7);
                    assert_eq!(binding.attempt_id, "fattempt_source");
                    let digest = binding.digest().unwrap();
                    value["_digest"] = digest.clone().into();
                    state.binding = Some(value);
                    let expiry = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64
                        + 60000;
                    let ticket = json!({"upload_id":"upload_owned","binding_digest":if matches!(fault,Fault::TicketBinding) {"sha256:wrong"} else {&digest},"upload_url":if matches!(fault,Fault::TicketUrl){"file:///never-follow-canary".into()}else{format!("{base}/object/upload?signature=object-url-canary")},"download_url":format!("{base}/object/download?signature=object-url-canary"),"expires_at_ms":if matches!(fault,Fault::TicketExpired){1}else{expiry}});
                    respond(&mut stream, "200 OK", &serde_json::to_vec(&ticket).unwrap());
                } else if path.ends_with("/source-archive") {
                    let mut binding = state.binding.clone().unwrap();
                    let digest = binding.as_object_mut().unwrap().remove("_digest").unwrap();
                    for key in [
                        "job_id",
                        "attempt_id",
                        "attempt_fence",
                        "job_digest",
                        "archive_descriptor",
                    ] {
                        assert_eq!(value[key], binding[key]);
                    }
                    assert_eq!(value["binding_digest"], digest);
                    assert!(!state.staging.is_empty());
                    binding["kind"] = "source_archive".into();
                    binding["status"] = "source_materialized".into();
                    binding["binding_digest"] = digest;
                    binding["source_revision_id"] = "srev_measured".into();
                    binding["source_materialization_id"] = "smat_stored".into();
                    if matches!(fault, Fault::WrongAckAttempt) {
                        binding["attempt_id"] = "fattempt_other".into();
                    } else {
                        state.accepted += 1;
                        state.staging.clear();
                    }
                    respond(
                        &mut stream,
                        "200 OK",
                        &serde_json::to_vec(&binding).unwrap(),
                    );
                } else {
                    panic!("unexpected source-only request: {method} {path}");
                }
            }
        });
        Self {
            url,
            state,
            stopped,
            task: Some(task),
        }
    }
    fn api(&self) -> SourceArchiveApi {
        SourceArchiveApi::new(self.url.clone(), "runner-token-canary".into()).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}
fn run(server: &Server, claim: &ClaimedSourceArchive, root: &Path) -> Result<SourceArchiveOutput> {
    materialize_source_archive(
        &server.api(),
        &Fetcher {
            bytes: archive(),
            calls: AtomicUsize::new(0),
        },
        claim,
        root,
    )
}
fn assert_clean(root: &Path) {
    assert_eq!(std::fs::read_dir(root.join(".tmp")).unwrap().count(), 0);
}
#[test]
fn source_only_stores_and_remeasures_actual_bytes_without_running_application() {
    let server = Server::new(Fault::None);
    let root = scratch();
    let output = run(&server, &claim("site"), root.path()).unwrap();
    let value = serde_json::to_value(&output).unwrap();
    assert_eq!(output.archive_descriptor.observed_file_count, 3);
    assert_eq!(
        output.archive_descriptor.archive_format_version,
        ato_formation::source::RAW_TAR_FORMAT
    );
    assert_eq!(output.archive_descriptor.size_bytes, archive().len() as u64);
    for forbidden in [
        "compute_schema_ref",
        "contract_ref",
        "fully_satisfied",
        "verification_receipt",
        "formation_key",
    ] {
        assert!(value.get(forbidden).is_none());
    }
    let state = server.state.lock().unwrap();
    assert_eq!(state.accepted, 1);
    assert_eq!(state.object_requests, 2);
    assert!(!state.object_bearer_leaked);
    assert!(state.staging.is_empty());
    assert!(!root.path().join("SOURCE_ONLY_MUST_NOT_RUN").exists());
    assert_clean(root.path());
}
#[test]
fn invalid_ticket_never_follows_object_url_and_cleans_only_bound_owned_upload() {
    for fault in [Fault::TicketBinding, Fault::TicketExpired, Fault::TicketUrl] {
        let server = Server::new(fault);
        let root = scratch();
        let error = run(&server, &claim(""), root.path()).unwrap_err();
        assert!(!format!("{error:#}").contains("object-url-canary"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.object_requests, 0);
        assert_eq!(state.aborted, 1);
        assert_eq!(state.accepted, 0);
        assert_clean(root.path());
    }
}
#[test]
fn failed_store_readback_and_wrong_completion_cannot_produce_accepted_output() {
    for fault in [
        Fault::PutFailure,
        Fault::ReadbackDigest,
        Fault::ReadbackSize,
        Fault::ReadbackShort,
        Fault::ReadbackRedirect,
        Fault::WrongAckAttempt,
    ] {
        let server = Server::new(fault);
        let root = scratch();
        let error = run(&server, &claim("site"), root.path()).unwrap_err();
        assert!(!format!("{error:#}").contains("object-url-canary"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.accepted, 0);
        assert_eq!(state.aborted, 1, "{fault:?}: {error:#}");
        assert!(state.staging.is_empty());
        assert!(!state.object_bearer_leaked);
        assert_clean(root.path());
    }
}
#[test]
fn cleanup_failure_remains_failure_with_no_materialized_output() {
    let server = Server::new(Fault::CleanupFailure);
    let root = scratch();
    let error = run(&server, &claim(""), root.path()).unwrap_err();
    assert!(format!("{error:#}").contains("source_only_storage_cleanup_unconfirmed"));
    let state = server.state.lock().unwrap();
    assert_eq!(state.accepted, 0);
    assert_eq!(state.aborted, 1);
    assert_clean(root.path());
}
#[test]
fn stale_fence_is_refused_by_authority_before_any_object_transfer() {
    let server = Server::new(Fault::StaleFence);
    let root = scratch();
    assert!(run(&server, &claim(""), root.path()).is_err());
    let state = server.state.lock().unwrap();
    assert_eq!(state.object_requests, 0);
    assert_eq!(state.accepted, 0);
    assert_clean(root.path());
}
#[test]
fn wrong_job_digest_attempt_or_selection_cannot_reach_storage() {
    let server = Server::new(Fault::None);
    for which in 0..4 {
        let root = scratch();
        let mut claim = claim(if which == 3 { "does-not-exist" } else { "" });
        match which {
            0 => claim.job_digest = "sha256:wrong".into(),
            1 => claim.attempt_id = "../other".into(),
            2 => claim.job_id = "other_job".into(),
            _ => {}
        }
        let fetcher = Fetcher {
            bytes: archive(),
            calls: AtomicUsize::new(0),
        };
        assert!(materialize_source_archive(&server.api(), &fetcher, &claim, root.path()).is_err());
        if which < 3 {
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
        } else {
            assert_clean(root.path());
        }
    }
    assert_eq!(server.state.lock().unwrap().api_requests, 0);
}
#[test]
fn output_rejects_added_schema_or_verification_success_fields() {
    let server = Server::new(Fault::None);
    let root = scratch();
    let output = run(&server, &claim(""), root.path()).unwrap();
    for field in ["fully_satisfied", "compute_schema_ref", "contract_ref"] {
        let mut value = serde_json::to_value(&output).unwrap();
        value[field] = true.into();
        assert!(serde_json::from_value::<SourceArchiveOutput>(value).is_err());
    }
}

#[test]
fn frozen_transfer_deadline_still_allows_bounded_owned_cleanup() {
    let server = Server::new(Fault::ReadbackSlow);
    let root = scratch();
    let bytes = archive();
    let mut file = tempfile::tempfile_in(root.path()).unwrap();
    file.write_all(&bytes).unwrap();
    let verified = FileVerifiedArchive::verify(
        file,
        &crate::job::digest(&bytes),
        bytes.len() as u64,
        SourceLimits::default(),
    )
    .unwrap();
    let claim = claim("");
    let binding = Binding {
        job_id: claim.job_id,
        attempt_id: claim.attempt_id,
        attempt_fence: claim.attempt_fence,
        job_digest: claim.job_digest,
        archive_descriptor: ArchiveDescriptor {
            archive_format_version: verified.archive_format().into(),
            source_archive_digest: verified.archive_digest().into(),
            size_bytes: verified.archive_bytes(),
            source_tree_digest: verified.tree_digest().into(),
            resolver_contract_version: verified.resolver_contract().into(),
            subdirectory: String::new(),
            source_closure_ref: verified.closure_ref("").unwrap().to_string(),
            observed_file_count: verified.expanded_file_count() as u64,
            observed_uncompressed_bytes: verified.expanded_bytes(),
        },
    };
    let api = server.api();
    let deadline = Instant::now() + Duration::from_secs(2);
    let ticket = api.authorize(&binding, deadline).unwrap();
    let start = Instant::now();
    assert!(
        api.transfer_and_verify(
            &ticket,
            &binding,
            &verified,
            root.path(),
            start + Duration::from_millis(50)
        )
        .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    api.abort(&ticket, &binding, start).unwrap();
    assert_eq!(server.state.lock().unwrap().aborted, 1);
    assert_clean(root.path());
}

#[test]
fn dedicated_claim_never_falls_back_to_the_application_claim_route() {
    for valid in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let expected = claim("site");
        let payload = if valid {
            json!({"job_id":expected.job_id,"attempt_id":expected.attempt_id,"attempt_fence":expected.attempt_fence,"job_digest":expected.job_digest,"job":expected.job})
        } else {
            json!({})
        };
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (method, path, headers, _) = request(&mut stream).unwrap();
            assert_eq!(method, "POST");
            assert_eq!(
                path,
                "/v1/internal/formation/source-archives/claim?worker_id=source_worker"
            );
            assert!(headers.contains("authorization: bearer runner-token-canary"));
            respond(
                &mut stream,
                if valid { "200 OK" } else { "404 Not Found" },
                &serde_json::to_vec(&payload).unwrap(),
            );
        });
        let api = SourceArchiveApi::new(url, "runner-token-canary".into()).unwrap();
        let result = api.claim_next("source_worker");
        if valid {
            assert_eq!(result.unwrap().unwrap().attempt_fence, 7);
        } else {
            assert!(result.is_err());
        }
        task.join().unwrap();
    }
}

#[test]
fn application_executor_refuses_source_output_before_fetch_or_ledger_reservation() {
    struct NoPack;
    impl crate::job::TreePacker for NoPack {
        fn pack(&self, _: &Path) -> Result<Vec<u8>> {
            panic!("source-only must not reach application packing")
        }
    }
    let root = scratch();
    let fetcher = Fetcher {
        bytes: archive(),
        calls: AtomicUsize::new(0),
    };
    let api = crate::api::FormationApi::new(
        reqwest::blocking::Client::new(),
        "http://127.0.0.1:1".into(),
        "unused".into(),
    );
    let context = crate::job::JobContext {
        api: &api,
        fetcher: &fetcher,
        packer: &NoPack,
        work_root: root.path(),
        shim: Path::new("/nonexistent-source-only-shim"),
        worker_id: "source_worker",
        limits: crate::sandbox::BuildLimits::default(),
        source_limits: SourceLimits::default(),
    };
    let claim = claim("");
    let result = crate::job::run_claimed_job(
        &context,
        &crate::build::BuildAttempt {
            job_id: claim.job_id,
            attempt_id: claim.attempt_id,
            attempt_fence: claim.attempt_fence,
        },
        &serde_json::to_value(claim.job).unwrap(),
        "",
        "",
        false,
    );
    let error = result.err().unwrap();
    assert!(
        error
            .to_string()
            .contains("source_archive_requires_source_only_executor")
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    assert!(!root.path().join("attempt-records").exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn exact_job_claim_is_filtered_and_rejects_an_unrelated_assignment() {
    for mismatch in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let expected = claim("");
        let requested = if mismatch {
            "fjob_other".to_owned()
        } else {
            expected.job_id.clone()
        };
        let query = requested.clone();
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, path, _, _) = request(&mut stream).unwrap();
            assert_eq!(
                path,
                format!(
                    "/v1/internal/formation/source-archives/claim?worker_id=source_worker&job_id={query}"
                )
            );
            let payload = json!({"job_id":expected.job_id,"attempt_id":expected.attempt_id,"attempt_fence":expected.attempt_fence,"job_digest":expected.job_digest,"job":expected.job});
            respond(
                &mut stream,
                "200 OK",
                &serde_json::to_vec(&payload).unwrap(),
            );
        });
        let api = SourceArchiveApi::new(url, "runner-token-canary".into()).unwrap();
        let result = api.claim("source_worker", &requested);
        if mismatch {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap().job_id, requested);
        }
        task.join().unwrap();
    }
    let api = SourceArchiveApi::new("http://127.0.0.1:1".into(), "unused".into()).unwrap();
    assert!(
        api.claim("worker", "../other")
            .unwrap_err()
            .to_string()
            .contains("source_archive_job_id_invalid")
    );
}
