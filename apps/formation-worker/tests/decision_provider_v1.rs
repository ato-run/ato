//! Stage 5a requester-side Jev DecisionProvider against a local HTTP stand-in
//! for `POST /v1/systemone`. Every failure is a fallback, never a choice.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use ato_formation_worker::decision_provider::*;

fn point() -> DecisionPoint {
    serde_json::from_value(serde_json::json!({
        "seq": 1,
        "default_choice_id": "c1111111111111111",
        "expires_at": "2026-09-26T00:00:30.000Z",
        "choices": [
            {"choice_id": "c1111111111111111",
             "action": {"kind": "attempt", "candidate_id": "cand-1",
                        "derivation_ref": "sha256:d1-UNTRUSTED-MARKER",
                        "runtime_id": "rt", "environment_id": "native"},
             "derivation": {"effects": "pure"}, "runtime_facts": {"platform": "linux/aarch64"},
             "prior_attempts": [{"status": "fail", "failure_code": "http_status_mismatch"}]},
            {"choice_id": "c2222222222222222",
             "action": {"kind": "attempt", "candidate_id": "cand-2",
                        "derivation_ref": "sha256:d2",
                        "runtime_id": "rt", "environment_id": "native"}},
            {"choice_id": "c3333333333333333",
             "action": {"kind": "inspect", "inspection": "candidate_refusals",
                        "target_ref": "sha256:d1-UNTRUSTED-MARKER"}},
            {"choice_id": "c4444444444444444",
             "action": {"kind": "stop", "reason_class": "no_promising_action"}}
        ],
        "evidence": [{"kind": "attempt_failures", "target_ref": "sha256:d0",
                      "failures": [{"runtime_id": "rt", "status": "fail",
                                    "failure_code": "http_status_mismatch"}]}]
    }))
    .unwrap()
}

/// One HTTP exchange: returns the request (head + body) the provider sent.
fn serve_once(
    status: u16,
    body: String,
    delay: Duration,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            head.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        std::thread::sleep(delay);
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        head + &String::from_utf8(request).unwrap()
    });
    (url, handle)
}

fn answer(choice: &str, probabilities: serde_json::Value) -> String {
    serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {"decision": {"type": "choice", "choice": choice, "confidence": 0.8,
                                 "probabilities": probabilities}},
        "usage": {"input_tokens": 900, "output_tokens": 3}
    })
    .to_string()
}
fn provider(url: &str, timeout: Duration) -> JevDecisionProvider {
    JevDecisionProvider::new(url, "decision-key-0123456789", "jev-1.13.0", timeout).unwrap()
}

#[test]
fn a_well_formed_choice_is_returned_with_evidence_and_the_question_carries_no_data() {
    let body = answer(
        "c2222222222222222",
        serde_json::json!({"c1111111111111111": 0.2, "c2222222222222222": 0.5,
                          "c3333333333333333": 0.2, "c4444444444444444": 0.1}),
    );
    let (url, server) = serve_once(200, body, Duration::ZERO);
    let got = provider(&url, Duration::from_secs(5)).decide(&point());
    let sent = server.join().unwrap();
    let ProviderAnswer::Choice {
        choice_id,
        evidence,
    } = got
    else {
        panic!("{got:?}")
    };
    assert_eq!(choice_id, "c2222222222222222");
    assert_eq!(evidence["model"], "jev-1.13.0");
    assert_eq!(evidence["usage"]["input_tokens"], 900);
    assert!(sent.starts_with("POST /v1/systemone "));
    assert!(
        sent.to_ascii_lowercase()
            .contains("authorization: bearer decision-key-0123456789")
    );
    let json: serde_json::Value =
        serde_json::from_str(sent.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(!json.to_string().contains("decision-key"));
    let question = json["questions"]["decision"].to_string();
    // Offered data is state; the question text is fixed and names labels only.
    assert!(!question.contains("UNTRUSTED-MARKER"));
    assert!(!question.contains("http_status_mismatch"));
    assert!(json["state"].to_string().contains("UNTRUSTED-MARKER"));
    assert_eq!(json["questions"]["decision"]["type"], "choice");
    // The recorded evidence projection is state data too.
    assert!(
        json["state"]["evidence"]
            .to_string()
            .contains("attempt_failures")
    );
}

#[test]
fn malformed_or_out_of_set_answers_are_invalid() {
    let both = serde_json::json!({"c1111111111111111": 0.5, "c2222222222222222": 0.5,
                                 "c3333333333333333": 0.0, "c4444444444444444": 0.0});
    for raw in [
        answer("c9999999999999999", both.clone()),
        answer(
            "c1111111111111111",
            serde_json::json!({"c1111111111111111": 1.0}),
        ),
        answer(
            "c1111111111111111",
            serde_json::json!({"c1111111111111111": 0.5, "c2222222222222222": 0.5, "c3": 0.0}),
        ),
        answer(
            "c1111111111111111",
            serde_json::json!({"c1111111111111111": 1.5, "c2222222222222222": 0.5}),
        ),
        answer("c1111111111111111", both.clone()).replace("jev-1.13.0", "gpt-x"),
        answer("c1111111111111111", both.clone())
            .replace("\"type\":\"choice\"", "\"type\":\"score\""),
        "not json".into(),
    ] {
        let (url, server) = serve_once(200, raw.clone(), Duration::ZERO);
        let got = provider(&url, Duration::from_secs(5)).decide(&point());
        server.join().unwrap();
        assert_eq!(got, ProviderAnswer::Fallback { reason: "invalid" }, "{raw}");
    }
}

#[test]
fn transport_failures_are_fallbacks() {
    let (url, server) = serve_once(500, "{}".into(), Duration::ZERO);
    assert_eq!(
        provider(&url, Duration::from_secs(5)).decide(&point()),
        ProviderAnswer::Fallback {
            reason: "provider_error"
        }
    );
    server.join().unwrap();
    let (url, _server) = serve_once(200, "{}".into(), Duration::from_secs(3));
    assert_eq!(
        provider(&url, Duration::from_millis(300)).decide(&point()),
        ProviderAnswer::Fallback { reason: "timeout" }
    );
    let (url, server) = serve_once(200, "x".repeat(70 * 1024), Duration::ZERO);
    assert_eq!(
        provider(&url, Duration::from_secs(5)).decide(&point()),
        ProviderAnswer::Fallback { reason: "invalid" }
    );
    server.join().unwrap();
    // Nothing listens: unreachable is a provider error, not a panic.
    assert_eq!(
        provider("http://127.0.0.1:9", Duration::from_secs(2)).decide(&point()),
        ProviderAnswer::Fallback {
            reason: "provider_error"
        }
    );
}

#[test]
fn an_oversized_request_is_never_sent() {
    let mut big = point();
    big.choices[0].derivation = serde_json::json!({"notes": "x".repeat(MAX_REQUEST_BYTES)});
    // Unroutable: if the provider tried to send, the answer would be provider_error.
    assert_eq!(
        provider("http://127.0.0.1:9", Duration::from_secs(2)).decide(&big),
        ProviderAnswer::Fallback { reason: "invalid" }
    );
}

#[test]
fn the_decision_key_is_separate_from_the_browser_judge_key() {
    // SAFETY: this test alone touches these variables.
    unsafe {
        std::env::remove_var("ATO_DECISION_JEV_API_KEY");
        std::env::set_var("JEV_API_KEY", "judge-key");
    }
    assert!(JevDecisionProvider::from_env(Duration::from_secs(1)).is_err());
    unsafe { std::env::remove_var("JEV_API_KEY") };
}

#[test]
fn submissions_have_the_coordinator_shape() {
    assert_eq!(
        ProviderAnswer::Fallback { reason: "timeout" }.submission(3),
        serde_json::json!({"seq": 3, "fallback": "timeout"})
    );
}

#[test]
fn only_requirement_facts_reach_the_provider() {
    let mut p = point();
    p.choices[0].derivation = serde_json::json!({
        "effects": "pure",
        "requirements": [{"fact": "platform", "one_of": ["linux/aarch64"]}]
    });
    p.choices[0].runtime_facts = [
        ("platform", "linux/aarch64"),
        ("runtime.hostname", "build-host-7.internal"),
        ("toolchain.secret_token", "s3cr3t-canary"),
        ("runtime.browser", "true"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let request = decision_request("jev-1.13.0", &p).to_string();
    assert!(request.contains("linux/aarch64"));
    for leaked in [
        "build-host-7.internal",
        "s3cr3t-canary",
        "runtime.hostname",
        "runtime.browser",
    ] {
        assert!(!request.contains(leaked), "{leaked}");
    }
    // A choice whose D requires nothing sends no Runtime facts at all.
    assert!(requirement_scoped_facts(&p.choices[1]).is_empty());
}

#[test]
fn escalation_is_a_finite_label_without_producer_payload() {
    let mut point = point();
    point.choices[0].action = OfferedAction::EscalateToCandidateProducer {};
    let request = decision_request("jev-test", &point);
    assert_eq!(
        request["state"]["options"][&point.default_choice_id],
        serde_json::json!({"action":"escalate_to_candidate_producer"})
    );
    assert_eq!(point.choices[0].attempt(), None);
    let wire = serde_json::json!({"decision_point": {
        "seq": 0, "default_choice_id": "escalate", "expires_at": "2026-09-29T00:00:30Z",
        "choices": [{"choice_id":"escalate", "action":{"kind":"escalate_to_candidate_producer"}}]
    }});
    let restored = DecisionPoint::from_status(&wire).unwrap();
    assert_eq!(
        restored.choices[0].action,
        OfferedAction::EscalateToCandidateProducer {}
    );
    assert_eq!(
        ProviderAnswer::Choice {
            choice_id: "escalate".into(),
            evidence: serde_json::json!({})
        }
        .submission(0),
        serde_json::json!({"seq":0,"choice_id":"escalate","evidence":{}})
    );
}

// --- the local Kev provider (roadmap PR-6) --------------------------------

/// A local Kev runtime answers the same contract. Its answer must stay named
/// "kev": a local decision must never be recorded as a remote jev one.
#[test]
fn a_local_kev_answer_is_recorded_as_kev() {
    let body = serde_json::json!({
        "model": "kev-0.8b",
        "answers": {"decision": {"type": "choice", "choice": "c2222222222222222",
                                 "confidence": 0.77,
                                 "probabilities": {"c1111111111111111": 0.2,
                                                   "c2222222222222222": 0.77,
                                                   "c3333333333333333": 0.02,
                                                   "c4444444444444444": 0.01}}}
    })
    .to_string();
    let (url, handle) = serve_once(200, body, Duration::from_millis(0));
    let provider =
        KevDecisionProvider::new(&url, None, "kev-0.8b", Duration::from_secs(5)).unwrap();
    let answer = provider.decide(&point());
    let ProviderAnswer::Choice {
        choice_id,
        evidence,
    } = answer
    else {
        panic!("expected a choice from the local Kev runtime, got {answer:?}");
    };
    assert_eq!(choice_id, "c2222222222222222");
    assert_eq!(evidence["provider"], "kev");
    assert_eq!(evidence["model"], "kev-0.8b");
    // An open local server must not receive credentials it never asked for.
    let request = handle.join().unwrap();
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "an open local kev server must not receive an Authorization header: {request}"
    );
    assert!(request.contains("\"model\":\"kev-0.8b\""));
}

/// A jev-family answer to the kev provider is invalid, not relabelled: otherwise
/// the record would claim a local model produced a remote model's decision.
#[test]
fn a_jev_answer_is_invalid_for_the_kev_provider() {
    let (url, handle) = serve_once(
        200,
        answer("c2222222222222222", serde_json::json!({})),
        Duration::from_millis(0),
    );
    let provider =
        KevDecisionProvider::new(&url, None, "kev-0.8b", Duration::from_secs(5)).unwrap();
    let answer = provider.decide(&point());
    assert!(
        matches!(answer, ProviderAnswer::Fallback { reason: "invalid" }),
        "a jev-family answer must not become a kev choice: {answer:?}"
    );
    handle.join().unwrap();
}

/// A configured key is sent when the local server was started with one.
#[test]
fn a_configured_kev_key_is_sent() {
    let body = serde_json::json!({
        "model": "kev-0.8b",
        "answers": {"decision": {"type": "choice", "choice": "c1111111111111111",
                                 "confidence": 0.6,
                                 "probabilities": {"c1111111111111111": 0.6,
                                                   "c2222222222222222": 0.2,
                                                   "c3333333333333333": 0.1,
                                                   "c4444444444444444": 0.1}}}
    })
    .to_string();
    let (url, handle) = serve_once(200, body, Duration::from_millis(0));
    let provider = KevDecisionProvider::new(
        &url,
        Some("local-kev-key-0123456789"),
        "kev-0.8b",
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(matches!(
        provider.decide(&point()),
        ProviderAnswer::Choice { .. }
    ));
    let request = handle.join().unwrap().to_ascii_lowercase();
    assert!(
        request.contains("authorization: bearer local-kev-key-0123456789"),
        "a configured key must be sent: {request}"
    );
}

/// An open request to a remote host would send the search state off this machine,
/// so it is refused before anything is sent.
#[test]
fn a_remote_kev_provider_without_a_key_is_refused() {
    let error = KevDecisionProvider::new(
        "https://kev.example.com",
        None,
        "kev-0.8b",
        Duration::from_secs(5),
    )
    .expect_err("a remote provider without a key must be refused");
    assert!(
        error.to_string().contains("ATO_DECISION_KEV_API_KEY"),
        "unexpected error: {error}"
    );
    assert!(
        KevDecisionProvider::new(
            "https://kev.example.com",
            Some("key"),
            "kev-0.8b",
            Duration::from_secs(5)
        )
        .is_ok()
    );
}

/// The model family is part of the provider's identity.
#[test]
fn a_kev_provider_refuses_a_foreign_model_family() {
    let error = KevDecisionProvider::new(
        "http://127.0.0.1:8008",
        None,
        "jev-1.13.0",
        Duration::from_secs(5),
    )
    .expect_err("a jev model on the kev provider must be refused");
    assert!(
        error.to_string().contains("kev-"),
        "unexpected error: {error}"
    );
}

/// Transport failures remain fallbacks, never choices.
#[test]
fn kev_transport_failures_are_fallbacks() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let provider =
        KevDecisionProvider::new(&url, None, "kev-0.8b", Duration::from_millis(500)).unwrap();
    assert!(matches!(
        provider.decide(&point()),
        ProviderAnswer::Fallback { .. }
    ));
}

/// The options the local server was asked about, so the captured response below
/// can be checked against a point that actually offers those labels.
fn point_over(labels: &[&str]) -> DecisionPoint {
    let choices: Vec<serde_json::Value> = labels
        .iter()
        .map(|label| {
            serde_json::json!({
                "choice_id": label,
                "action": {"kind": "attempt", "candidate_id": format!("cand-{label}"),
                           "derivation_ref": format!("sha256:{label}"),
                           "runtime_id": "rt", "environment_id": "native"}
            })
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "seq": 7,
        "default_choice_id": labels[labels.len() - 1],
        "expires_at": "2026-11-18T00:00:30.000Z",
        "choices": choices,
        "evidence": []
    }))
    .unwrap()
}

/// The response this machine's local Kev-0.8B server returned through
/// "ato model serve" (captured verbatim). This is the cross-module contract: the
/// CLI serves it and the provider must accept it as a kev Choice.
#[test]
fn the_local_server_response_is_accepted_as_a_kev_choice() {
    let body = serde_json::json!({
        "answers": {"decision": {"choice": "c3", "confidence": 0.4281,
                                 "probabilities": {"c1": 0.1975, "c2": 0.1838, "c3": 0.6187},
                                 "type": "choice"}},
        "checkpoint": {"base": "Qwen/Qwen3.5-0.8B-Base",
                       "base_revision": "dc7cdfe2ee4154fa7e30f5b51ca41bfa40174e68",
                       "head_dim": 256, "temperature": 2.3510958125672174, "weights": "lora"},
        "model": "kev-0.8b",
        "runtime": {"backend": "mlx", "device": "mps", "inference_ms": 2243.0,
                    "load_seconds": 2.971, "mlx": "0.32.4", "mlx_lm": "0.31.3",
                    "python": "3.12.4"}
    });
    let (url, handle) = serve_once(200, body.to_string(), Duration::from_millis(0));
    let provider =
        KevDecisionProvider::new(&url, None, "kev-0.8b", Duration::from_secs(30)).unwrap();
    let answer = provider.decide(&point_over(&["c1", "c2", "c3"]));
    let ProviderAnswer::Choice {
        choice_id,
        evidence,
    } = answer
    else {
        panic!("the local server's answer must be accepted, got {answer:?}");
    };
    assert_eq!(choice_id, "c3");
    assert_eq!(evidence["provider"], "kev");
    assert_eq!(evidence["model"], "kev-0.8b");
    handle.join().unwrap();
}
