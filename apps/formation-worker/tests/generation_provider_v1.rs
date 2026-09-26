//! Typed generation through the real, shared HTTP transport, using loopback only.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ato_formation_worker::generation_provider::*;
use serde_json::{Value, json};

const KEY: &str = "generation-key-canary";

fn point_value() -> Value {
    json!({
        "revision": 7,
        "expires_at": "2026-09-26T00:00:30Z",
        "entrypoint_ids": ["ep_A1", "ep_B2"],
        "failures": [{"status": "fail", "failure_code": "http_status_mismatch"}]
    })
}

fn point() -> GenerationPoint {
    serde_json::from_value(point_value()).unwrap()
}

fn choice(selected: &str, probabilities: Value) -> Value {
    json!({"type": "choice", "choice": selected, "confidence": 0.9,
           "probabilities": probabilities})
}

fn response() -> Value {
    json!({
        "model": DEFAULT_GENERATION_MODEL,
        "answers": {
            "operation": choice("python_script", json!({"python_script": 0.9, "decline": 0.1})),
            "entrypoint": choice("ep_B2", json!({"ep_A1": 0.1, "ep_B2": 0.8, "none": 0.1}))
        },
        "usage": {"input_tokens": 123, "output_tokens": 8}
    })
}

/// Keep accepting until generate returns, so retries are observable too.
struct Server {
    url: String,
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Vec<String>>,
}

impl Server {
    fn new(status: u16, body: String, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopping) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "loopback server was not stopped");
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut head = String::new();
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                            head.push_str(&line);
                            if line == "\r\n" {
                                break;
                            }
                        }
                        assert!(length <= 48 * 1024);
                        let mut request = vec![0; length];
                        reader.read_exact(&mut request).unwrap();
                        requests.push(head + &String::from_utf8(request).unwrap());
                        if !delay.is_zero() && stopping.recv_timeout(delay).is_ok() {
                            break;
                        }
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping.recv_timeout(Duration::from_millis(2)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("loopback accept failed: {error}"),
                }
            }
            requests
        });
        Self { url, stop, thread }
    }

    fn finish(self) -> Vec<String> {
        self.stop.send(()).unwrap();
        self.thread.join().unwrap()
    }
}

fn provider(url: &str, timeout: Duration) -> JevGenerationProvider {
    JevGenerationProvider::new(url, KEY, DEFAULT_GENERATION_MODEL, timeout).unwrap()
}

fn invalid() -> GenerationAnswer {
    GenerationAnswer::Fallback { reason: "invalid" }
}

#[test]
fn one_http_request_composes_a_draft_with_separate_bounded_provenance() {
    let server = Server::new(200, response().to_string(), Duration::ZERO);
    let answer = provider(&server.url, Duration::from_secs(5)).generate(&point());
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.starts_with("POST /v1/systemone "));
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}"))
    );
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(!body.contains(KEY));
    let sent: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        sent,
        generation_request(DEFAULT_GENERATION_MODEL, &point()).unwrap()
    );
    assert_eq!(
        answer.submission(7),
        json!({
            "revision": 7,
            "draft": {"schema": "ato.formation-derivation-draft/1",
                      "operation": "python_script", "entrypoint_id": "ep_B2"},
            "provenance": {"provider": "jev", "model": DEFAULT_GENERATION_MODEL,
                           "prompt_version": GENERATION_PROMPT_VERSION,
                           "usage": {"input_tokens": 123, "output_tokens": 8}}
        })
    );
    assert!(!answer.submission(7).to_string().contains(KEY));
}

#[test]
fn projection_excludes_canaries_even_in_unknown_or_unbounded_failure_strings() {
    let mut raw = point_value();
    for field in [
        "source",
        "raw_logs",
        "K",
        "permissions",
        "runtime_facts",
        "host",
        "path",
        "receipts",
        "unrelated",
        "base_derivation_ref",
        "entrypoints",
    ] {
        raw[field] = json!(format!("{field}_PRIVATE_CANARY"));
    }
    raw["expires_at"] = json!("EXPIRY_PRIVATE_CANARY");
    raw["revision"] = json!(765432109);
    raw["claimed"] = json!(true);
    raw["failures"] = json!([
        {"status": "fail", "failure_code": "http_body_digest_mismatch", "logs": "LOG_PRIVATE_CANARY"},
        {"status": "STATUS_PRIVATE_CANARY", "failure_code": "SECRET_PRIVATE_CANARY"},
        {"status": "inconclusive", "failure_code": null},
        {"status": "x".repeat(100_000), "failure_code": "x".repeat(100_000)},
        {"status": "expired", "failure_code": "timeout"}
    ]);
    let point = GenerationPoint::from_status(&json!({"generation_point": raw})).unwrap();
    assert!(point.claimed);
    let server = Server::new(200, response().to_string(), Duration::ZERO);
    assert!(matches!(
        provider(&server.url, Duration::from_secs(5)).generate(&point),
        GenerationAnswer::Draft { .. }
    ));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    let (_, body) = requests[0].split_once("\r\n\r\n").unwrap();
    assert!(!body.contains("PRIVATE_CANARY"));
    assert!(!body.contains("765432109"));
    assert!(body.len() < 4096);
    let sent: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        sent["state"],
        json!({
            "entrypoint_ids": ["ep_A1", "ep_B2"],
            "failures": [
                {"status": "fail", "failure_code": "http_body_digest_mismatch"},
                {"status": "other", "failure_code": "other"},
                {"status": "inconclusive", "failure_code": null},
                {"status": "other", "failure_code": "other"},
                {"status": "expired", "failure_code": "timeout"}
            ]
        })
    );
    for field in [
        "claimed",
        "revision",
        "expires_at",
        "source",
        "raw_logs",
        "K",
        "permissions",
        "runtime_facts",
        "host",
        "path",
        "receipts",
        "unrelated",
    ] {
        assert!(sent["state"].get(field).is_none(), "{field}");
    }
}

#[test]
fn request_asks_exactly_two_choices_over_the_frozen_domain() {
    let request = generation_request(DEFAULT_GENERATION_MODEL, &point()).unwrap();
    assert_eq!(request["questions"].as_object().unwrap().len(), 2);
    for (question, expected) in [
        ("operation", vec!["decline", "python_script"]),
        ("entrypoint", vec!["ep_A1", "ep_B2", "none"]),
    ] {
        assert_eq!(request["questions"][question]["type"], "choice");
        let keys: Vec<&str> = request["questions"][question]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, expected);
        assert_eq!(
            request["questions"][question]["instructions"],
            GENERATION_INSTRUCTIONS
        );
    }
    assert!(
        !request["questions"]
            .to_string()
            .contains("http_status_mismatch")
    );
}

#[test]
fn invalid_domains_never_call_the_transport() {
    let mut points = Vec::new();
    for id in [
        "",
        "none",
        "bad-id",
        "a.py",
        "a/b",
        "é",
        "a b",
        "a;sh",
        &"x".repeat(33),
    ] {
        let mut p = point();
        p.entrypoint_ids = vec![id.to_owned()];
        points.push(p);
    }
    for ids in [
        vec![],
        vec!["duplicate".to_owned(); 2],
        (0..17).map(|i| format!("e{i}")).collect(),
    ] {
        let mut p = point();
        p.entrypoint_ids = ids;
        points.push(p);
    }
    let mut too_many_failures = point();
    too_many_failures.failures = vec![too_many_failures.failures[0].clone(); 17];
    points.push(too_many_failures);
    let server = Server::new(200, response().to_string(), Duration::ZERO);
    let provider = provider(&server.url, Duration::from_secs(5));
    for p in points {
        assert_eq!(
            generation_request(DEFAULT_GENERATION_MODEL, &p),
            Err("invalid")
        );
        assert_eq!(provider.generate(&p), invalid());
        assert_eq!(
            validate_response(DEFAULT_GENERATION_MODEL, &p, &response()),
            invalid()
        );
    }
    assert!(server.finish().is_empty());
}

#[test]
fn inclusive_domain_bounds_and_claim_default_are_supported() {
    let mut p = point();
    assert!(!p.claimed);
    p.entrypoint_ids = (0..16).map(|i| format!("e{i}")).collect();
    p.entrypoint_ids[0] = "X".repeat(32);
    p.failures = vec![p.failures[0].clone(); 16];
    assert!(generation_request(DEFAULT_GENERATION_MODEL, &p).is_ok());
    p.entrypoint_ids = vec!["_".into()];
    p.failures.clear();
    assert!(generation_request(DEFAULT_GENERATION_MODEL, &p).is_ok());
    assert!(GenerationPoint::from_status(&json!({})).is_none());
    assert!(GenerationPoint::from_status(&json!({"generation_point": {}})).is_none());
    assert!(GenerationPoint::from_status(&json!({"generation_point": null})).is_none());
}

#[test]
fn malformed_extra_or_arbitrary_shell_and_contract_answers_are_rejected_over_http() {
    let mut bodies = vec!["not json".into(), "null".into(), "{}".into(),
        json!({"draft": {"operation": "shell", "argv": ["sh", "-c", "evil"], "K": {"rewrite": true}}}).to_string()];
    for (pointer, replacement) in [
        ("/model", json!("jev-1.14.0")),
        ("/answers", json!({})),
        ("/answers/operation/type", json!("text")),
        ("/answers/entrypoint/type", json!("score")),
        ("/answers/operation/choice", json!("sh -c evil")),
        (
            "/answers/operation/choice",
            json!({"argv": ["sh"], "K": {}}),
        ),
        ("/answers/entrypoint/choice", json!("unoffered")),
        ("/answers/entrypoint/choice", json!("none")),
        ("/answers/entrypoint/choice", json!("../../secret.py")),
        (
            "/answers/operation/probabilities",
            json!({"python_script": 1.0}),
        ),
        (
            "/answers/entrypoint/probabilities",
            json!({"ep_A1": 0.1, "ep_B2": 0.8, "extra": 0.1}),
        ),
        (
            "/answers/operation/probabilities",
            json!({"python_script": 0.8, "decline": 0.1, "shell": 0.1}),
        ),
        ("/answers/operation/confidence", json!(1.01)),
        ("/answers/entrypoint/confidence", json!(-0.1)),
        ("/answers/operation/confidence", json!("NaN")),
        ("/answers/entrypoint/confidence", json!(null)),
        ("/answers/entrypoint/probabilities/ep_B2", json!(1.1)),
        ("/answers/operation/probabilities/decline", json!(-0.1)),
        ("/answers/entrypoint/probabilities/none", json!("Infinity")),
        ("/answers/entrypoint/probabilities/none", json!(null)),
    ] {
        let mut raw = response();
        *raw.pointer_mut(pointer).unwrap() = replacement;
        bodies.push(raw.to_string());
    }
    for pointer in ["", "/answers", "/answers/operation", "/answers/entrypoint"] {
        let mut raw = response();
        raw.pointer_mut(pointer).unwrap()["K"] = json!({"argv": ["sh", "-c", "evil"]});
        bodies.push(raw.to_string());
    }
    bodies.push(response().to_string().replace("0.9", "1e999"));
    for body in bodies {
        let server = Server::new(200, body.clone(), Duration::ZERO);
        assert_eq!(
            provider(&server.url, Duration::from_secs(5)).generate(&point()),
            invalid(),
            "{body}"
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn usage_requires_only_unsigned_token_counts_and_cannot_smuggle_model_text() {
    for usage in [
        json!(null),
        json!("SECRET"),
        json!({}),
        json!({"input_tokens": -1, "output_tokens": 1}),
        json!({"input_tokens": 1.5, "output_tokens": 1}),
        json!({"input_tokens": "123", "output_tokens": 1}),
        json!({"input_tokens": 123, "output_tokens": 1, "source": "SECRET"}),
    ] {
        let mut raw = response();
        raw["usage"] = usage;
        assert_eq!(
            validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
            invalid()
        );
    }
    let mut raw = response();
    raw.as_object_mut().unwrap().remove("usage");
    assert_eq!(
        validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
        invalid()
    );
    raw["usage"] = json!({"input_tokens": 0, "output_tokens": 0});
    assert!(matches!(
        validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
        GenerationAnswer::Draft { .. }
    ));
}

#[test]
fn model_is_pinned_to_the_configuration_not_a_prefix_or_the_default() {
    let model = "jev-1.14.0";
    let mut raw = response();
    raw["model"] = json!(model);
    assert_eq!(
        validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
        invalid()
    );
    let server = Server::new(200, raw.to_string(), Duration::ZERO);
    let provider =
        JevGenerationProvider::new(&server.url, KEY, model, Duration::from_secs(5)).unwrap();
    let GenerationAnswer::Draft { provenance, .. } = provider.generate(&point()) else {
        panic!("configured pin rejected")
    };
    assert_eq!(provenance["model"], model);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    let sent: Value = serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(sent["model"], model);
    assert_eq!(validate_response(model, &point(), &response()), invalid());
}

#[test]
fn decline_requires_none_and_never_yields_a_draft() {
    let mut raw = response();
    raw["answers"]["operation"]["choice"] = json!("decline");
    assert_eq!(
        validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
        invalid()
    );
    raw["answers"]["entrypoint"]["choice"] = json!("none");
    let answer = validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw);
    assert_eq!(answer, GenerationAnswer::Fallback { reason: "declined" });
    assert_eq!(
        answer.submission(9),
        json!({"revision": 9, "fallback": "declined"})
    );
}

#[test]
fn failed_timeout_redirect_and_oversized_http_responses_fall_back_without_retry() {
    for (status, body, delay, timeout, reason) in [
        (
            500,
            "{}".into(),
            Duration::ZERO,
            Duration::from_secs(5),
            "provider_error",
        ),
        (
            401,
            "{}".into(),
            Duration::ZERO,
            Duration::from_secs(5),
            "provider_error",
        ),
        (
            302,
            "{}".into(),
            Duration::ZERO,
            Duration::from_secs(5),
            "provider_error",
        ),
        (
            200,
            response().to_string(),
            Duration::from_secs(2),
            Duration::from_millis(150),
            "timeout",
        ),
        (
            200,
            "x".repeat(MAX_RESPONSE_BYTES + 1),
            Duration::ZERO,
            Duration::from_secs(5),
            "invalid",
        ),
    ] {
        let server = Server::new(status, body, delay);
        assert_eq!(
            provider(&server.url, timeout).generate(&point()),
            GenerationAnswer::Fallback { reason }
        );
        assert_eq!(server.finish().len(), 1);
    }
    // Reserve a port without listening; no external service can receive this request.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        provider(&address, Duration::from_secs(1)).generate(&point()),
        GenerationAnswer::Fallback {
            reason: "provider_error"
        }
    );
    let mut raw = response();
    raw["extra"] = json!("x".repeat(MAX_RESPONSE_BYTES));
    assert_eq!(
        validate_response(DEFAULT_GENERATION_MODEL, &point(), &raw),
        invalid()
    );
}

#[test]
fn constructor_requires_a_key_and_explicit_bounded_timeout() {
    for timeout in [Duration::ZERO, Duration::from_millis(30_001)] {
        assert!(
            JevGenerationProvider::new(
                "http://127.0.0.1:9",
                KEY,
                DEFAULT_GENERATION_MODEL,
                timeout
            )
            .is_err()
        );
    }
    for key in ["", " \n\t"] {
        assert!(
            JevGenerationProvider::new(
                "http://127.0.0.1:9",
                key,
                DEFAULT_GENERATION_MODEL,
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
    assert!(
        JevGenerationProvider::new(
            "http://127.0.0.1:9",
            KEY,
            DEFAULT_GENERATION_MODEL,
            Duration::from_secs(30)
        )
        .is_ok()
    );
    for model in ["", "gpt-x", "jev-1.13.0\nSECRET"] {
        assert!(
            JevGenerationProvider::new("http://127.0.0.1:9", KEY, model, Duration::from_secs(1))
                .is_err()
        );
        assert_eq!(generation_request(model, &point()), Err("invalid"));
    }
}

#[test]
fn environment_requires_a_dedicated_key_without_mutating_the_test_process() {
    const PROBE: &str = "ATO_GENERATION_ENV_TEST_PROBE";
    if let Ok(case) = std::env::var(PROBE) {
        // Construction only: never contacts the live API.
        assert_eq!(
            JevGenerationProvider::from_env(Duration::from_secs(1)).is_ok(),
            case == "valid"
        );
        return;
    }
    for (case, key, model) in [
        ("missing", None, None),
        ("empty", Some(""), None),
        ("valid", Some(KEY), None),
        ("valid", Some(KEY), Some("jev-1.14.0")),
        ("bad_model", Some(KEY), Some("")),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "environment_requires_a_dedicated_key_without_mutating_the_test_process",
            ])
            .env(PROBE, case)
            .env_remove("ATO_GENERATION_JEV_API_KEY")
            .env_remove("ATO_GENERATION_JEV_MODEL")
            .env("JEV_API_KEY", "generic-key")
            .env("ATO_DECISION_JEV_API_KEY", "decision-key")
            .env("ATO_DECISION_JEV_MODEL", "NOT_THE_GENERATION_MODEL");
        if let Some(key) = key {
            child.env("ATO_GENERATION_JEV_API_KEY", key);
        }
        if let Some(model) = model {
            child.env("ATO_GENERATION_JEV_MODEL", model);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "case {case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
