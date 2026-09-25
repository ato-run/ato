//! Generate cross-language acceptance fixtures through the native Rust authority.
use ato_formation::authoring::BoundContract;
use ato_formation::browser::{
    BROWSER_VERIFIER_PROTOCOL, BrowserContractV0, BrowserEvent, BrowserEvidence, BrowserTarget,
    BrowserVerdict, BrowserVerificationReceipt, BrowserVerificationResult, CriterionResult,
    EVENT_NAVIGATION, EVIDENCE_BROWSER_SNAPSHOT, JudgeDecision, PRIMARY_CRITERION_ID,
    VerifierContainment, VerifierIdentity, effective_contract_ref,
};
use ato_formation::verify::{
    ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation, verify_runtime,
};
use serde_json::{Value, json};
fn browser_receipt(
    contract: &BrowserContractV0,
    verdict: BrowserVerdict,
    attempt_id: &str,
) -> serde_json::Value {
    let choice = match verdict {
        BrowserVerdict::Pass => Some("complete"),
        BrowserVerdict::Fail => Some("incomplete"),
        BrowserVerdict::Inconclusive => None,
    };
    let result = BrowserVerificationResult {
        protocol: BROWSER_VERIFIER_PROTOCOL.to_owned(),
        verdict,
        criteria: vec![CriterionResult {
            id: PRIMARY_CRITERION_ID.to_owned(),
            verdict,
            decision: choice.map(|choice| JudgeDecision {
                choice: choice.to_owned(),
                confidence: 0.9,
                probabilities: [(choice.to_owned(), 0.95)].into(),
                model: "jev-1.13.0".to_owned(),
            }),
            rounds: 1,
            evidence_refs: vec!["e1".to_owned()],
            reason: None,
        }],
        evidence: vec![BrowserEvidence {
            id: "e1".to_owned(),
            kind: EVIDENCE_BROWSER_SNAPSHOT.to_owned(),
            sequence: 1,
            url: Some("http://127.0.0.1:41234/".to_owned()),
            title: Some("hi".to_owned()),
            facts: vec!["the page says hi".to_owned()],
            text_excerpt: None,
        }],
        observed_events: vec![BrowserEvent {
            sequence: 1,
            kind: EVENT_NAVIGATION.to_owned(),
            url: Some("http://127.0.0.1:41234/".to_owned()),
        }],
        action_trace: Vec::new(),
        verifier: VerifierIdentity {
            verifier: "formation-browser-verifier/0".to_owned(),
            stagehand_version: None,
            browser_version: None,
            agent_model: None,
            judge_model: Some("jev-1.13.0".to_owned()),
        },
        reason: None,
    };
    let mut receipt = BrowserVerificationReceipt::new(
        contract,
        BrowserTarget {
            runtime_id: "rt_test".to_owned(),
            endpoint: "http://127.0.0.1:41234/".to_owned(),
            attempt_id: Some(attempt_id.to_owned()),
        },
        result,
    )
    .expect("a valid result");
    receipt.containment = Some(VerifierContainment {
        containment: "bwrap".to_owned(),
        filesystem: "allowlisted".to_owned(),
        network: "loopback candidate only".to_owned(),
        browser_process: "own pid namespace".to_owned(),
        secrets: "fd".to_owned(),
    });
    serde_json::json!({ "kind": "browser_contract", "receipt": receipt })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("fixture directory required")?;
    std::fs::create_dir_all(&directory)?;
    let contract: BoundContract = serde_json::from_value(json!({
        "schema":"ato.contract/1", "requirements":[{
            "id":"root", "verifier":"ato.contract.http@1", "port":"app.http",
            "method":"GET", "path":"/", "status":200
        }]
    }))?;
    let k = contract.contract_ref()?;
    let d = format!("sha256:{}", "d".repeat(64));
    let observed = RuntimeObservation {
        http: vec![RuntimeHttpObservation::from_response(
            "app.http", "GET", "/", 200, b"hi",
        )],
        ..Default::default()
    };
    let mut receipt = ContractVerificationReceipt::from_attempt(
        &k,
        &d,
        &contract,
        &observed,
        verify_runtime(&contract, &observed),
    );
    receipt.execution = Some(serde_json::from_value(
        json!({"realization":"process", "attempt_id":"att_test", "request_id":"sat_test"}),
    )?);
    for browser in [false, true] {
        let browser_contract = if browser {
            Some(BrowserContractV0::from_prompt(
                "Create a note and see it after a reload.",
            )?)
        } else {
            None
        };
        let effective = effective_contract_ref(&k, browser_contract.as_ref());
        let mut receipts = vec![json!({"kind":"contract_verification", "receipt":receipt})];
        if let Some(b) = &browser_contract {
            receipts.push(browser_receipt(b, BrowserVerdict::Pass, "att_test"));
        }
        let request: Value = json!({
            "operation":"accept_route", "frozen": { "base_contract":contract, "base_contract_ref":k, "effective_contract_ref":effective, "browser_contract":browser_contract },
            "request_id":"sat_test", "authorized_derivations":[d],
            "route": {"effective_contract_ref":effective,"derivation_ref":d,"runtime_id":"rt_test","environment_id":"native","attempt_id":"att_test","verifier_receipts":receipts},
            "attempt": {"status":"pass","derivation_ref":d,"runtime_id":"rt_test","environment_id":"native","attempt_id":"att_test"}
        });
        let bytes = serde_json::to_vec_pretty(&request)?;
        assert!(matches!(
            ato_receipt_authority::evaluate(&bytes),
            ato_receipt_authority::Decision::Accepted
        ));
        let name = if browser {
            "browser-pass.json"
        } else {
            "base-pass.json"
        };
        std::fs::write(std::path::Path::new(&directory).join(name), bytes)?;
    }
    Ok(())
}
