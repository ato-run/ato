//! What a v1 build says about the artifact it produced, and the two digests
//! that must never be swapped.
//!
//! # What a receipt is, and what it is not
//!
//! A v1 build mints an Execution Identity into `capsule.lock` and writes a
//! packed guest image beside it. The lock commits WHAT the execution is; it
//! deliberately says nothing about which file on disk holds it, because the
//! packed bytes are not reproducible (ADR-015 §9.2: `mke2fs` stamps every inode
//! it creates with the wall clock and ignores `SOURCE_DATE_EPOCH`, so two packs
//! of one identical tree differ in thousands of timestamp fields).
//!
//! The receipt is the missing half: it NAMES the file a build produced and
//! records its length and digest, so a later reader can tell whether the image
//! in front of it is the one the build reported.
//!
//! **A receipt is a producer's attestation, not a proof.** Verifying one
//! establishes that a lock, a receipt and a file on disk describe the same
//! build. It does NOT establish that the packed filesystem actually contains
//! the contents `filesystem.view_digest` commits — reading those back requires
//! unpacking or booting the image, which happens later in the pipeline. Code
//! consuming a receipt must not claim the stronger property.
//!
//! # The two digests
//!
//! ```text
//! filesystem.view_digest   IDENTITY        digest of the guest's CONTENTS
//!                                          reproducible; committed by the
//!                                          Execution Contract
//!
//! guest artifact digest    MATERIALIZATION digest of the packed ext4 BYTES
//!                                          NOT reproducible; names which file
//!                                          a build wrote; appears nowhere in
//!                                          the execution_id preimage
//! ```
//!
//! Both are a `ContentDigest`, so nothing but a name stops one being passed
//! where the other belongs — and swapping them is not a detectable mistake at
//! runtime: each is a well-formed digest, and the wrong one compares unequal
//! only when it happens to. Committing the materialization digest would make an
//! `apt upgrade` on a builder change every capsule's identity with no source
//! change; treating the view digest as the artifact's name would let a corrupt
//! image pass as the one the build wrote.
//!
//! [`GuestFilesystemViewDigest`] and [`GuestArtifactDigest`] therefore exist to
//! make the swap a compile error rather than a code-review responsibility.

use std::path::{Path, PathBuf};

use capsule::execution_contract::{
    ContentDigest, DigestAlgorithm, ExecutionContractV1, ResolvedTargetContract,
};
use serde::{Deserialize, Serialize};

/// The identity-bearing digest of the guest's CONTENTS.
///
/// Only constructible from an Execution Contract, because that is the only
/// place the value is ever authoritative. There is deliberately no constructor
/// taking a bare `ContentDigest`: one would re-open exactly the substitution
/// this type exists to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestFilesystemViewDigest(ContentDigest);

impl GuestFilesystemViewDigest {
    /// Read the committed view digest out of a contract.
    pub fn from_contract(contract: &ExecutionContractV1) -> Self {
        Self(contract.filesystem.view_digest)
    }

    pub fn as_content_digest(self) -> ContentDigest {
        self.0
    }
}

impl std::fmt::Display for GuestFilesystemViewDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// The materialization digest naming WHICH packed file a build produced.
///
/// Only constructible by measuring an artifact ([`measure_guest_artifact`]) or
/// by parsing one out of a receipt, because those are the only two places the
/// value legitimately comes from. It is never an identity input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestArtifactDigest(ContentDigest);

impl GuestArtifactDigest {
    /// Adopt a digest that was measured over packed artifact bytes.
    ///
    /// The caller asserts the provenance; [`measure_guest_artifact`] is the
    /// only production path that can honestly do so.
    pub fn measured(digest: ContentDigest) -> Self {
        Self(digest)
    }

    pub fn as_content_digest(self) -> ContentDigest {
        self.0
    }
}

impl std::fmt::Display for GuestArtifactDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Blake3 over a packed artifact, streamed.
///
/// A MATERIALIZATION measurement, never an identity input. Streamed because the
/// artifact is a filesystem image sized in gigabytes.
///
/// Both the producer (`ato build`) and any consumer verifying a receipt call
/// this, so the two cannot drift into measuring the same file differently.
pub fn measure_guest_artifact(path: &Path) -> std::io::Result<GuestArtifactDigest> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(GuestArtifactDigest(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    )))
}

/// The `os/architecture/abi` triple, as a receipt spells it.
///
/// Shared so the producer that writes a receipt and the consumer that checks
/// one cannot disagree about the spelling. `libc` is deliberately absent: it is
/// carried by the contract, and a receipt that repeated it would give two
/// places to disagree about one fact.
pub fn target_triple(target: &ResolvedTargetContract) -> String {
    format!("{}/{}/{}", target.os, target.architecture, target.abi)
}

/// What a v1 build produced, as the build itself reports it.
///
/// This is the `v1` object of `ato build --json` and the intake format a
/// Snapshot Builder consumes. Field names are the wire format; changing one is
/// a breaking change to both sides at once, which is the point of there being a
/// single definition.
///
/// Unknown fields are refused rather than ignored: a receipt written by a newer
/// producer may describe a build this consumer cannot reason about, and
/// silently dropping the field it disagreed about is the failure mode receipts
/// exist to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V1MaterializationReceipt {
    /// The Execution Identity the build minted, as `blake3:<64 hex>`.
    pub execution_id: String,
    /// Where the build published the lock that carries the contract.
    pub lock: PathBuf,
    /// Where the build wrote the packed guest image.
    pub guest_image: PathBuf,
    pub guest_image_bytes: u64,
    /// The packed artifact's digest — a materialization receipt naming WHICH
    /// file this build wrote. Not an identity input; see the module doc.
    pub guest_image_digest: String,
    /// The identity-bearing digest of the guest's contents, as
    /// `filesystem.view_digest` commits it.
    pub filesystem_view_digest: String,
    pub source_digest: String,
    /// The resolved runtime ref, digest-pinned (`name@sha256:...`).
    pub runtime: String,
    /// `os/architecture/abi`, as [`target_triple`] spells it.
    pub target: String,
    /// Whether the producer read its own lock back off disk and found the
    /// execution it minted.
    ///
    /// Always `true` from the v1 lane, which has no success path without the
    /// read-back agreeing. It is on the wire anyway so a consumer can refuse on
    /// seeing the claim absent rather than infer it from a missing field.
    pub trusted_load_verified: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn target(os: &str, arch: &str, abi: &str) -> ResolvedTargetContract {
        ResolvedTargetContract {
            os: os.to_string(),
            architecture: arch.to_string(),
            abi: abi.to_string(),
            libc: Some("glibc-2.39".to_string()),
            observable_features: Default::default(),
        }
    }

    /// The triple omits `libc`, so a receipt cannot disagree with the contract
    /// about it.
    #[test]
    fn the_target_triple_is_os_architecture_abi_and_nothing_else() {
        assert_eq!(
            target_triple(&target("linux", "amd64", "gnu")),
            "linux/amd64/gnu"
        );
    }

    /// The measurement is over the file's bytes, and is the same function on
    /// both sides of the boundary.
    #[test]
    fn measuring_an_artifact_digests_its_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rootfs.ext4");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"packed bytes")
            .expect("write");

        let measured = measure_guest_artifact(&path).expect("measure");

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"packed bytes");
        let expected = ContentDigest::new(DigestAlgorithm::Blake3, *hasher.finalize().as_bytes());
        assert_eq!(measured.as_content_digest(), expected);
    }

    /// Two packs of one guest differ here — that is the whole reason this value
    /// is not an identity input — so the measurement must reflect the bytes it
    /// was given, not the tree they came from.
    #[test]
    fn artifacts_with_different_bytes_measure_differently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.ext4");
        let b = dir.path().join("b.ext4");
        std::fs::write(&a, b"one").expect("write a");
        std::fs::write(&b, b"two").expect("write b");

        assert_ne!(
            measure_guest_artifact(&a).expect("measure a"),
            measure_guest_artifact(&b).expect("measure b")
        );
    }

    /// A receipt round-trips through the wire format unchanged.
    #[test]
    fn a_receipt_round_trips() {
        let receipt = V1MaterializationReceipt {
            execution_id: format!("blake3:{}", "a".repeat(64)),
            lock: PathBuf::from("/w/capsule.lock"),
            guest_image: PathBuf::from("/w/guest.ext4"),
            guest_image_bytes: 12,
            guest_image_digest: format!("blake3:{}", "b".repeat(64)),
            filesystem_view_digest: format!("blake3:{}", "c".repeat(64)),
            source_digest: format!("sha256:{}", "d".repeat(64)),
            runtime: format!("python@sha256:{}", "e".repeat(64)),
            target: "linux/amd64/gnu".to_string(),
            trusted_load_verified: true,
        };

        let encoded = serde_json::to_string(&receipt).expect("encode");
        let decoded: V1MaterializationReceipt = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, receipt);
    }

    /// A field this consumer does not know about is a refusal, never a silent
    /// drop: it may be the one field that says this build is not what the
    /// reader assumes.
    #[test]
    fn a_receipt_carrying_an_unknown_field_is_refused() {
        let json = r#"{
            "execution_id": "blake3:aaaa",
            "lock": "/w/capsule.lock",
            "guest_image": "/w/guest.ext4",
            "guest_image_bytes": 12,
            "guest_image_digest": "blake3:bbbb",
            "filesystem_view_digest": "blake3:cccc",
            "source_digest": "sha256:dddd",
            "runtime": "python@sha256:eeee",
            "target": "linux/amd64/gnu",
            "trusted_load_verified": true,
            "capture_mode": "something-this-build-does-not-understand"
        }"#;
        assert!(serde_json::from_str::<V1MaterializationReceipt>(json).is_err());
    }
}
