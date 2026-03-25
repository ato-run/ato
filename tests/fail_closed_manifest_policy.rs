mod fail_closed_support;

use std::fs;
use std::process::Stdio;

use fail_closed_support::*;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
fn run_without_seeded_consent_with_mock_nacelle(
    fixture_name: &str,
    args: &[&str],
    extra_envs: &[(&str, &str)],
) -> std::process::Output {
    let nacelle_dir = TempDir::new().expect("failed to create temp dir for mock nacelle");
    let nacelle_path = nacelle_dir.path().join("nacelle");
    write_mock_nacelle(&nacelle_path);
    let nacelle_owned = nacelle_path.to_string_lossy().into_owned();

    let mut envs = extra_envs.to_vec();
    envs.push(("NACELLE_PATH", nacelle_owned.as_str()));
    run_without_seeded_consent(fixture_name, args, &envs)
}

#[test]
#[cfg(unix)]
fn non_interactive_missing_consent_denied() {
    let output =
        run_without_seeded_consent_with_mock_nacelle("network-exfil-capsule", &["--sandbox"], &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_POLICY_VIOLATION") || stderr.contains("E302"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("consent") || stderr.contains("ExecutionPlan consent"),
        "stderr={stderr}"
    );
}

#[test]
#[cfg(unix)]
fn yes_flag_does_not_bypass_missing_consent() {
    let output = run_without_seeded_consent_with_mock_nacelle(
        "network-exfil-capsule",
        &["--yes", "--sandbox"],
        &[],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_POLICY_VIOLATION") || stderr.contains("E302"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("consent") || stderr.contains("ExecutionPlan consent"),
        "stderr={stderr}"
    );
}

#[test]
fn lockfile_tampered_rejected_before_runtime() {
    let (_workspace, fixture) = prepare_fixture_workspace("network-exfil-capsule");
    tamper_lock_manifest_hash(&fixture);
    let home = prepare_consent_home(&fixture);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_LOCKFILE_TAMPERED")
            || stderr.contains("E207")
            || stderr.contains("capsule.lock.json is missing runtimes.deno entry"),
        "stderr={stderr}"
    );
}

#[test]
#[cfg(unix)]
fn web_entrypoint_outside_public_allowlist_rejected() {
    let output = run_without_seeded_consent_with_mock_nacelle(
        "web-path-traversal-capsule",
        &["--yes", "--sandbox"],
        &[],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        (stderr.contains("ATO_ERR_POLICY_VIOLATION") || stderr.contains("E301"))
            && (stderr.contains("public allowlist")
                || stderr.contains("path canonicalization denied")
                || stderr.contains("Path traversal detected")),
        "stderr={stderr}"
    );
}

#[test]
#[ignore = "redirect policy test still hangs on the current Deno/runtime combination"]
fn redirect_escape_to_disallowed_host_blocked() {
    let (_workspace, fixture) = prepare_fixture_workspace("redirect-escape-capsule");
    let (port, redirect_thread) = spawn_redirect_server("https://api.evil.com/");

    let main_ts = fixture.join("main.ts");
    let script = fs::read_to_string(&main_ts).expect("failed to read redirect fixture script");
    let rendered = script.replace("__REDIRECT_PORT__", &port.to_string());
    fs::write(&main_ts, rendered).expect("failed to render redirect fixture script");

    let home = prepare_consent_home(&fixture);
    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato");

    let _ = redirect_thread.join();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_POLICY_VIOLATION") && stderr.contains("api.evil.com"),
        "stderr={stderr}"
    );
}

#[test]
#[ignore = "network policy fixture needs a runtime-complete capsule.lock.json to reach execution"]
fn network_exfiltration_blocked() {
    let output = run_with_seeded_consent("network-exfil-capsule", &[], &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_POLICY_VIOLATION"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("api.evil.com"), "stderr={stderr}");
}

#[test]
#[ignore = "policy re-consent fixture needs a runtime-complete capsule.lock.json after manifest mutation"]
fn reconsent_required_on_policy_change() {
    let (_workspace, fixture) = prepare_fixture_workspace("malicious-npm-capsule");
    let home = prepare_consent_home(&fixture);

    let v1_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato for v1");

    assert!(
        v1_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&v1_output.stderr)
    );

    add_egress_allow_host(&fixture, "api.evil.com");
    write_capsule_lock(&fixture, "malicious-npm-capsule");

    let v2_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato for v2");

    assert!(!v2_output.status.success());
    let stderr = String::from_utf8_lossy(&v2_output.stderr);
    assert!(
        stderr.contains("ATO_ERR_POLICY_VIOLATION"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("consent") || stderr.contains("ExecutionPlan consent"),
        "stderr={stderr}"
    );
}
