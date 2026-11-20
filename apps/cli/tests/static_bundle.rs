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

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn v1_rejects_binary_media_and_v2_carries_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    copy_dir(&sample(), dir.path());
    fs::write(
        dir.path().join("audio.ogg"),
        [0x4f, 0x67, 0x67, 0x53, 0x00, 0x01],
    )
    .unwrap();
    let out_v1 = dir.path().join("v1.capsule");
    let v1 = ato()
        .args([
            "__static-bundle",
            dir.path().to_str().unwrap(),
            "--title",
            "v1",
            "-o",
            out_v1.to_str().unwrap(),
        ])
        .output()
        .expect("v1 run");
    assert!(!v1.status.success(), "v1 tree must reject audio.ogg");
    assert!(String::from_utf8_lossy(&v1.stderr).contains("unsupported media type"));
    let out_v2 = dir.path().join("v2.capsule");
    let v2 = ato()
        .args([
            "__static-bundle",
            dir.path().to_str().unwrap(),
            "--title",
            "v2",
            "-o",
            out_v2.to_str().unwrap(),
            "--tree-schema",
            "v2",
        ])
        .output()
        .expect("v2 run");
    assert!(
        v2.status.success(),
        "v2 tree must accept audio.ogg: {}",
        String::from_utf8_lossy(&v2.stderr)
    );
    assert!(out_v2.exists());
}

#[cfg(unix)]
#[test]
fn local_formation_prints_a_working_static_preview_command() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command as ProcessCommand, Stdio};
    use std::time::Duration;

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("ato-home");
    let source = root.path().join("source");
    let destination = root.path().join("formed site's output");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("index.html"),
        "<!doctype html><h1>Formed site</h1>",
    )
    .unwrap();
    fs::write(source.join("style.css"), "h1 { color: blue }").unwrap();
    let formed = ato()
        .env("ATO_HOME", &home)
        .arg("form")
        .arg(&source)
        .arg("--out")
        .arg(&destination)
        .output()
        .unwrap();
    let summary = String::from_utf8_lossy(&formed.stderr);
    assert!(formed.status.success(), "{summary}");
    let result: serde_json::Value = serde_json::from_slice(&formed.stdout).unwrap();
    assert_eq!(result["status"], "formed");
    let command = summary
        .lines()
        .find_map(|line| line.strip_prefix("Preview it with: "))
        .unwrap();
    let mut preview = ProcessCommand::new("/bin/sh")
        .env("ATO_HOME", &home)
        .env(
            "PATH",
            Path::new(env!("CARGO_BIN_EXE_ato")).parent().unwrap(),
        )
        .args(["-c", &format!("exec {command} --no-open")])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = preview.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let output = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(output);
    });
    let result = (|| -> anyhow::Result<_> {
        let line = receive.recv_timeout(Duration::from_secs(10))??;
        let url = line
            .trim()
            .strip_prefix("Preview: ")
            .ok_or_else(|| anyhow::anyhow!("missing preview URL: {line:?}"))?;
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?;
        let html = client.get(url).send()?.error_for_status()?.text()?;
        let css = client
            .get(format!("{url}/style.css"))
            .send()?
            .error_for_status()?
            .text()?;
        Ok((url.to_owned(), html, css))
    })();
    // SAFETY: send SIGTERM to the specific child started above.
    unsafe { libc::kill(preview.id().try_into().unwrap(), libc::SIGTERM) };
    assert!(preview.wait().unwrap().success());
    let (url, html, css) = result.unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(html.contains("Formed site"), "{html}");
    assert_eq!(css, "h1 { color: blue }");

    let bundles = destination.join("bundles");
    let artifact = fs::read_dir(bundles)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let blob = fs::read_dir(artifact.join("blobs/sha256"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(blob, "tampered").unwrap();
    let refused = ato()
        .env("ATO_HOME", home)
        .args(["form-preview", "--no-open"])
        .arg(artifact)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("blob digest/size mismatch"));
    assert!(refused.stdout.is_empty());
}

#[test]
fn local_formation_reports_missing_source_requirements_on_stderr() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("empty");
    fs::create_dir(&source).unwrap();
    let formed = ato()
        .arg("form")
        .arg(source)
        .arg("--out")
        .arg(root.path().join("output"))
        .args(["--max-attempts", "1", "--json"])
        .output()
        .unwrap();
    assert!(!formed.status.success());
    let result: serde_json::Value = serde_json::from_slice(&formed.stdout).unwrap();
    assert_eq!(result["status"], "no_verified_route");
    assert!(String::from_utf8_lossy(&formed.stderr).contains("index.html"));
}
