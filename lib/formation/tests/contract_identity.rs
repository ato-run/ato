//! Contract identity is authoring intent, never part of `K`.
//!
//! The preset synthesizer has always observed the source tree's identity. These
//! tests pin that Contract byte-for-byte, and show the two explicit modes:
//! `behavioral` drops the source observation (the same behavior is the same
//! Capsule across sources) while `source-bound` keeps it and refuses a
//! Contract that does not observe its source. An absent identity is exactly
//! today's behaviour.

use ato_formation::authoring::{
    AuthoringDraft, BindingContext, ContractIdentity, HTTP_CONTRACT_VERIFIER,
    WORKSPACE_CONTRACT_VERIFIER, bind,
};
use ato_formation::capsule_toml::{parse_capsule_toml, render_capsule_toml};
use ato_formation::capsule_toml_v2::parse_capsule_toml_v2;
use ato_formation::preset::{
    AppPreset, apply_contract_identity, synthesize_authoring, synthesize_authoring_with_identity,
};

const CLOSURE_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const CLOSURE_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The preset Contract for `CLOSURE_A`, pinned as bytes. If this changes, an
/// existing Capsule's identity changed with it.
const PINNED_CONTRACT_REF: &str =
    "sha256:88a92b99913677c12355e3633c1c35a9db75492c0c1b516ab6c9a2627ac2c4d8";
/// The same document rendered before the identity key existed. It must stay
/// byte-identical.
const PINNED_RENDERED_AUTHORED: &str = "platform = []\nruntime = []\nschema = \"ato.capsule/1\"\nstate = []\n\n[[contract.require]]\nid = \"page\"\nmethod = \"GET\"\npath = \"/\"\nport = \"app.http\"\nuse = \"ato.contract.http@1\"\n\n[contract.require.expect]\nstatus = 200\n\n[[derive.step]]\ncwd = \"\"\nentry = \"index.html\"\nid = \"web\"\nop = \"serve\"\nsource = \"workspace\"\nuse = \"ato.browser@1\"\n\n[derive.step.env]\n\n[effects]\ndefault = \"pure\"\n\n[[input]]\nid = \"workspace\"\npath = \".\"\nuse = \"ato.workspace@1\"\n\n[[port]]\nfrom = \"web\"\nid = \"app.http\"\nuse = \"ato.http@1\"\n\n[requirements]\n";
const PINNED_CONTRACT_JSON: &str = "{\"requirements\":[{\"id\":\"root\",\"method\":\"GET\",\"path\":\"/\",\"port\":\"app.http\",\"status\":200,\"verifier\":\"ato.contract.http@1\"},{\"digest\":\"sha256:1111111111111111111111111111111111111111111111111111111111111111\",\"id\":\"source-identity\",\"input\":\"workspace\",\"verifier\":\"ato.contract.workspace@1\"}],\"schema\":\"ato.contract/1\"}";

fn ctx(closure: &'static str) -> BindingContext<'static> {
    BindingContext {
        source_closure_ref: closure,
    }
}

fn bound_contract(
    draft: &AuthoringDraft,
    closure: &'static str,
) -> ato_formation::authoring::BoundContract {
    bind(draft, &ctx(closure)).expect("binds").0
}

/// A minimal authored v1 route that observes the HTTP surface only.
const AUTHORED_NO_IDENTITY: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "web"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"

[[port]]
id = "app.http"
use = "ato.http@1"
from = "web"

[[contract.require]]
id = "page"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
method = "GET"

[contract.require.expect]
status = 200
"#;

fn with_v1_contract_header(identity: Option<&str>) -> String {
    let header = match identity {
        Some(value) => format!("[contract]\nidentity = \"{value}\"\n\n"),
        None => String::new(),
    };
    AUTHORED_NO_IDENTITY.replacen(
        "[[contract.require]]",
        &format!("{header}[[contract.require]]"),
        1,
    )
}

#[test]
fn todays_preset_contract_bytes_are_pinned() {
    let draft = synthesize_authoring(AppPreset::StaticFiles);
    // The preset states its identity explicitly; it just is not part of K.
    assert_eq!(draft.contract.identity, ContractIdentity::SourceBound);

    let contract = bound_contract(&draft, CLOSURE_A);
    assert_eq!(contract.schema, "ato.contract/1");
    assert_eq!(contract.requirements.len(), 2);
    assert_eq!(contract.requirements[0].id, "root");
    assert_eq!(contract.requirements[0].verifier, HTTP_CONTRACT_VERIFIER);
    assert_eq!(contract.requirements[1].id, "source-identity");
    assert_eq!(
        contract.requirements[1].verifier,
        WORKSPACE_CONTRACT_VERIFIER
    );
    assert_eq!(contract.requirements[1].input.as_deref(), Some("workspace"));
    assert_eq!(
        contract.requirements[1].digest.as_deref(),
        Some(CLOSURE_A),
        "the preset still captures the source closure into K"
    );

    let json =
        String::from_utf8(serde_jcs::to_vec(&contract).expect("canonicalizes")).expect("utf8");
    assert_eq!(contract.contract_ref().unwrap(), PINNED_CONTRACT_REF);
    assert_eq!(json, PINNED_CONTRACT_JSON);

    // Every preset shares this K template; only the Derivation differs.
    for preset in [
        AppPreset::SingleHtml,
        AppPreset::StaticFiles,
        AppPreset::NodeStatic,
        AppPreset::SingleJsx,
    ] {
        assert_eq!(
            bound_contract(&synthesize_authoring(preset), CLOSURE_A)
                .contract_ref()
                .unwrap(),
            PINNED_CONTRACT_REF,
            "{preset:?} changed the preset Contract"
        );
    }
}

#[test]
fn a_behavioral_preset_contract_is_the_same_for_every_source() {
    let draft =
        synthesize_authoring_with_identity(AppPreset::StaticFiles, ContractIdentity::Behavioral);
    assert_eq!(draft.contract.identity, ContractIdentity::Behavioral);

    let mut refs = Vec::new();
    for closure in [CLOSURE_A, CLOSURE_B] {
        let contract = bound_contract(&draft, closure);
        assert!(
            contract
                .requirements
                .iter()
                .all(|requirement| requirement.verifier != WORKSPACE_CONTRACT_VERIFIER),
            "a behavioral Contract must not observe the source: {contract:?}"
        );
        assert_eq!(contract.requirements.len(), 1);
        assert_eq!(contract.requirements[0].verifier, HTTP_CONTRACT_VERIFIER);
        refs.push(contract.contract_ref().unwrap());
    }
    assert_eq!(
        refs[0], refs[1],
        "two sources that behave the same are one behavioral Capsule"
    );
}

#[test]
fn a_source_bound_preset_contract_tracks_the_source_tree() {
    let draft =
        synthesize_authoring_with_identity(AppPreset::SingleHtml, ContractIdentity::SourceBound);
    let first = bound_contract(&draft, CLOSURE_A).contract_ref().unwrap();
    let second = bound_contract(&draft, CLOSURE_B).contract_ref().unwrap();
    assert_ne!(
        first, second,
        "a source-bound Contract must name the exact source closure"
    );
}

#[test]
fn a_behavioral_contract_may_not_observe_the_source() {
    let mut draft = synthesize_authoring(AppPreset::SingleHtml);
    draft.contract.identity = ContractIdentity::Behavioral;
    let error = bind(&draft, &ctx(CLOSURE_A)).expect_err("must refuse");
    assert_eq!(error.code(), "behavioral_contract_has_source_requirement");
}

#[test]
fn a_source_bound_contract_must_observe_the_source() {
    let mut draft =
        synthesize_authoring_with_identity(AppPreset::SingleHtml, ContractIdentity::Behavioral);
    draft.contract.identity = ContractIdentity::SourceBound;
    let error = bind(&draft, &ctx(CLOSURE_A)).expect_err("must refuse");
    assert_eq!(
        error.code(),
        "source_bound_contract_has_no_source_requirement"
    );
}

#[test]
fn an_authored_contract_without_an_identity_is_unchanged() {
    let draft = parse_capsule_toml(AUTHORED_NO_IDENTITY).expect("parses");
    assert_eq!(draft.contract.identity, ContractIdentity::Unspecified);
    let contract = bound_contract(&draft, CLOSURE_A);
    assert!(
        contract
            .requirements
            .iter()
            .all(|requirement| requirement.verifier != WORKSPACE_CONTRACT_VERIFIER),
        "no identity means bind exactly what was authored"
    );

    let rendered = render_capsule_toml(&draft).expect("renders");
    assert!(
        !rendered.contains("identity"),
        "an absent identity must not be written: {rendered}"
    );
    assert_eq!(rendered, PINNED_RENDERED_AUTHORED, "rendered bytes moved");
    let reparsed = parse_capsule_toml(&rendered).expect("reparses");
    assert_eq!(reparsed.contract, draft.contract);
    assert_eq!(render_capsule_toml(&reparsed).expect("rerenders"), rendered);
}

#[test]
fn an_explicit_identity_round_trips_through_the_v1_grammar() {
    let text = with_v1_contract_header(Some("behavioral"));
    let draft = parse_capsule_toml(&text).expect("explicit behavioral parses");
    assert_eq!(draft.contract.identity, ContractIdentity::Behavioral);
    // A behavioral Contract whose authored observations are HTTP-only binds.
    bind(&draft, &ctx(CLOSURE_A)).expect("binds");

    let rendered = render_capsule_toml(&draft).expect("renders");
    assert!(
        rendered.contains("identity"),
        "an explicit identity must round-trip: {rendered}"
    );
    let reparsed = parse_capsule_toml(&rendered).expect("reparses");
    assert_eq!(reparsed.contract, draft.contract);
}

#[test]
fn a_source_bound_v1_contract_without_a_source_observation_is_refused_by_bind() {
    let text = with_v1_contract_header(Some("source-bound"));
    let draft = parse_capsule_toml(&text).expect("grammar accepts the identity");
    let error = bind(&draft, &ctx(CLOSURE_A)).expect_err("must refuse");
    assert_eq!(
        error.code(),
        "source_bound_contract_has_no_source_requirement"
    );
}

#[test]
fn an_unknown_v1_identity_is_refused_by_name() {
    let text = with_v1_contract_header(Some("source_bound"));
    let error = parse_capsule_toml(&text).expect_err("must refuse");
    assert_eq!(error.code(), "authoring_malformed");
    assert!(error.to_string().contains("contract.identity"), "{error}");
}

#[test]
fn a_v2_manifest_may_state_behavioral_without_changing_the_contract_body() {
    let base = include_str!("../../../samples/portable-model-set-probe/capsule.toml");
    let absent = parse_capsule_toml_v2(base).expect("absent identity parses");
    let behavioral = parse_capsule_toml_v2(&base.replacen(
        "[contract]",
        "[contract]\nidentity = \"behavioral\"",
        1,
    ))
    .expect("behavioral parses");
    assert_eq!(
        absent, behavioral,
        "identity is authoring metadata; the portable Contract body is unchanged"
    );
}

#[test]
fn a_v2_manifest_may_not_state_source_bound_or_an_unknown_identity() {
    let base = include_str!("../../../samples/portable-model-set-probe/capsule.toml");
    for (identity, expected) in [
        ("source-bound", "cannot bind a source identity"),
        ("bogus", "is not a Contract identity"),
    ] {
        let text = base.replacen(
            "[contract]",
            &format!("[contract]\nidentity = \"{identity}\""),
            1,
        );
        let error = parse_capsule_toml_v2(&text).expect_err("must refuse");
        match error {
            ato_formation::capsule_toml_v2::CapsuleTomlV2Error::Invalid { path, message } => {
                assert_eq!(path, "contract.identity");
                assert!(message.contains(expected), "{message}");
            }
            other => panic!("expected contract.identity refusal, got {other:?}"),
        }
    }
}

#[test]
fn restating_a_synthesized_draft_under_an_identity_matches_direct_synthesis() {
    // `node-static/v2` starts from the `NodeStatic` draft and replaces its D,
    // then applies the requested identity. That restatement must be exactly
    // what the identity-aware synthesizer would have produced.
    let restated = apply_contract_identity(
        synthesize_authoring(AppPreset::NodeStatic),
        ContractIdentity::Behavioral,
    );
    let direct =
        synthesize_authoring_with_identity(AppPreset::NodeStatic, ContractIdentity::Behavioral);
    assert_eq!(restated.contract, direct.contract);

    let restated_bound = apply_contract_identity(
        synthesize_authoring(AppPreset::NodeStatic),
        ContractIdentity::SourceBound,
    );
    assert_eq!(
        restated_bound.contract,
        synthesize_authoring(AppPreset::NodeStatic).contract
    );

    let untouched = apply_contract_identity(
        synthesize_authoring(AppPreset::NodeStatic),
        ContractIdentity::Unspecified,
    );
    assert_eq!(
        untouched.contract,
        synthesize_authoring(AppPreset::NodeStatic).contract,
        "an absent request must not restate anything"
    );
}
