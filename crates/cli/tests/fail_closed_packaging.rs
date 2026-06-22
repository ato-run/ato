mod fail_closed_support;

use std::process::Stdio;

use fail_closed_support::*;

#[test]
#[ignore = "packaging cache behavior is host-dependent; keep separated from always-on fail-closed suite"]
fn npm_package_lock_fallback_success() {
    let (_workspace, fixture) = prepare_fixture_workspace("npm-fallback-capsule");
    let home = prepare_consent_home(&fixture);

    let build_output = ato_cmd()
        .arg("build")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato build");

    assert!(
        build_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let archive_path = find_built_capsule_path(&fixture);
    let run_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&archive_path)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato run");

    assert!(
        run_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    assert!(String::from_utf8_lossy(&run_output.stdout).contains("npm package-lock fallback OK"));
}

#[test]
#[ignore = "air-gap replay still depends on local cache warmth and host runtime availability"]
fn airgap_offline_execution_success() {
    let (_workspace, fixture) = prepare_fixture_workspace("airgap-npm-fallback-capsule");
    let home = prepare_consent_home(&fixture);

    let warmup_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute warmup run");

    assert!(
        warmup_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&warmup_output.stderr)
    );

    let offline_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute offline run");

    assert!(
        offline_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&offline_output.stderr)
    );
    assert!(String::from_utf8_lossy(&offline_output.stdout).contains("airgap cached-only run OK"));
}
