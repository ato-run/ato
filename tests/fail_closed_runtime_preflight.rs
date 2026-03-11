mod fail_closed_support;

use std::process::Stdio;

use fail_closed_support::*;
use tempfile::TempDir;

#[test]
fn deno_lock_missing_fail_closed() {
    let output = run_with_seeded_consent("deno-lock-missing-capsule", &[], &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("deno.lock"), "stderr={stderr}");
}

#[test]
fn native_python_uv_lock_missing_fail_closed() {
    let output = run_with_seeded_consent(
        "native-python-no-uv-lock-capsule",
        &["--unsafe-bypass-sandbox"],
        &[],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("uv.lock"), "stderr={stderr}");
}

#[test]
#[ignore = "current runtime manager fails earlier on missing tools.uv before binary lookup"]
fn native_python_uv_binary_missing_fail_closed() {
    let output = run_with_seeded_consent(
        "native-python-with-uv-lock-capsule",
        &["--unsafe-bypass-sandbox"],
        &[("PATH", "")],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_PROVISIONING_LOCK_INCOMPLETE"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("uv CLI") || stderr.contains("uv run --offline"),
        "stderr={stderr}"
    );
}

#[test]
#[cfg(unix)]
#[ignore = "current runtime manager fails earlier on missing tools.uv before sandbox capability probing"]
fn native_sandbox_unavailable_fail_closed_even_with_unsafe_flag() {
    let (_workspace, fixture) = prepare_fixture_workspace("native-sandbox-unavailable-capsule");
    let home = prepare_consent_home(&fixture);
    let nacelle_dir = TempDir::new().expect("failed to create temp dir for mock binaries");
    let nacelle_path = nacelle_dir.path().join("nacelle");
    let uv_path = nacelle_dir.path().join("uv");
    write_mock_nacelle_without_sandbox(&nacelle_path);
    write_mock_uv(&uv_path);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--unsafe-bypass-sandbox")
        .arg(&fixture)
        .env("HOME", home.path())
        .env("NACELLE_PATH", &nacelle_path)
        .env("PATH", nacelle_dir.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ato");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_COMPAT_HARDWARE"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("sandbox") && stderr.contains("not available"),
        "stderr={stderr}"
    );
}

#[test]
fn glibc_preflight_rejection() {
    let output = run_with_seeded_consent("future-glibc-capsule", &["--unsafe-bypass-sandbox"], &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_COMPAT_HARDWARE"),
        "stderr={stderr}"
    );
    assert!(
        stderr.to_ascii_lowercase().contains("glibc"),
        "stderr={stderr}"
    );
}

#[test]
fn elf_overrides_lock_preflight() {
    let output =
        run_with_seeded_consent("glibc-mismatch-capsule", &["--unsafe-bypass-sandbox"], &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ATO_ERR_COMPAT_HARDWARE"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("2.99"), "stderr={stderr}");
}
