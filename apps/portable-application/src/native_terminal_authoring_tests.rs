//! The producer end: an `ato.capsule/1` route that declares a Terminal
//! `native_surface` compiled into the wire v3 bundle the hosted validator
//! imports. The validator report — not this crate's own opinion — is what says
//! the bundle is a native OCI route, so these tests read it.

use std::path::Path;

use super::*;

const IMAGE: &str = "docker.io/library/python@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BODY_DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const MANIFEST: &str = r#"
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

fn source(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(CAPSULE_FILE_NAME), manifest).expect("manifest");
    std::fs::write(
        dir.path().join("pty_app.py"),
        "import sys\nsys.stdin.readline()\n",
    )
    .expect("app");
    dir
}

fn build(dir: &Path) -> (Vec<u8>, PortableApplicationBundle) {
    build_native_terminal_bundle(dir, &dir.join(CAPSULE_FILE_NAME), "Native Terminal")
        .expect("builds a native bundle")
}

#[test]
fn a_declared_terminal_surface_becomes_a_native_oci_bundle_the_validator_names() {
    let dir = source(MANIFEST);
    let (bytes, bundle) = build(dir.path());

    // Transport equality: the bundle re-decodes and re-validates from its bytes.
    let (decoded, routes) = validate_bytes_all(&bytes).expect("validated bytes");
    assert_eq!(
        bundle_sha256(&bytes),
        bundle_sha256(&encode_portable_application_bundle(&decoded).unwrap()),
        "bundle bytes must be canonical and stable"
    );
    assert_eq!(decoded.index.derivations, bundle.index.derivations);
    assert_eq!(
        bundle.index.root_contract_ref,
        routes[0].contract_ref.to_string()
    );
    assert_eq!(routes[0].realization, PortableRealizationKind::OciContainer);
    let route = &routes[0];
    assert_eq!(
        route
            .derivation
            .native_surface
            .as_ref()
            .unwrap()
            .surface
            .kind(),
        "terminal"
    );
    assert_eq!(
        route
            .derivation
            .native_surface
            .as_ref()
            .unwrap()
            .surface
            .profile(),
        "ato.terminal-surface.v1"
    );
    assert!(
        route
            .derivation
            .ports
            .iter()
            .all(|port| port.guest_port.is_none())
    );
    assert_eq!(route.application.surfaces[0].port, "app.native");
    assert_eq!(route.application.surfaces[0].path, "/");
    assert_eq!(
        route.derivation.runtimes.get(OCI_IMAGE_RUNTIME),
        Some(&IMAGE.to_owned())
    );
    assert_eq!(
        route.derivation.runtimes.get(OCI_USER_RUNTIME),
        Some(&"1000:1000".to_owned())
    );
    assert_eq!(
        route.contract.requirements[0].body_digest.as_deref(),
        Some(BODY_DIGEST)
    );

    // The validator, not this test, decides the realization string the API
    // dispatches on, and names the exact Session Surface.
    let reports = validator_agent::tests_support::route_reports(&routes).expect("reports");
    assert_eq!(reports[0].realization, "native_oci_container");
    assert_eq!(reports[0].port, "app.native");
    assert!(reports[0].native_surface.is_some());
    assert_eq!(
        reports[0].native_ports.as_deref(),
        Some(route.derivation.ports.as_slice())
    );
    let session = reports[0]
        .session_surface
        .as_ref()
        .expect("native session surface");
    let ato_ipc::session_surface::SessionSurfaceDescriptor::Terminal {
        profile,
        surface_id,
        ..
    } = session
    else {
        panic!("expected a Terminal session surface, got {session:?}");
    };
    assert_eq!(profile, "ato.terminal-surface.v1");
    assert_eq!(surface_id, "main");

    // Authoring input never becomes artifact content.
    assert!(
        route
            .tree
            .entries
            .iter()
            .all(|entry| entry.path != CAPSULE_FILE_NAME)
    );
    assert!(
        route
            .tree
            .entries
            .iter()
            .any(|entry| entry.path == "pty_app.py")
    );
}

#[test]
fn a_route_without_a_native_surface_is_not_emitted_by_this_producer() {
    let text = MANIFEST.replace(
        r#"[derive.native_surface]
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
"#,
        "",
    );
    let dir = source(&text);
    assert!(
        build_native_terminal_bundle(
            dir.path(),
            &dir.path().join(CAPSULE_FILE_NAME),
            "No Surface"
        )
        .is_err()
    );
}

#[test]
fn geometry_outside_the_wire_bounds_never_reaches_a_bundle() {
    let text = MANIFEST.replace("cols = 120", "cols = 501");
    let dir = source(&text);
    assert!(
        build_native_terminal_bundle(dir.path(), &dir.path().join(CAPSULE_FILE_NAME), "Too Wide")
            .is_err()
    );
}
