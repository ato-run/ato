//! Two authoring frontends, one Contract + Derivation pipeline.
//!
//! The unit tests beside each module pin what it does alone. These pin the
//! claims that only hold ACROSS the modules — and every one of them is a claim
//! that would decay silently:
//!
//! - a Preset and an equivalent hand-written `capsule.toml` are the same
//!   Capsule and the same route, digest for digest;
//! - who wrote a draft never reaches a digest;
//! - the author controls how coarse their Capsule is, in both directions;
//! - a document that is not this grammar's is handed back, not misread;
//! - the same route run twice lands on the same identity.

use std::path::Path;

use ato_formation::authoring::{
    AuthoringDraft, BindingContext, BoundContract, BoundDerivation, bind,
};
use ato_formation::capsule_toml::{
    CAPSULE_FILE_NAME, CapsuleDocumentKind, classify, parse_capsule_toml, read_capsule_toml,
};
use ato_formation::detect::detect;
use ato_formation::preset::{select_preset, synthesize_authoring};
use ato_formation::projection::project;
use ato_formation::verify::{CandidateObservation, ObservationOutcome, verify};

const SOURCE_A: &str = "sha256:aaaaaaaa";
const SOURCE_B: &str = "sha256:bbbbbbbb";

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn refs_of(draft: &AuthoringDraft, source: &str) -> (String, String) {
    let (contract, derivation) = bind(
        draft,
        &BindingContext {
            source_closure_ref: source,
        },
    )
    .expect("binds");
    (
        contract.contract_ref().expect("contract digest"),
        derivation.derivation_ref().expect("derivation digest"),
    )
}

fn bound(draft: &AuthoringDraft, source: &str) -> (BoundContract, BoundDerivation) {
    bind(
        draft,
        &BindingContext {
            source_closure_ref: source,
        },
    )
    .expect("binds")
}

/// The draft a source with no `capsule.toml` gets — through the real frontend
/// selection, so a change to either half shows up here.
fn preset_draft(dir: &Path) -> AuthoringDraft {
    assert_eq!(
        read_capsule_toml(dir).expect("reads"),
        None,
        "this fixture must have no capsule.toml, or it is not testing the Preset"
    );
    let evidence = detect(dir).expect("detects");
    synthesize_authoring(select_preset(&evidence).expect("fits a preset"))
}

/// A hand-written document that says exactly what `single-html/v1` synthesizes.
const EQUIVALENT_TO_SINGLE_HTML: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"
spa_fallback = false

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"

[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"

[contract.require.expect]
status = 200

[[contract.require]]
id = "source-identity"
use = "ato.contract.workspace@1"
input = "workspace"

[contract.require.expect]
digest = "capture"
"#;

// ─── 1 + 2. one HTML file, and the TOML that means the same thing ───────────

#[test]
fn a_single_html_file_needs_no_toml_and_still_gets_a_contract() {
    let dir = tree(&[("expense.html", "<!doctype html><p>hi")]);
    let draft = preset_draft(dir.path());
    let (contract, derivation) = bound(&draft, SOURCE_A);

    // The Contract observes that the app answers, AND which source it came
    // from. Both, because "answers 200" alone would make every static page in
    // the world the same Capsule.
    let ids: Vec<&str> = contract
        .requirements
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, vec!["root", "source-identity"]);
    assert!(contract.contract_ref().is_ok());
    assert_eq!(derivation.inputs[0].content_ref, SOURCE_A);

    // And it projects onto something this worker can actually run.
    let projected = project(&derivation, &contract).expect("projects");
    assert_eq!(projected.overrides.get("lane"), Some("static_web"));
}

#[test]
fn an_equivalent_hand_written_document_is_the_same_capsule_and_the_same_route() {
    // The claim the whole Preset unification rests on. Two frontends, one
    // canonical pair: a person who writes out what the Preset would have
    // synthesized gets the same Capsule identity and the same Derivation
    // identity, not a parallel one that merely behaves alike.
    let dir = tree(&[("index.html", "<!doctype html><p>hi")]);
    let preset = refs_of(&preset_draft(dir.path()), SOURCE_A);
    let authored = refs_of(
        &parse_capsule_toml(EQUIVALENT_TO_SINGLE_HTML).expect("parses"),
        SOURCE_A,
    );
    assert_eq!(preset.0, authored.0, "ContractRef");
    assert_eq!(preset.1, authored.1, "DerivationRef");
}

#[test]
fn who_wrote_the_draft_never_reaches_a_digest() {
    // Stated separately from the test above because it is the invariant, not
    // the example: provenance is real, recorded, and outside both digests.
    let dir = tree(&[("index.html", "<!doctype html><p>hi")]);
    let synthesized = preset_draft(dir.path());
    let authored = parse_capsule_toml(EQUIVALENT_TO_SINGLE_HTML).expect("parses");
    assert_ne!(synthesized.provenance, authored.provenance);
    assert_eq!(
        refs_of(&synthesized, SOURCE_A),
        refs_of(&authored, SOURCE_A)
    );
}

// ─── 3. a deliberately weak Contract ────────────────────────────────────────

const DELIBERATELY_WEAK: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"

[[contract.require]]
id = "up"
use = "ato.contract.http@1"
port = "app.http"
path = "/"

[contract.require.expect]
status = 200
"#;

#[test]
fn an_author_who_asked_to_preserve_less_gets_one_capsule_and_two_routes() {
    // The author said: I do not care which page this is, only that something
    // answers. Two different sources are then the SAME resumable point — one
    // ContractRef — reached by two different routes, separately addressed.
    //
    // Ato must not quietly add source identity back. Strengthening a Contract
    // the author deliberately weakened is the same class of mistake as
    // replacing an invalid one with a guess.
    let draft = parse_capsule_toml(DELIBERATELY_WEAK).expect("parses");
    let (contract_a, derivation_a) = refs_of(&draft, SOURCE_A);
    let (contract_b, derivation_b) = refs_of(&draft, SOURCE_B);
    assert_eq!(contract_a, contract_b, "the author chose this coarseness");
    assert_ne!(derivation_a, derivation_b, "two sources are two routes");
}

// ─── 4. the Preset's Contract is conservative ───────────────────────────────

#[test]
fn two_different_static_sources_are_two_different_capsules_under_the_preset() {
    // The collision this prevents: without source identity in the Preset's
    // Contract, every static upload that answers 200 would share one Capsule
    // identity, and one person's App could be resumed as another's.
    let dir = tree(&[("index.html", "<!doctype html><p>one")]);
    let draft = preset_draft(dir.path());
    let (contract_a, _) = refs_of(&draft, SOURCE_A);
    let (contract_b, _) = refs_of(&draft, SOURCE_B);
    assert_ne!(contract_a, contract_b);
}

// ─── 5 + 6 + 7. grammar ownership ───────────────────────────────────────────

#[test]
fn a_malformed_capsule_document_fails_hard_and_never_falls_back_to_a_preset() {
    // The strict authoring rule. This tree ALSO fits `single-html/v1` — the
    // Preset would happily form it — which is exactly why the fallback must
    // not happen: an author who wrote a route and a contract must not silently
    // get a guessed one because their document had a typo.
    let dir = tree(&[
        ("index.html", "<!doctype html><p>hi"),
        (
            CAPSULE_FILE_NAME,
            "schema = \"ato.capsule/1\"\n[[derive.step]]\nid = \"a\"\nuse = \"ato.process@1\"\nop = \"serve\"\n",
        ),
    ]);
    let evidence = detect(dir.path()).expect("detects");
    assert!(
        select_preset(&evidence).is_ok(),
        "the fallback this test forbids must be available, or it proves nothing"
    );

    let text = read_capsule_toml(dir.path())
        .expect("reads")
        .expect("present");
    let error = parse_capsule_toml(&text).unwrap_err();
    assert_eq!(error.code(), "authoring_malformed");
}

#[test]
fn an_unknown_field_is_a_strict_failure() {
    let error = parse_capsule_toml(&EQUIVALENT_TO_SINGLE_HTML.replace(
        "[[port]]\nid = \"app.http\"",
        "[[port]]\nnope = 1\nid = \"app.http\"",
    ))
    .unwrap_err();
    assert_eq!(error.code(), "authoring_malformed");
}

#[test]
fn a_store_submission_manifest_is_never_fed_to_this_parser() {
    // The regression that started the rework, closed by ownership rather than
    // by loosening: a store manifest is a different grammar with a different
    // owner. It is recognised by its own discriminator and declined by name.
    const LEGACY: &str = r#"
schema_version = "1"
name = "expense-tracker"
version = "1.0.0"

[source]
root = "."

[metadata]
tags = ["demo"]

[run]
command = ["python3", "app.py"]

[web]
port = 8000
bind = "0.0.0.0"

[build]
steps = []
"#;
    assert_eq!(
        classify(LEGACY).expect("classifies"),
        CapsuleDocumentKind::LegacyStoreManifest
    );
    assert_eq!(
        parse_capsule_toml(LEGACY).unwrap_err().code(),
        "legacy_store_manifest"
    );

    // And a document that declares neither is refused rather than guessed at.
    assert_eq!(
        classify("name = \"x\"\n").expect("classifies"),
        CapsuleDocumentKind::Unknown
    );
}

// ─── 8. the same route, run twice ───────────────────────────────────────────

#[test]
fn the_same_route_executed_twice_lands_on_the_same_identity() {
    // Semantic idempotence, at the level this layer can assert it: the same
    // Derivation over the same inputs canonicalizes identically every time,
    // and both executions are judged against the same Contract. PIDs, ports
    // and timestamps are outside `K` and cannot move it.
    let dir = tree(&[("index.html", "<!doctype html><p>hi")]);
    let draft = preset_draft(dir.path());
    let first = refs_of(&draft, SOURCE_A);
    let second = refs_of(&draft, SOURCE_A);
    assert_eq!(first, second);

    let (contract, derivation) = bound(&draft, SOURCE_A);
    let candidate = CandidateObservation {
        input_refs: [("workspace".to_owned(), SOURCE_A.to_owned())].into(),
        exported_ports: ["app.http".to_owned()].into(),
        statically_served_paths: ["/".to_owned()].into(),
        runtime_readiness: None,
    };
    for _ in 0..2 {
        let verification = verify(&contract, &candidate);
        assert!(verification.passed(), "{verification:?}");
    }
    assert!(project(&derivation, &contract).is_ok());
}

// ─── 9. the committed FastAPI fixture ───────────────────────────────────────

#[test]
fn the_committed_fixture_is_a_contract_and_a_route_this_build_can_run() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/fastapi-sqlite-personal/capsule.toml"
    );
    let text = std::fs::read_to_string(path).expect("the fixture is committed");
    let draft = parse_capsule_toml(&text).expect("the fixture parses");
    let (contract, derivation) = bound(&draft, SOURCE_A);

    // What it chose to preserve: that the app answers, and which source it is.
    // Not the response body — this app's notes change on every save, and
    // observing them would mint a new Capsule each time somebody typed.
    let ids: Vec<&str> = contract
        .requirements
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, vec!["app-responds", "source-identity"]);

    // Exactly one writable slot, declared, mounted where the app was told its
    // database lives.
    assert_eq!(derivation.state.len(), 1);
    assert_eq!(derivation.state[0].mount, "/data");

    let projected = project(&derivation, &contract).expect("projects");
    assert_eq!(projected.overrides.get("lane"), Some("python_process"));
    assert_eq!(
        projected.overrides.get("env.APP_DB_PATH"),
        Some("/data/app.sqlite")
    );

    // The readiness gate probes the path the Contract observes, because it was
    // built from it. That is what lets the HTTP observation be deferred to the
    // Run rather than assumed.
    let readiness = projected.readiness.clone().expect("a process is probed");
    assert_eq!(readiness.path, "/health");

    let verification = verify(
        &contract,
        &CandidateObservation {
            input_refs: [("workspace".to_owned(), SOURCE_A.to_owned())].into(),
            exported_ports: ["app.http".to_owned()].into(),
            statically_served_paths: Default::default(),
            runtime_readiness: Some((readiness.port_id, readiness.path)),
        },
    );
    assert!(verification.passed(), "{verification:?}");
    assert_eq!(
        verification.verdicts[1].outcome,
        ObservationOutcome::Satisfied,
        "the source's identity is decided at formation time"
    );
    assert!(matches!(
        verification.verdicts[0].outcome,
        ObservationOutcome::Deferred { .. }
    ));
}

// ─── 10. Formation succeeds only when the Contract does ─────────────────────

#[test]
fn a_candidate_that_does_not_satisfy_the_contract_is_not_this_capsule() {
    let dir = tree(&[("index.html", "<!doctype html><p>hi")]);
    let (contract, _) = bound(&preset_draft(dir.path()), SOURCE_A);
    // The build ran and produced an artifact — from a different tree. Under a
    // model where identity IS the Contract, sealing this would hand back a
    // Capsule that claims to be one source while holding another.
    let verification = verify(
        &contract,
        &CandidateObservation {
            input_refs: [("workspace".to_owned(), SOURCE_B.to_owned())].into(),
            exported_ports: ["app.http".to_owned()].into(),
            statically_served_paths: ["/".to_owned()].into(),
            runtime_readiness: None,
        },
    );
    assert!(!verification.passed());
    assert_eq!(verification.failure().unwrap().1, "input_identity_mismatch");
}
