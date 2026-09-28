use super::super::{
    budget::{BudgetPlan, CallBudget},
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
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":content,"reasoning_content":"DO_NOT_PERSIST_REASONING"}}],"usage":{"prompt_tokens":123,"completion_tokens":45}})
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
    assert!(
        !serve_general_proposal(&client, "id", &initial, &mut sub, "owner", producer.clone())
            .unwrap()
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
    sub.proposal_state.as_mut().unwrap().observed_call = Some(observed);
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
