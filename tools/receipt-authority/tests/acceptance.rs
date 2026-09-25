use ato_receipt_authority::{Decision, evaluate};
use serde_json::{Value, json};

fn fixture(browser: bool) -> Value {
    serde_json::from_str(if browser {
        include_str!("fixtures/browser-pass.json")
    } else {
        include_str!("fixtures/base-pass.json")
    })
    .unwrap()
}
fn rejected(value: &Value, code: &str) {
    match evaluate(&serde_json::to_vec(value).unwrap()) {
        Decision::Rejected { code: actual, .. } => assert_eq!(actual, code),
        Decision::Accepted => panic!("accepted forged receipt: {code}"),
    }
}
#[test]
fn native_authority_accepts_both_shared_golden_fixtures() {
    for browser in [false, true] {
        assert!(matches!(
            evaluate(&serde_json::to_vec(&fixture(browser)).unwrap()),
            Decision::Accepted
        ));
    }
}
#[test]
fn authority_rejects_missing_frozen_k_and_forged_digest() {
    rejected(&json!({}), "authority_input_malformed");
    let mut v = fixture(false);
    v["frozen"]["base_contract"]["requirements"][0]["status"] = json!(500);
    rejected(&v, "contract_digest_mismatch");
}
#[test]
fn a_valid_failed_receipt_never_becomes_a_verified_route() {
    let mut v = fixture(false);
    let r = &mut v["route"]["verifier_receipts"][0]["receipt"];
    r["fully_satisfied"] = json!(false);
    r["observations"][0]["outcome"] = json!("failed");
    r["observations"][0]["failure"] = json!("http_status_mismatch");
    rejected(&v, "receipt_contract_not_satisfied");
}
#[test]
fn browser_is_required_for_effective_k() {
    let mut v = fixture(true);
    v["route"]["verifier_receipts"]
        .as_array_mut()
        .unwrap()
        .pop();
    rejected(&v, "browser_receipt_missing");
}
#[test]
fn abi_rejects_unknown_operation_and_bounded_input() {
    rejected(
        &json!({"operation":"trust_pass"}),
        "authority_input_malformed",
    );
    assert!(
        matches!(evaluate(&vec![b' '; 1024*1024+1]), Decision::Rejected { code, .. } if code == "authority_input_too_large")
    );
}
