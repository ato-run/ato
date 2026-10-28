//! `ato __static-bundle` exposes the existing no-build static packer.
//!
//! The packer is a library function (`ato_portable_application::build_static_bundle`);
//! these tests guard the thin CLI surface only: it must consume the current
//! `ato.capsule/1` static route grammar, produce a deterministic wire v3
//! bundle, and refuse inputs the packer would refuse.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn ato() -> Command {
    Command::cargo_bin("ato").expect("the ato binary is built for integration tests")
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
}

#[test]
fn packs_an_authored_static_route_without_a_build() {
    let output_dir = tempfile::tempdir().expect("temporary output directory");
    let output = output_dir.path().join("static.capsule");
    let run = ato()
        .args([
            "__static-bundle",
            sample().to_str().unwrap(),
            "--title",
            "Ato portability proof",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("ato __static-bundle runs");
    assert!(
        run.status.success(),
        "packing a static route must succeed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("bundle_sha256=sha256:"), "{stdout}");
    assert!(stdout.contains("contract_ref=sha256:"), "{stdout}");
    assert!(stdout.contains("derivation_ref=sha256:"), "{stdout}");
    let bytes = fs::read(&output).expect("bundle file exists");
    assert!(
        bytes.starts_with(b"{\"index\":") || bytes.starts_with(b"{\"index\": {"),
        "bundle must be a JSON envelope"
    );
}

#[test]
fn the_bundle_is_deterministic_for_the_same_tree_and_title() {
    let first = tempfile::tempdir().expect("first output directory");
    let second = tempfile::tempdir().expect("second output directory");
    let mut hashes = Vec::new();
    for dir in [&first, &second] {
        let output = dir.path().join("static.capsule");
        let run = ato()
            .args([
                "__static-bundle",
                sample().to_str().unwrap(),
                "--title",
                "Ato portability proof",
                "-o",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("ato __static-bundle runs");
        assert!(run.status.success());
        let stdout = String::from_utf8_lossy(&run.stdout);
        let line = stdout
            .lines()
            .find(|line| line.starts_with("bundle_sha256="))
            .expect("bundle_sha256 line");
        hashes.push(line.to_owned());
    }
    assert_eq!(
        hashes[0], hashes[1],
        "same tree and title must pack identically"
    );
}

#[test]
fn refuses_an_empty_title() {
    let output_dir = tempfile::tempdir().expect("temporary output directory");
    let output = output_dir.path().join("static.capsule");
    let status = ato()
        .args([
            "__static-bundle",
            sample().to_str().unwrap(),
            "--title",
            "   ",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("ato __static-bundle runs");
    assert!(!status.status.success(), "empty title must be refused");
    assert!(!output.exists(), "no bundle may be written on refusal");
}

#[test]
fn refuses_a_directory_without_an_authoring_file() {
    let source = tempfile::tempdir().expect("empty source directory");
    let output_dir = tempfile::tempdir().expect("temporary output directory");
    let output = output_dir.path().join("static.capsule");
    let status = ato()
        .args([
            "__static-bundle",
            source.path().to_str().unwrap(),
            "--title",
            "No manifest",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("ato __static-bundle runs");
    assert!(
        !status.status.success(),
        "a source without capsule.toml must be refused"
    );
}
