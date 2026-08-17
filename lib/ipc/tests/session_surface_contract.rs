use std::collections::BTreeMap;

use ato_ipc::session_surface::*;

fn pixel_surface() -> SessionSurface {
    SessionSurface {
        descriptor: SessionSurfaceDescriptor::PixelStream {
            profile: PIXEL_STREAM_PROFILE.to_string(),
            surface_id: "surface-1".to_string(),
            transport: SessionSurfaceTransport::RfbWebsocket,
            viewport: PixelStreamViewport {
                width: 1280,
                height: 720,
            },
            capabilities: BTreeMap::from([
                (
                    "keyboard".to_string(),
                    SurfaceCapabilityValue::Text("us".to_string()),
                ),
                ("pointer".to_string(), SurfaceCapabilityValue::Boolean(true)),
            ]),
        },
        access: SessionSurfaceAccess {
            connect_url: "wss://session.example/surface".to_string(),
            auth_exchange_url: Some("https://session.example/auth/exchange".to_string()),
            expires_at: "2026-07-14T12:00:00Z".to_string(),
            generation: 1,
        },
    }
}

fn web_surface() -> SessionSurface {
    SessionSurface {
        descriptor: SessionSurfaceDescriptor::Web {
            profile: WEB_SURFACE_PROFILE.to_string(),
            surface_id: "surface-web".to_string(),
            embed_policy: WebEmbedPolicy::Sandboxed,
        },
        access: SessionSurfaceAccess {
            connect_url: "https://session.example/".to_string(),
            auth_exchange_url: None,
            expires_at: "2026-07-14T12:00:00Z".to_string(),
            generation: 1,
        },
    }
}

fn terminal_surface() -> SessionSurface {
    SessionSurface {
        descriptor: SessionSurfaceDescriptor::Terminal {
            profile: TERMINAL_SURFACE_PROFILE.to_string(),
            surface_id: "surface-terminal".to_string(),
            transport: SessionSurfaceTransport::TerminalWebsocket,
            capabilities: BTreeMap::from([
                ("input".to_string(), SurfaceCapabilityValue::Boolean(true)),
                ("resize".to_string(), SurfaceCapabilityValue::Boolean(true)),
                (
                    "encoding".to_string(),
                    SurfaceCapabilityValue::Text("utf-8".to_string()),
                ),
            ]),
        },
        access: SessionSurfaceAccess {
            connect_url: "wss://session.example/surfaces/surface-terminal".to_string(),
            auth_exchange_url: Some(
                "https://session.example/surfaces/surface-terminal/auth".to_string(),
            ),
            expires_at: "2026-07-14T12:00:00Z".to_string(),
            generation: 1,
        },
    }
}

#[test]
fn pixel_surface_round_trips_with_the_v1_wire_shape() {
    let surface = pixel_surface();

    let value = serde_json::to_value(&surface).expect("serialize surface");
    let parsed: SessionSurface = serde_json::from_value(value.clone()).expect("parse surface");

    assert_eq!(parsed, surface);
    assert_eq!(value["descriptor"]["kind"], "pixel_stream");
    assert_eq!(value["descriptor"]["transport"], "rfb_websocket");
}

#[test]
fn authenticated_pixel_surface_requires_secure_access_and_exchange() {
    let mut missing_exchange = pixel_surface();
    missing_exchange.access.auth_exchange_url = None;
    assert_eq!(
        missing_exchange
            .validate()
            .expect_err("authenticated Pixel needs an exchange"),
        SessionSurfaceContractError::PixelAuthExchangeRequired,
    );

    let mut insecure_socket = pixel_surface();
    insecure_socket.access.connect_url = "ws://session.example/surface".to_string();
    assert_eq!(
        insecure_socket
            .validate()
            .expect_err("Pixel socket must be secure"),
        SessionSurfaceContractError::PixelAccessMustUseWss,
    );

    let mut insecure_exchange = pixel_surface();
    insecure_exchange.access.auth_exchange_url =
        Some("http://session.example/auth/exchange".to_string());
    assert_eq!(
        insecure_exchange
            .validate()
            .expect_err("Pixel exchange must be secure"),
        SessionSurfaceContractError::PixelAuthExchangeMustUseHttps,
    );
}

#[test]
fn terminal_surface_round_trips_with_secure_same_host_access() {
    let surface = terminal_surface();
    surface.validate().expect("valid terminal surface");
    let value = serde_json::to_value(&surface).expect("serialize terminal surface");
    assert_eq!(value["descriptor"]["transport"], "terminal_websocket");
    assert_eq!(
        serde_json::from_value::<SessionSurface>(value).expect("parse terminal surface"),
        surface
    );
}

#[test]
fn terminal_surface_rejects_insecure_missing_or_cross_host_access() {
    let mut wrong_transport = terminal_surface();
    if let SessionSurfaceDescriptor::Terminal { transport, .. } = &mut wrong_transport.descriptor {
        *transport = SessionSurfaceTransport::RfbWebsocket;
    }
    assert_eq!(
        wrong_transport.validate().expect_err("wrong transport"),
        SessionSurfaceContractError::UnsupportedTransport
    );

    let mut insecure_socket = terminal_surface();
    insecure_socket.access.connect_url = "ws://session.example/surface".to_string();
    assert_eq!(
        insecure_socket.validate().expect_err("insecure socket"),
        SessionSurfaceContractError::TerminalAccessMustUseWss
    );

    let mut missing_exchange = terminal_surface();
    missing_exchange.access.auth_exchange_url = None;
    assert_eq!(
        missing_exchange.validate().expect_err("missing exchange"),
        SessionSurfaceContractError::TerminalAuthExchangeRequired
    );

    let mut insecure_exchange = terminal_surface();
    insecure_exchange.access.auth_exchange_url =
        Some("http://session.example/auth/exchange".to_string());
    assert_eq!(
        insecure_exchange.validate().expect_err("insecure exchange"),
        SessionSurfaceContractError::TerminalAuthExchangeMustUseHttps
    );

    let mut other_host = terminal_surface();
    other_host.access.auth_exchange_url = Some("https://other.example/auth/exchange".to_string());
    assert_eq!(
        other_host.validate().expect_err("cross-host exchange"),
        SessionSurfaceContractError::TerminalAccessHostsMustMatch
    );
}

#[test]
fn unknown_descriptor_kind_is_typed_unsupported_instead_of_web_fallback() {
    let raw = serde_json::json!({
        "descriptor": {
            "kind": "future_surface",
            "profile": "ato.future.v1",
            "surface_id": "surface-future"
        },
        "access": {
            "connect_url": "wss://session.example/future",
            "expires_at": "2026-07-14T12:00:00Z",
            "generation": 1
        }
    });

    let surface: SessionSurface = serde_json::from_value(raw).expect("parse unknown kind");

    assert_eq!(surface.descriptor, SessionSurfaceDescriptor::Unknown);
    assert_eq!(
        surface.validate().expect_err("unknown kind must fail"),
        SessionSurfaceContractError::UnsupportedDescriptorKind,
    );
}

#[test]
fn legacy_web_is_read_only_when_surface_field_is_absent() {
    let legacy = LegacyWebSurfaceFields {
        app_url: Some("https://legacy.example/".to_string()),
        embed_policy: Some(WebEmbedPolicy::Sandboxed),
        app_expires_at: Some("2026-07-14T12:00:00Z".to_string()),
    };

    let resolved = resolve_session_surface(false, None, None, legacy, "legacy-surface")
        .expect("resolve legacy")
        .expect("legacy surface");

    assert_eq!(resolved.source, SessionSurfaceSource::LegacyAppUrl);
    assert!(matches!(
        resolved.envelope.surface.descriptor,
        SessionSurfaceDescriptor::Web { .. }
    ));
}

#[test]
fn present_invalid_surface_never_falls_back_to_legacy_web() {
    let legacy = LegacyWebSurfaceFields {
        app_url: Some("https://legacy.example/".to_string()),
        embed_policy: Some(WebEmbedPolicy::Sandboxed),
        app_expires_at: Some("2026-07-14T12:00:00Z".to_string()),
    };

    let error = resolve_session_surface(true, Some("1"), None, legacy, "legacy-surface")
        .expect_err("present null surface must fail");

    assert_eq!(error, SessionSurfaceContractError::MalformedSurface);
}

#[test]
fn legacy_write_projection_is_emitted_for_web_only() {
    assert!(web_surface().legacy_web_projection().is_some());
    assert!(pixel_surface().legacy_web_projection().is_none());
}

#[test]
fn negotiation_requires_capsule_runner_and_client_profile_intersection() {
    let requirement = SessionSurfaceRequirement {
        kind: SessionSurfaceKind::PixelStream,
        profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
    };
    let client = ClientSessionSurfaceCapabilities {
        accepted_session_surfaces: Some(vec![AcceptedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
        }]),
    };
    let runner = RunnerSessionSurfaceCapabilities {
        supported_session_surfaces: Some(vec![SupportedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![PIXEL_STREAM_PROFILE.to_string()]),
            transports: Some(vec![SessionSurfaceTransport::RfbWebsocket]),
        }]),
    };

    let selected =
        negotiate_session_surface(&requirement, &client, &runner).expect("negotiate pixel surface");

    assert_eq!(selected.kind, SessionSurfaceKind::PixelStream);
    assert_eq!(selected.profile, PIXEL_STREAM_PROFILE);
    assert_eq!(selected.transport, SessionSurfaceTransport::RfbWebsocket);
}

#[test]
fn negotiation_does_not_guess_a_transport_for_an_unknown_profile() {
    let future_profile = "ato.pixel-stream.v2".to_string();
    let requirement = SessionSurfaceRequirement {
        kind: SessionSurfaceKind::PixelStream,
        profiles: Some(vec![future_profile.clone()]),
    };
    let client = ClientSessionSurfaceCapabilities {
        accepted_session_surfaces: Some(vec![AcceptedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![future_profile.clone()]),
        }]),
    };
    let runner = RunnerSessionSurfaceCapabilities {
        supported_session_surfaces: Some(vec![SupportedSessionSurface {
            kind: SessionSurfaceKind::PixelStream,
            profiles: Some(vec![future_profile.clone()]),
            transports: Some(vec![SessionSurfaceTransport::RfbWebsocket]),
        }]),
    };

    assert_eq!(
        negotiate_session_surface(&requirement, &client, &runner)
            .expect_err("unknown profile must fail closed"),
        SurfaceNegotiationError::NoCompatibleTransport(future_profile),
    );
}

#[test]
fn client_omission_and_empty_advertisement_are_distinct_errors() {
    let omitted = ClientSessionSurfaceCapabilities {
        accepted_session_surfaces: None,
    };
    let empty = ClientSessionSurfaceCapabilities {
        accepted_session_surfaces: Some(Vec::new()),
    };

    assert_eq!(
        omitted.validate().expect_err("omission must fail"),
        SurfaceAdvertisementError::MissingAcceptedSessionSurfaces,
    );
    assert_eq!(
        empty.validate().expect_err("empty must fail"),
        SurfaceAdvertisementError::EmptyAcceptedSessionSurfaces,
    );
}

#[test]
fn surface_assertion_claims_reject_empty_session_binding() {
    let claims = SurfaceAssertionClaims {
        aud: SURFACE_GATEWAY_ASSERTION_AUDIENCE.to_string(),
        session_id: String::new(),
        surface_id: "surface-1".to_string(),
        principal: SurfaceAssertionPrincipal {
            kind: SurfacePrincipalKind::User,
            id: "user-1".to_string(),
        },
        exp: 1_800_000_000,
        jti: "assertion-1".to_string(),
        kid: "key-1".to_string(),
    };

    assert_eq!(
        claims.validate().expect_err("empty session id must fail"),
        SurfaceAssertionError::EmptyClaim("session_id"),
    );
}

#[test]
fn endpoint_exposure_is_required_and_unknown_values_are_rejected() {
    let missing = serde_json::json!({
        "role": "pixel_rfb",
        "protocol": "tcp",
        "port": 5900,
        "readiness": { "kind": "tcp_connect" }
    });
    let unknown = serde_json::json!({
        "role": "pixel_rfb",
        "protocol": "tcp",
        "exposure": "internet",
        "port": 5900,
        "readiness": { "kind": "tcp_connect" }
    });

    assert!(serde_json::from_value::<EndpointContract>(missing).is_err());
    assert!(serde_json::from_value::<EndpointContract>(unknown).is_err());
}

#[test]
fn pixel_rfb_endpoint_cannot_be_publicly_proxied() {
    let endpoint = EndpointContract {
        role: EndpointRole::PixelRfb,
        protocol: EndpointProtocol::Tcp,
        exposure: EndpointExposure::PublicProxy,
        port: 5900,
        readiness: EndpointReadiness::FirstFrame,
    };

    assert_eq!(
        endpoint.validate().expect_err("public RFB must fail"),
        EndpointContractError::RoleCannotBePublic(EndpointRole::PixelRfb),
    );
}

#[test]
fn pixel_rfb_requires_first_frame_readiness() {
    let early = EndpointContract {
        role: EndpointRole::PixelRfb,
        protocol: EndpointProtocol::Tcp,
        exposure: EndpointExposure::GuestPrivate,
        port: 5900,
        readiness: EndpointReadiness::TcpConnect,
    };
    assert_eq!(
        early
            .validate()
            .expect_err("TCP connect is not interactive readiness"),
        EndpointContractError::PixelFirstFrameRequired,
    );

    let ready = EndpointContract {
        readiness: EndpointReadiness::FirstFrame,
        ..early
    };
    ready.validate().expect("private RFB first-frame endpoint");
}

// ── the v1 submission subset ────────────────────────────────────────────────
//
// What the contract can DESCRIBE and what a submission can PUBLISH are
// different sets, and these pin the difference so widening one does not
// silently widen the other.

/// Web is the surface the submission pipeline can carry end to end today.
#[test]
fn web_is_admitted_by_the_v1_submission_subset() {
    assert_eq!(
        v1_submission_surface_kind(SessionSurfaceKind::Web),
        Ok(SessionSurfaceKind::Web)
    );
}

/// Pixel is describable and renderable but not submittable.
///
/// Not an oversight: the wizard's interactive lane admits only the `recipe` job
/// kind, and the recipe lane refuses a pixel surface requirement outright. If
/// this test starts failing because someone added `PixelStream` to the slice,
/// the two refusals above must have been lifted first — otherwise the failure
/// simply moves from submission time to capture time, which is strictly worse.
#[test]
fn pixel_stream_is_refused_by_the_v1_submission_subset() {
    assert_eq!(
        v1_submission_surface_kind(SessionSurfaceKind::PixelStream),
        Err(SessionSurfaceContractError::SurfaceNotInSubmissionSubset(
            SessionSurfaceKind::PixelStream
        ))
    );
}

/// Terminal is published only after its producer and runtime path are wired.
#[test]
fn terminal_is_admitted_by_the_v1_submission_subset() {
    assert_eq!(
        v1_submission_surface_kind(SessionSurfaceKind::Terminal),
        Ok(SessionSurfaceKind::Terminal)
    );
}

/// The fail-closed arm: a kind this build cannot even name is refused.
///
/// `Unknown` is what `#[serde(other)]` produces when a newer producer emits a
/// kind this build has never heard of. Admitting it would let an unrecognised
/// surface through as though it had been negotiated.
#[test]
fn an_unknown_kind_is_refused_by_the_v1_submission_subset() {
    assert!(v1_submission_surface_kind(SessionSurfaceKind::Unknown).is_err());
}

/// Every kind the contract can describe is either admitted or refused with the
/// subset's own error — never with a generic one.
///
/// This is the test that makes the subset fail-closed in practice: adding a
/// variant to `SessionSurfaceKind` without deciding about it lands here.
#[test]
fn every_describable_kind_gets_an_explicit_subset_verdict() {
    for kind in [
        SessionSurfaceKind::Web,
        SessionSurfaceKind::PixelStream,
        SessionSurfaceKind::Terminal,
        SessionSurfaceKind::Unknown,
    ] {
        match v1_submission_surface_kind(kind) {
            Ok(admitted) => assert_eq!(admitted, kind, "an admitted kind must round-trip"),
            Err(SessionSurfaceContractError::SurfaceNotInSubmissionSubset(refused)) => {
                assert_eq!(refused, kind, "a refusal must name the kind it refused")
            }
            Err(other) => panic!("{kind:?} was refused with an unrelated error: {other}"),
        }
    }
}

/// The subset does not admit duplicates or an empty set by accident.
#[test]
fn the_v1_submission_subset_is_a_non_empty_set() {
    assert!(!V1_SUBMISSION_SURFACE_KINDS.is_empty());
    let mut seen = Vec::new();
    for kind in V1_SUBMISSION_SURFACE_KINDS {
        assert!(!seen.contains(kind), "{kind:?} is listed twice");
        seen.push(*kind);
    }
}
