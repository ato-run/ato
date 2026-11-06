//! The native process service group stays inside the existing portable
//! Application contract: one more multi-step route shape, no new identity.

use std::path::Path;

use ato_portable_application::{
    PortableRealizationKind, build_authored_bundle_v2, validate_all_derivations,
};

fn sample(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

#[test]
fn the_native_process_group_binds_one_route_with_its_declared_edge() {
    let (bytes, bundle) =
        build_authored_bundle_v2(&sample("native-process-group")).expect("the sample packs");
    assert!(!bytes.is_empty());
    let routes = validate_all_derivations(&bundle).expect("the route is admitted");
    let [route] = routes.as_slice() else {
        panic!("one route");
    };
    assert_eq!(
        route.realization,
        PortableRealizationKind::ProcessServiceGroup
    );
    assert_eq!(route.derivation.steps.len(), 2);
    assert_eq!(route.derivation.runtimes.len(), 0);
    assert_eq!(route.derivation.steps[0].id, "backend");
    assert!(route.derivation.steps[0].peer_ports.is_empty());
    assert_eq!(route.derivation.steps[1].id, "web");
    assert_eq!(route.derivation.steps[1].peer_ports, ["backend.http"]);
    assert_eq!(route.derivation.steps[1].runtimes["python"], "3.12");
    assert_eq!(
        route
            .derivation
            .ports
            .iter()
            .map(|port| (port.id.as_str(), port.protocol.as_str()))
            .collect::<Vec<_>>(),
        [("backend.http", "ato.tcp@1"), ("app.http", "ato.http@1"),]
    );
    // K observes the responses the author declared; the group adds no
    // topology, internal reachability, or process observation to it.
    assert_eq!(
        route
            .contract
            .requirements
            .iter()
            .map(|requirement| requirement.id.as_str())
            .collect::<Vec<_>>(),
        ["external-egress-denied", "surface", "through-proxy"]
    );
}

#[test]
fn the_oci_service_group_still_admits_unchanged() {
    let (_, bundle) =
        build_authored_bundle_v2(&sample("oci-service-group-proof")).expect("the sample packs");
    let routes = validate_all_derivations(&bundle).expect("the OCI group is admitted");
    let [route] = routes.as_slice() else {
        panic!("one route");
    };
    assert_eq!(route.realization, PortableRealizationKind::OciServiceGroup);
    assert!(
        route
            .derivation
            .steps
            .iter()
            .all(|step| step.peer_ports.is_empty()),
        "the OCI group's canonical D must not grow a native peer field"
    );
}

#[test]
fn the_single_process_route_is_unchanged() {
    let (_, bundle) = build_authored_bundle_v2(&sample("portable-multi-process-authored"))
        .expect("the sample packs");
    let routes = validate_all_derivations(&bundle).expect("both routes are admitted");
    assert_eq!(routes.len(), 2);
    for route in routes {
        assert_eq!(route.realization, PortableRealizationKind::LocalProcess);
        assert_eq!(route.derivation.steps.len(), 1);
        assert!(route.derivation.steps[0].peer_ports.is_empty());
    }
}

/// The runtime-owned broker and shim are exercised without a host sandbox, so
/// this covers the edge wiring itself even where Seatbelt cannot be nested.
#[cfg(unix)]
#[test]
fn a_service_reaches_its_declared_peer_only_through_the_broker_shim() {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use ato_portable_application::process_group::{ProcessServiceGroup, ServiceSpec};
    use ato_runtime_attempt::peer_broker::{PeerBroker, serve_shim};

    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    fn http_get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.0\r\nHost: proxy\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default()
    }

    let backend_port = free_port();
    let shim_port = free_port();
    let proxy_port = free_port();
    let root = tempfile::tempdir().unwrap();
    let token = "declared-edge";
    let broker = PeerBroker::start(
        &root.path().join("peer"),
        BTreeMap::from([(
            token.to_owned(),
            std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, backend_port)),
        )]),
    )
    .expect("the broker starts");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, shim_port)).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let broker_path = broker.socket_path().to_path_buf();
    let stop = stopped.clone();
    let shim = std::thread::spawn(move || serve_shim(listener, &broker_path, token, stop));
    std::thread::sleep(Duration::from_millis(50));

    let workspace = sample("native-process-group");
    let group = ProcessServiceGroup::start(
        &workspace,
        vec![
            ServiceSpec {
                name: "backend".to_owned(),
                command: vec!["python3".to_owned(), "backend.py".to_owned()],
                cwd: ".".into(),
                environment: BTreeMap::from([(
                    "ATO_ENDPOINT_BACKEND_HTTP_PORT".to_owned(),
                    backend_port.to_string(),
                )]),
                readiness: (Ipv4Addr::LOCALHOST, backend_port).into(),
                helpers: Vec::new(),
            },
            ServiceSpec {
                name: "web".to_owned(),
                command: vec!["python3".to_owned(), "proxy.py".to_owned()],
                cwd: ".".into(),
                environment: BTreeMap::from([
                    (
                        "ATO_ENDPOINT_APP_HTTP_PORT".to_owned(),
                        proxy_port.to_string(),
                    ),
                    (
                        "ATO_ENDPOINT_BACKEND_HTTP_PORT".to_owned(),
                        shim_port.to_string(),
                    ),
                ]),
                readiness: (Ipv4Addr::LOCALHOST, proxy_port).into(),
                helpers: Vec::new(),
            },
        ],
    )
    .expect("the group starts");
    assert_eq!(http_get(proxy_port, "/healthz"), "native-proxy-ok\n");
    assert_eq!(http_get(proxy_port, "/"), "from-backend\n");
    let pids = group.pids();
    group.stop().expect("the group stops");
    for (name, pid) in &pids {
        assert!(
            !ato_adapter_process::process_group_is_alive(*pid),
            "process `{name}` (pid {pid}) survived stop"
        );
    }
    stopped.store(true, Ordering::Release);
    shim.join().unwrap().unwrap();
}
