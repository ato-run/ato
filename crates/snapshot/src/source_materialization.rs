//! Turning a pinned commit into a frozen, re-fetchable source archive.
//!
//! ```text
//! PinnedCommit
//!   → SourceEligibilityVerified      nothing the v1 lane cannot carry
//!   → source_tree_digest             A1v2 over the projected tree
//!   → canonical archive              deterministic .tar.zst
//!   → round-trip verification        extract it back, re-digest, compare
//!   → content-addressed object key   derived from the ARCHIVE digest
//!   → SourceReceiptV1                what was resolved
//!   → SourceMaterializationReceiptV1 where it was stored
//! ```
//!
//! # Why the round trip
//!
//! The digest and the archive are produced by two walks of a live directory.
//! Between them the tree can change — a stray process, a leftover build, a slow
//! NFS mount — and the result would be a receipt naming a tree the archive does
//! not contain. Nothing downstream could detect that: the digest verifies, the
//! archive verifies, and they describe different trees.
//!
//! So the archive is extracted back and re-digested before anything is uploaded.
//! A mismatch is a refusal, not a warning. This is the "if that is difficult,
//! read the archive back and confirm the reconstructed tree digest matches"
//! branch of the requirement, chosen because it verifies the artifact that
//! actually ships rather than trusting that two walks agreed.
//!
//! # Why the object key comes from the archive digest
//!
//! The key names the BYTES that were stored, so a fetch either returns those
//! bytes or fails. Deriving it from the tree digest instead would make one key
//! ambiguous across archiver versions — two different byte sequences for one
//! tree, one key, and the fetcher cannot tell which it got.
//!
//! No temporary path, no builder-local path, and no caller-supplied URL is ever
//! recorded: those name a place rather than a content, and a place can change
//! under a stored reference.

use std::path::{Path, PathBuf};

use capsule::blob::{
    MaterializedSource, SourceMaterializeError, materialize_source_archive,
    materialized_source_tree_hash,
};

use crate::source_eligibility::{SourceIneligible, verify_source_eligibility};
use crate::source_receipt::{
    SOURCE_MATERIALIZATION_RECEIPT_V1_SCHEMA, SOURCE_RECEIPT_V1_SCHEMA,
    SourceMaterializationReceiptV1, SourceReceiptV1,
};

/// The archive format this lane writes. Part of the materialization receipt so
/// a reader knows how to open the bytes without guessing from the key.
pub const SOURCE_ARCHIVE_FORMAT_V1: &str = "ato.source-archive/v1";

/// What a materialization needs to know about the source it is freezing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedSource {
    pub provider: String,
    /// Server-canonicalized. Never the submitter's spelling, never credentialed.
    pub canonical_repository: String,
    pub commit_algorithm: String,
    /// Full provider form. The tree is read from THIS, not from `HEAD`.
    pub resolved_commit_sha: String,
    /// Which projection rules produced the tree digest.
    pub resolver_contract_version: String,
}

/// Everything a completed materialization produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedSourceOutcome {
    pub receipt: SourceReceiptV1,
    pub materialization: SourceMaterializationReceiptV1,
    /// Where the archive was written locally, for the uploader to read.
    pub archive_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceMaterializationError {
    /// The source is one the v1 lane cannot carry. Produces nothing.
    #[error("{0}")]
    Ineligible(#[from] SourceIneligible),
    #[error("the source could not be frozen: {0}")]
    Materialize(#[from] SourceMaterializeError),
    /// The archive does not reconstruct the tree it claims. See the module doc.
    #[error(
        "the archive does not reproduce the tree it names: archived {archived}, \
         reconstructed {reconstructed}"
    )]
    RoundTripMismatch {
        archived: String,
        reconstructed: String,
    },
    #[error("{stage}: {reason}")]
    Io { stage: &'static str, reason: String },
}

impl SourceMaterializationError {
    /// A stable code, so a client renders a message rather than parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Ineligible(inner) => inner.code(),
            Self::Materialize(_) => "SOURCE_MATERIALIZE_FAILED",
            Self::RoundTripMismatch { .. } => "SOURCE_ARCHIVE_ROUND_TRIP_MISMATCH",
            Self::Io { .. } => "SOURCE_MATERIALIZE_IO",
        }
    }
}

/// The content-addressed key an archive is stored under.
///
/// Derived from the ARCHIVE digest, which names the bytes. `<algo>:<hex>` is
/// split on the colon so the key has no colon in it — object stores differ on
/// whether that is legal, and a key that works on one and not another is a
/// portability problem discovered at the worst time.
pub fn object_key_for_archive(archive_digest: &str) -> Result<String, SourceMaterializationError> {
    let (algorithm, hex) =
        archive_digest
            .split_once(':')
            .ok_or_else(|| SourceMaterializationError::Io {
                stage: "object_key",
                reason: format!("archive digest {archive_digest} is not <algorithm>:<hex>"),
            })?;
    if algorithm.is_empty() || hex.is_empty() {
        return Err(SourceMaterializationError::Io {
            stage: "object_key",
            reason: format!("archive digest {archive_digest} has an empty half"),
        });
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SourceMaterializationError::Io {
            stage: "object_key",
            reason: "archive digest is not hex".to_string(),
        });
    }
    Ok(format!("source-archives/{algorithm}/{hex}"))
}

/// Freeze a pinned checkout into an archive, verifying the round trip.
///
/// Produces nothing on an ineligible source: the eligibility gate runs first,
/// and the archive path is not created until it passes.
pub fn materialize_pinned_source(
    checkout: &Path,
    pinned: &PinnedSource,
    archive_path: &Path,
) -> Result<MaterializedSourceOutcome, SourceMaterializationError> {
    // (1) Eligibility, from the COMMIT tree, before anything is written. An
    //     unexpanded submodule is invisible on the filesystem, so this cannot be
    //     folded into the archive walk below.
    verify_source_eligibility(checkout, &pinned.resolved_commit_sha)?;

    // (2) A1v2 admissibility + identity, then the deterministic archive.
    //     `materialize_source_archive` hashes BEFORE it writes a byte, so an
    //     inadmissible tree leaves no partial archive.
    let materialized: MaterializedSource = materialize_source_archive(checkout, archive_path)?;

    // (3) The round trip. Extract what was just written and re-derive the tree
    //     digest from the EXTRACTED bytes — not from the live directory, which
    //     is the thing that could have changed underneath.
    let reconstructed = reconstruct_tree_digest(archive_path)?;
    if reconstructed != materialized.materialized_source_tree_hash {
        // The archive is not what the receipt would have claimed. Remove it so a
        // later stage cannot pick up a file that failed verification.
        let _ = std::fs::remove_file(archive_path);
        return Err(SourceMaterializationError::RoundTripMismatch {
            archived: materialized.materialized_source_tree_hash,
            reconstructed,
        });
    }

    let object_key = object_key_for_archive(&materialized.source_archive_hash)?;

    Ok(MaterializedSourceOutcome {
        receipt: SourceReceiptV1 {
            canonical_repository: pinned.canonical_repository.clone(),
            commit_algorithm: pinned.commit_algorithm.clone(),
            provider: pinned.provider.clone(),
            resolved_commit_sha: pinned.resolved_commit_sha.clone(),
            resolver_contract_version: pinned.resolver_contract_version.clone(),
            schema: SOURCE_RECEIPT_V1_SCHEMA.to_string(),
            source_tree_digest: materialized.materialized_source_tree_hash.clone(),
        },
        materialization: SourceMaterializationReceiptV1 {
            archive_format_version: SOURCE_ARCHIVE_FORMAT_V1.to_string(),
            object_key,
            schema: SOURCE_MATERIALIZATION_RECEIPT_V1_SCHEMA.to_string(),
            size_bytes: materialized.compressed_bytes,
            source_archive_digest: materialized.source_archive_hash,
            source_tree_digest: materialized.materialized_source_tree_hash,
        },
        archive_path: archive_path.to_path_buf(),
    })
}

/// Extract an archive to a scratch directory and re-derive its tree digest.
fn reconstruct_tree_digest(archive: &Path) -> Result<String, SourceMaterializationError> {
    let scratch = tempfile::tempdir().map_err(|e| SourceMaterializationError::Io {
        stage: "round_trip",
        reason: format!("create the verification directory: {e}"),
    })?;
    capsule::program_source_projection::extract_source_archive(archive, scratch.path()).map_err(
        |e| SourceMaterializationError::Io {
            stage: "round_trip",
            reason: format!("extract the archive for verification: {e}"),
        },
    )?;
    materialized_source_tree_hash(scratch.path()).map_err(|e| SourceMaterializationError::Io {
        stage: "round_trip",
        reason: format!("re-digest the extracted tree: {e}"),
    })
}

/// Verify a fetched archive is the one a receipt names.
///
/// Both halves are checked, because they can fail independently: the BYTES can
/// be wrong (wrong object, truncated transfer, tampering) and the CONTENT can be
/// wrong (an archive of a different tree stored under a colliding key). A
/// consumer that checked only the archive digest would accept an archive whose
/// tree is not the one the identity receipt commits.
pub fn verify_fetched_archive(
    archive: &Path,
    expected_archive_digest: &str,
    expected_tree_digest: &str,
) -> Result<(), SourceMaterializationError> {
    let bytes = std::fs::read(archive).map_err(|e| SourceMaterializationError::Io {
        stage: "verify_archive",
        reason: format!("read the fetched archive: {e}"),
    })?;
    let actual = capsule::blob::source_archive_hash(&bytes);
    if actual != expected_archive_digest {
        return Err(SourceMaterializationError::RoundTripMismatch {
            archived: expected_archive_digest.to_string(),
            reconstructed: actual,
        });
    }
    let reconstructed = reconstruct_tree_digest(archive)?;
    if reconstructed != expected_tree_digest {
        return Err(SourceMaterializationError::RoundTripMismatch {
            archived: expected_tree_digest.to_string(),
            reconstructed,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn repo(dir: &Path) -> String {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.invalid"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("app.py"), "print('hi')\n").unwrap();
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/util.py"), "X = 1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", "init"]);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn pinned(sha: &str) -> PinnedSource {
        PinnedSource {
            provider: "github".to_string(),
            canonical_repository: "https://github.com/acme/menuflow".to_string(),
            commit_algorithm: "sha1".to_string(),
            resolved_commit_sha: sha.to_string(),
            resolver_contract_version: "ato.capsule-program-source-projection/v1".to_string(),
        }
    }

    #[test]
    fn a_pinned_source_materializes_into_a_verified_archive() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let out_dir = tempfile::tempdir().unwrap();
        let archive = out_dir.path().join("source.tar.zst");

        let outcome =
            materialize_pinned_source(dir.path(), &pinned(&sha), &archive).expect("materialize");

        assert!(archive.exists(), "the archive was written");
        assert_eq!(outcome.receipt.resolved_commit_sha, sha);
        assert!(outcome.receipt.source_tree_digest.starts_with("sha256:"));
        // The two receipts name ONE tree.
        assert_eq!(
            outcome.materialization.source_tree_digest,
            outcome.receipt.source_tree_digest
        );
        assert!(outcome.materialization.size_bytes > 0);
        // The key names the ARCHIVE bytes, and carries no colon.
        let hex = outcome
            .materialization
            .source_archive_digest
            .split(':')
            .nth(1)
            .unwrap();
        assert_eq!(
            outcome.materialization.object_key,
            format!("source-archives/sha256/{hex}")
        );
        assert!(!outcome.materialization.object_key.contains(':'));
    }

    /// The archive digest is NOT the tree digest — they name different things.
    #[test]
    fn the_archive_digest_and_the_tree_digest_are_distinct() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let out_dir = tempfile::tempdir().unwrap();
        let outcome =
            materialize_pinned_source(dir.path(), &pinned(&sha), &out_dir.path().join("s.tar.zst"))
                .expect("materialize");
        assert_ne!(
            outcome.materialization.source_archive_digest,
            outcome.materialization.source_tree_digest,
            "the packed bytes and the tree they contain are different values"
        );
    }

    /// The same source materializes to the same tree digest twice.
    #[test]
    fn materializing_twice_yields_the_same_tree_digest() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = materialize_pinned_source(dir.path(), &pinned(&sha), &a.path().join("s.tar.zst"))
            .unwrap();
        let two = materialize_pinned_source(dir.path(), &pinned(&sha), &b.path().join("s.tar.zst"))
            .unwrap();
        assert_eq!(
            one.receipt.source_tree_digest,
            two.receipt.source_tree_digest
        );
        assert_eq!(one.receipt.digest(), two.receipt.digest());
    }

    /// Changing the source changes the tree digest.
    #[test]
    fn changing_the_source_changes_the_tree_digest() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let a = tempfile::tempdir().unwrap();
        let before =
            materialize_pinned_source(dir.path(), &pinned(&sha), &a.path().join("s.tar.zst"))
                .unwrap();

        fs::write(dir.path().join("app.py"), "print('changed')\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "change"]);
        let sha2 = {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let b = tempfile::tempdir().unwrap();
        let after =
            materialize_pinned_source(dir.path(), &pinned(&sha2), &b.path().join("s.tar.zst"))
                .unwrap();

        assert_ne!(
            before.receipt.source_tree_digest,
            after.receipt.source_tree_digest
        );
    }

    /// An ineligible source produces NO archive.
    #[test]
    fn an_ineligible_source_writes_no_archive() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0000000000000000000000000000000000000001,vendor/dep",
            ],
        );
        git(dir.path(), &["commit", "-qm", "gitlink"]);
        let sha = {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let out_dir = tempfile::tempdir().unwrap();
        let archive = out_dir.path().join("source.tar.zst");

        let err =
            materialize_pinned_source(dir.path(), &pinned(&sha), &archive).expect_err("refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_SUBMODULE");
        assert!(
            !archive.exists(),
            "an ineligible source must leave no archive"
        );
        assert!(
            fs::read_dir(out_dir.path()).unwrap().next().is_none(),
            "an ineligible source must leave the output directory empty"
        );
    }

    // ── fetched-archive verification ────────────────────────────────────────

    #[test]
    fn a_faithful_archive_verifies() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let out_dir = tempfile::tempdir().unwrap();
        let archive = out_dir.path().join("s.tar.zst");
        let outcome = materialize_pinned_source(dir.path(), &pinned(&sha), &archive).unwrap();

        verify_fetched_archive(
            &archive,
            &outcome.materialization.source_archive_digest,
            &outcome.receipt.source_tree_digest,
        )
        .expect("a faithful archive verifies");
    }

    /// Tampered bytes are refused.
    ///
    /// The case an object store cannot rule out on its own: the key is right,
    /// the object is there, and the bytes are not the ones that were stored.
    #[test]
    fn tampered_archive_bytes_are_refused() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sha = repo(dir.path());
        let out_dir = tempfile::tempdir().unwrap();
        let archive = out_dir.path().join("s.tar.zst");
        let outcome = materialize_pinned_source(dir.path(), &pinned(&sha), &archive).unwrap();

        let mut bytes = fs::read(&archive).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        fs::write(&archive, &bytes).unwrap();

        let err = verify_fetched_archive(
            &archive,
            &outcome.materialization.source_archive_digest,
            &outcome.receipt.source_tree_digest,
        )
        .expect_err("tampered bytes must be refused");
        assert_eq!(err.code(), "SOURCE_ARCHIVE_ROUND_TRIP_MISMATCH");
    }

    /// An archive of a DIFFERENT tree is refused even when its own bytes hash
    /// correctly — the two checks are independent and both are needed.
    #[test]
    fn an_archive_of_another_tree_is_refused_even_though_its_bytes_are_intact() {
        if !git_available() {
            return;
        }
        let a_dir = tempfile::tempdir().unwrap();
        let a_sha = repo(a_dir.path());
        let b_dir = tempfile::tempdir().unwrap();
        let b_sha = repo(b_dir.path());
        fs::write(b_dir.path().join("extra.py").as_path(), "Y = 2\n").unwrap();
        git(b_dir.path(), &["add", "-A"]);
        git(b_dir.path(), &["commit", "-qm", "extra"]);
        let b_sha2 = {
            let out = Command::new("git")
                .arg("-C")
                .arg(b_dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let _ = (a_sha.clone(), b_sha);

        let oa = tempfile::tempdir().unwrap();
        let ob = tempfile::tempdir().unwrap();
        let arch_a = oa.path().join("a.tar.zst");
        let arch_b = ob.path().join("b.tar.zst");
        let out_a = materialize_pinned_source(a_dir.path(), &pinned(&a_sha), &arch_a).unwrap();
        let out_b = materialize_pinned_source(b_dir.path(), &pinned(&b_sha2), &arch_b).unwrap();
        assert_ne!(
            out_a.receipt.source_tree_digest,
            out_b.receipt.source_tree_digest
        );

        // Archive B, with B's own (correct) archive digest, but A's tree digest.
        let err = verify_fetched_archive(
            &arch_b,
            &out_b.materialization.source_archive_digest,
            &out_a.receipt.source_tree_digest,
        )
        .expect_err("an archive of another tree must be refused");
        assert_eq!(err.code(), "SOURCE_ARCHIVE_ROUND_TRIP_MISMATCH");
    }

    #[test]
    fn an_object_key_is_derived_from_the_archive_digest() {
        assert_eq!(
            object_key_for_archive(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            format!("source-archives/sha256/{}", "a".repeat(64))
        );
        assert!(object_key_for_archive("nocolon").is_err());
        assert!(object_key_for_archive("sha256:").is_err());
        assert!(object_key_for_archive("sha256:nothex!!").is_err());
    }
}
