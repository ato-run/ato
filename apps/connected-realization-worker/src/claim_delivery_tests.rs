//! A claim is committed by the control plane before its response arrives.
//! These tests pin that the Runner accounts for every response body, and that
//! a lease it never received is found while the process stays up.
use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;

fn portable_command() -> serde_json::Value {
    json!({
        "kind": "portable_capsule_v2",
        "bundle_id": "bnd_1",
        "transport_digest": "sha256:00",
        "expected_root_computation_ref": "blake3:00",
        "run_id": "run_1",
        "session_id": "run:run_1",
        "exported_port_id": "app.http",
        "surface_contract_version": "1",
        "session_surface": {},
        "accepted_session_surfaces": [{}],
    })
}

fn body(value: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn an_empty_claim_keeps_the_advertised_poll_interval() {
    let delivery = decode_claim(&body(json!({ "lease": null, "next_poll_seconds": 5 })));
    assert!(matches!(
        delivery,
        ClaimDelivery::Empty {
            next_poll_seconds: 5
        }
    ));
    assert!(matches!(
        decode_claim(b"{}"),
        ClaimDelivery::Empty {
            next_poll_seconds: 2
        }
    ));
}

#[test]
fn a_decodable_lease_is_delivered_with_its_kind() {
    let delivery = decode_claim(&body(json!({
        "lease": { "id": "lease_1", "run_id": "run_1", "command": portable_command() },
    })));
    let ClaimDelivery::Lease(lease) = delivery else {
        panic!("expected a lease, got {delivery:?}");
    };
    assert_eq!(lease.id, "lease_1");
    assert_eq!(lease.command.kind(), "portable_capsule_v2");
}

#[test]
fn a_claimed_lease_the_runner_cannot_decode_is_named_not_dropped() {
    // The control plane has already flipped this lease to `claimed`.
    let delivery = decode_claim(&body(json!({
        "lease": {
            "id": "lease_2",
            "run_id": "run_2",
            "command": { "kind": "future_kind_v9", "token": "not-for-logs" },
            "proxy_assertion_key": "not-for-logs",
        },
    })));
    let ClaimDelivery::Undecodable { lease_id, kind } = delivery else {
        panic!("expected an undecodable lease, got {delivery:?}");
    };
    assert_eq!(lease_id, "lease_2");
    assert_eq!(kind.as_deref(), Some("future_kind_v9"));
    let message = undecodable_lease_message(kind.as_deref());
    assert!(message.contains("future_kind_v9"));
    assert!(!message.contains("not-for-logs"));
}

#[test]
fn a_lease_kind_that_is_not_an_identifier_is_never_echoed() {
    let delivery = decode_claim(&body(json!({
        "lease": { "id": "lease_3", "run_id": "run_3", "command": { "kind": "a b\nc" } },
    })));
    assert!(matches!(
        delivery,
        ClaimDelivery::Undecodable { ref lease_id, kind: None } if lease_id == "lease_3"
    ));
}

#[test]
fn a_body_without_a_usable_lease_id_is_unreadable() {
    for raw in [
        &b"not json"[..],
        b"[]",
        br#"{"lease":{"command":{"kind":"runtime_launch"}}}"#,
        br#"{"lease":{"id":"../escape","run_id":"r","command":{}}}"#,
    ] {
        assert!(
            matches!(decode_claim(raw), ClaimDelivery::Unreadable { .. }),
            "{}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn the_idle_audit_is_periodic_and_can_be_forced() {
    let start = Instant::now();
    let mut audit = IdleOpenLeaseAudit::new(start);
    assert!(!audit.take_due(start));
    assert!(!audit.take_due(start + IDLE_OPEN_LEASE_AUDIT_INTERVAL - Duration::from_millis(1)));
    assert!(audit.take_due(start + IDLE_OPEN_LEASE_AUDIT_INTERVAL));
    // Taking it re-arms the interval.
    assert!(!audit.take_due(start + IDLE_OPEN_LEASE_AUDIT_INTERVAL));
    audit.force(start);
    assert!(audit.take_due(start));
}

/// Serves each scripted body once and records `(request line, body)`.
fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut request_body = vec![0; length];
            reader.read_exact(&mut request_body).unwrap();
            seen.push((
                request_line.trim().to_owned(),
                String::from_utf8(request_body).unwrap(),
            ));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        }
        seen
    });
    (base, handle)
}

fn worker(base: &str, work_root: &Path, max_slots: &str) -> ConnectedWorker {
    let config = WorkerConfig::try_parse_from([
        "worker",
        "--api-base",
        base,
        "--runner-id",
        "runner_1",
        "--runner-token",
        "fixture-token",
        "--work-root",
        work_root.to_str().unwrap(),
        "--surface-target",
        "127.0.0.1:8080",
        "--max-slots",
        max_slots,
    ])
    .unwrap();
    ConnectedWorker {
        api: HttpRunnerApi::new(base, "runner_1", "fixture-token").unwrap(),
        config,
        volume_store: None,
        fixed_tcp: runtime_launch::network_broker::FixedTcpRegistry::new("").unwrap(),
    }
}

fn lost_lease() -> String {
    json!({ "leases": [{
        "id": "lease_lost", "run_id": "run_lost", "status": "claimed",
        "recovery_kind": "oci_labeled",
    }] })
    .to_string()
}

/// The observed incident: the control plane holds a lease `claimed` for a
/// Runner that never received it and keeps answering its polls with "full".
#[test]
fn an_idle_single_slot_runner_finds_a_lease_it_never_received() {
    let root = tempfile::tempdir().unwrap();
    let (base, server) = serve(vec![json!({ "leases": [] }).to_string(), lost_lease()]);
    let worker = worker(&base, root.path(), "1");
    let start = Instant::now();
    let mut audit = IdleOpenLeaseAudit::new(start);

    // Not due: no request is made at all.
    assert!(!worker.idle_runner_has_open_leases(&mut audit));
    audit.force(start);
    assert!(!worker.idle_runner_has_open_leases(&mut audit));
    audit.force(start);
    assert!(worker.idle_runner_has_open_leases(&mut audit));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|(line, _)| line.starts_with("GET /v1/runners/runner_1/leases/open "))
    );
}

/// The control plane does not record which slot owns a lease, so a
/// multi-slot Runner must not infer that an open lease is undelivered.
#[test]
fn a_multi_slot_runner_does_not_infer_ownership() {
    let root = tempfile::tempdir().unwrap();
    let worker = worker("http://127.0.0.1:9", root.path(), "2");
    let start = Instant::now();
    let mut audit = IdleOpenLeaseAudit::new(start);
    audit.force(start);
    assert!(!worker.idle_runner_has_open_leases(&mut audit));
}

#[test]
fn the_refusal_of_an_undecodable_lease_reaches_the_control_plane() {
    let (base, server) = serve(vec!["{}".to_owned()]);
    let api = HttpRunnerApi::new(&base, "runner_1", "fixture-token").unwrap();
    api.report_failed(
        "lease_2",
        LEASE_UNDECODABLE_CODE,
        &undecodable_lease_message(Some("future_kind_v9")),
    )
    .unwrap();
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .0
            .starts_with("POST /v1/runner-leases/lease_2/status ")
    );
    let report: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], "lease_undecodable");
}
