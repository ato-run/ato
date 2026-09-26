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
            {"choice_id": "c1111111111111111", "derivation_ref": "sha256:d1-UNTRUSTED-MARKER",
             "runtime_id": "rt", "environment_id": "native",
             "derivation": {"effects": "pure"}, "runtime_facts": {"platform": "linux/aarch64"},
             "prior_attempts": [{"status": "fail", "failure_code": "http_status_mismatch"}]},
            {"choice_id": "c2222222222222222", "derivation_ref": "sha256:d2",
             "runtime_id": "rt", "environment_id": "native"}
        ]
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
        serde_json::json!({"c1111111111111111": 0.2, "c2222222222222222": 0.8}),
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
}

#[test]
fn malformed_or_out_of_set_answers_are_invalid() {
    let both = serde_json::json!({"c1111111111111111": 0.5, "c2222222222222222": 0.5});
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
