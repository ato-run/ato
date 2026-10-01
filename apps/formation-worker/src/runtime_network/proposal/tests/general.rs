use super::super::{
    budget::{BudgetPlan, CallBudget, FinishReason, RequestEvidence, ResponseEvidence},
    deepseek::*,
    provenance::*,
};
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct Mock {
    endpoint: String,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    raw_seen: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
struct Reply {
    status: u16,
    bytes: Vec<u8>,
    delay: Duration,
    lost: bool,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            bytes: serde_json::to_vec(&value).unwrap(),
            delay: Duration::ZERO,
            lost: false,
        }
    }
}
impl Mock {
    fn new(handler: impl Fn(&str, &Value) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let seen = Arc::new(Mutex::new(vec![]));
        let log = seen.clone();
        let raw_seen = Arc::new(Mutex::new(vec![]));
        let raw_log = raw_seen.clone();
        let thread = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let Ok((mut s, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut bytes = vec![];
                let mut buffer = [0; 4096];
                let (headers, end) = loop {
                    let n = s.read(&mut buffer).unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break (String::from_utf8(bytes[..i].to_vec()).unwrap(), i + 4);
                    }
                };
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|n| n.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() - end < length {
                    let n = s.read(&mut buffer).unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                raw_log
                    .lock()
                    .unwrap()
                    .push(bytes[end..end + length].to_vec());
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[end..end + length]).unwrap()
                };
                let path = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                log.lock().unwrap().push((path.to_string(), body.clone()));
                let reply = handler(path, &body);
                thread::sleep(reply.delay);
                if !reply.lost {
                    let _ = write!(
                        s,
                        "HTTP/1.1 {} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.bytes.len()
                    );
                    let _ = s.write_all(&reply.bytes);
                }
            }
        });
        Self {
            endpoint,
            seen,
            raw_seen,
            stop,
            thread: Some(thread),
        }
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let result = t.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn plan() -> BudgetPlan {
    BudgetPlan {
        max_calls: 1,
        input_token_cap: 20000,
        output_token_cap: 2048,
        input_price: 1_000_000,
        output_price: 1_000_000,
        ceiling_usd_micros: 5_000_000,
    }
}
fn config(endpoint: &str) -> DeepSeekConfig {
    DeepSeekConfig {
        provider: "deepseek".into(),
        model: "mock-pinned-model".into(),
        endpoint: endpoint.into(),
        prompt_version: PROMPT_VERSION.into(),
        max_output_tokens: 2048,
        timeout_ms: 500,
        thinking: ThinkingMode::Disabled,
    }
}
fn enabled() -> (tempfile::TempDir, Submission) {
    let (root, mut sub) = prepared(|p| {
        std::fs::write(p.join("app.py"), "print('authorized frozen source')").unwrap();
        std::fs::write(p.join("other.py"), "print('other source')").unwrap();
        std::fs::write(p.join("unrelated.txt"), "UNRELATED_SECRET").unwrap();
    });
    let mut auth = authorization();
    auth.policy.allow_source_text = true;
    auth.policy.max_source_bytes = 16384;
    auth.source_domain
        .entrypoints
        .insert("other".into(), "other.py".into());
    sub.enable_candidate_producer(auth).unwrap();
    (root, sub)
}
fn envelope(content: &str) -> Value {
    json!({"model":"mock-pinned-model","choices":[{"finish_reason":"stop","index":0,"message":{"role":"assistant","content":content,"reasoning_content":"DO_NOT_PERSIST_REASONING"}}],"usage":{"prompt_tokens":123,"completion_tokens":45}})
}
fn adapter(root: &Path, mock: &Mock) -> DeepSeekCandidateProducer {
    let budget = Arc::new(CallBudget::create(&root.join("budget.jsonl"), plan()).unwrap());
    DeepSeekCandidateProducer::new_mock(config(&mock.endpoint), budget).unwrap()
}
#[test]
fn source_v2_uses_frozen_opt_in_and_never_private_inventory() {
    let (root, sub) = enabled();
    let view = status(&sub);
    let before = sub.proposal_request_v2(&view).unwrap();
    std::fs::write(root.path().join("input/app.py"), "MUTATED_PRIVATE").unwrap();
    std::fs::write(root.path().join("input/new.py"), "NEW_PRIVATE").unwrap();
    assert_eq!(before, sub.proposal_request_v2(&view).unwrap());
    let bytes = serde_jcs::to_string(&before).unwrap();
    assert!(bytes.contains("authorized frozen source"));
    for private in [
        "app.py",
        "other.py",
        "source_domain",
        "UNRELATED_SECRET",
        "MUTATED_PRIVATE",
        "NEW_PRIVATE",
        "archive_digest",
        root.path().to_str().unwrap(),
    ] {
        assert!(!bytes.contains(private), "{private}");
    }
    let (_root, mut fixed) = prepared(|_| {});
    fixed.enable_candidate_producer(authorization()).unwrap();
    assert!(fixed.proposal_request_v2(&status(&fixed)).is_err());
    assert!(
        !serde_json::to_string(&fixed.proposal_request(&status(&fixed)).unwrap())
            .unwrap()
            .contains("source_context")
    );
}
#[test]
fn source_module_utf8_binary_bounds_and_immutable_hash() {
    let (_root, mut sub) = prepared(|p| {
        std::fs::write(p.join("app.py"), "あ".repeat(20000)).unwrap();
        std::fs::write(p.join("binary.py"), [0, 1, 255]).unwrap();
        std::fs::create_dir(p.join("pkg")).unwrap();
        std::fs::write(p.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(p.join("pkg/__main__.py"), "module text").unwrap();
    });
    let mut auth = authorization();
    auth.policy.allow_source_text = true;
    auth.policy.max_source_bytes = 19;
    auth.source_domain
        .entrypoints
        .insert("binary".into(), "binary.py".into());
    auth.source_domain
        .modules
        .insert("module".into(), "pkg".into());
    sub.enable_candidate_producer(auth).unwrap();
    let req = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert_eq!(req.source_context.len(), 2);
    assert!(
        req.source_context
            .iter()
            .map(|e| e.text.len())
            .sum::<usize>()
            <= 19
    );
    assert!(req.source_context[0].truncated);
    assert_eq!(req.source_context[0].text, "あああ");
    let auth = sub.request.policy.proposal.as_ref().unwrap();
    assert_eq!(
        req.source_context_sha256(auth).unwrap(),
        sub.proposal_request_v2(&status(&sub))
            .unwrap()
            .source_context_sha256(auth)
            .unwrap()
    );
}
#[test]
fn m0_through_m12_exact_raw_transport_and_validator_authority() {
    for case in 0..=12 {
        let (root, sub) = enabled();
        let raw = String::from_utf8(output().raw().to_vec()).unwrap();
        let mut content = raw.clone();
        let mut reply = envelope(&content);
        let mut http_status = 200;
        let mut delay = Duration::ZERO;
        match case {
            1 => {
                content = String::new();
                reply = envelope(&content);
            }
            2 => reply = json!({"choices":"malformed"}),
            3 => {
                content = format!("```json\n{raw}\n```");
                reply = envelope(&content);
            }
            4 => {
                content = "x".repeat(16385);
                reply = envelope(&content);
            }
            5 => http_status = 401,
            6 => http_status = 429,
            7 => http_status = 500,
            9 => delay = Duration::from_millis(650),
            10 => {
                reply.as_object_mut().unwrap().remove("usage");
            }
            11 => {
                content=r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"shell","argv":["FORBIDDEN"]}]}]}"#.into();
                reply = envelope(&content);
            }
            12 => {
                let mut b: Value = serde_json::from_str(&raw).unwrap();
                let mut second = b["proposals"][0].clone();
                second["operations"][0]["entrypoint_id"] = json!("other");
                b["proposals"].as_array_mut().unwrap().push(second);
                content = serde_json::to_string(&b).unwrap();
                reply = envelope(&content);
            }
            _ => {}
        }
        let mock = Mock::new(move |_, _| Reply {
            status: http_status,
            bytes: serde_json::to_vec(&reply).unwrap(),
            delay,
            lost: false,
        });
        let mut cfg = config(&mock.endpoint);
        if case == 8 {
            let port = TcpListener::bind("127.0.0.1:0").unwrap();
            cfg.endpoint = format!("http://{}", port.local_addr().unwrap());
            drop(port);
        }
        let guard =
            Arc::new(CallBudget::create(&root.path().join("budget.jsonl"), plan()).unwrap());
        let producer = DeepSeekCandidateProducer::new_mock(cfg, guard).unwrap();
        let req = sub.proposal_request_v2(&status(&sub)).unwrap();
        let result = producer.propose(&req);
        let expected = match case {
            1 | 2 | 10 => Some(ErrorClass::MalformedResponse),
            4 => Some(ErrorClass::ResponseTooLarge),
            5..=7 => Some(ErrorClass::ProviderRefused),
            8 => Some(ErrorClass::TransportError),
            9 => Some(ErrorClass::Timeout),
            _ => None,
        };
        if let Some(error) = expected {
            let failure = result.err().unwrap();
            assert_eq!(failure.class, error, "M{case}");
            assert!(!format!("{failure:?}").contains("DO_NOT_PERSIST_REASONING"));
        } else {
            let result = result.ok().unwrap();
            assert_eq!(
                result.output.raw(),
                content.as_bytes(),
                "no output repair M{case}"
            );
            result
                .provenance
                .validate(&producer.identity(), true)
                .unwrap();
            assert_eq!(result.provenance.usage.input_tokens, Some(123));
            let frozen = frozen_request(&sub.request).unwrap();
            let mut registry = CandidateRegistry::new(&frozen).unwrap();
            let outcomes = registry.validate_batch(&BTreeMap::new(), &result.output);
            match case {
                3 => assert!(outcomes.is_err()),
                11 => {
                    assert!(matches!(
                        &outcomes.unwrap()[0],
                        ProposalOutcome::Rejected(_)
                    ));
                    assert_eq!(registry.generated().len(), 0);
                }
                12 => assert_eq!(registry.generated().len(), 2),
                _ => assert_eq!(registry.generated().len(), 1),
            }
            assert!(sub.contracts.is_empty());
        }
        assert_eq!(mock.count(), usize::from(case != 8), "M{case} never retry");
        if case != 8 {
            let seen = mock.seen.lock().unwrap();
            let (path, body) = &seen[0];
            assert_eq!(path, "/chat/completions");
            assert_eq!(body["stream"], false);
            assert_eq!(body["max_tokens"], 2048);
            assert_eq!(body["thinking"], json!({"type":"disabled"}));
            assert_eq!(body["response_format"]["type"], "json_object");
            assert_eq!(
                body["messages"][1]["content"],
                serde_jcs::to_string(&req).unwrap()
            );
            let bytes = serde_json::to_string(body).unwrap();
            for private in [
                "synthetic-mock-key",
                "app.py",
                "source_domain",
                "UNRELATED_SECRET",
                "DO_NOT_PERSIST_REASONING",
                "Authorization",
                "01234567-0123-0123-0123-012345678901",
            ] {
                assert!(!bytes.contains(private), "{private}");
            }
        }
    }
}
#[test]
fn spend_journal_bounds_restart_concurrency_and_no_second_http() {
    let (root, sub) = enabled();
    let response = envelope(std::str::from_utf8(output().raw()).unwrap());
    let mock = Mock::new(move |_, _| Reply::json(response.clone()));
    let producer = adapter(root.path(), &mock);
    let req = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert!(producer.propose(&req).is_ok());
    let mut second = req.clone();
    second.search_id = "second".into();
    assert!(producer.propose(&second).is_err());
    assert_eq!(mock.count(), 1);
    let reopened = Arc::new(CallBudget::reopen(&root.path().join("budget.jsonl"), plan()).unwrap());
    let restarted = DeepSeekCandidateProducer::new_mock(config(&mock.endpoint), reopened).unwrap();
    assert!(restarted.propose(&req).is_err());
    assert_eq!(mock.count(), 1);
    let mut costly = plan();
    costly.input_price = 1_000_000_000;
    assert!(CallBudget::create(&root.path().join("expensive"), costly).is_err());
    assert!(!root.path().join("expensive").exists());
    assert_eq!(mock.count(), 1);
    let guard = Arc::new(CallBudget::create(&root.path().join("race"), plan()).unwrap());
    let a = guard.clone();
    let b = guard.clone();
    let x = thread::spawn(move || a.reserve("one").is_ok());
    let y = thread::spawn(move || b.reserve("one").is_ok());
    assert_ne!(x.join().unwrap(), y.join().unwrap());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(root.path().join("race"))
        .unwrap();
    file.write_all(b"partial").unwrap();
    assert!(CallBudget::reopen(&root.path().join("race"), plan()).is_err());
}
#[test]
fn production_endpoint_and_mock_credentials_are_separate() {
    let (root, _) = enabled();
    let guard = Arc::new(CallBudget::create(&root.path().join("budget"), plan()).unwrap());
    for url in [
        "https://evil.invalid",
        "https://api.deepseek.com.evil.invalid",
        "https://api.deepseek.com/beta",
        "http://api.deepseek.com",
    ] {
        assert!(DeepSeekCandidateProducer::new(config(url), "UNREAD_KEY", guard.clone()).is_err());
    }
    for url in [
        "https://api.deepseek.com",
        "http://localhost:8080",
        "http://user@127.0.0.1:8080",
        "http://127.0.0.1:8080/other",
    ] {
        assert!(DeepSeekCandidateProducer::new_mock(config(url), guard.clone()).is_err());
    }
    // Construction reads no real key, even when production endpoint is selected.
    assert!(
        DeepSeekCandidateProducer::new(config("https://api.deepseek.com"), "UNREAD_KEY", guard)
            .is_ok()
    );
}

fn general_completed(sub: &Submission, identity: &ProviderIdentity) -> Value {
    let mut value = completed(sub);
    let mut provenance = identity.unknown_usage();
    provenance.usage = Usage {
        input_tokens: Some(123),
        output_tokens: Some(45),
    };
    provenance.estimated_cost_usd_micros = Some(168);
    value["proposal_round"]["provenance"] = json!(&provenance);
    value["proposal_round"]["provider_call"] = json!(ProviderCall {
        provenance,
        status: CallStatus::Success,
        error_class: None
    });
    value
}
#[test]
fn m13_completion_loss_get_recompile_and_restart_do_not_reinvoke() {
    let (root, mut sub) = enabled();
    let response = envelope(std::str::from_utf8(output().raw()).unwrap());
    let model = Mock::new(move |_, _| Reply::json(response.clone()));
    let producer = Arc::new(adapter(root.path(), &model));
    sub.configure_general_producer(producer.identity()).unwrap();
    let durable = general_completed(&sub, &producer.identity());
    let saved = durable.clone();
    let coordinator = Mock::new(move |path, body| {
        if path.ends_with("/claim") {
            Reply::json(
                json!({"revision":2,"round_seq":1,"fence":"01234567-0123-0123-0123-012345678901"}),
            )
        } else if path.ends_with("/complete") {
            assert_eq!(body["provenance"], saved["proposal_round"]["provenance"]);
            assert_eq!(
                body["raw_output_base64"],
                saved["proposal_round"]["raw_output_base64"]
            );
            Reply {
                status: 200,
                bytes: vec![],
                delay: Duration::ZERO,
                lost: true,
            }
        } else {
            Reply::json(saved.clone())
        }
    });
    let client = Client::new(&coordinator.endpoint, "mock-requester-token").unwrap();
    let initial = status(&sub);
    assert!(
        serve_general_proposal(&client, "id", &initial, &mut sub, "owner", producer.clone())
            .unwrap()
    );
    assert_eq!(model.count(), 1);
    assert_eq!(coordinator.count(), 2);
    assert!(
        !serve_general_proposal(&client, "id", &initial, &mut sub, "owner", producer.clone())
            .unwrap()
    );
    sub.accept_proposal_round(&durable).unwrap();
    assert_eq!(sub.proposal_recipes().count(), 1);
    assert!(accept_verified_routes(&sub, "id", &durable).0.is_empty());
    // Restart: same frozen snapshot + owner config, not a recovered claim fence.
    let (_root, mut restarted) = enabled();
    restarted
        .configure_general_producer(producer.identity())
        .unwrap();
    // enabled() uses identical source bytes/K; transport archive may vary, so
    // bind the fixture status to this request while keeping compiled D checked.
    let recovered = general_completed(&restarted, &producer.identity());
    assert!(
        !serve_general_proposal(&client, "id", &recovered, &mut restarted, "owner", producer)
            .unwrap()
    );
    assert_eq!(restarted.proposal_recipes().count(), 1);
    assert_eq!(model.count(), 1);
    assert_eq!(coordinator.count(), 2);
}
#[test]
fn general_claim_loss_never_calls_or_reclaims() {
    let (root, mut sub) = enabled();
    let model = Mock::new(|_, _| panic!("producer must not be called"));
    let producer = Arc::new(adapter(root.path(), &model));
    let coordinator = Mock::new(|_, _| Reply {
        status: 200,
        bytes: vec![],
        delay: Duration::ZERO,
        lost: true,
    });
    let client = Client::new(&coordinator.endpoint, "mock-requester-token").unwrap();
    let initial = status(&sub);
    let error =
        serve_general_proposal(&client, "id", &initial, &mut sub, "owner", producer.clone())
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("claim failed before provider send; no retry")
    );
    assert!(!serve_general_proposal(&client, "id", &initial, &mut sub, "owner", producer).unwrap());
    assert_eq!(model.count(), 0);
    assert_eq!(coordinator.count(), 1);
    assert_eq!(
        std::fs::read_to_string(root.path().join("budget.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[test]
fn general_provenance_and_raw_mismatch_fail_before_admission() {
    for field in ["model", "prompt_version", "usage", "provider"] {
        let (_root, mut sub) = enabled();
        let identity = config("http://127.0.0.1:1").identity();
        sub.configure_general_producer(identity.clone()).unwrap();
        let mut saved = general_completed(&sub, &identity);
        saved["proposal_round"]["provider_call"]["provenance"][field] = json!("corrupt");
        assert!(sub.accept_proposal_round(&saved).is_err());
        assert!(sub.contracts.is_empty());
    }
    let (_root, mut sub) = enabled();
    let identity = config("http://127.0.0.1:1").identity();
    sub.configure_general_producer(identity.clone()).unwrap();
    let mut saved = general_completed(&sub, &identity);
    let observed: ProviderCall =
        serde_json::from_value(saved["proposal_round"]["provider_call"].clone()).unwrap();
    sub.proposal_state
        .as_mut()
        .unwrap()
        .observed_calls
        .insert(1, observed);
    saved["proposal_round"]["provider_call"]["provenance"]["usage"]["input_tokens"] = json!(456);
    saved["proposal_round"]["provenance"]["usage"]["input_tokens"] = json!(456);
    assert!(sub.accept_proposal_round(&saved).is_err());
    assert!(sub.contracts.is_empty());
}
#[test]
fn general_error_timeout_durable_evidence_without_k_evidence() {
    for class in [ErrorClass::Timeout, ErrorClass::ProviderRefused] {
        let (_root, mut sub) = enabled();
        let identity = config("http://127.0.0.1:1").identity();
        sub.configure_general_producer(identity.clone()).unwrap();
        let state = if class == ErrorClass::Timeout {
            CallStatus::Timeout
        } else {
            CallStatus::ProviderError
        };
        let mut saved = status(&sub);
        saved["proposal_point"] = Value::Null;
        saved["search_state"]["proposal_round"]["outcome"] = json!(state);
        saved["proposal_round"] = json!({"round_seq":1,"status":state,"raw_output_base64":null,"raw_output_digest":null,"provenance":null,"outcomes":[],"error_class":state,
            "provider_call":ProviderCall{provenance:identity.unknown_usage(),status:state,error_class:Some(class)}});
        sub.accept_proposal_round(&saved).unwrap();
        assert!(sub.contracts.is_empty());
        assert_eq!(sub.proposal_recipes().count(), 0);
        assert!(
            saved["search_state"]["attempts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn d3_finish_model_and_usage_fail_closed_and_halt_across_restart() {
    for mutation in 0..7 {
        let (root, sub) = enabled();
        let mut reply = envelope(std::str::from_utf8(output().raw()).unwrap());
        match mutation {
            0 => reply["choices"][0]["finish_reason"] = json!("length"),
            1 => reply["choices"][0]["finish_reason"] = json!("tool_calls"),
            2 => {
                reply["choices"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("finish_reason");
            }
            3 => reply["usage"]["prompt_tokens"] = json!(20001),
            4 => reply["usage"]["completion_tokens"] = json!(2049),
            5 => reply["model"] = json!("unexpected-model"),
            _ => {
                reply.as_object_mut().unwrap().remove("model");
            }
        }
        let mock = Mock::new(move |_, _| Reply::json(reply.clone()));
        let mut budget = plan();
        budget.max_calls = 6;
        let path = root.path().join("d3-journal");
        let guard = Arc::new(CallBudget::create(&path, budget.clone()).unwrap());
        let producer = DeepSeekCandidateProducer::new_mock(config(&mock.endpoint), guard).unwrap();
        let request = sub.proposal_request_v2(&status(&sub)).unwrap();
        assert_eq!(
            producer.propose(&request).err().unwrap().class,
            ErrorClass::MalformedResponse
        );
        let reopened = Arc::new(CallBudget::reopen(&path, budget).unwrap());
        let restarted =
            DeepSeekCandidateProducer::new_mock(config(&mock.endpoint), reopened).unwrap();
        let mut next = request;
        next.search_id = "next_cell".into();
        assert!(restarted.propose(&next).is_err());
        assert_eq!(mock.count(), 1, "mutation {mutation}");
        let journal = std::fs::read_to_string(path).unwrap();
        assert!(!journal.contains("unexpected-model"));
        assert!(!journal.contains("DO_NOT_PERSIST_REASONING"));
    }
}

#[test]
fn d3_peak_reservation_rounds_each_side_up_and_bounds_total() {
    let budget = BudgetPlan {
        max_calls: 6,
        input_token_cap: 262144,
        output_token_cap: 2048,
        input_price: 300000,
        output_price: 1200000,
        ceiling_usd_micros: 5000000,
    };
    assert_eq!(budget.cost(262144, 0).unwrap(), 78644);
    assert_eq!(budget.cost(0, 2048).unwrap(), 2458);
    assert_eq!(budget.validate().unwrap(), 81102);
    assert_eq!(budget.validate().unwrap() * 6, 486612);
}

#[test]
fn preregistration_uses_exact_frozen_source_without_provider_or_status() {
    let (root, sub) = enabled();
    let evidence = sub.proposal_preregistration().unwrap();
    let request = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert_eq!(evidence["source_context"], json!(request.source_context));
    assert_eq!(
        evidence["source_context_sha256"],
        json!(
            request
                .source_context_sha256(sub.request.policy.proposal.as_ref().unwrap())
                .unwrap()
        )
    );
    std::fs::write(root.path().join("input/app.py"), "changed").unwrap();
    assert_eq!(evidence, sub.proposal_preregistration().unwrap());
}

#[test]
fn d3_unresolved_reservation_blocks_next_cell_without_refund() {
    let (root, _) = enabled();
    let mut budget = plan();
    budget.max_calls = 6;
    let path = root.path().join("pending");
    let guard = CallBudget::create(&path, budget.clone()).unwrap();
    guard.reserve("G0").unwrap();
    assert!(guard.reserve("G1").is_err());
    let restarted = CallBudget::reopen(&path, budget).unwrap();
    assert!(restarted.reserve("G0").is_err());
    assert!(restarted.reserve("G1").is_err());
}

fn request_evidence(cell: &str) -> RequestEvidence {
    RequestEvidence {
        cell: cell.into(),
        proposal_request_sha256: format!("sha256:{}", "a".repeat(64)),
        provider_body_sha256: format!("sha256:{}", "b".repeat(64)),
        timeout_ms: 29481,
        proposal_request_bytes: 1837,
        provider_body_bytes: 4261,
        transmitted_context: None,
    }
}
fn response_evidence(cell: &str) -> ResponseEvidence {
    ResponseEvidence {
        cell: cell.into(),
        finish_reason: FinishReason::Stop,
        model_matches: true,
        input_tokens: 123,
        output_tokens: 45,
    }
}
fn capture_request(
    request: &ato_formation::proposal::ProposalRequestV2,
) -> (RequestEvidence, Vec<u8>, String) {
    let root = tempfile::tempdir().unwrap();
    let reply = envelope(std::str::from_utf8(output().raw()).unwrap());
    let mock = Mock::new(move |_, _| Reply::json(reply.clone()));
    let producer = adapter(root.path(), &mock);
    producer.propose(request).unwrap();
    let path = root.path().join("budget.jsonl");
    let guard = CallBudget::reopen(&path, plan()).unwrap();
    let evidence = guard.inspect_request(&request.search_id).unwrap();
    assert!(evidence.response_resolved);
    let raw = mock.raw_seen.lock().unwrap()[0].clone();
    (
        evidence.request,
        raw,
        std::fs::read_to_string(path).unwrap(),
    )
}
#[test]
fn r1_dynamic_final_timeout_changes_request_hash() {
    let (_root, sub) = enabled();
    let mut request = sub.proposal_request_v2(&status(&sub)).unwrap();
    request.remaining_budget.timeout_ms = 29481;
    let first = capture_request(&request).0;
    request.remaining_budget.timeout_ms -= 1;
    let second = capture_request(&request).0;
    assert_ne!(
        first.proposal_request_sha256,
        second.proposal_request_sha256
    );
    assert_ne!(first.provider_body_sha256, second.provider_body_sha256);
    assert_eq!(first.timeout_ms, 29481);
    assert_eq!(second.timeout_ms, 29480);
}
#[test]
fn r2_same_final_request_has_same_hash() {
    let (_root, sub) = enabled();
    let request = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert_eq!(capture_request(&request).0, capture_request(&request).0);
}
#[test]
fn r3_exact_received_http_body_matches_durable_hash() {
    use sha2::{Digest, Sha256};
    let (_root, sub) = enabled();
    let request = sub.proposal_request_v2(&status(&sub)).unwrap();
    let (evidence, raw, _) = capture_request(&request);
    assert_eq!(
        evidence.provider_body_sha256,
        format!("sha256:{:x}", Sha256::digest(&raw))
    );
    assert_eq!(evidence.provider_body_bytes, raw.len() as u64);
    let body: Value = serde_json::from_slice(&raw).unwrap();
    let canonical = body["messages"][1]["content"].as_str().unwrap();
    assert_eq!(canonical, serde_jcs::to_string(&request).unwrap());
    assert_eq!(evidence.proposal_request_bytes, canonical.len() as u64);
    assert_eq!(
        evidence.proposal_request_sha256,
        format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
    );
}
#[test]
fn r4_evidence_write_failure_reads_no_credential_and_sends_no_http() {
    let (root, sub) = enabled();
    let mock = Mock::new(|_, _| panic!("HTTP forbidden"));
    let producer = adapter(root.path(), &mock);
    let path = root.path().join("budget.jsonl");
    // Force journal open/append failure without relying on user permissions.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        producer
            .propose(&sub.proposal_request_v2(&status(&sub)).unwrap())
            .is_err()
    );
    assert_eq!(producer.credential_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.count(), 0);
}
#[test]
fn r5_duplicate_request_and_legacy_request_mix_reject() {
    for legacy in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("budget");
        let guard = CallBudget::create(&path, plan()).unwrap();
        if legacy {
            guard.reserve("G0").unwrap();
        } else {
            guard.reserve_request(request_evidence("G0")).unwrap();
        }
        assert!(guard.reserve_request(request_evidence("G0")).is_err());
        // Tampering is also rejected when reopening, not just when appending.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", json!({"request":request_evidence("G0")})).unwrap();
        assert!(CallBudget::reopen(&path, plan()).is_err());
    }
}
#[test]
fn r6_response_requires_request_and_is_write_once() {
    let root = tempfile::tempdir().unwrap();
    let guard = CallBudget::create(&root.path().join("budget"), plan()).unwrap();
    assert!(guard.record_response(response_evidence("G0")).is_err());
    guard.reserve_request(request_evidence("G0")).unwrap();
    guard.record_response(response_evidence("G0")).unwrap();
    assert!(guard.record_response(response_evidence("G0")).is_err());
    let historical = CallBudget::create(&root.path().join("historical"), plan()).unwrap();
    historical.reserve("G0").unwrap();
    assert!(historical.record_response(response_evidence("G0")).is_err());
}
#[test]
fn r7_crash_reopen_preserves_request_and_consumes_reservation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("budget");
    let request = request_evidence("G0");
    CallBudget::create(&path, plan())
        .unwrap()
        .reserve_request(request.clone())
        .unwrap();
    let reopened = CallBudget::reopen(&path, plan()).unwrap();
    let saved = reopened.inspect_request("G0").unwrap();
    assert_eq!(saved.request, request);
    assert!(!saved.response_resolved);
    assert!(reopened.reserve_request(request_evidence("G1")).is_err());
    assert!(reopened.reserve_request(request).is_err());
}
#[test]
fn r8_provider_body_never_contains_auth_header_or_key() {
    let (_root, sub) = enabled();
    let (_, raw, _) = capture_request(&sub.proposal_request_v2(&status(&sub)).unwrap());
    let text = String::from_utf8(raw).unwrap();
    for secret in [
        "Authorization",
        "Bearer",
        "synthetic-mock-key",
        "DEEPSEEK_API_KEY",
    ] {
        assert!(!text.contains(secret));
    }
}
#[test]
fn r9_source_bodies_are_not_stored_in_request_journal() {
    let (_root, sub) = enabled();
    let (_, _, journal) = capture_request(&sub.proposal_request_v2(&status(&sub)).unwrap());
    for private in [
        "authorized frozen source",
        "other source",
        "source_context",
        "messages",
        "synthetic-mock-key",
        "DO_NOT_PERSIST_REASONING",
    ] {
        assert!(!journal.contains(private));
    }
    let event: Value = serde_json::from_str(journal.lines().nth(1).unwrap()).unwrap();
    assert!(event["request"]["proposal_request_bytes"].is_u64());
    assert!(event["request"]["provider_body_bytes"].is_u64());
    assert_eq!(event["request"].as_object().unwrap().len(), 7);
    assert!(event["request"]["transmitted_context"]["source_entries"].is_array());
}
#[test]
fn historical_cell_response_journal_remains_readable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("budget");
    std::fs::write(
        &path,
        format!(
            "{}\n\"G0\"\n{}\n",
            serde_json::to_string(&plan()).unwrap(),
            json!({"response": response_evidence("G0")})
        ),
    )
    .unwrap();
    let saved = CallBudget::reopen(&path, plan()).unwrap();
    assert!(saved.snapshot().unwrap().cells["G0"].response.is_some());
    assert!(saved.inspect_request("G0").is_err());
}

#[test]
fn transmitted_context_evidence_matches_actual_http_projection_without_text_or_secrets() {
    let (_root, sub) = enabled();
    let request = sub.proposal_request_v2(&status(&sub)).unwrap();
    let (evidence, raw, journal) = capture_request(&request);
    let body: Value = serde_json::from_slice(&raw).unwrap();
    let sent: ato_formation::proposal::ProposalRequestV2 =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let actual = super::super::budget::TransmittedContextEvidence::from_request(&sent);
    assert_eq!(evidence.transmitted_context.as_ref(), Some(&actual));
    assert_eq!(actual.source_entries.len(), sent.source_context.len());
    for entry in &sent.source_context {
        assert!(!journal.contains(&entry.text));
    }
    assert!(!journal.contains("synthetic-mock-key"));
    let old = request_evidence("old");
    assert!(
        !serde_json::to_string(&old)
            .unwrap()
            .contains("transmitted_context")
    );
}

#[test]
fn shared_api_uses_exact_input_and_distinct_inspection_call_keys() {
    use super::super::reasoning::{INPUT_SCHEMA, ReasoningInput, SourceIdentity};
    let (root, sub) = enabled();
    let view = status(&sub);
    let request = sub.proposal_request_v2(&view).unwrap();
    let mut input = ReasoningInput {
        available_variables: vec![],
        max_retries: 3,
        catalog_sources_in_inventory: false,
        goal: None,
        schema: INPUT_SCHEMA.into(),
        call_id: "shared_r1_s1".into(),
        frozen_contract_ref: sub.request.contract_ref.clone(),
        source_identity: SourceIdentity {
            archive_digest: sub.request.source.archive_digest.clone(),
            closure_ref: sub.request.source.closure_ref.clone(),
        },
        inventory: vec![],
        request,
        projection: vec![],
        rounds_remaining: 3,
        calls_remaining: 2,
        inspections_remaining: 4,
        inspection_source_bytes_remaining: 32768,
        inspection_feedback: vec![],
    };
    let mock = Mock::new(|_, _| {
        Reply::json(envelope(
            r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"insufficient_source"}]}"#,
        ))
    });
    let mut c = config(&mock.endpoint);
    c.prompt_version = PROMPT_VERSION_V5.into();
    let mut p = plan();
    p.max_calls = 2;
    p.input_token_cap = 24576;
    let budget = Arc::new(CallBudget::create(&root.path().join("shared-budget.jsonl"), p).unwrap());
    let adapter = DeepSeekCandidateProducer::new_mock(c, budget.clone()).unwrap();
    for step in 1..=2 {
        input.call_id = format!("shared_r1_s{step}");
        let output = adapter.propose_reasoning(&input).unwrap();
        assert_eq!(output.provenance.usage.input_tokens, Some(123));
        assert!(output.provenance.latency_ms.is_some());
        let seen = mock.seen.lock().unwrap();
        assert_eq!(
            seen.last().unwrap().1["messages"][1]["content"],
            serde_jcs::to_string(&input).unwrap()
        );
    }
    assert_eq!(mock.count(), 2);
    assert_eq!(budget.snapshot().unwrap().cells.len(), 2);
}

fn recovery_input(sub: &Submission) -> super::super::reasoning::ReasoningInput {
    super::super::reasoning::ReasoningInput {
        schema: super::super::reasoning::INPUT_SCHEMA.into(),
        catalog_sources_in_inventory: false,
        goal: None,
        available_variables: vec![],
        max_retries: 3,
        call_id: "recovery_r1_s1".into(),
        frozen_contract_ref: sub.request.contract_ref.clone(),
        source_identity: super::super::reasoning::SourceIdentity {
            archive_digest: sub.request.source.archive_digest.clone(),
            closure_ref: sub.request.source.closure_ref.clone(),
        },
        inventory: vec![],
        request: sub.proposal_request_v2(&status(sub)).unwrap(),
        projection: vec![],
        rounds_remaining: 3,
        calls_remaining: 6,
        inspections_remaining: 4,
        inspection_source_bytes_remaining: 32768,
        inspection_feedback: vec![],
    }
}
#[test]
fn shared_transient_calls_are_separate_charged_reservations_and_restart_cannot_reset_retry_three() {
    let (root, sub) = enabled();
    let input = recovery_input(&sub);
    let mock = Mock::new(|_, _| {
        let mut reply = Reply::json(json!({"error":"temporary"}));
        reply.status = 503;
        reply
    });
    let mut config = config(&mock.endpoint);
    config.prompt_version = PROMPT_VERSION_V6.into();
    let mut plan = plan();
    plan.max_calls = 6;
    plan.input_token_cap = 24576;
    let path = root.path().join("retry-budget.jsonl");
    let budget = Arc::new(CallBudget::create(&path, plan.clone()).unwrap());
    let adapter = DeepSeekCandidateProducer::new_mock(config.clone(), budget.clone()).unwrap();
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30000;
    assert_eq!(
        adapter
            .propose_reasoning_recover(&input, expires, 3)
            .err()
            .unwrap()
            .class,
        ErrorClass::ProviderUnavailable
    );
    assert_eq!(mock.count(), 4);
    assert!(
        budget
            .snapshot()
            .unwrap()
            .cells
            .values()
            .all(|c| c.charged_unknown && c.transport.as_ref().unwrap().http_status == Some(503))
    );
    let reopened = Arc::new(CallBudget::reopen(&path, plan).unwrap());
    let adapter = DeepSeekCandidateProducer::new_mock(config, reopened).unwrap();
    assert!(
        adapter
            .propose_reasoning_recover(&input, expires, 3)
            .is_err()
    );
    assert_eq!(mock.count(), 4);
}
#[test]
fn shared_authentication_failure_is_infrastructure_and_is_not_retried() {
    let (root, sub) = enabled();
    let input = recovery_input(&sub);
    let mock = Mock::new(|_, _| {
        let mut reply = Reply::json(json!({"error":"authentication"}));
        reply.status = 401;
        reply
    });
    let mut config = config(&mock.endpoint);
    config.prompt_version = PROMPT_VERSION_V6.into();
    let mut plan = plan();
    plan.max_calls = 6;
    plan.input_token_cap = 24576;
    let budget =
        Arc::new(CallBudget::create(&root.path().join("auth-budget.jsonl"), plan).unwrap());
    let adapter = DeepSeekCandidateProducer::new_mock(config, budget.clone()).unwrap();
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30000;
    assert_eq!(
        adapter
            .propose_reasoning_recover(&input, expires, 3)
            .err()
            .unwrap()
            .class,
        ErrorClass::ProviderAuthentication
    );
    assert_eq!(mock.count(), 1);
    assert!(budget.snapshot().unwrap().stopped);
}
