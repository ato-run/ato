//! A runtime pin belongs to the Derivation, not to the Contract.
//!
//! This is the premise the execution-time runtime policy (CVE-2026-111497)
//! rests on: the authoring catalog may change, and execution may refuse a
//! pinned runtime it considers affected, without rewriting any Capsule's K.
//! Two drafts identical but for `[[runtime]] version` must therefore yield
//! different `DerivationRef`s and the same `ContractRef`.
//!
//! Binding is the pure semantic step, so it does not consult the authoring
//! catalog: the affected form here stands in for a Derivation that was sealed
//! before the catalog changed, which is exactly the case execution refuses.

use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
};

/// The same route and the same Contract, with only the Node pin changed.
fn node_route(node_version: &str) -> String {
    format!(
        r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "node"
version = "{node_version}"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["node", "server.js"]

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"

[contract.require.expect]
status = 200

[[contract.require]]
id = "source"
use = "ato.contract.workspace@1"
input = "workspace"

[contract.require.expect]
digest = "capture"
"#
    )
}

#[test]
fn a_runtime_only_change_moves_the_derivation_and_keeps_the_contract() {
    let context = BindingContext {
        source_closure_ref: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    };
    let affected_draft = parse_capsule_toml(&node_route("22.14.0")).expect("affected route parses");
    let patched_draft = parse_capsule_toml(&node_route("22.23.4")).expect("patched route parses");
    let (affected_k, affected_d) = bind(&affected_draft, &context).expect("affected route binds");
    let (patched_k, patched_d) = bind(&patched_draft, &context).expect("patched route binds");

    // The pin is part of D: a different Node is a different route.
    assert_eq!(affected_d.runtimes["node"], "22.14.0");
    assert_eq!(patched_d.runtimes["node"], "22.23.4");
    assert_ne!(
        affected_d.derivation_ref().unwrap(),
        patched_d.derivation_ref().unwrap()
    );

    // K is the observations, not the runtime: it does not move.
    assert_eq!(
        affected_k.contract_ref().unwrap(),
        patched_k.contract_ref().unwrap()
    );
    // And the runtime pin is not smuggled into K's canonical bytes.
    assert!(
        !serde_json::to_string(&affected_k)
            .unwrap()
            .contains("22.14.0")
    );
}
