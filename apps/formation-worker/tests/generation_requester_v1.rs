use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use ato_formation::generation::{GenerationDraft, compile};
use ato_formation_worker::generation_provider::{
    GenerationAnswer, GenerationPoint, GenerationProvider,
};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, Submission, prepare_submission,
    serve_generation,
};
use serde_json::{Value, json};

fn submission() -> (tempfile::TempDir, Submission) {
    submission_with_source(|_| {})
}

fn submission_with_source(edit: impl FnOnce(&Path)) -> (tempfile::TempDir, Submission) {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let root = tempfile::tempdir_in(scratch).unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    ato_formation_worker::job::copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/runtime-network/notes"),
        &source,
    )
    .unwrap();
    std::fs::copy(source.join("app.py"), source.join("repaired.py")).unwrap();
    edit(&source);
    let submission = prepare_submission(
        &source,
        &[],
        None,
        &root.path().join("work"),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "denied".into(),
            allow_managed: false,
            decision: None,
            generation: None,
        },
        SatisfyBudget::ceilings(2, "first_pass"),
        "generation_requester_test",
    )
    .unwrap();
    (root, submission)
}

fn admitted(submission: &Submission) -> Value {
    admitted_entrypoint(submission, "repair")
}

fn admitted_entrypoint(submission: &Submission, entrypoint: &str) -> Value {
    let draft: GenerationDraft = serde_json::from_value(json!({
        "schema": "ato.formation-derivation-draft/1",
        "operation": "python_script", "entrypoint_id": entrypoint,
    }))
    .unwrap();
    let compiled = compile(
        submission.request.policy.generation.as_ref().unwrap(),
        &submission.request.authorized_derivations[0].capsule_toml,
        &submission.request.source.closure_ref,
        &submission.request.base_contract_ref,
        &draft,
    )
    .unwrap();
    json!({"generation": {
        "outcome": "admitted", "draft": draft,
        "derivation_ref": compiled.derivation_ref,
        "capsule_toml": compiled.capsule_toml,
    }})
}

#[test]
fn only_files_in_the_frozen_source_can_be_authorized() {
    let (root, mut submission) = submission();
    // A later source write is not part of the immutable snapshot.
    std::fs::write(root.path().join("source/later.py"), "pass\n").unwrap();
    assert!(
        submission
            .authorize_generation(BTreeMap::from([("late".into(), "later.py".into())]), 30_000)
            .is_err()
    );
    assert!(submission.request.policy.generation.is_none());
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "repaired.py".into())]),
            30_000,
        )
        .unwrap();
}

#[test]
fn admitted_draft_is_recompiled_and_reused_without_changing_frozen_request_or_k() {
    let (_root, mut submission) = submission();
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "repaired.py".into())]),
            30_000,
        )
        .unwrap();
    let request = serde_json::to_value(&submission.request).unwrap();
    let status = admitted(&submission);
    submission.accept_generated_candidate(&status).unwrap();
    submission.accept_generated_candidate(&status).unwrap();
    assert_eq!(serde_json::to_value(&submission.request).unwrap(), request);
    assert_eq!(submission.contracts.len(), 2);
    assert!(
        submission
            .contracts
            .values()
            .all(|k| k == &submission.request.base_contract)
    );
}

#[test]
fn receiver_claims_cannot_authorize_a_d_or_rewrite_a_recipe() {
    let (_root, mut submission) = submission();
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "repaired.py".into())]),
            30_000,
        )
        .unwrap();
    let status = admitted(&submission);
    for field in ["derivation_ref", "capsule_toml"] {
        let mut forged = status.clone();
        forged["generation"][field] = json!("unauthorized");
        assert!(submission.accept_generated_candidate(&forged).is_err());
        assert_eq!(submission.contracts.len(), 1);
    }
    let mut forged = status;
    forged["generation"]["draft"]["contract"] = json!({"requirements": []});
    assert!(submission.accept_generated_candidate(&forged).is_err());
}

#[test]
fn nested_regular_files_are_authorized_from_the_snapshot_only() {
    let (root, mut submission) = submission_with_source(|source| {
        std::fs::create_dir_all(source.join("src/nested")).unwrap();
        std::fs::copy(source.join("app.py"), source.join("src/nested/repaired.py")).unwrap();
        std::fs::create_dir(source.join("directory.py")).unwrap();
    });
    // The original directory can change or disappear after the archive is frozen.
    std::fs::remove_file(root.path().join("source/src/nested/repaired.py")).unwrap();
    std::fs::write(root.path().join("source/src/nested/later.py"), "pass\n").unwrap();
    for path in ["src/nested/later.py", "directory.py", "missing.py"] {
        assert!(
            submission
                .authorize_generation(BTreeMap::from([("repair".into(), path.into())]), 30_000,)
                .is_err(),
            "{path}"
        );
        assert!(submission.request.policy.generation.is_none());
    }
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "src/nested/repaired.py".into())]),
            30_000,
        )
        .unwrap();
    let status = admitted(&submission);
    submission.accept_generated_candidate(&status).unwrap();
    assert_eq!(submission.contracts.len(), 2);
}

#[cfg(unix)]
#[test]
fn inventory_neither_authorizes_symlink_files_nor_traverses_symlink_directories() {
    let (_root, mut submission) = submission_with_source(|source| {
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::copy(source.join("app.py"), source.join("nested/repaired.py")).unwrap();
        std::os::unix::fs::symlink("repaired.py", source.join("linked.py")).unwrap();
        std::os::unix::fs::symlink("nested", source.join("linked_dir")).unwrap();
        std::os::unix::fs::symlink("../repaired.py", source.join("nested/linked.py")).unwrap();
    });
    for path in ["linked.py", "linked_dir/repaired.py", "nested/linked.py"] {
        assert!(
            submission
                .authorize_generation(BTreeMap::from([("repair".into(), path.into())]), 30_000,)
                .is_err(),
            "{path}"
        );
        assert!(submission.request.policy.generation.is_none());
    }
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "nested/repaired.py".into())]),
            30_000,
        )
        .unwrap();
}

#[test]
fn receiver_cannot_accumulate_different_policy_valid_generated_derivations() {
    let (_root, mut submission) = submission_with_source(|source| {
        std::fs::copy(source.join("app.py"), source.join("alternative.py")).unwrap();
    });
    submission
        .authorize_generation(
            BTreeMap::from([
                ("repair".into(), "repaired.py".into()),
                ("alternative".into(), "alternative.py".into()),
            ]),
            30_000,
        )
        .unwrap();
    let first = admitted(&submission);
    let second = admitted_entrypoint(&submission, "alternative");
    assert_ne!(
        first["generation"]["derivation_ref"],
        second["generation"]["derivation_ref"]
    );
    submission.accept_generated_candidate(&first).unwrap();
    let contracts = submission.contracts.clone();
    let frozen = serde_json::to_value(&submission.request).unwrap();
    // Absence or a contradictory non-admitted view must not reset the pin.
    for intermediate in [json!({}), json!({"generation": {"outcome": "invalid"}})] {
        submission
            .accept_generated_candidate(&intermediate)
            .unwrap();
        let error = submission.accept_generated_candidate(&second).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("admitted generated candidate changed")
        );
        assert_eq!(submission.contracts, contracts);
    }
    submission.accept_generated_candidate(&first).unwrap();
    assert_eq!(submission.contracts, contracts);
    assert_eq!(serde_json::to_value(&submission.request).unwrap(), frozen);
}

#[test]
fn a_rejected_echo_does_not_pin_the_generation_slot() {
    let (_root, mut submission) = authorized_submission();
    let valid = admitted(&submission);
    let mut forged = valid.clone();
    forged["generation"]["capsule_toml"] = json!("forged");
    assert!(submission.accept_generated_candidate(&forged).is_err());
    submission.accept_generated_candidate(&valid).unwrap();
    assert_eq!(submission.contracts.len(), 2);
}

fn authorized_submission() -> (tempfile::TempDir, Submission) {
    let (root, mut submission) = submission();
    submission
        .authorize_generation(
            BTreeMap::from([("repair".into(), "repaired.py".into())]),
            30_000,
        )
        .unwrap();
    (root, submission)
}

fn open_point() -> Value {
    json!({
        "status": "running",
        "generation_point": {
            "revision": 41, "expires_at": "2099-01-01T00:00:30Z",
            "claimed": false, "entrypoint_ids": ["repair"], "failures": []
        }
    })
}

#[derive(Default)]
struct RecordingProvider {
    points: Mutex<Vec<GenerationPoint>>,
}

impl GenerationProvider for RecordingProvider {
    fn generate(&self, point: &GenerationPoint) -> GenerationAnswer {
        self.points.lock().unwrap().push(point.clone());
        GenerationAnswer::Fallback { reason: "declined" }
    }
}

const CLAIM_PATH: &str = "/v1/runtime-network/satisfy/test/generation/claim";
const ANSWER_PATH: &str = "/v1/runtime-network/satisfy/test/generation";

/// Loopback only. `None` closes the connection without a response, modelling
/// a successful durable write whose response was lost before the requester saw it.
struct Coordinator {
    url: String,
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Vec<(String, Value)>>,
}

impl Coordinator {
    fn new(mut reply: impl FnMut(&str, &Value) -> Option<(u16, Value)> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopping) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::new();
            loop {
                assert!(
                    Instant::now() < deadline,
                    "coordinator mock was not stopped"
                );
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS can inherit O_NONBLOCK from the listener.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut first = String::new();
                        assert_ne!(reader.read_line(&mut first).unwrap(), 0);
                        let parts: Vec<_> = first.split_whitespace().collect();
                        assert_eq!(parts[0], "POST");
                        let path = parts[1].to_owned();
                        let mut length = 0usize;
                        loop {
                            let mut line = String::new();
                            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                            if line == "\r\n" {
                                break;
                            }
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                length = value.trim().parse().unwrap();
                            }
                        }
                        assert!(length <= 8192);
                        let mut bytes = vec![0; length];
                        reader.read_exact(&mut bytes).unwrap();
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        let response = reply(&path, &body);
                        requests.push((path, body));
                        if let Some((status, value)) = response {
                            let body = value.to_string();
                            write!(stream,
                                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()).unwrap();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping.recv_timeout(Duration::from_millis(2)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("coordinator mock accept failed: {error}"),
                }
            }
            requests
        });
        Self { url, stop, thread }
    }

    fn client(&self) -> Client {
        Client::new(&self.url, "requester-test-key").unwrap()
    }

    fn finish(self) -> Vec<(String, Value)> {
        self.stop.send(()).unwrap();
        self.thread.join().unwrap()
    }
}

#[test]
fn serve_claims_before_generation_and_submits_the_returned_revision() {
    let (_root, mut submission) = authorized_submission();
    let server = Coordinator::new(|path, body| match path {
        CLAIM_PATH => {
            assert_eq!(*body, json!({"revision": 41}));
            Some((200, json!({"revision": 42})))
        }
        ANSWER_PATH => {
            assert_eq!(*body, json!({"revision": 42, "fallback": "declined"}));
            Some((200, json!({"outcome": "declined", "revision": 43})))
        }
        _ => panic!("unexpected path {path}"),
    });
    let provider = RecordingProvider::default();
    let call = serve_generation(
        &mut submission,
        &server.client(),
        "test",
        &open_point(),
        &provider,
    )
    .unwrap()
    .unwrap();
    assert_eq!(call["submission_accepted"], true);
    assert_eq!(
        call["answer"],
        json!({"revision": 42, "fallback": "declined"})
    );
    let points = provider.points.lock().unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].revision, 42);
    assert_eq!(
        server
            .finish()
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        [CLAIM_PATH, ANSWER_PATH]
    );
}

#[test]
fn competing_requesters_only_invoke_the_provider_after_winning_the_claim() {
    let (_root_a, mut first) = authorized_submission();
    let (_root_b, mut second) = authorized_submission();
    let mut claimed = false;
    let server = Coordinator::new(move |path, _| match path {
        CLAIM_PATH if !claimed => {
            claimed = true;
            Some((200, json!({"revision": 42})))
        }
        CLAIM_PATH => Some((409, json!({"error": "generation_closed"}))),
        ANSWER_PATH => Some((200, json!({"outcome": "declined"}))),
        _ => panic!("unexpected path {path}"),
    });
    let provider = RecordingProvider::default();
    let client = server.client();
    let barrier = std::sync::Barrier::new(2);
    let invoke = |submission: &mut Submission| {
        barrier.wait();
        serve_generation(submission, &client, "test", &open_point(), &provider).unwrap()
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| invoke(&mut first));
        let b = scope.spawn(|| invoke(&mut second));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a.is_some(), b.is_some());
    assert_eq!(provider.points.lock().unwrap().len(), 1);
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == CLAIM_PATH)
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == ANSWER_PATH)
            .count(),
        1
    );
}

#[test]
fn lost_claim_or_answer_response_never_causes_another_model_call_after_restart() {
    for lose_claim_response in [true, false] {
        let (_root, mut submission) = authorized_submission();
        let mut claimed = false;
        let server = Coordinator::new(move |path, _| match path {
            CLAIM_PATH if !claimed => {
                claimed = true;
                if lose_claim_response {
                    None
                } else {
                    Some((200, json!({"revision": 42})))
                }
            }
            CLAIM_PATH => Some((409, json!({"error": "generation_closed"}))),
            ANSWER_PATH => None,
            _ => panic!("unexpected path {path}"),
        });
        let provider = RecordingProvider::default();
        let client = server.client();
        let result =
            serve_generation(&mut submission, &client, "test", &open_point(), &provider).unwrap();
        if lose_claim_response {
            assert!(result.is_none());
        } else {
            assert_eq!(result.unwrap()["submission_accepted"], false);
        }
        let (_restarted_root, mut restarted) = authorized_submission();
        // Even a stale pre-claim view cannot bypass the Coordinator's durable claim.
        assert!(
            serve_generation(&mut restarted, &client, "test", &open_point(), &provider)
                .unwrap()
                .is_none()
        );
        let mut latest = open_point();
        latest["generation_point"]["claimed"] = json!(true);
        assert!(
            serve_generation(&mut restarted, &client, "test", &latest, &provider)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            provider.points.lock().unwrap().len(),
            usize::from(!lose_claim_response)
        );
        let requests = server.finish();
        assert_eq!(
            requests
                .iter()
                .filter(|(path, _)| path == CLAIM_PATH)
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(path, _)| path == ANSWER_PATH)
                .count(),
            usize::from(!lose_claim_response)
        );
    }
}

#[test]
fn failed_or_malformed_claims_do_not_invoke_the_provider() {
    for (status, body) in [
        (409, json!({"error": "generation_stale"})),
        (500, json!({"error": "unavailable"})),
        (200, json!({})),
        (200, json!({"revision": "42"})),
        (200, json!({"revision": -1})),
    ] {
        let (_root, mut submission) = authorized_submission();
        let server = Coordinator::new(move |path, _| {
            assert_eq!(path, CLAIM_PATH);
            Some((status, body.clone()))
        });
        let provider = RecordingProvider::default();
        let result = serve_generation(
            &mut submission,
            &server.client(),
            "test",
            &open_point(),
            &provider,
        );
        if status == 200 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_none());
        }
        assert!(provider.points.lock().unwrap().is_empty());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn unknown_closed_or_claimed_points_and_receiver_only_authority_cannot_generate() {
    let (_root, mut submission) = authorized_submission();
    let server = Coordinator::new(|_, _| panic!("invalid frontier must not contact Coordinator"));
    let provider = RecordingProvider::default();
    let client = server.client();
    for state in ["unknown", "stopped", "satisfied", "failed"] {
        let mut status = open_point();
        status["status"] = json!(state);
        assert!(
            serve_generation(&mut submission, &client, "test", &status, &provider)
                .unwrap()
                .is_none()
        );
    }
    let mut claimed = open_point();
    claimed["generation_point"]["claimed"] = json!(true);
    assert!(
        serve_generation(&mut submission, &client, "test", &claimed, &provider)
            .unwrap()
            .is_none()
    );
    for entrypoints in [
        json!(["evil"]),
        json!(["repair", "evil"]),
        json!(["repair", "repair"]),
    ] {
        let mut status = open_point();
        status["generation_point"]["entrypoint_ids"] = entrypoints;
        assert!(serve_generation(&mut submission, &client, "test", &status, &provider).is_err());
    }
    let mut status = open_point();
    status["policy"] = serde_json::to_value(&submission.request.policy).unwrap();
    submission.request.policy.generation = None;
    assert!(serve_generation(&mut submission, &client, "test", &status, &provider).is_err());
    assert!(provider.points.lock().unwrap().is_empty());
    assert!(server.finish().is_empty());
}
