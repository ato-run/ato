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

#[test]
fn retained_descriptor_authority_binds_canonical_bytes_to_frozen_assignment() {
    use ato_formation::{authoring::BoundDerivation, retained::*};
    let base = fixture(false);
    let source = content_ref(b"source closure");
    let d: BoundDerivation=serde_json::from_value(json!({
        "schema":"ato.derivation/1", "inputs":[{"id":"workspace","protocol":"ato.workspace@1","content_ref":source}],
        "steps":[{"id":"site","protocol":"ato.browser@1","op":"serve","source":"workspace","root":"","entry":"index.html"}],
        "ports":[],"state":[],"effects":"pure"
    })).unwrap();
    let descriptor = RetainedCandidateV1 {
        schema: RETAINED_SCHEMA.into(),
        shape: RetainedShape::StaticWeb,
        artifact: RetainedArtifact {
            content_ref: content_ref(b"archive"),
            bytes: 7,
            expanded_bytes: 10,
        },
        materialization_ref: content_ref(b"manifest"),
        source_closure_ref: source,
        derivation_ref: d.derivation_ref().unwrap(),
        derivation: d,
        base_contract_ref: base["frozen"]["base_contract_ref"].as_str().unwrap().into(),
        contract_ref: base["frozen"]["effective_contract_ref"]
            .as_str()
            .unwrap()
            .into(),
        base_contract: serde_json::from_value(base["frozen"]["base_contract"].clone()).unwrap(),
        browser_contract: None,
        creation_attempt_id: "creation".into(),
        validation_profile: "ato.retained-static-web/1".into(),
    };
    let mut input = json!({"operation":"validate_retained","frozen":base["frozen"],"derivation_ref":descriptor.derivation_ref,
        "retained_ref":descriptor.retained_ref().unwrap(),"descriptor_json":String::from_utf8(descriptor.canonical_bytes().unwrap()).unwrap()});
    assert!(matches!(
        evaluate(&serde_json::to_vec(&input).unwrap()),
        Decision::Accepted
    ));
    input["derivation_ref"] = json!(content_ref(b"unauthorized D"));
    rejected(&input, "retained_assignment_mismatch");
    input["descriptor_json"] = json!("{}");
    rejected(&input, "retained_descriptor_invalid");
}

#[test]
fn shared_retained_fixture_has_canonical_bytes() {
    use ato_formation::retained::{RetainedCandidateV1, content_ref};
    let bytes = include_bytes!("fixtures/retained-static.json");
    let descriptor = RetainedCandidateV1::parse(bytes, &content_ref(bytes)).unwrap();
    assert_eq!(descriptor.canonical_bytes().unwrap(), bytes);
}
