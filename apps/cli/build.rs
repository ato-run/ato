//! Embeds the build's git identity into the binary.
//!
//! `ato --version` alone reports the crate version (`0.7.9`), which every build
//! of a release line shares. That makes "which commit is this binary" —
//! the question the Desktop assembly's SHA-coherence invariant is built on —
//! unanswerable from the artifact. This resolves the commit at BUILD time and
//! bakes it in, so the runtime never shells out to `git`.
//!
//! Resolution order:
//!
//! 1. `ATO_BUILD_GIT_SHA` — an explicit override. This is the path that works
//!    where no `.git` exists: a published crate, a vendored source tree, a CI
//!    job that checked out with `--depth 1` into a detached worktree, or a
//!    packaging step that already knows the SHA it asked for.
//! 2. `git rev-parse HEAD` in the manifest directory.
//! 3. `unknown`.
//!
//! Reaching (3) is legal for a developer's `cargo build` and MUST NOT be legal
//! for a release or a Desktop assembly. Set `ATO_BUILD_REQUIRE_GIT_SHA=1` and
//! the build fails instead of silently producing an unidentifiable binary —
//! "fail closed" belongs at the build, not at the consumer that later cannot
//! tell what it got.

use std::process::Command;

const SHA_ENV: &str = "ATO_BUILD_GIT_SHA";
const REQUIRE_ENV: &str = "ATO_BUILD_REQUIRE_GIT_SHA";
const UNKNOWN: &str = "unknown";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={SHA_ENV}");
    println!("cargo:rerun-if-env-changed={REQUIRE_ENV}");
    // Rebuild when HEAD moves, so a binary never reports a stale commit.
    for path in [".git/HEAD", "../../.git/HEAD"] {
        println!("cargo:rerun-if-changed={path}");
    }

    let (sha, dirty) = resolve();
    let required = std::env::var(REQUIRE_ENV).is_ok_and(|v| v == "1" || v == "true");
    if required && sha == UNKNOWN {
        panic!(
            "{REQUIRE_ENV} is set but the build git commit could not be resolved. \
             Set {SHA_ENV} explicitly, or build from a git checkout."
        );
    }

    println!("cargo:rustc-env=ATO_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=ATO_BUILD_GIT_DIRTY={dirty}");
}

/// `(sha, dirty)`. `dirty` is "true"/"false"/"unknown" — a tri-state on
/// purpose, because "we could not tell" is not the same claim as "clean".
fn resolve() -> (String, &'static str) {
    if let Ok(sha) = std::env::var(SHA_ENV) {
        let sha = sha.trim().to_owned();
        if !sha.is_empty() {
            // An override says nothing about the working tree it came from.
            return (sha, "unknown");
        }
    }
    let Some(sha) = git(&["rev-parse", "HEAD"]) else {
        return (UNKNOWN.to_owned(), "unknown");
    };
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(out) if out.is_empty() => "false",
        Some(_) => "true",
        None => "unknown",
    };
    (sha, dirty)
}

fn git(args: &[&str]) -> Option<String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git")
        .args(args)
        .current_dir(manifest)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}
