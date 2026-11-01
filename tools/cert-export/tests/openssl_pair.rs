//! End-to-end checks against the host's real openssl.
//!
//! Fixtures are generated per test run into the crate's .tmp directory: no
//! private key is ever committed, and each certificate is valid relative to
//! the moment the test runs. When openssl is absent the tests report a skip
//! instead of silently passing.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ato_cert_export::allowlist::{self, Entry};
use ato_cert_export::cert::{CertInspector, OpensslCli};
use ato_cert_export::export::{self, Action, Thresholds};

fn openssl() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("ATO_CERT_EXPORT_OPENSSL") {
        return Some(PathBuf::from(configured));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join("openssl"))
            .find(|candidate| candidate.is_file())
    })
}

macro_rules! require_openssl {
    () => {
        match openssl() {
            Some(path) => path,
            None => {
                eprintln!("skipped: no openssl on PATH");
                return;
            }
        }
    };
}

fn workspace() -> tempfile::TempDir {
    std::fs::create_dir_all(".tmp").unwrap();
    tempfile::tempdir_in(".tmp").unwrap()
}

fn generate(root: &Path, tag: &str, domain: &str, days: u32) -> (PathBuf, PathBuf) {
    let openssl = openssl().expect("openssl");
    let certificate = root.join(format!("{tag}.crt"));
    let private_key = root.join(format!("{tag}.key"));
    let output = Command::new(openssl)
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-keyout",
        ])
        .arg(&private_key)
        .arg("-out")
        .arg(&certificate)
        .args(["-days", &days.to_string()])
        .args(["-subj", &format!("/CN={domain}")])
        .args(["-addext", &format!("subjectAltName=DNS:{domain}")])
        .output()
        .expect("run openssl req");
    assert!(
        output.status.success(),
        "openssl req failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (certificate, private_key)
}

fn entry(certificate: PathBuf, private_key: PathBuf, domain: &str) -> Entry {
    Entry {
        instance_id: "i_test".to_owned(),
        domain: domain.to_owned(),
        certificate,
        private_key,
        certificate_binding: "mail_cert_pem".to_owned(),
        private_key_binding: "mail_key_pem".to_owned(),
    }
}

fn write_allowlist(root: &Path, entry: &Entry) -> PathBuf {
    let path = root.join("allowlist.conf");
    std::fs::write(
        &path,
        format!(
            "allowlist = \"{}\"\n{} {} {} {} {} {}\n",
            allowlist::HEADER,
            entry.instance_id,
            entry.domain,
            entry.certificate.display(),
            entry.private_key.display(),
            entry.certificate_binding,
            entry.private_key_binding,
        ),
    )
    .unwrap();
    path
}

fn now() -> i64 {
    ato_cert_export::time::now_epoch_secs()
}

#[test]
fn a_real_pair_is_validated_and_exported_once() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let entry = entry(certificate, private_key, "mail.example.test");
    let inspector = OpensslCli::new(openssl, false).unwrap();
    let export_dir = root.path().join("export");

    let first = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &export_dir,
    )
    .unwrap();
    assert_eq!(first.action, Action::Created);
    assert!(
        first
            .meta
            .certificate
            .sans
            .contains(&"mail.example.test".to_owned())
    );
    assert_eq!(
        first.meta.certificate.serial.len(),
        first.meta.certificate.serial.trim().len()
    );
    assert_eq!(
        first.meta.certificate.serial,
        first.meta.certificate.serial.to_ascii_lowercase()
    );
    let meta_text = std::fs::read_to_string(export_dir.join("meta.json")).unwrap();
    assert!(!meta_text.contains("PRIVATE KEY"));
    assert!(meta_text.contains(&first.meta.certificate.serial));

    let second = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &export_dir,
    )
    .unwrap();
    assert_eq!(second.action, Action::Unchanged);
    assert!(!export_dir.join("cert.pem.prev").exists());
}

#[test]
fn a_renewed_pair_replaces_the_export_and_keeps_the_previous_one() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let entry = entry(
        certificate.clone(),
        private_key.clone(),
        "mail.example.test",
    );
    let inspector = OpensslCli::new(openssl.clone(), false).unwrap();
    let export_dir = root.path().join("export");
    let first = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &export_dir,
    )
    .unwrap();
    let old_certificate = std::fs::read(export_dir.join("cert.pem")).unwrap();

    // A renewal: Caddy writes a different serial into the same source paths.
    let (renewed_certificate, renewed_key) = generate(root.path(), "b", "mail.example.test", 90);
    std::fs::copy(&renewed_certificate, &certificate).unwrap();
    std::fs::copy(&renewed_key, &private_key).unwrap();
    let second = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &export_dir,
    )
    .unwrap();
    assert_eq!(second.action, Action::Updated);
    assert_ne!(
        second.meta.certificate.serial,
        first.meta.certificate.serial
    );
    assert_eq!(
        std::fs::read(export_dir.join("cert.pem.prev")).unwrap(),
        old_certificate
    );
    let previous = second.meta.previous.expect("previous record");
    assert_eq!(previous.serial, first.meta.certificate.serial);
    assert_eq!(
        std::fs::read(export_dir.join("cert.pem")).unwrap(),
        std::fs::read(&renewed_certificate).unwrap()
    );
}

#[test]
fn a_real_mismatched_key_is_rejected_without_writing() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, _) = generate(root.path(), "a", "mail.example.test", 90);
    let (_, other_key) = generate(root.path(), "b", "mail.example.test", 90);
    let entry = entry(certificate, other_key, "mail.example.test");
    let inspector = OpensslCli::new(openssl, false).unwrap();
    let export_dir = root.path().join("export");
    let error = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &export_dir,
    )
    .unwrap_err();
    assert_eq!(error.class(), ato_cert_export::ErrorClass::Validation);
    assert!(error.to_string().contains("does not match"), "{error}");
    assert!(!export_dir.exists());
}

#[test]
fn a_real_san_miss_is_rejected() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "other.example.test", 90);
    let entry = entry(certificate, private_key, "mail.example.test");
    let inspector = OpensslCli::new(openssl, false).unwrap();
    let error = export::export(
        &entry,
        Thresholds::default(),
        now(),
        &inspector,
        &root.path().join("export"),
    )
    .unwrap_err();
    assert_eq!(error.class(), ato_cert_export::ErrorClass::Validation);
    assert!(error.to_string().contains("SAN"), "{error}");
}

#[test]
fn a_real_pair_inside_the_remaining_threshold_is_rejected() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 2);
    let entry = entry(certificate, private_key, "mail.example.test");
    let inspector = OpensslCli::new(openssl, false).unwrap();
    let thresholds = Thresholds {
        min_remaining_hours: 240,
        ..Thresholds::default()
    };
    let error = export::export(
        &entry,
        thresholds,
        now(),
        &inspector,
        &root.path().join("export"),
    )
    .unwrap_err();
    assert_eq!(error.class(), ato_cert_export::ErrorClass::Validation);
    assert!(
        error.to_string().contains("expires within 240 hours"),
        "{error}"
    );
}

#[test]
fn an_encrypted_private_key_is_rejected_instead_of_prompting() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let encrypted = root.path().join("encrypted.key");
    let status = Command::new(&openssl)
        .args([
            "pkey",
            "-in",
            private_key.to_str().unwrap(),
            "-aes-256-cbc",
            "-passout",
            "pass:test-only",
            "-out",
            encrypted.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let entry = entry(certificate, encrypted, "mail.example.test");
    let inspector = OpensslCli::new(openssl, false).unwrap();
    let error = inspector
        .inspect_pair(&entry.certificate, &entry.private_key)
        .unwrap_err();
    assert!(format!("{error:#}").contains("private key"), "{error:#}");
}

fn run_cli(arguments: &[String]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ato-cert-export"))
        .args(arguments)
        .output()
        .expect("run ato-cert-export")
}

fn cli_arguments(allowlist: &Path, openssl: &Path, export_dir: &Path) -> Vec<String> {
    vec![
        "--allow-unprivileged".to_owned(),
        "--allowlist".to_owned(),
        allowlist.display().to_string(),
        "--instance".to_owned(),
        "i_test".to_owned(),
        "--export-dir".to_owned(),
        export_dir.display().to_string(),
        "--openssl".to_owned(),
        openssl.display().to_string(),
    ]
}

fn joined(first: Vec<String>, second: Vec<String>) -> Vec<String> {
    let mut all = first;
    all.extend(second);
    all
}

#[test]
fn the_cli_check_prints_a_plan_without_key_material() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let entry = entry(certificate, private_key, "mail.example.test");
    let allowlist = write_allowlist(root.path(), &entry);
    let export_dir = root.path().join("export");
    let arguments = joined(
        vec!["check".to_owned()],
        cli_arguments(&allowlist, &openssl, &export_dir),
    );
    let output = run_cli(&arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("BEGIN"), "{stdout}");
    let plan: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(plan["schema"], "ato.certificate-binding-plan/1");
    assert_eq!(plan["action"], "check");
    assert_eq!(plan["instance_id"], "i_test");
    assert_eq!(plan["domain"], "mail.example.test");
    assert!(!export_dir.exists(), "check must not write");
}

#[test]
fn the_cli_prints_the_binding_update_payload_it_was_asked_for() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let entry = entry(certificate, private_key, "mail.example.test");
    let allowlist = write_allowlist(root.path(), &entry);
    let export_dir = root.path().join("export");
    let arguments = joined(
        vec![
            "export".to_owned(),
            "--print-binding-update".to_owned(),
            "--grant-id".to_owned(),
            "opcg_01".to_owned(),
            "--expected-generation".to_owned(),
            "4".to_owned(),
        ],
        cli_arguments(&allowlist, &openssl, &export_dir),
    );
    let output = run_cli(&arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("never write it to a log"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout is the payload alone: {stdout}");
    let payload: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    // The journal-safe plan moved to stderr in this mode.
    let plan: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .expect("plan line on stderr");
    assert_eq!(plan["action"], "created");
    assert_eq!(payload["schema"], "ato.certificate-binding-update/1");
    assert_eq!(payload["expected_generation"], 4);
    assert_eq!(
        payload["path"],
        "/v1/operator/certificate-bindings/opcg_01/apply"
    );
    assert!(
        payload["values"]["mail_cert_pem"]
            .as_str()
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );
    assert!(
        payload["values"]["mail_key_pem"]
            .as_str()
            .unwrap()
            .contains("BEGIN PRIVATE KEY")
    );
    assert!(!stdout.contains("key_sha256"));
    assert_eq!(payload["evidence"]["serial"], plan["serial"]);
    assert_eq!(
        payload["evidence"]["certificate_sha256"],
        plan["certificate_sha256"]
    );
}

#[test]
fn the_cli_refuses_a_pair_inside_the_remaining_threshold() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 2);
    let entry = entry(certificate, private_key, "mail.example.test");
    let allowlist = write_allowlist(root.path(), &entry);
    let export_dir = root.path().join("export");
    let arguments = joined(
        vec![
            "export".to_owned(),
            "--min-remaining-hours".to_owned(),
            "240".to_owned(),
        ],
        cli_arguments(&allowlist, &openssl, &export_dir),
    );
    let output = run_cli(&arguments);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("expires within 240 hours"), "{stderr}");
    assert!(!export_dir.exists());
}

#[test]
fn the_cli_refuses_to_handle_a_key_without_root() {
    let openssl = require_openssl!();
    let root = workspace();
    let (certificate, private_key) = generate(root.path(), "a", "mail.example.test", 90);
    let entry = entry(certificate, private_key, "mail.example.test");
    let allowlist = write_allowlist(root.path(), &entry);
    let arguments = vec![
        "check".to_owned(),
        "--allowlist".to_owned(),
        allowlist.display().to_string(),
        "--instance".to_owned(),
        "i_test".to_owned(),
        "--openssl".to_owned(),
        openssl.display().to_string(),
        "--export-dir".to_owned(),
        root.path().join("export").display().to_string(),
    ];
    let output = run_cli(&arguments);
    if ato_cert_export::effective_uid() == Some(0) {
        assert!(output.status.success());
    } else {
        assert_eq!(output.status.code(), Some(4));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("privilege: "), "{stderr}");
        assert!(stderr.contains("--allow-unprivileged"), "{stderr}");
    }
}
