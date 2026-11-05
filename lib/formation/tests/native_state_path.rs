//! Native state delivery (issue #1690 Phase E): canonical bytes and the
//! fail-closed direction of the new `mount_required` declaration.
//!
//! The golden strings below were produced by the code before the field
//! existed. They are the regression: a Derivation formed without the new
//! declaration must keep exactly those bytes and exactly that
//! `DerivationRef`, because that ref is what a bundle and a receipt name.

use std::collections::BTreeMap;

use ato_formation::{
    authoring::{BindingContext, BoundDerivation, bind},
    capsule_toml::parse_capsule_toml,
};

/// The pre-existing authored grammar, unchanged.
const AUTHORED: &str = r#"
schema = "ato.capsule/1"
[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
[[runtime]]
name = "python"
version = "3.12.7"
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["python3", "-B", "/app/app.py"]
[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000
[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"
[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
[contract.require.expect]
status = 200
"#;

/// Bytes the pre-change builder emitted for the Derivation above.
const OLD_DERIVATION_JSON: &str = concat!(
    r#"{"effects":"pure","inputs":[{"content_ref":"sha256:"#,
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    r#"","id":"workspace","protocol":"ato.workspace@1"}],"ports":[{"from":"app","guest_port":8000,"id":"app.http","protocol":"ato.http@1"}],"runtimes":{"python":"3.12.7"},"schema":"ato.derivation/1","state":[{"access":"read-write","id":"data","mount":"/data","protocol":"ato.state.filesystem@1"}],"steps":[{"argv":["python3","-B","/app/app.py"],"id":"app","op":"serve","protocol":"ato.process@1"}]}"#,
);
const OLD_DERIVATION_REF: &str =
    "sha256:c1c2d2a7e103dfcb759ac4aae28aabc6cf714d87d488b593bfb697978f4d8e04";
const OLD_STATE_JSON: &str =
    r#"{"access":"read-write","id":"data","mount":"/data","protocol":"ato.state.filesystem@1"}"#;

fn bound_derivation() -> BoundDerivation {
    let draft = parse_capsule_toml(AUTHORED).expect("the authored grammar still parses");
    let closure = format!("sha256:{}", "a".repeat(64));
    let (_, derivation) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &closure,
        },
    )
    .expect("binding succeeds");
    derivation
}

#[test]
fn an_undeclared_slot_keeps_the_exact_bytes_and_ref_it_had_before() {
    let derivation = bound_derivation();
    assert_eq!(
        serde_jcs::to_string(&derivation).expect("canonical bytes"),
        OLD_DERIVATION_JSON,
        "adding the optional field must not move a single byte of an existing D"
    );
    assert_eq!(
        derivation.derivation_ref().expect("derivation ref"),
        OLD_DERIVATION_REF
    );
    assert_eq!(
        serde_jcs::to_string(&derivation.state[0]).expect("canonical state bytes"),
        OLD_STATE_JSON
    );
    assert_eq!(
        derivation.state[0].mount_required, None,
        "omission is how a fixed-mount slot is declared"
    );
    assert!(derivation.state[0].requires_fixed_mount());
}

#[test]
fn a_path_delivered_slot_is_a_different_d_and_writes_the_declaration() {
    let derivation = bound_derivation();
    let mut path_delivered = derivation.clone();
    path_delivered.state[0].mount_required = Some(false);

    let bytes = serde_jcs::to_string(&path_delivered).expect("canonical bytes");
    assert!(
        bytes.contains(r#""mount_required":false"#),
        "the declaration is part of the canonical object"
    );
    assert_eq!(
        serde_jcs::to_string(&path_delivered.state[0]).expect("canonical state bytes"),
        r#"{"access":"read-write","id":"data","mount":"/data","mount_required":false,"protocol":"ato.state.filesystem@1"}"#
    );
    let path_ref = path_delivered.derivation_ref().expect("derivation ref");
    assert_ne!(
        path_ref, OLD_DERIVATION_REF,
        "a different declaration is a different addressed route"
    );
    assert!(!path_delivered.state[0].requires_fixed_mount());

    // `true` and absence would be the same claim in two byte forms.
    let mut redundant = derivation;
    redundant.state[0].mount_required = Some(true);
    let error = redundant.derivation_ref().unwrap_err();
    assert!(
        error.to_string().contains("state.mount_required"),
        "{error}"
    );
}

/// The shape an older runtime decodes. It mirrors the pre-change
/// `BoundDerivation` including `deny_unknown_fields` on the nested state
/// slot, which is what makes the new declaration fail closed there.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // a decode mirror: the fields exist to be refused or accepted
struct OldBoundDerivation {
    #[serde(default)]
    native_surface: Option<serde_json::Value>,
    #[serde(default)]
    runtime_port_operations: Vec<serde_json::Value>,
    #[serde(default)]
    variable_bindings: Vec<serde_json::Value>,
    schema: String,
    #[serde(default)]
    requirements: Option<serde_json::Value>,
    inputs: Vec<serde_json::Value>,
    #[serde(default)]
    runtimes: BTreeMap<String, String>,
    steps: Vec<serde_json::Value>,
    ports: Vec<serde_json::Value>,
    state: Vec<OldBoundState>,
    #[serde(default)]
    workspace_build: Option<String>,
    #[serde(default)]
    workspace_compiler: Option<String>,
    #[serde(default)]
    source_oci: Option<serde_json::Value>,
    #[serde(default)]
    platforms: Vec<serde_json::Value>,
    effects: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // a decode mirror: the fields exist to be refused or accepted
struct OldBoundState {
    id: String,
    protocol: String,
    mount: String,
    access: serde_json::Value,
}

#[test]
fn a_runtime_that_predates_the_field_refuses_a_path_delivered_d() {
    // The old bytes still decode: nothing about the omitting D changed.
    serde_json::from_str::<OldBoundDerivation>(OLD_DERIVATION_JSON)
        .expect("an old runtime still reads an old Derivation");

    let mut path_delivered = bound_derivation();
    path_delivered.state[0].mount_required = Some(false);
    let bytes = serde_jcs::to_string(&path_delivered).expect("canonical bytes");
    let error = serde_json::from_str::<OldBoundDerivation>(&bytes)
        .expect_err("an old runtime must not admit a declaration it cannot honour");
    assert!(
        error.to_string().contains("mount_required"),
        "the refusal names the field: {error}"
    );
}
