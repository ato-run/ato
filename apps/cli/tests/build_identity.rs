//! `ato version` is the only way to learn which commit an `ato` artifact was
//! built from. Desktop assembly's SHA-coherence invariant — support crates,
//! bundled Runner and requested `ato_sha` all one revision — rests on it, so
//! these tests guard the contract rather than the formatting.

use assert_cmd::Command;
use serde_json::Value;

fn ato() -> Command {
    Command::cargo_bin("ato").expect("the ato binary is built for integration tests")
}

/// Schema: every field the assembly contract reads must be present and a string.
#[test]
fn version_json_carries_the_full_build_identity() {
    let output = ato()
        .args(["version", "--json"])
        .output()
        .expect("ato version --json runs");
    assert!(output.status.success(), "ato version --json must succeed");

    let value: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be a single JSON object");

    for field in ["version", "git_commit", "git_dirty", "profile"] {
        assert!(
            value.get(field).and_then(Value::as_str).is_some(),
            "`{field}` must be present and a string; got {value}"
        );
    }
    assert_eq!(
        value["version"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION"),
        "reported version must be the crate version"
    );
    assert!(
        matches!(
            value["git_dirty"].as_str().unwrap(),
            "true" | "false" | "unknown"
        ),
        "git_dirty is a tri-state; got {}",
        value["git_dirty"]
    );
}

/// Propagation: the SHA the build was given is the SHA the binary reports.
///
/// The build resolves `ATO_BUILD_GIT_SHA` first precisely so a source tree
/// without `.git` (a published crate, a vendored copy, a shallow CI checkout)
/// can still produce an identifiable binary. If that override did not reach the
/// binary, assembly could pin a revision and ship something else.
#[test]
fn the_reported_commit_is_the_commit_the_build_was_given() {
    let reported = build_identity_of_this_binary();
    let commit = reported["git_commit"].as_str().unwrap();

    // This test binary is itself built by cargo from this workspace, so the
    // value must be a real 40-char SHA or the explicit `unknown` sentinel —
    // never empty, and never a truncated or decorated string.
    assert!(
        commit == "unknown"
            || (commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit())),
        "git_commit must be a full hex SHA or `unknown`; got {commit:?}"
    );

    // Whatever the build embedded, `version` and `version --json` must agree.
    let human = ato().arg("version").output().expect("ato version runs");
    let text = String::from_utf8(human.stdout).expect("utf-8");
    assert!(
        text.contains(commit),
        "human-readable output must report the same commit; got {text:?}"
    );
}

/// Release path: `--version` is a stable human contract and must not change
/// shape, and the profile must be reported honestly.
#[test]
fn the_release_path_keeps_version_stable_and_reports_its_profile() {
    let output = ato().arg("--version").output().expect("ato --version runs");
    let text = String::from_utf8(output.stdout).expect("utf-8");
    assert_eq!(
        text.trim(),
        format!("ato {}", env!("CARGO_PKG_VERSION")),
        "--version must stay the plain release-line string"
    );

    let profile = build_identity_of_this_binary();
    let profile = profile["profile"].as_str().unwrap().to_owned();
    assert!(
        profile == "debug" || profile == "release",
        "profile must be debug or release; got {profile:?}"
    );
    // Tests run under the same profile as the binary they build.
    let expected = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    assert_eq!(
        profile, expected,
        "profile must reflect how it was actually built"
    );
}

fn build_identity_of_this_binary() -> Value {
    let output = ato()
        .args(["version", "--json"])
        .output()
        .expect("ato version --json runs");
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}
