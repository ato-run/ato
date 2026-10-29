use super::*;
use ato_formation::native_surface::{
    NativePresentation, NativeProbeOperation, NativeSurfaceAdapter,
};
use ato_formation::verify::{RuntimeHttpObservation, VerificationExecutionEvidence};
use ato_runtime_attempt::admission::{EffectAuthorization, admit};
use ato_runtime_attempt::verification::{
    ReceiptContext, required_http_observations, verify_observed_candidate,
};

const ANSWER: &[u8] = b"4\r\nEND";

fn replace_object<T: Serialize>(
    bundle: &mut PortableApplicationBundle,
    old: &str,
    value: &T,
) -> String {
    let bytes = serde_jcs::to_vec(value).unwrap();
    let reference = bundle_sha256(&bytes);
    let descriptor = bundle
        .index
        .objects
        .iter_mut()
        .find(|object| object.reference == old)
        .unwrap();
    descriptor.reference = reference.clone();
    descriptor.size = bytes.len() as u64;
    let payload = bundle
        .payloads
        .iter_mut()
        .find(|payload| payload.reference == old)
        .unwrap();
    payload.reference = reference.clone();
    payload.bytes = base64::engine::general_purpose::STANDARD.encode(bytes);
    bundle
        .index
        .objects
        .sort_by(|a, b| a.reference.cmp(&b.reference));
    bundle
        .payloads
        .sort_by(|a, b| a.reference.cmp(&b.reference));
    reference
}

/// A real canonical bundle, not a prevalidated route fixture. No native
/// executable is run; format admission and runtime refusal are separate tests.
pub(crate) fn fixture(pixel: bool) -> PortableApplicationBundle {
    let (_, mut bundle) = build_dynamic_process_oci_bundle(
        &tests::datasette_fixture_root(),
        &tests::datasette_spec(),
    )
    .unwrap();
    let old_routes = validate_all_derivations(&bundle).unwrap();
    let route = old_routes
        .iter()
        .find(|route| route.realization == PortableRealizationKind::OciContainer)
        .unwrap();
    let mut contract = route.contract.clone();
    contract
        .requirements
        .retain(|requirement| requirement.verifier != HTTP_CONTRACT_VERIFIER);
    contract.requirements.push(BoundRequirement {
        id: "native-result".to_owned(),
        verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
        port: Some("app.probe".to_owned()),
        method: Some("GET".to_owned()),
        path: Some("/result/calculate".to_owned()),
        status: Some(200),
        body_digest: Some(bundle_sha256(&if pixel {
            ato_formation::native_surface::pixel_probe_result(&vec![0; 640 * 480 * 4]).unwrap()
        } else {
            ANSWER.to_vec()
        })),
        input: None,
        digest: None,
    });
    contract.requirements.sort_by(|a, b| a.id.cmp(&b.id));
    let mut derivation = route.derivation.clone();
    derivation
        .runtimes
        .insert(OCI_USER_RUNTIME.to_owned(), "1000:1000".to_owned());
    let surface = if pixel {
        NativePresentation::PixelStream {
            port: "app.native".to_owned(),
            width: 640,
            height: 480,
        }
    } else {
        NativePresentation::Terminal {
            port: "app.native".to_owned(),
            cols: 80,
            rows: 24,
        }
    };
    let operation = if pixel {
        NativeProbeOperation::PixelKeyFrame {
            path: "/result/calculate".to_owned(),
            keysyms: vec![97],
            max_frame_bytes: 640 * 480 * 4,
            timeout_ms: 1000,
            settle_ms: 0,
        }
    } else {
        NativeProbeOperation::TerminalExchange {
            path: "/result/calculate".to_owned(),
            input: b"2+2\n".to_vec(),
            output_delimiter: b"END".to_vec(),
            max_output_bytes: 4096,
            timeout_ms: 1000,
            ready_output: Vec::new(),
        }
    };
    derivation.ports = vec![
        BoundPort {
            id: "app.native".to_owned(),
            protocol: surface.protocol().to_owned(),
            from: "serve".to_owned(),
            guest_port: Some(1026),
            client_address_transport: None,
        },
        BoundPort {
            id: "app.probe".to_owned(),
            protocol: HTTP_PROTOCOL.to_owned(),
            from: "serve".to_owned(),
            guest_port: Some(8081),
            client_address_transport: None,
        },
    ];
    derivation.native_surface = Some(NativeSurfaceAdapter {
        surface,
        probe_port: "app.probe".to_owned(),
        operations: vec![operation],
    });
    let application = ApplicationV2 {
        schema: APPLICATION_V2_SCHEMA.to_owned(),
        title: route.application.title.clone(),
        bindings: vec![],
        surfaces: vec![ApplicationSurfaceV2 {
            id: "primary".to_owned(),
            port: "app.native".to_owned(),
            path: "/".to_owned(),
        }],
    };
    for old in old_routes
        .iter()
        .filter(|old| old.derivation_ref != route.derivation_ref)
    {
        bundle
            .index
            .derivations
            .retain(|reference| reference != old.derivation_ref.as_str());
        bundle
            .index
            .objects
            .retain(|object| object.reference != old.derivation_ref.as_str());
        bundle
            .payloads
            .retain(|payload| payload.reference != old.derivation_ref.as_str());
    }
    bundle.index.root_contract_ref =
        replace_object(&mut bundle, route.contract_ref.as_str(), &contract);
    bundle.index.application_ref =
        replace_object(&mut bundle, route.application_ref.as_str(), &application);
    bundle.index.derivations = vec![replace_object(
        &mut bundle,
        route.derivation_ref.as_str(),
        &derivation,
    )];
    bundle
}

#[test]
fn native_bundle_admits_separate_presentation_and_observation_ports_but_refuses_execution() {
    for pixel in [false, true] {
        let bundle = fixture(pixel);
        let bytes = encode_portable_application_bundle(&bundle).unwrap();
        let (_, routes) = validate_bytes_all(&bytes).unwrap();
        let route = &routes[0];
        assert_eq!(route.realization, PortableRealizationKind::OciContainer);
        assert_eq!(route.application.surfaces[0].port, "app.native");
        let spec = route.attempt_spec(None);
        let required = required_http_observations(&spec);
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].port_id, "app.probe");
        assert_eq!(
            admit(
                &spec,
                EffectAuthorization::UserInvoked {
                    derivation_ref: route.derivation_ref.as_str()
                },
                None
            )
            .unwrap()
            .code,
            "native_surface_unavailable"
        );
        let reports = validator_agent::tests_support::route_reports(&routes).unwrap();
        assert_eq!(reports[0].realization, "native_oci_container");
        assert_eq!(reports[0].port, "app.native");
        assert!(reports[0].native_surface.is_some());
        assert_eq!(
            route.contract.contract_ref().unwrap(),
            bundle.index.root_contract_ref
        );
        assert_eq!(
            route.derivation.derivation_ref().unwrap(),
            bundle.index.derivations[0]
        );
    }
}

#[test]
fn native_k_requires_actual_probe_bytes_and_cannot_be_satisfied_by_surface_ready() {
    let bundle = fixture(false);
    let route = validate_bundle(&bundle).unwrap();
    let original_k = route.contract.contract_ref().unwrap();
    let spec = route.attempt_spec(None);
    let verify = |port, body: &[u8]| {
        verify_observed_candidate(
            &spec,
            vec![RuntimeHttpObservation::from_response(
                port,
                "GET",
                "/result/calculate",
                200,
                body,
            )],
            Vec::new(),
            serde_json::from_value::<VerificationExecutionEvidence>(
                serde_json::json!({"realization":"native-fixture"}),
            )
            .unwrap(),
            &ReceiptContext::formation(),
        )
    };
    assert!(!verify("app.native", ANSWER).verification.fully_satisfied());
    assert!(
        !verify("app.probe", b"surface_ready")
            .verification
            .fully_satisfied()
    );
    assert!(
        !verify("app.probe", b"5\r\nEND")
            .verification
            .fully_satisfied()
    );
    assert!(verify("app.probe", ANSWER).verification.fully_satisfied());
    assert_eq!(route.contract.contract_ref().unwrap(), original_k);
}

#[test]
fn native_pixel_k_verifies_the_compact_measured_frame_response() {
    let bundle = fixture(true);
    let route = validate_bundle(&bundle).unwrap();
    let spec = route.attempt_spec(None);
    let verify = |body: &[u8]| {
        verify_observed_candidate(
            &spec,
            vec![RuntimeHttpObservation::from_response(
                "app.probe",
                "GET",
                "/result/calculate",
                200,
                body,
            )],
            Vec::new(),
            serde_json::from_value::<VerificationExecutionEvidence>(
                serde_json::json!({"realization":"native-fixture"}),
            )
            .unwrap(),
            &ReceiptContext::formation(),
        )
        .verification
        .fully_satisfied()
    };
    let mut frame = vec![0; 640 * 480 * 4];
    let measured = ato_formation::native_surface::pixel_probe_result(&frame).unwrap();
    assert_eq!(measured.len(), 72);
    assert!(verify(&measured));
    frame[0] = 1;
    assert!(!verify(
        &ato_formation::native_surface::pixel_probe_result(&frame).unwrap()
    ));
    assert!(!verify(b"surface_ready"));
    assert!(!verify(&frame));
    assert!(!verify(&measured[..71]));
}

#[test]
fn native_profile_refuses_missing_k_digest_unobserved_actions_and_unsupported_routes() {
    let bundle = fixture(false);
    let route = validate_bundle(&bundle).unwrap();
    for modify in [
        |d: &mut BoundDerivation| {
            d.runtimes.remove(OCI_USER_RUNTIME);
        },
        |d: &mut BoundDerivation| {
            d.steps[0].protocol = PROCESS_PROTOCOL.to_owned();
        },
        |d: &mut BoundDerivation| {
            d.native_surface.as_mut().unwrap().probe_port = "app.native".to_owned();
        },
        |d: &mut BoundDerivation| {
            d.ports[1].from = "sibling".to_owned();
        },
        |d: &mut BoundDerivation| {
            d.native_surface.as_mut().unwrap().operations.push(
                NativeProbeOperation::TerminalExchange {
                    path: "/result/extra".to_owned(),
                    input: b"x\n".to_vec(),
                    output_delimiter: b"DONE".to_vec(),
                    max_output_bytes: 100,
                    timeout_ms: 1000,
                    ready_output: Vec::new(),
                },
            );
        },
    ] {
        let mut d = route.derivation.clone();
        modify(&mut d);
        assert!(validate_initial_route(&route.contract, &route.application, &d).is_err());
    }
    for mutate in [
        |k: &mut BoundContract| {
            k.requirements[0].body_digest = None;
        },
        |k: &mut BoundContract| {
            k.requirements[0].port = Some("app.native".to_owned());
        },
        |k: &mut BoundContract| {
            k.requirements[0].method = Some("POST".to_owned());
        },
        |k: &mut BoundContract| {
            k.requirements[0].status = Some(204);
        },
    ] {
        let mut k = route.contract.clone();
        mutate(&mut k);
        assert!(validate_initial_route(&k, &route.application, &route.derivation).is_err());
    }
}

#[test]
fn host_terminal_ports_are_exact_d_facts_without_fake_guest_listeners() {
    let mut bundle = fixture(false);
    let original = validate_bundle(&bundle).unwrap();
    let original_k = original.contract_ref.to_string();
    let old_ref = original.derivation_ref.to_string();
    let mut derivation = original.derivation.clone();
    for port in &mut derivation.ports {
        port.guest_port = None;
    }
    bundle.index.derivations = vec![replace_object(&mut bundle, &old_ref, &derivation)];
    let bytes = encode_portable_application_bundle(&bundle).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let route = &routes[0];
    assert_eq!(route.contract_ref.as_str(), original_k);
    assert_ne!(route.derivation_ref.as_str(), old_ref);
    assert!(
        route
            .derivation
            .ports
            .iter()
            .all(|port| port.guest_port.is_none())
    );
    let reports = validator_agent::tests_support::route_reports(&routes).unwrap();
    assert_eq!(
        reports[0].native_ports.as_ref(),
        Some(&route.derivation.ports)
    );
    assert_eq!(reports[0].guest_port, None);
    let mut spec = ato_ipc::runtime_launch::RuntimeLaunchSpecV1::parse(include_str!(
        "../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/native-terminal-host-ports.json"
    ))
    .unwrap();
    let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
        unreachable!()
    };
    oci.native_surface = None;
    reports[0].lower_native_surface(oci).unwrap();
    let declaration = ato_runtime_attempt::native_launch::NativeLaunchDeclaration::decode(
        oci.native_surface.as_ref().unwrap(),
    )
    .unwrap();
    declaration.require_host_terminal_ports().unwrap();
    assert_eq!(declaration.ports.as_ref(), Some(&derivation.ports));
    assert_eq!(declaration.derivation_ref, route.derivation_ref.as_str());
    assert_eq!(route.contract.contract_ref().unwrap(), original_k);

    // Host binding is a Native Adapter permission, not a relaxation for Web OCI.
    let mut ordinary = derivation.clone();
    ordinary.native_surface = None;
    ordinary.ports.retain(|port| port.id == "app.probe");
    let mut app = route.application.clone();
    app.surfaces[0].port = "app.probe".into();
    assert!(validate_initial_route(&route.contract, &app, &ordinary).is_err());
    let mut mixed = derivation.clone();
    mixed.ports[0].guest_port = Some(1026);
    assert!(mixed.derivation_ref().is_err());
    let mut pixel = validate_bundle(&fixture(true)).unwrap().derivation.clone();
    for port in &mut pixel.ports {
        port.guest_port = None;
    }
    let host_ref = pixel.derivation_ref().unwrap();
    assert_ne!(
        host_ref,
        validate_bundle(&fixture(true))
            .unwrap()
            .derivation_ref
            .as_str()
    );
    let native = ato_runtime_attempt::native_launch::NativeLaunchDeclaration::new(
        host_ref,
        pixel.native_surface.clone().unwrap(),
    )
    .unwrap()
    .with_ports(pixel.ports.clone())
    .unwrap();
    native.require_host_pixel_ports().unwrap();
    assert!(native.require_host_terminal_ports().is_err());
}

#[test]
fn host_pixel_ports_and_settle_lower_exactly_without_changing_k_or_old_guest_facts() {
    let mut bundle = fixture(true);
    let original = validate_bundle(&bundle).unwrap();
    let k_ref = original.contract_ref.to_string();
    let old_ref = original.derivation_ref.to_string();
    let mut d = original.derivation.clone();
    for port in &mut d.ports {
        port.guest_port = None;
    }
    let NativeProbeOperation::PixelKeyFrame { settle_ms, .. } =
        &mut d.native_surface.as_mut().unwrap().operations[0]
    else {
        unreachable!()
    };
    *settle_ms = 50;
    bundle.index.derivations = vec![replace_object(&mut bundle, &old_ref, &d)];
    let bytes = encode_portable_application_bundle(&bundle).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let route = &routes[0];
    assert_eq!(route.contract_ref.as_str(), k_ref);
    assert_ne!(route.derivation_ref.as_str(), old_ref);
    let reports = validator_agent::tests_support::route_reports(&routes).unwrap();
    assert_eq!(reports[0].native_ports.as_ref(), Some(&d.ports));
    assert_eq!(reports[0].guest_port, None);
    let mut spec = ato_ipc::runtime_launch::RuntimeLaunchSpecV1::parse(include_str!(
        "../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/native-terminal-host-ports.json"
    ))
    .unwrap();
    let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
        unreachable!()
    };
    oci.native_surface = None;
    reports[0].lower_native_surface(oci).unwrap();
    let declaration = ato_runtime_attempt::native_launch::NativeLaunchDeclaration::decode(
        oci.native_surface.as_ref().unwrap(),
    )
    .unwrap();
    declaration.require_host_pixel_ports().unwrap();
    assert_eq!(declaration.adapter, d.native_surface.unwrap());
    assert_eq!(declaration.derivation_ref, route.derivation_ref.as_str());
    let older = validate_bundle(&fixture(true)).unwrap();
    assert!(
        older
            .derivation
            .ports
            .iter()
            .all(|p| p.guest_port.is_some())
    );
    assert_eq!(older.derivation_ref.as_str(), old_ref);
}
