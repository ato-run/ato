mod fail_closed_support;

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;

use fail_closed_support::*;
use tempfile::TempDir;

#[test]
#[ignore = "requires host sandbox bootstrap or ATO_STRICT_CI-capable environment"]
#[cfg(unix)]
fn tier2_native_fs_isolation_enforced() {
    let (_workspace, fixture) = prepare_fixture_workspace("tier2-fs-isolation-capsule");
    let home = prepare_consent_home(&fixture);

    let leak_outside = fixture
        .parent()
        .expect("fixture must have parent")
        .join("pwned-outside.txt");
    let leak_tmp = PathBuf::from("/tmp/ato_host_leak_test_17.txt");
    let _ = fs::remove_file(&leak_outside);
    let _ = fs::remove_file(&leak_tmp);

    let uv_dir = TempDir::new().expect("failed to create temp dir for mock uv");
    let uv_path = uv_dir.path().join("uv");
    write_mock_uv(&uv_path);

    let base_path = std::env::var("PATH").unwrap_or_default();
    let merged_path = if base_path.is_empty() {
        uv_dir.path().display().to_string()
    } else {
        format!("{}:{}", uv_dir.path().display(), base_path)
    };
    let nacelle_path = resolve_test_nacelle_path();

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--unsafe-bypass-sandbox")
        .arg("--nacelle")
        .arg(&nacelle_path)
        .arg(&fixture)
        .env("HOME", home.path())
        .env("PATH", merged_path)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute tier2 fs isolation fixture");

    let strict_ci = std::env::var("ATO_STRICT_CI")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let host_limited = stderr.contains("pfctl failed to load anchor")
            || stderr.contains("Sandbox unavailable")
            || stderr.contains("No compatible native sandbox backend is available");
        if host_limited {
            assert!(
                !strict_ci,
                "strict CI requires sandbox bootstrap; stderr={stderr}"
            );
            return;
        }
    }

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("tier2 fs isolation enforced"));
    assert!(fixture.join("output").join("safe.txt").exists());
    assert!(!leak_outside.exists());
    assert!(!leak_tmp.exists());
}
