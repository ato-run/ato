use std::process::Command;

#[test]
fn internal_features_accepts_file_input_and_returns_json() {
    let exe = env!("CARGO_BIN_EXE_nacelle");

    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    let path = tmp.join(format!("nacelle-internal-features-{pid}.json"));

    std::fs::write(&path, r#"{"spec_version":"0.1.0"}"#).expect("write payload");

    let out = Command::new(exe)
        .args(["internal", "--input"])
        .arg(&path)
        .arg("features")
        .output()
        .expect("run nacelle internal features");

    // Cleanup best-effort.
    let _ = std::fs::remove_file(&path);

    assert!(out.status.success(), "status={:?} stderr={}", out.status, String::from_utf8_lossy(&out.stderr));

    let stdout = String::from_utf8(out.stdout).expect("stdout utf8");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is json");

    assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(json.get("spec_version").and_then(|v| v.as_str()), Some("0.1.0"));

    let engine_name = json
        .get("engine")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str());
    assert_eq!(engine_name, Some("nacelle"));

    let capabilities = json.get("capabilities");
    assert!(capabilities.is_some(), "missing capabilities");
}
