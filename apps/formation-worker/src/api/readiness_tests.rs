use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn fixture(
    replies: Vec<(u16, serde_json::Value)>,
) -> (
    FormationApi,
    std::thread::JoinHandle<Vec<serde_json::Value>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(pos) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while bytes.len() < header_end + length {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
            }
            requests.push(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap());
            let body = serde_json::to_vec(&body).unwrap();
            write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
        }
        requests
    });
    (
        FormationApi::new(
            reqwest::blocking::Client::new(),
            base,
            "synthetic-token".into(),
        ),
        thread,
    )
}

#[test]
fn validator_ack_retries_the_identical_result_and_fence() {
    let (api, thread) = fixture(vec![
        (
            409,
            serde_json::json!({"error":"portable_bundle_artifact_not_ready"}),
        ),
        (
            200,
            serde_json::json!({"registered":true,"compute_schema_id":"schema"}),
        ),
    ]);
    let result = serde_json::json!({"formation_kind":"portable_v2","attempt_fence":7,"result_digest":"immutable-result"});
    let outcome = api
        .publish_result_with_readiness_wait(
            &result,
            "compute",
            "revision",
            Duration::from_secs(5),
            Duration::ZERO,
        )
        .unwrap();
    assert!(matches!(outcome, PublishOutcome::Accepted { .. }));
    let requests = thread.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[0]["result"], result);
}

#[test]
fn readiness_budget_is_bounded_and_other_refusals_are_final() {
    for (kind, code, budget) in [
        (
            "portable_v2",
            "portable_bundle_artifact_not_ready",
            Duration::ZERO,
        ),
        ("portable_v2", "stale_attempt", Duration::from_secs(5)),
        (
            "legacy",
            "portable_bundle_artifact_not_ready",
            Duration::from_secs(5),
        ),
    ] {
        let (api, thread) = fixture(vec![(409, serde_json::json!({"error":code}))]);
        let outcome = api
            .publish_result_with_readiness_wait(
                &serde_json::json!({"formation_kind":kind}),
                "compute",
                "revision",
                budget,
                Duration::ZERO,
            )
            .unwrap();
        assert!(matches!(outcome, PublishOutcome::Refused { code: actual } if actual == code));
        assert_eq!(thread.join().unwrap().len(), 1);
    }
}
