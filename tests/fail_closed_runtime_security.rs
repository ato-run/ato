mod fail_closed_support;

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;

use fail_closed_support::*;

#[test]
#[cfg(unix)]
fn consent_store_permissions_are_hardened() {
    use std::os::unix::fs::PermissionsExt;

    let (_workspace, fixture) = prepare_fixture_workspace("network-exfil-capsule");
    let home = tempfile::TempDir::new().expect("failed to create temporary HOME");

    let output = ato_cmd()
        .arg("run")
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato");

    assert!(!output.status.success());

    let consent_dir = home.path().join(".ato").join("consent");
    let consent_file = consent_dir.join("executionplan_v1.jsonl");
    assert!(consent_dir.exists());
    assert!(consent_file.exists());

    let dir_mode = fs::metadata(&consent_dir).unwrap().permissions().mode() & 0o777;
    let file_mode = fs::metadata(&consent_file).unwrap().permissions().mode() & 0o777;

    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}

#[test]
#[ignore = "fixture needs a runtime-complete capsule.lock to execute the Deno runtime path"]
fn npm_lifecycle_isolation() {
    let pwn_target = PathBuf::from("/tmp/ato_pwned_test_6");
    let _ = fs::remove_file(&pwn_target);

    let output = run_with_seeded_consent("malicious-npm-capsule", &[], &[]);

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !pwn_target.exists(),
        "postinstall script unexpectedly executed"
    );
}

#[test]
#[ignore = "fixture needs a runtime-complete capsule.lock to execute the secret-fd path"]
fn secret_fd_injection_no_env_leak() {
    let output = run_with_seeded_consent(
        "env-dump-capsule",
        &[],
        &[("OPENAI_API_KEY", "sk-secret-do-not-leak")],
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("sk-secret-do-not-leak"), "stdout={stdout}");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "still requires structured policy target JSON output for the healing loop"]
fn self_healing_loop_recovers_from_policy_violation() {
    let (_workspace, fixture) = prepare_fixture_workspace("network-exfil-capsule");
    let (port, server_handle) = spawn_plain_http_server("heal-ok");

    let script = format!(
        "const response = await fetch(\"http://127.0.0.1:{}/heal\");\nconsole.log(await response.text());\n",
        port
    );
    fs::write(fixture.join("main.ts"), script).expect("failed to rewrite fixture script");

    let deny_home = prepare_consent_home(&fixture);
    let deny_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", deny_home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute deny phase");

    assert!(!deny_output.status.success());
    let deny_stderr = String::from_utf8_lossy(&deny_output.stderr);
    let target = extract_policy_violation_target(&deny_stderr)
        .expect("policy violation JSONL with target must be present");
    let host = normalize_host_from_target(&target);
    assert!(host == "127.0.0.1" || host == "localhost");

    add_egress_allow_host(&fixture, &host);
    write_capsule_lock(&fixture, "network-exfil-capsule");

    let healed_home = prepare_consent_home(&fixture);
    let healed_output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg(&fixture)
        .env("HOME", healed_home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute healed phase");

    let _ = server_handle.join();

    assert!(healed_output.status.success());
}
