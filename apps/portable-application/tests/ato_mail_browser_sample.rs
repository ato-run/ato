use std::collections::BTreeSet;
use std::path::Path;

use ato_formation::authoring::{HTTP_CONTRACT_VERIFIER, TLS_CONTRACT_VERIFIER};
use ato_portable_application::ValidatedPortableApplication;
use ato_portable_application::build_authored_bundle_v2;

const HTTP_PROTOCOL: &str = "ato.http@1";
const STATE_FILESYSTEM_PROTOCOL: &str = "ato.state.filesystem@1";
const STALWART_IMAGE: &str = "ghcr.io/koh0920/stalwart-staging@sha256:25723c49ee4968d410f78b35041eb1c79f28fe0cc2afe2487a5c58f99f90629b";
const MISSIVE_IMAGE: &str = "ghcr.io/koh0920/govcraft-missive@sha256:3dc89054b1a85ae6ad07e5cf1dc483bffa0ba7253b40fd8274803c765b04bc7c";

fn browser_sample_route() -> ValidatedPortableApplication {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/ato-mail-browser");
    let (_, bundle) =
        build_authored_bundle_v2(&source).expect("the ato-mail-browser sample builds");
    ato_portable_application::validate_bundle(&bundle).expect("the sample validates")
}

#[test]
fn the_browser_slice_declares_only_the_browser_surface() {
    let route = browser_sample_route();
    assert_eq!(route.contract.requirements.len(), 2);
    let verifiers = route
        .contract
        .requirements
        .iter()
        .map(|requirement| requirement.verifier.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(verifiers, BTreeSet::from([HTTP_CONTRACT_VERIFIER]));
    assert!(
        route
            .contract
            .requirements
            .iter()
            .all(|requirement| requirement.verifier != TLS_CONTRACT_VERIFIER),
        "the browser slice must not claim the mail TLS contract"
    );
    for (id, path, status) in [
        ("webmail-health", "/health", 200u16),
        ("webmail-ui", "/login", 200),
    ] {
        let requirement = route
            .contract
            .requirements
            .iter()
            .find(|requirement| requirement.id == id)
            .unwrap_or_else(|| panic!("K has no requirement {id:?}"));
        assert_eq!(requirement.port.as_deref(), Some("app.http"));
        assert_eq!(requirement.path.as_deref(), Some(path));
        assert_eq!(requirement.status, Some(status));
    }
    assert_eq!(route.application.surfaces[0].port, "app.http");
    assert_eq!(route.application.surfaces[0].path, "/health");
}

#[test]
fn the_browser_slice_has_no_mail_ports_or_bindings() {
    let route = browser_sample_route();
    assert!(
        route
            .derivation
            .steps
            .iter()
            .all(|step| step.bindings.is_empty()),
        "the browser slice needs no secret or egress grant"
    );
    let ports = route
        .derivation
        .ports
        .iter()
        .map(|port| (port.id.as_str(), port))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (port, guest_port) in [("stalwart.jmap", 8080u16), ("app.http", 8080)] {
        let bound = ports
            .get(port)
            .unwrap_or_else(|| panic!("Port {port:?} is not declared"));
        assert_eq!(bound.guest_port, Some(guest_port), "guest port of {port}");
        assert_eq!(
            bound.client_address_transport, None,
            "{port} is internal/local"
        );
    }
    // Only the Surface is HTTP; the JMAP listener is internal TCP.
    assert_eq!(ports["app.http"].protocol, HTTP_PROTOCOL);
    assert_eq!(ports["stalwart.jmap"].protocol, "ato.tcp@1");
    assert_eq!(route.derivation.ports.len(), 2);

    let images = route
        .derivation
        .steps
        .iter()
        .map(|step| {
            (
                step.id.as_str(),
                step.runtimes.get("oci.image").map(String::as_str),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(images.get("stalwart"), Some(&Some(STALWART_IMAGE)));
    assert_eq!(images.get("missive"), Some(&Some(MISSIVE_IMAGE)));

    let [state] = route.derivation.state.as_slice() else {
        panic!("the browser slice has exactly one state slot");
    };
    assert_eq!(state.id, "mail_data");
    assert_eq!(state.protocol, STATE_FILESYSTEM_PROTOCOL);
    assert_eq!(state.mount, "/var/lib/stalwart");
}
