//! `ato.capsule/2`: a native route may declare a Terminal surface, and its K
//! observes the declared exchanges.
//!
//! The surface reuses the existing `terminal_exchange` vocabulary and its
//! bounds; there is no `probe_port`, because the Adapter owns the PTY and the
//! route opens no listening socket. Every check here is fail-closed.

use ato_formation::capsule_toml_v2::{CapsuleTomlV2Error, parse_capsule_toml_v2};
use ato_formation::native_surface::NativeProbeOperation;

fn manifest(surface: &str, observation: &str, extra_route: &str) -> String {
    format!(
        r#"
schema = "ato.capsule/2"

[application]
title = "terminal"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derivation]]
id = "native"
use = "ato.native@1"
runtimes = {{}}
os = "macos"
arch = "aarch64"
entry = "run.sh"
artifact = "artifact.tar.gz"
{extra_route}
[derivation.native_surface]
{surface}

{observation}

[effects]
default = "pure"
"#
    )
}

const SURFACE: &str = r#"kind = "terminal"
port = "app.terminal"
cols = 100
rows = 30

[[derivation.native_surface.operations]]
kind = "terminal_exchange"
path = "/result"
input = [112, 105, 110, 103, 10]
output_delimiter = [69, 78, 68, 10]
max_output_bytes = 4096
timeout_ms = 5000
ready_output = [114, 101, 97, 100, 121, 62, 32]"#;

const OBSERVATION: &str = r#"[[contract.observation]]
id = "exchange"
use = "ato.contract.terminal@1"
port = "app.terminal"
path = "/result""#;

fn error_of(text: &str) -> CapsuleTomlV2Error {
    parse_capsule_toml_v2(text).expect_err("must be refused")
}

fn refusal_path(error: &CapsuleTomlV2Error) -> &str {
    match error {
        CapsuleTomlV2Error::Invalid { path, .. } => path,
        CapsuleTomlV2Error::Syntax(_) => "syntax",
    }
}

#[test]
fn a_terminal_surface_parses_with_the_existing_operation_vocabulary() {
    let draft = parse_capsule_toml_v2(&manifest(SURFACE, OBSERVATION, ""))
        .expect("a terminal native route parses");
    let native = draft.derivations[0].native.as_ref().expect("native route");
    let surface = native
        .native_surface
        .as_ref()
        .expect("declared terminal surface");
    assert_eq!(surface.surface.kind(), "terminal");
    assert_eq!(surface.surface.port(), "app.terminal");
    assert_eq!(surface.terminal_geometry(), Some((100, 30)));
    // No HTTP probe: nothing in this route serves one.
    assert!(surface.probe_port.is_none());
    assert_eq!(surface.operations.len(), 1);
    assert!(matches!(
        &surface.operations[0],
        NativeProbeOperation::TerminalExchange { path, max_output_bytes, .. }
            if path == "/result" && *max_output_bytes == 4096
    ));
    // A terminal route promises no guest TCP port.
    assert_eq!(draft.derivations[0].guest_port, 0);
    assert_eq!(draft.terminal_observations.len(), 1);
    assert_eq!(draft.terminal_observations[0].operation, "/result");
    assert!(draft.terminal_observations[0].body_digest.is_none());
}

#[test]
fn an_unknown_surface_kind_is_refused() {
    let surface = SURFACE.replace("kind = \"terminal\"", "kind = \"pixel\"");
    let error = error_of(&manifest(&surface, OBSERVATION, ""));
    assert_eq!(refusal_path(&error), "derivation.native_surface.kind");
}

#[test]
fn a_pixel_operation_is_refused_by_the_shared_catalog_bounds() {
    let surface = SURFACE.replace("kind = \"terminal_exchange\"", "kind = \"pixel_key_frame\"");
    // The operation vocabulary is the shared one, so a pixel operation does
    // not deserialize as a terminal exchange at all.
    let error = error_of(&manifest(&surface, OBSERVATION, ""));
    assert!(matches!(error, CapsuleTomlV2Error::Syntax(_)));
}

#[test]
fn an_operation_past_the_shared_bounds_is_refused() {
    for (from, to, path) in [
        (
            "max_output_bytes = 4096",
            "max_output_bytes = 0",
            "derivation.native_surface",
        ),
        (
            "timeout_ms = 5000",
            "timeout_ms = 30001",
            "derivation.native_surface",
        ),
        (
            "output_delimiter = [69, 78, 68, 10]",
            "output_delimiter = []",
            "derivation.native_surface",
        ),
    ] {
        let surface = SURFACE.replace(from, to);
        let error = error_of(&manifest(&surface, OBSERVATION, ""));
        assert_eq!(refusal_path(&error), path, "{from}");
    }
}

#[test]
fn a_probe_port_is_not_part_of_a_v2_terminal_surface() {
    let surface = SURFACE.replace(
        "port = \"app.terminal\"",
        "port = \"app.terminal\"\nprobe_port = \"app.probe\"",
    );
    let error = error_of(&manifest(&surface, OBSERVATION, ""));
    assert!(matches!(error, CapsuleTomlV2Error::Syntax(_)));
}

#[test]
fn a_terminal_route_must_not_declare_a_guest_port() {
    let error = error_of(&manifest(SURFACE, OBSERVATION, "guest_port = 18080"));
    assert_eq!(refusal_path(&error), "derivation.guest_port");
}

#[test]
fn a_terminal_route_refuses_an_http_observation() {
    let observation = r#"[[contract.observation]]
id = "http"
use = "ato.contract.http@1"
port = "app.terminal"
path = "/"
status = 200"#;
    let error = error_of(&manifest(SURFACE, observation, ""));
    assert_eq!(refusal_path(&error), "contract.observation");
}

#[test]
fn every_declared_operation_must_be_observed() {
    let observation = r#"[[contract.observation]]
id = "other"
use = "ato.contract.terminal@1"
port = "app.terminal"
path = "/elsewhere""#;
    let error = error_of(&manifest(SURFACE, observation, ""));
    assert_eq!(refusal_path(&error), "contract.observation.path");

    // An operation K does not name is refused too: an exchange nothing
    // observes would let a Run claim a fully satisfied receipt for an input it
    // never typed.
    let two = [
        SURFACE,
        "",
        "[[derivation.native_surface.operations]]",
        "kind = \"terminal_exchange\"",
        "path = \"/second\"",
        "input = [112, 10]",
        "output_delimiter = [68, 79, 78, 69, 10]",
        "max_output_bytes = 4096",
        "timeout_ms = 5000",
    ]
    .join("\n");
    let error = error_of(&manifest(&two, OBSERVATION, ""));
    assert_eq!(refusal_path(&error), "contract.observation");
}

#[test]
fn a_pinned_output_digest_is_carried_into_k() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let observation = format!("{OBSERVATION}\nbody_digest = \"{digest}\"");
    let draft = parse_capsule_toml_v2(&manifest(SURFACE, &observation, ""))
        .expect("a pinned digest parses");
    assert_eq!(
        draft.terminal_observations[0].body_digest.as_deref(),
        Some(digest.as_str())
    );
    let bad = format!("{OBSERVATION}\nbody_digest = \"sha256:not-a-digest\"");
    let error = error_of(&manifest(SURFACE, &bad, ""));
    assert_eq!(refusal_path(&error), "contract.observation.body_digest");
}

#[test]
fn an_http_native_route_still_requires_its_guest_port() {
    // The existing HTTP native profile is unchanged: no terminal surface, and
    // the loopback port its app serves is mandatory.
    let text = manifest("", OBSERVATION, "");
    let text = text.replace(
        "[derivation.native_surface]\n\n\n[[contract.observation]]",
        "[[contract.observation]]",
    );
    let error = error_of(&text);
    assert_eq!(refusal_path(&error), "derivation.guest_port");
}
