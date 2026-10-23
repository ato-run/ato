use ato_formation::authoring::{BoundDerivation, BoundPort, BoundStep};
use ato_formation::native_surface::NativeSurfaceAdapter;
use serde_json::{Value, json};

fn fixture() -> (Value, Vec<BoundPort>, Vec<BoundStep>) {
    let native = json!({
        "surface": {"kind":"terminal", "port":"app.terminal", "cols":80, "rows":24},
        "probe_port":"app.probe",
        "operations":[{"kind":"terminal_exchange", "path":"/result/calculate",
            "input":[50,43,50,10], "output_delimiter":[69,78,68],
            "max_output_bytes":4096, "timeout_ms":1000}]
    });
    let ports = serde_json::from_value(json!([
        {"id":"app.terminal", "protocol":"ato.pty@1", "from":"serve", "guest_port":1026},
        {"id":"app.probe", "protocol":"ato.http@1", "from":"serve", "guest_port":8081}
    ]))
    .unwrap();
    let steps = serde_json::from_value(json!([
        {"id":"serve", "protocol":"ato.oci@1", "op":"serve"}
    ]))
    .unwrap();
    (native, ports, steps)
}

#[test]
fn native_catalog_is_typed_bounded_and_has_no_expected_answer_or_authority() {
    let (value, ports, steps) = fixture();
    let native: NativeSurfaceAdapter = serde_json::from_value(value.clone()).unwrap();
    native.validate(&ports, &steps).unwrap();
    for (field, value) in [
        ("expected_digest", json!("sha256:fixture")),
        ("url", json!("http://127.0.0.1/admin")),
        ("assertion_key", json!("credential")),
    ] {
        let mut invalid = serde_json::to_value(&native).unwrap();
        invalid[field] = value;
        assert!(serde_json::from_value::<NativeSurfaceAdapter>(invalid).is_err());
    }
    let mut invalid = value;
    invalid["operations"][0]["expected_output"] = json!([52]);
    assert!(serde_json::from_value::<NativeSurfaceAdapter>(invalid).is_err());
}

#[test]
fn native_catalog_rejects_echo_completion_unsafe_paths_and_unbounded_actions() {
    let (value, ports, steps) = fixture();
    let mutations = [
        ("/operations/0/input", json!([69, 78, 68, 10])),
        ("/operations/0/output_delimiter", json!([])),
        ("/operations/0/input", json!(vec![65_u8; 4097])),
        ("/operations/0/timeout_ms", json!(30_001)),
        ("/operations/0/max_output_bytes", json!(65_537)),
        ("/operations/0/path", json!("/../control")),
        ("/operations/0/path", json!("/result?url=http://host")),
        ("/operations/0/path", json!("/%2e%2e/control")),
        ("/surface/cols", json!(501)),
        ("/probe_port", json!("app.terminal")),
    ];
    for (pointer, replacement) in mutations {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        let native: NativeSurfaceAdapter = serde_json::from_value(invalid).unwrap();
        assert!(native.validate(&ports, &steps).is_err(), "{pointer}");
    }
    let mut duplicate = value.clone();
    duplicate["operations"]
        .as_array_mut()
        .unwrap()
        .push(value["operations"][0].clone());
    let native: NativeSurfaceAdapter = serde_json::from_value(duplicate).unwrap();
    assert!(native.validate(&ports, &steps).is_err());
}

#[test]
fn native_surface_and_probe_cannot_target_sibling_or_aliased_ports() {
    let (value, ports, steps) = fixture();
    let native: NativeSurfaceAdapter = serde_json::from_value(value).unwrap();
    for mutate in [
        |p: &mut Vec<BoundPort>| p[1].from = "sibling".to_owned(),
        |p: &mut Vec<BoundPort>| p[1].guest_port = p[0].guest_port,
        |p: &mut Vec<BoundPort>| p[1].protocol = "ato.tcp@1".to_owned(),
        |p: &mut Vec<BoundPort>| p.push(p[0].clone()),
    ] {
        let mut invalid = ports.clone();
        mutate(&mut invalid);
        assert!(native.validate(&invalid, &steps).is_err());
    }
}

#[test]
fn native_pixel_capture_has_fixed_bgrx32_size_and_matching_operation_kind() {
    let (_, mut ports, steps) = fixture();
    ports[0].protocol = "ato.tcp@1".to_owned();
    let value = json!({
        "surface":{"kind":"pixel_stream","port":"app.terminal","width":640,"height":480},
        "probe_port":"app.probe",
        "operations":[{"kind":"pixel_key_frame","path":"/result/key", "keysyms":[97],
            "max_frame_bytes":640*480*4,"timeout_ms":1000}]
    });
    let native: NativeSurfaceAdapter = serde_json::from_value(value.clone()).unwrap();
    native.validate(&ports, &steps).unwrap();
    for (pointer, replacement) in [
        ("/operations/0/max_frame_bytes", json!(640 * 480 * 3)),
        ("/operations/0/keysyms", json!([0])),
        ("/operations/0/keysyms", json!(vec![97_u32; 33])),
        ("/surface/width", json!(0)),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        let native: NativeSurfaceAdapter = serde_json::from_value(invalid).unwrap();
        assert!(native.validate(&ports, &steps).is_err(), "{pointer}");
    }
    let (terminal, _, _) = fixture();
    let mut invalid = value;
    invalid["operations"] = terminal["operations"].clone();
    let native: NativeSurfaceAdapter = serde_json::from_value(invalid).unwrap();
    assert!(native.validate(&ports, &steps).is_err());
}

#[test]
fn omission_preserves_old_canonical_derivation_bytes() {
    let old = json!({"schema":"ato.derivation/1","inputs":[],"steps":[],"ports":[],"state":[],"effects":"pure"});
    let old_bytes = serde_jcs::to_vec(&old).unwrap();
    let derivation: BoundDerivation = serde_json::from_value(old).unwrap();
    assert!(derivation.native_surface.is_none());
    assert_eq!(serde_jcs::to_vec(&derivation).unwrap(), old_bytes);
}

#[test]
fn aggregate_catalog_time_and_frame_memory_are_bounded_before_capture() {
    let (mut value, _, _) = fixture();
    value["operations"] = json!(
        (0..5)
            .map(|index| {
                json!({"kind":"terminal_exchange","path":format!("/result/{index}"),
            "input":[97],"output_delimiter":[69,78,68],"max_output_bytes":4096,"timeout_ms":30000})
            })
            .collect::<Vec<_>>()
    );
    let native: NativeSurfaceAdapter = serde_json::from_value(value.clone()).unwrap();
    assert!(native.validate_catalog().is_err());
    value["surface"] = json!({"kind":"pixel_stream","port":"app.pixel","width":4096,"height":4096});
    value["operations"] = json!(
        (0..2)
            .map(|index| {
                json!({"kind":"pixel_key_frame","path":format!("/result/{index}"),
            "keysyms":[97],"max_frame_bytes":64*1024*1024,"timeout_ms":1000})
            })
            .collect::<Vec<_>>()
    );
    let native: NativeSurfaceAdapter = serde_json::from_value(value).unwrap();
    assert!(native.validate_catalog().is_err());
}

#[test]
fn terminal_line_echo_cannot_complete_probe_after_termios_newline_translation() {
    let (value, _, _) = fixture();
    for (input, delimiter) in [
        (b"compute\n".as_slice(), b"compute\r\n".as_slice()),
        (b"compute\r".as_slice(), b"compute\n".as_slice()),
        (b"com\rpute\n".as_slice(), b"compute".as_slice()),
        (b"compute\r".as_slice(), b"compute^M".as_slice()),
        (b"compute\n".as_slice(), b"compute^M".as_slice()),
        (b"compute\n".as_slice(), b"\r\n".as_slice()),
        (b"compute\t\n".as_slice(), b"END".as_slice()),
        (b"\x1bcompute\n".as_slice(), b"END".as_slice()),
    ] {
        let mut invalid = value.clone();
        invalid["operations"][0]["input"] = json!(input);
        invalid["operations"][0]["output_delimiter"] = json!(delimiter);
        let native: NativeSurfaceAdapter = serde_json::from_value(invalid).unwrap();
        assert!(
            native.validate_catalog().is_err(),
            "{input:?} / {delimiter:?}"
        );
    }
}
