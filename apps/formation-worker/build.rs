//! Stamp the build with what produced it.
//!
//! A deployed worker used to carry no identity at all. When one refused a
//! source for a reason that no code on `main` could produce, answering "which
//! revision is this?" meant hashing the binary, grepping it for strings, and
//! searching history for a matching message — and the source tree beside it
//! turned out not to be what it was built from, so the obvious check was
//! actively misleading.
//!
//! So the answer is compiled in. The one rule that makes it worth trusting:
//! **a build that cannot determine something says `unknown`.** It never
//! borrows a plausible value. A binary that claims a commit it was not built
//! from is worse than one that admits it does not know, because the first
//! ends an investigation with the wrong answer.
//!
//! `dirty` is tracked separately from the commit for the same reason: a commit
//! hash alone would describe a tree that was edited before `cargo build`, and
//! that is exactly how a local experiment gets mistaken for a release.

use std::process::Command;

fn main() {
    // Re-stamp when HEAD moves or the index changes. Without this, a rebuild
    // after a checkout keeps the previous commit — a stale identity, which is
    // the failure mode this whole file exists to prevent.
    for path in ["../../.git/HEAD", "../../.git/index"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-env-changed=ATO_BUILD_ID");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    println!("cargo:rustc-env=ATO_BUILD_COMMIT={}", commit());
    println!("cargo:rustc-env=ATO_BUILD_DIRTY={}", dirty());
    println!("cargo:rustc-env=ATO_BUILD_ID={}", build_id());
    println!("cargo:rustc-env=ATO_BUILD_RUSTC={}", rustc());
}

/// The commit, or `unknown` — never a guess.
fn commit() -> String {
    git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
}

/// `clean`, `dirty`, or `unknown` when git could not be asked.
///
/// Three values rather than a bool: "not dirty" and "we could not tell" are
/// different claims, and collapsing them is how an unverifiable build starts
/// looking verified.
fn dirty() -> String {
    match git(&["status", "--porcelain"]) {
        None => "unknown".to_owned(),
        Some(output) if output.trim().is_empty() => "clean".to_owned(),
        Some(_) => "dirty".to_owned(),
    }
}

/// Whatever the build system calls this build, when it says so.
///
/// A CI run id, a release tag — supplied through `ATO_BUILD_ID`. Absent for a
/// developer build, which is the honest answer there.
fn build_id() -> String {
    std::env::var("ATO_BUILD_ID").unwrap_or_else(|_| "unknown".to_owned())
}

fn rustc() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}
