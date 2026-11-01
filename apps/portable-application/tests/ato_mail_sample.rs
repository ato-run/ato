//! The D1 mail sample (S3) as a build-time guarantee.
//!
//! `samples/ato-mail` is the staging Derivation from the self-hosted mail plan.
//! The two images it pins are preloaded on the dedicated Runner and are not
//! pulled here, so this test never starts a container: it builds the portable
//! bundle OFFLINE and checks that the authored sample still *says* what the
//! plan and the staging runbook assume — which Ports carry a certificate
//! requirement and how TLS starts on each, which Ports carry PROXY protocol v2,
//! the state slot, the Bindings, and which component consumes each one.
//!
//! It exists because those facts are the whole of D1. A mail sample whose
//! certificate observation silently moved to another Port, lost its start mode
//! (`implicit` vs `smtp-starttls`), or dropped PROXY v2 would still build and
//! still validate, and the failure would only show up as an unobserved
//! requirement during staging.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ato_formation::authoring::{
    BoundDerivation, BoundRequirement, ClientAddressTransport, HTTP_CONTRACT_VERIFIER,
    HTTP_PROTOCOL, STATE_FILESYSTEM_PROTOCOL, StateAccess, TCP_EGRESS_PROTOCOL,
    TLS_CONTRACT_VERIFIER, TlsStart,
};
use ato_portable_application::{
    ValidatedPortableApplication, build_authored_bundle_v2, validate_all_derivations,
};

const STALWART_IMAGE: &str = "ghcr.io/koh0920/stalwart-staging@sha256:d0a846d7ab2749baa5bcd15a56b51f90ba99aea0c3256ce04d4f7e5e3076e8e0";
const ADAPTER_IMAGE: &str = "ghcr.io/koh0920/ato-fixed-socks5-tls-adapter@sha256:868ae56c18a004e7e4cc0e8d0707364b9d28242b914ec25f18f3fe1ca3a6aabf";
/// The one host name these samples declare for staging. Changing it changes the
/// certificate the staging Runner must present on every mail Port.
const STAGING_SERVER_NAME: &str = "mx-test.atousercontent.com";

fn mail_sample_route() -> ValidatedPortableApplication {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/ato-mail");
    let (_, bundle) = build_authored_bundle_v2(&source).expect("the ato-mail sample builds");
    let mut routes = validate_all_derivations(&bundle).expect("every derivation validates");
    assert_eq!(routes.len(), 1, "D1 is one service group");
    routes.remove(0)
}

fn requirement<'a>(contract: &'a [BoundRequirement], id: &str) -> &'a BoundRequirement {
    contract
        .iter()
        .find(|requirement| requirement.id == id)
        .unwrap_or_else(|| panic!("K has no requirement {id:?}"))
}

fn step_bindings<'a>(derivation: &'a BoundDerivation, step_id: &str) -> &'a [String] {
    &derivation
        .steps
        .iter()
        .find(|step| step.id == step_id)
        .unwrap_or_else(|| panic!("no step {step_id:?}"))
        .bindings
}

#[test]
fn k5_asks_for_a_certificate_on_every_mail_port_in_the_right_start_mode() {
    let route = mail_sample_route();
    // Port -> how TLS starts there. 25 and 587 are plaintext-then-STARTTLS;
    // 465 and 993 are implicit. Anything else is a staging failure waiting to
    // happen, because the certificate is presented at different points.
    let expected = BTreeMap::from([
        ("stalwart.smtp", TlsStart::SmtpStarttls),
        ("stalwart.submissions", TlsStart::Implicit),
        ("stalwart.submissions-starttls", TlsStart::SmtpStarttls),
        ("stalwart.imaps", TlsStart::Implicit),
    ]);
    let mut observed_ports = BTreeSet::new();
    for (port, start) in &expected {
        let requirement = route
            .contract
            .requirements
            .iter()
            .find(|requirement| {
                requirement.verifier == TLS_CONTRACT_VERIFIER
                    && requirement.port.as_deref() == Some(*port)
            })
            .unwrap_or_else(|| panic!("no TLS requirement on Port {port:?}"));
        assert_eq!(
            requirement.server_name.as_deref(),
            Some(STAGING_SERVER_NAME)
        );
        assert_eq!(requirement.start, Some(*start), "start mode on {port}");
        observed_ports.insert(*port);
    }
    let tls_requirements = route
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == TLS_CONTRACT_VERIFIER)
        .count();
    assert_eq!(
        tls_requirements,
        expected.len(),
        "exactly the four mail Ports carry a certificate requirement"
    );
    assert_eq!(observed_ports.len(), 4);

    // 443 is the external Caddy Binding from ADR-043, which is not implemented:
    // the sample must not pretend to declare it.
    for requirement in &route.contract.requirements {
        if requirement.verifier == TLS_CONTRACT_VERIFIER {
            let port = requirement.port.as_deref().unwrap_or_default();
            assert_ne!(
                route
                    .derivation
                    .ports
                    .iter()
                    .find(|bound| bound.id == port)
                    .and_then(|bound| bound.guest_port),
                Some(443),
                "a 443 certificate requirement belongs to ADR-043, not this sample"
            );
        }
    }
}

#[test]
fn k3_observes_definitions_the_pinned_http_listener_answers_without_a_session() {
    let route = mail_sample_route();
    let expected = BTreeMap::from([
        // Proves the HTTP listener and its routing table are alive. 200 on the
        // pinned build before any authentication or storage access, subject
        // only to Stalwart's anonymous-request rate limiter.
        ("http-live", ("/healthz/live", 200u16)),
        // 200 only when a data storage backend is configured (503 otherwise),
        // so it also covers "the state mount is wired to a store".
        ("http-ready", ("/healthz/ready", 200)),
        // JMAP session discovery: 307 to /jmap/session, returned on the
        // pinned build before any authentication is attempted.
        ("http-jmap-discovery", ("/.well-known/jmap", 307)),
    ]);
    for (id, (path, status)) in &expected {
        let requirement = requirement(&route.contract.requirements, id);
        assert_eq!(requirement.verifier, HTTP_CONTRACT_VERIFIER);
        assert_eq!(requirement.port.as_deref(), Some("app.http"));
        assert_eq!(requirement.method.as_deref(), Some("GET"));
        assert_eq!(requirement.path.as_deref(), Some(*path));
        assert_eq!(requirement.status, Some(*status));
        // No body digest is pinned: the /healthz bodies are serde-generated
        // JSON problem documents, and pinning their bytes without observing the
        // pinned image once would make K fragile for no gain.
        assert_eq!(requirement.body_digest, None);
    }
    // A 401 is not a functional claim about mail, so the sample does not
    // observe an authenticated route as "the endpoint answers".
    assert!(
        route
            .contract
            .requirements
            .iter()
            .all(|requirement| requirement.status != Some(401)),
        "the sample must not call an unauthenticated 401 a satisfied K3"
    );
}

#[test]
fn public_mail_ports_carry_proxy_protocol_v2_and_the_surface_does_not() {
    let route = mail_sample_route();
    let transports = route
        .derivation
        .ports
        .iter()
        .map(|port| (port.id.as_str(), port.client_address_transport))
        .collect::<BTreeMap<_, _>>();
    for port in [
        "stalwart.smtp",
        "stalwart.submissions",
        "stalwart.submissions-starttls",
        "stalwart.imaps",
    ] {
        assert_eq!(
            transports.get(port),
            Some(&Some(ClientAddressTransport::ProxyProtocolV2)),
            "public fixed-TCP Port {port:?} must carry the client address (G4)"
        );
    }
    // The Surface Port is observed by the runtime, not published to remote
    // MTAs/IMAP clients; the internal relay hop is inside the group. Neither
    // carries a PROXY header.
    assert_eq!(transports.get("app.http"), Some(&None));
    assert_eq!(transports.get("relay.smtp"), Some(&None));
}

#[test]
fn the_group_serves_the_standard_mail_ports_and_the_jmap_surface() {
    let route = mail_sample_route();
    let guest_ports = route
        .derivation
        .ports
        .iter()
        .map(|port| (port.id.as_str(), port))
        .collect::<BTreeMap<_, _>>();
    for (port, guest_port) in [
        ("stalwart.smtp", 25u16),
        ("stalwart.submissions", 465),
        ("stalwart.submissions-starttls", 587),
        ("stalwart.imaps", 993),
        ("app.http", 8080),
        ("relay.smtp", 2525),
    ] {
        let bound = guest_ports
            .get(port)
            .unwrap_or_else(|| panic!("Port {port:?} is not declared"));
        assert_eq!(bound.guest_port, Some(guest_port), "guest port of {port}");
    }
    // Only the Surface Port is HTTP; every mail Port is plain TCP.
    for (id, protocol) in [
        ("app.http", HTTP_PROTOCOL),
        ("stalwart.smtp", "ato.tcp@1"),
        ("stalwart.submissions", "ato.tcp@1"),
        ("stalwart.submissions-starttls", "ato.tcp@1"),
        ("stalwart.imaps", "ato.tcp@1"),
        ("relay.smtp", "ato.tcp@1"),
    ] {
        assert_eq!(guest_ports[id].protocol, protocol, "protocol of {id}");
    }
    assert_eq!(route.application.surfaces[0].port, "app.http");
    assert_eq!(route.application.surfaces[0].path, "/healthz/live");

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
        .collect::<BTreeMap<_, _>>();
    assert_eq!(images.get("stalwart"), Some(&Some(STALWART_IMAGE)));
    assert_eq!(images.get("relay"), Some(&Some(ADAPTER_IMAGE)));
}

#[test]
fn the_mail_store_is_one_state_slot_on_the_stalwart_step() {
    let route = mail_sample_route();
    let [state] = route.derivation.state.as_slice() else {
        panic!("D1 has exactly one state slot");
    };
    assert_eq!(state.id, "mail_data");
    assert_eq!(state.protocol, STATE_FILESYSTEM_PROTOCOL);
    assert_eq!(state.mount, "/var/lib/stalwart");
    assert_eq!(state.access, StateAccess::ReadWrite);

    let stalwart = route
        .derivation
        .steps
        .iter()
        .find(|step| step.id == "stalwart")
        .expect("the stalwart step");
    assert_eq!(stalwart.state, vec!["mail_data".to_owned()]);
    // The transport Adapter owns no mail store.
    let relay = route
        .derivation
        .steps
        .iter()
        .find(|step| step.id == "relay")
        .expect("the relay step");
    assert!(relay.state.is_empty());
}

#[test]
fn every_binding_is_declared_and_visible_to_exactly_the_step_that_needs_it() {
    let route = mail_sample_route();
    let declared = route
        .application
        .bindings
        .iter()
        .map(|binding| (binding.id.as_str(), binding))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(declared.len(), 4);
    for (id, protocol) in [
        ("relay_auth", "ato.secret@1"),
        ("mail_cert_pem", "ato.secret@1"),
        ("mail_key_pem", "ato.secret@1"),
        ("smtp_egress", TCP_EGRESS_PROTOCOL),
    ] {
        let binding = declared
            .get(id)
            .unwrap_or_else(|| panic!("Binding {id:?} is not declared"));
        assert_eq!(binding.protocol, protocol, "protocol of Binding {id}");
        assert!(binding.required, "Binding {id} is required");
    }
    // Stalwart reads the certificate pair and the relay secret through
    // EnvironmentVariable references; the Adapter consumes the egress grant.
    assert_eq!(
        step_bindings(&route.derivation, "stalwart"),
        [
            "relay_auth".to_owned(),
            "mail_cert_pem".to_owned(),
            "mail_key_pem".to_owned()
        ]
    );
    assert_eq!(
        step_bindings(&route.derivation, "relay"),
        ["smtp_egress".to_owned()]
    );
    // No authored environment value may shadow a Binding's environment name.
    for step in &route.derivation.steps {
        for binding in step
            .bindings
            .iter()
            .filter_map(|id| declared.get(id.as_str()))
        {
            let name = ato_portable_application::application_binding_environment_name(binding);
            assert!(
                !step.env.contains_key(&name),
                "step {} authors {name}, which the binding path owns",
                step.id
            );
        }
    }
}
