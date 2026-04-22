use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "until-pythonprovisioner-v0.5.x"]
fn host_python_leakage_uses_managed_python_instead_of_host_python() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("docker_host_python_leakage_e2e.sh");

    let output = Command::new("bash")
        .arg(&script)
        .output()
        .expect("failed to launch host-python-leakage Docker E2E script");

    assert!(
        output.status.success(),
        "stdout=\n{}\n\nstderr=\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}