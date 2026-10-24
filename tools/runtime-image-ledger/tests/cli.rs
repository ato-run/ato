//! End-to-end CLI contract: exit codes, rule codes, and report content.
//!
//! The binary is exercised exactly as `fixtures/README.md` documents, so a
//! fixture that stops producing its rule code fails here.

use std::path::PathBuf;
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_ato-runtime-image-ledger");

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn run(arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .args(arguments)
        .output()
        .expect("the ledger binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("the process exited normally")
}

/// Which record a finding must name.
enum Subject {
    Inventory,
    Patch(usize),
    Image(usize),
}

struct NegativeCase {
    fixture: &'static str,
    code: &'static str,
    field: &'static str,
    subject: Subject,
}

const NEGATIVE_CASES: &[NegativeCase] = &[
    NegativeCase {
        fixture: "invalid-digest.json",
        code: "image_digest_invalid",
        field: "images[0].image_digest",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "duplicate-digest.json",
        code: "image_digest_duplicate",
        field: "images[1].image_digest",
        subject: Subject::Image(1),
    },
    NegativeCase {
        fixture: "invalid-base-digest.json",
        code: "base_digest_invalid",
        field: "images[0].base_digests[0]",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "invalid-sbom-digest.json",
        code: "sbom_digest_invalid",
        field: "images[0].sbom.digest",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "runtime-version-range.json",
        code: "runtime_version_not_exact",
        field: "images[0].runtime_versions.node",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "invalid-scan-time.json",
        code: "scan_checked_at_invalid",
        field: "images[0].scan.checked_at",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "verified-without-receipt.json",
        code: "receipt_required",
        field: "images[0].receipt",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "receipt-not-fully-satisfied.json",
        code: "receipt_not_fully_satisfied",
        field: "images[0].receipt.fully_satisfied",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "receipt-derivation-mismatch.json",
        code: "receipt_derivation_mismatch",
        field: "images[0].receipt.derivation_ref",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "receipt-contract-mismatch.json",
        code: "receipt_contract_mismatch",
        field: "images[0].receipt.contract_ref",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "waiver-missing.json",
        code: "waiver_required",
        field: "images[0].findings_waiver",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "waiver-expired.json",
        code: "waiver_expired",
        field: "images[0].findings_waiver.expires_at",
        subject: Subject::Image(0),
    },
    NegativeCase {
        fixture: "unknown-patch-reference.json",
        code: "patch_reference_unknown",
        field: "patch_candidates[0].from_digest",
        subject: Subject::Patch(0),
    },
    NegativeCase {
        fixture: "patch-self-reference.json",
        code: "patch_self_reference",
        field: "patch_candidates[0].to_digest",
        subject: Subject::Patch(0),
    },
    NegativeCase {
        fixture: "patch-contract-mismatch.json",
        code: "patch_contract_mismatch",
        field: "patch_candidates[0].rebuild.contract_ref",
        subject: Subject::Patch(0),
    },
    NegativeCase {
        fixture: "auto-build-not-satisfied.json",
        code: "patch_auto_build_not_satisfied",
        field: "patch_candidates[0].rebuild.fully_satisfied",
        subject: Subject::Patch(0),
    },
    NegativeCase {
        fixture: "advisories-empty.json",
        code: "patch_advisories_empty",
        field: "patch_candidates[0].advisories",
        subject: Subject::Patch(0),
    },
    NegativeCase {
        fixture: "schema-unsupported.json",
        code: "schema_unsupported",
        field: "schema",
        subject: Subject::Inventory,
    },
];

impl Subject {
    fn resolve(&self, fixture: &PathBuf) -> String {
        match self {
            Subject::Inventory => "inventory".to_owned(),
            Subject::Patch(index) => format!("patch_candidates[{index}]"),
            Subject::Image(index) => {
                let bytes = std::fs::read(fixture).expect("fixture is readable");
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).expect("fixture is JSON");
                value["images"][*index]["image_digest"]
                    .as_str()
                    .expect("fixture image digest")
                    .to_owned()
            }
        }
    }
}

#[test]
fn valid_inventory_exits_zero() {
    let path = fixture_path("valid-inventory.json");
    let output = run(&["validate", path.to_str().expect("utf-8 path")]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty(), "stderr: {}", stderr(&output));
    let stdout = stdout(&output);
    assert!(
        stdout.contains("valid: 2 image(s), 1 patch candidate(s)"),
        "stdout: {stdout}"
    );
}

#[test]
fn every_negative_fixture_exits_one_with_its_rule_code() {
    for case in NEGATIVE_CASES {
        let path = fixture_path(case.fixture);
        let output = run(&["validate", path.to_str().expect("utf-8 path")]);
        assert_eq!(
            exit_code(&output),
            1,
            "{}: stdout={} stderr={}",
            case.fixture,
            stdout(&output),
            stderr(&output)
        );
        let subject = case.subject.resolve(&path);
        let expected_prefix = format!("{}: {}: {}:", case.code, subject, case.field);
        let error = stderr(&output);
        assert!(
            error.contains(&expected_prefix),
            "{}: expected {expected_prefix:?} in stderr:\n{error}",
            case.fixture
        );
        assert!(
            !stdout(&output).contains("valid:"),
            "{} must not print a valid line: {}",
            case.fixture,
            stdout(&output)
        );
    }
}

#[test]
fn malformed_and_unknown_fields_are_invalid_but_not_usage_errors() {
    for fixture in ["malformed-json.json", "unknown-field.json"] {
        let path = fixture_path(fixture);
        let output = run(&["validate", path.to_str().expect("utf-8 path")]);
        assert_eq!(exit_code(&output), 1, "{fixture}");
        let error = stderr(&output);
        assert!(
            error.contains("inventory_json_invalid: inventory: document:"),
            "{fixture}: {error}"
        );
    }
}

#[test]
fn report_shows_missing_evidence_as_missing() {
    let path = fixture_path("verified-without-receipt.json");
    let output = run(&["report", path.to_str().expect("utf-8 path")]);
    assert_eq!(exit_code(&output), 1);
    let stdout = stdout(&output);
    assert!(stdout.contains("IMAGES (1)"), "stdout: {stdout}");
    assert!(stdout.contains("verified"), "stdout: {stdout}");
    assert!(stdout.contains("missing"), "stdout: {stdout}");
    assert!(!stdout.contains("PASS"), "stdout: {stdout}");
    assert!(stderr(&output).contains("receipt_required"));
}

#[test]
fn report_json_is_machine_readable() {
    let path = fixture_path("valid-inventory.json");
    let output = run(&["report", path.to_str().expect("utf-8 path"), "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("report --json is JSON");
    assert_eq!(value["schema"], "ato.runtime-image-inventory/1");
    assert_eq!(value["valid"], true);
    assert_eq!(value["images"].as_array().expect("images").len(), 2);
    assert_eq!(
        value["patch_candidates"][0]["decision"], "auto_build",
        "patch decisions are reported"
    );
    assert!(
        value["inventory_sha256"]
            .as_str()
            .expect("inventory sha")
            .starts_with("sha256:")
    );
    assert_eq!(value["findings"].as_array().expect("findings").len(), 0);
}

#[test]
fn quiet_suppresses_stdout_but_not_findings() {
    let valid = fixture_path("valid-inventory.json");
    let output = run(&["validate", valid.to_str().expect("utf-8 path"), "--quiet"]);
    assert_eq!(exit_code(&output), 0);
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));

    let invalid = fixture_path("invalid-digest.json");
    let output = run(&["validate", invalid.to_str().expect("utf-8 path"), "--quiet"]);
    assert_eq!(exit_code(&output), 1);
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert!(stderr(&output).contains("image_digest_invalid"));
}

#[test]
fn usage_errors_exit_two() {
    let no_arguments = run(&[]);
    assert_eq!(exit_code(&no_arguments), 2);
    assert!(stderr(&no_arguments).contains("usage error:"));

    let unknown_subcommand = run(&["inspect", "whatever.json"]);
    assert_eq!(exit_code(&unknown_subcommand), 2);

    let missing_path = run(&["validate"]);
    assert_eq!(exit_code(&missing_path), 2);

    let unreadable = run(&["validate", "/nonexistent/inventory.json"]);
    assert_eq!(exit_code(&unreadable), 2);
    assert!(stderr(&unreadable).contains("cannot read inventory"));

    let json_on_validate = run(&["validate", "a.json", "--json"]);
    assert_eq!(exit_code(&json_on_validate), 2);

    let help = run(&["--help"]);
    assert_eq!(exit_code(&help), 0);
    assert!(stdout(&help).contains("does not scan, sign, or verify"));
}
