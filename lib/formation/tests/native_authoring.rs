//! `ato.capsule/1` authoring of a Terminal `native_surface`, and the promise
//! that adding it did not move any Capsule that already existed.
//!
//! The wire type and its catalog already existed (`native_surface.rs`); what
//! was missing was a way for an author to declare one. These tests pin the
//! grammar, the validation both frontends share, and the one thing an optional
//! digested field must never change: the bytes of a Derivation that does not
//! declare it.

use ato_formation::authoring::{BindingContext, BoundContract, BoundDerivation, bind};
use ato_formation::capsule_toml::{parse_capsule_toml, render_capsule_toml};
use ato_formation::native_surface::{NativePresentation, NativeProbeOperation};
use sha2::{Digest, Sha256};

const SOURCE_CLOSURE: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A route that existed before `native_surface` did. Its `DerivationRef` is
/// pinned below at the exact value the pre-feature build produced.
const LEGACY_MANIFEST: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "python"
version = "3.12"

[[derive.step]]
id = "serve"
use = "ato.process@1"
op = "serve"
argv = ["python3", "server.py"]
cwd = "."

[[port]]
id = "app.http"
use = "ato.http@1"
from = "serve"
guest_port = 8000

[[contract.require]]
id = "index"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
expect = { status = 200 }
"#;

/// The digest `bind(LEGACY_MANIFEST, SOURCE_CLOSURE)` produced before this
/// feature existed. Recomputed from the canonical JCS bytes, never typed by
/// hand into the system under test.
const LEGACY_DERIVATION_REF: &str =
    "sha256:e47e0036aad1146981716ffc660e59f5c92913b4265856b9e524d0f1810753e2";
const LEGACY_CONTRACT_REF: &str =
    "sha256:446a8af31617f4c02b3866395750dcf30c3555117f45a4befda91eb4bbedb6af";

const NATIVE_MANIFEST: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "oci.image"
version = "docker.io/library/python@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
[[runtime]]
name = "oci.platform"
version = "linux/amd64"
[[runtime]]
name = "oci.memory_bytes"
version = "268435456"
[[runtime]]
name = "oci.cpu_limit_millis"
version = "1000"
[[runtime]]
name = "oci.pids_limit"
version = "128"
[[runtime]]
name = "oci.user"
version = "1000:1000"

[[derive.step]]
id = "serve"
use = "ato.oci@1"
op = "serve"
argv = ["python3", "pty_app.py"]
cwd = "."

[derive.native_surface]
kind = "terminal"
profile = "ato.terminal-surface.v1"
cols = 120
rows = 40
port = "app.native"
probe_port = "app.probe"
probe_path = "/result"
probe_input = "go\n"
probe_output_delimiter = "END"
probe_max_output_bytes = 4096
probe_timeout_ms = 5000

[[port]]
id = "app.native"
use = "ato.pty@1"
from = "serve"

[[port]]
id = "app.probe"
use = "ato.http@1"
from = "serve"

[[contract.require]]
id = "result"
use = "ato.contract.http@1"
port = "app.probe"
path = "/result"
expect = { status = 200, body_digest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
"#;

fn bound(text: &str) -> (BoundContract, BoundDerivation) {
    let draft = parse_capsule_toml(text).expect("parses");
    bind(
        &draft,
        &BindingContext {
            source_closure_ref: SOURCE_CLOSURE,
        },
    )
    .expect("binds")
}

/// Replace one `[derive.native_surface]` value in the native fixture.
fn native_with(from: &str, to: &str) -> String {
    assert!(NATIVE_MANIFEST.contains(from), "fixture has no {from:?}");
    NATIVE_MANIFEST.replacen(from, to, 1)
}

#[test]
fn a_derivation_without_native_surface_keeps_its_exact_pre_feature_identity() {
    let (contract, derivation) = bound(LEGACY_MANIFEST);
    let bytes = serde_jcs::to_vec(&derivation).expect("canonical bytes");
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&bytes)),
        LEGACY_DERIVATION_REF,
        "adding an optional field changed an existing route's canonical bytes"
    );
    assert_eq!(
        derivation.derivation_ref().expect("digest"),
        LEGACY_DERIVATION_REF
    );
    assert_eq!(
        contract.contract_ref().expect("digest"),
        LEGACY_CONTRACT_REF,
        "the Contract must not move either"
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains("native_surface"),
        "an omitted field must not reach the canonical bytes at all"
    );
}

#[test]
fn the_grammar_authors_one_bounded_terminal_native_surface() {
    let draft = parse_capsule_toml(NATIVE_MANIFEST).expect("parses");
    let native = draft
        .derivation
        .native_surface
        .as_ref()
        .expect("declared native surface");
    assert_eq!(
        native.surface,
        NativePresentation::Terminal {
            port: "app.native".to_owned(),
            cols: 120,
            rows: 40,
        }
    );
    assert_eq!(native.probe_port, "app.probe");
    match &native.operations[0] {
        NativeProbeOperation::TerminalExchange {
            path,
            input,
            output_delimiter,
            max_output_bytes,
            timeout_ms,
        } => {
            assert_eq!(path, "/result");
            assert_eq!(input, b"go\n");
            assert_eq!(output_delimiter, b"END");
            assert_eq!(max_output_bytes, &4096);
            assert_eq!(timeout_ms, &5000);
        }
        other => panic!("unexpected operation: {other:?}"),
    }

    let (_, derivation) = bound(NATIVE_MANIFEST);
    assert_eq!(derivation.native_surface.as_ref(), Some(native));
    assert_eq!(derivation.steps.len(), 1);
    assert_eq!(derivation.steps[0].protocol, "ato.oci@1");
    assert!(
        derivation
            .ports
            .iter()
            .all(|port| port.guest_port.is_none())
    );
    assert_ne!(
        derivation.derivation_ref().expect("digest"),
        LEGACY_DERIVATION_REF,
        "a declared native Surface is a different route"
    );
}

#[test]
fn the_grammar_renders_a_native_surface_losslessly() {
    let draft = parse_capsule_toml(NATIVE_MANIFEST).expect("parses");
    let rendered = render_capsule_toml(&draft).expect("renders");
    let restored = parse_capsule_toml(&rendered).expect("reparses");
    assert_eq!(restored.derivation, draft.derivation);
}

#[test]
fn a_kind_or_profile_outside_the_terminal_catalog_is_refused() {
    for text in [
        native_with(r#"kind = "terminal""#, r#"kind = "pixel_stream""#),
        native_with(
            r#"profile = "ato.terminal-surface.v1""#,
            r#"profile = "ato.pixel-stream.v1""#,
        ),
    ] {
        assert!(
            parse_capsule_toml(&text).is_err(),
            "accepted an unsupported kind/profile"
        );
    }
}

#[test]
fn terminal_geometry_outside_the_wire_bounds_is_refused() {
    for (field, value) in [
        ("cols = 120", "cols = 1"),
        ("cols = 120", "cols = 501"),
        ("rows = 40", "rows = 1"),
        ("rows = 40", "rows = 201"),
    ] {
        let text = native_with(field, value);
        let error = parse_capsule_toml(&text).expect_err("must refuse");
        assert_eq!(error.code(), "authoring_malformed", "{value}: {error}");
    }
    // 120x40, the acceptance target, stays inside the bounds.
    parse_capsule_toml(NATIVE_MANIFEST).expect("accepts 120x40");
}

#[test]
fn a_native_surface_on_a_non_runtime_launch_route_is_refused_at_binding() {
    // A process route: the adapter exists, but no Runner realizes a Terminal
    // through a process launch, so the draft must not bind.
    let process_step = native_with(r#"use = "ato.oci@1""#, r#"use = "ato.process@1""#);
    let draft = parse_capsule_toml(&process_step).expect("grammar alone stays valid");
    let error = bind(
        &draft,
        &BindingContext {
            source_closure_ref: SOURCE_CLOSURE,
        },
    )
    .expect_err("a process route cannot carry a native Surface");
    assert_eq!(error.code(), "authoring_malformed");

    // A static-web route has no native realization at all.
    let browser_step = native_with(
        r#"use = "ato.oci@1"
op = "serve"
argv = ["python3", "pty_app.py"]
cwd = ".""#,
        r#"use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html""#,
    );
    let draft = parse_capsule_toml(&browser_step).expect("grammar alone stays valid");
    assert!(
        bind(
            &draft,
            &BindingContext {
                source_closure_ref: SOURCE_CLOSURE,
            },
        )
        .is_err()
    );
}

#[test]
fn a_terminal_port_without_a_native_surface_is_refused() {
    // Drop the declaration but keep the `ato.pty@1` Port it named.
    let start = NATIVE_MANIFEST
        .find("[derive.native_surface]")
        .expect("fixture table");
    let end = NATIVE_MANIFEST.find("[[port]]").expect("fixture ports");
    let text = format!("{}{}", &NATIVE_MANIFEST[..start], &NATIVE_MANIFEST[end..]);
    assert!(parse_capsule_toml(&text).is_err());
}

#[test]
fn unknown_keys_inside_the_native_table_are_refused() {
    let text = native_with(
        r#"probe_timeout_ms = 5000"#,
        "probe_timeout_ms = 5000\nexpected_output = \"END\"",
    );
    assert!(parse_capsule_toml(&text).is_err());
}
