//! Capsule Bundle Format **v3** — the source-only import bundle.
//!
//! Normative specification: `docs/rfcs/draft/CAPSULE_FORMAT_V3.md`. This module
//! is Slice 1 of that RFC: the reader, verifier, writer, and trust model for a
//! `.capsule` container whose outer PAX TAR holds exactly four members —
//!
//! ```text
//! .capsule (v3)
//! ├── index.json        # ato.capsule-index/v1 — exact member manifest
//! ├── signature.json    # ato.capsule-index-signature/v1 — signs index.json
//! ├── capsule.toml      # outer, AUTHORITATIVE manifest
//! └── source.tar.zst    # existing ato.source-archive/v1 encoding, verbatim
//! ```
//!
//! # What this module is not
//!
//! It never *executes* a bundle. It hands back an [`ImportedCapsuleWorkspace`]
//! and stops; wiring that workspace into `resolve_authoritative_input` /
//! `install_local_directory` / `InstallRevisionFinalizer` is a later slice.
//!
//! It also does not re-implement anything the v2 pipeline already owns:
//!
//! * source-archive decoding and admissibility come from
//!   [`crate::program_source_projection::extract_source_archive`];
//! * `capsule_program_id` comes from the single existing mint,
//!   [`VerifiedPinnedSourceMaterialization::from_source_archive`](crate::program_source_projection::VerifiedPinnedSourceMaterialization::from_source_archive)
//!   plus [`derive_capsule_program_contract`](crate::capsule_program_contract::derive_capsule_program_contract);
//! * `did:key` encoding/decoding comes from [`crate::types::identity`];
//! * `crates/capsule/src/packers/capsule.rs` (the v2 writer) is not modified,
//!   and no v2 shape is re-enumerated here — see [`reader::BundleFormat`].
//!
//! # Proof-carrying flow
//!
//! Raw paths and unverified bytes never leave this module. The only way to a
//! workspace is:
//!
//! ```text
//! verify_capsule_envelope(..)  -> VerifiedCapsuleEnvelope
//! derive_imported_capsule(..)  -> VerifiedCapsuleImport
//! VerifiedCapsuleImport::into_workspace() -> ImportedCapsuleWorkspace
//! ```
//!
//! Every one of those types has private fields and no public constructor, so a
//! caller cannot self-attest a "verified" value it did not earn.

mod index;
mod policy;
mod reader;
mod signature;
mod trust;
mod verify;
mod writer;

#[cfg(test)]
mod tests;

pub use index::{
    CapsuleIndexV1, INDEX_SCHEMA, IndexMember, MANIFEST_MEDIA_TYPE, MANIFEST_MEMBER_PATH,
    MemberRole, SOURCE_MEDIA_TYPE, SOURCE_MEMBER_PATH, Sha256Digest, SizeBytes, parse_index_json,
};
pub use policy::CapsuleImportPolicy;
pub use reader::{
    BundleFormat, INDEX_MEMBER_PATH, SIGNATURE_MEMBER_PATH, V3_OUTER_MEMBER_PATHS,
    classify_bundle_format,
};
pub use signature::{
    CapsuleIndexSignatureV1, ClaimedIssuer, DidKey, Ed25519SignatureBytes, SIGNATURE_DOMAIN_TAG,
    SIGNATURE_SCHEMA, parse_signature_json, signing_message,
};
pub use trust::{
    CapsuleImportContext, CapsuleIndexSigner, CapsuleTrustPolicy, EphemeralLocalSigner,
    MAX_PINNED_KEYS_PER_ORIGIN, NormalizedOrigin, PinnedStoreOrigin, SignerTrust,
};
pub use verify::{
    ImportedCapsuleWorkspace, VerifiedCapsuleEnvelope, VerifiedCapsuleImport,
    derive_imported_capsule, verify_capsule_envelope,
};
pub use writer::{CapsuleBundleWriteInput, write_capsule_bundle_v3};

/// Why an import was refused.
///
/// The categories are deliberately not collapsed into one another: RFC
/// §"Resource policy" requires that "this worker cannot process it right now"
/// stay distinguishable from "the bundle itself is wrong", because only the
/// latter means the artifact is permanently bad.
#[derive(Debug, thiserror::Error)]
pub enum CapsuleImportError {
    /// Structural / schema / digest violation — the bundle itself is wrong.
    #[error("capsule bundle is invalid: {0}")]
    CapsuleInvalid(String),

    /// `signature.json` is absent, unparseable, or does not verify.
    #[error("capsule bundle signature is invalid: {0}")]
    SignatureInvalid(String),

    /// The whole-bundle SHA-256 does not match the digest the caller expected.
    ///
    /// Distinct from [`Self::CapsuleInvalid`]: the bundle may be perfectly
    /// well-formed and simply not be the bundle that was asked for.
    #[error("capsule bundle digest mismatch: expected {expected}, got {actual}")]
    BundleDigestMismatch {
        /// The digest the caller asserted, in `sha256:<hex>` form.
        expected: String,
        /// The digest of the bytes actually presented.
        actual: String,
    },

    /// [`CapsuleImportPolicy`] refused the work — not a statement about the
    /// bundle's validity.
    #[error("capsule import exceeded this importer's resource budget: {0}")]
    ResourceBudgetExceeded(String),

    /// The local device does not have room for the import.
    #[error("insufficient local storage for capsule import: {0}")]
    InsufficientLocalStorage(String),

    /// The signature is valid but the signer carries no trust the caller's
    /// policy accepts on this path.
    #[error("capsule bundle signer is not trusted: {0}")]
    UntrustedSigner(String),

    /// The archive has no root `index.json`, so it is not a v3 bundle at all.
    ///
    /// This is a **dispatch signal, not a rejection**: per RFC §"v2 / v3
    /// dispatch", the caller must hand the same bytes to the existing v2 reader
    /// (`crates/cli/src/utils/archive.rs`), whose validity rules are its own.
    /// It is emphatically not [`Self::CapsuleInvalid`].
    #[error(
        "archive has no root index.json, so it is not a v3 capsule bundle; \
         dispatch it to the existing v2 reader"
    )]
    NotV3Bundle,

    /// An I/O failure while reading or staging.
    #[error("capsule import I/O failure: {0}")]
    Io(String),
}

impl CapsuleImportError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::CapsuleInvalid(message.into())
    }

    pub(crate) fn signature(message: impl Into<String>) -> Self {
        Self::SignatureInvalid(message.into())
    }

    pub(crate) fn io(action: &str, source: std::io::Error) -> Self {
        Self::Io(format!("failed to {action}: {source}"))
    }
}
