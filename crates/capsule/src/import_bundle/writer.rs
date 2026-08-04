//! The v3 writer: a deterministic, source-only bundle.
//!
//! Determinism is a hard requirement, not a nicety: the whole-bundle SHA-256 is
//! what a Store hands a client as `expected_bundle_digest`, so the same inputs
//! and the same signer must produce byte-identical output. Everything that could
//! vary is pinned — member order, header type, mode, mtime, uid/gid, and the JCS
//! encoding of both JSON members.
//!
//! The source archive bytes are **reused verbatim**. Re-compressing them would
//! produce a different `source.tar.zst` for the same logical source (zstd output
//! depends on the encoder version and level), which would move the bundle digest
//! for a bundle nobody changed — and, worse, would mean the archive a Store
//! already holds in `source_materializations` could not be shipped as-is.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use tar::{Builder, EntryType, Header};

use super::CapsuleImportError;
use super::index::{
    CapsuleIndexV1, INDEX_SCHEMA, IndexMember, MANIFEST_MEDIA_TYPE, MANIFEST_MEMBER_PATH,
    MemberRole, SOURCE_MEDIA_TYPE, SOURCE_MEMBER_PATH, Sha256Digest, SizeBytes,
};
use super::reader::{INDEX_MEMBER_PATH, SIGNATURE_MEMBER_PATH};
use super::signature::{
    CapsuleIndexSignatureV1, ClaimedIssuer, Ed25519SignatureBytes, SIGNATURE_SCHEMA,
    signing_message,
};
use super::trust::CapsuleIndexSigner;

/// The fixed mtime every member header carries.
///
/// A real timestamp would make two otherwise identical bundles differ, so there
/// is deliberately no `SOURCE_DATE_EPOCH` escape hatch here: the v3 bundle
/// digest is an identity, and an environment variable must not be able to move
/// it.
const V3_MEMBER_MTIME: u64 = 0;

/// The fixed mode every member header carries. No v3 outer member is executable.
const V3_MEMBER_MODE: u32 = 0o644;

/// The bytes a v3 bundle is made of.
///
/// The manifest arrives as bytes rather than a parsed model on purpose: the
/// outer `capsule.toml` is signed and digested byte-for-byte, so a re-serialized
/// normalization of it would be a different member than the one the author
/// wrote.
#[derive(Debug, Clone)]
pub struct CapsuleBundleWriteInput<'a> {
    /// The outer, authoritative `capsule.toml`.
    pub manifest_bytes: &'a [u8],
    /// An existing `ato.source-archive/v1` `.tar.zst`, written verbatim.
    pub source_archive_bytes: &'a [u8],
    /// The signer's own claim about itself. Display-only downstream; it never
    /// influences the trust a reader assigns.
    pub claimed_issuer: ClaimedIssuer,
}

impl<'a> CapsuleBundleWriteInput<'a> {
    /// Read the source archive from disk and pair it with manifest bytes.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::Io`] if the archive cannot be read.
    pub fn read_source_archive(path: &Path) -> Result<Vec<u8>, CapsuleImportError> {
        fs::read(path).map_err(|source| CapsuleImportError::io("read the source archive", source))
    }
}

/// Build a complete v3 bundle in memory.
///
/// Guarantees, each of which has a test: exactly four outer members;
/// deterministic member order and headers; `index.json` written as its exact JCS
/// canonicalization; `signature.json` matching the strict schema and verifying
/// over that index; source archive bytes untouched; and the same input plus the
/// same signer producing byte-identical output.
///
/// # Errors
///
/// [`CapsuleImportError`] on canonicalization, signing, or TAR-assembly failure.
pub fn write_capsule_bundle_v3(
    input: &CapsuleBundleWriteInput<'_>,
    signer: &dyn CapsuleIndexSigner,
) -> Result<Vec<u8>, CapsuleImportError> {
    // `members` is emitted in ascending UTF-8 byte order of `path`, which the
    // format fixes (JCS canonicalizes object keys but never reorders arrays).
    // "capsule.toml" < "source.tar.zst", so this literal order IS that order —
    // asserted below rather than assumed.
    let index = CapsuleIndexV1 {
        schema: INDEX_SCHEMA.to_string(),
        members: vec![
            IndexMember {
                role: MemberRole::Manifest,
                path: MANIFEST_MEMBER_PATH.to_string(),
                media_type: MANIFEST_MEDIA_TYPE.to_string(),
                sha256: Sha256Digest::of_bytes(input.manifest_bytes),
                size_bytes: SizeBytes::of_measured(input.manifest_bytes.len() as u64),
            },
            IndexMember {
                role: MemberRole::Source,
                path: SOURCE_MEMBER_PATH.to_string(),
                media_type: SOURCE_MEDIA_TYPE.to_string(),
                sha256: Sha256Digest::of_bytes(input.source_archive_bytes),
                size_bytes: SizeBytes::of_measured(input.source_archive_bytes.len() as u64),
            },
        ],
    };
    debug_assert!(
        MANIFEST_MEMBER_PATH.as_bytes() < SOURCE_MEMBER_PATH.as_bytes(),
        "the emitted member order must be ascending UTF-8 byte order of path"
    );

    let index_bytes = index.to_canonical_bytes()?;
    let signature_bytes = build_signature_member(&index_bytes, input.claimed_issuer, signer)?;

    let mut tar_bytes = Vec::new();
    {
        let mut builder = Builder::new(Cursor::new(&mut tar_bytes));
        // Ascending byte order, matching the index's own ordering rule so the
        // container and its manifest agree without a second convention.
        append_member(&mut builder, MANIFEST_MEMBER_PATH, input.manifest_bytes)?;
        append_member(&mut builder, INDEX_MEMBER_PATH, &index_bytes)?;
        append_member(&mut builder, SIGNATURE_MEMBER_PATH, &signature_bytes)?;
        append_member(&mut builder, SOURCE_MEMBER_PATH, input.source_archive_bytes)?;
        builder
            .finish()
            .map_err(|source| CapsuleImportError::io("finish the outer v3 TAR", source))?;
    }
    Ok(tar_bytes)
}

fn build_signature_member(
    index_bytes: &[u8],
    claimed_issuer: ClaimedIssuer,
    signer: &dyn CapsuleIndexSigner,
) -> Result<Vec<u8>, CapsuleImportError> {
    let raw = signer
        .sign(&signing_message(index_bytes))
        .map_err(|reason| {
            CapsuleImportError::signature(format!("signer refused to sign index.json: {reason}"))
        })?;
    let signature = CapsuleIndexSignatureV1 {
        schema: SIGNATURE_SCHEMA.to_string(),
        algorithm: "ed25519".to_string(),
        key_id: signer.key_id().clone(),
        claimed_issuer,
        index_digest: Sha256Digest::of_bytes(index_bytes),
        signature: Ed25519SignatureBytes::from_raw(raw),
    };
    // Written as JCS too. The format only *requires* it of index.json, but a
    // canonical signature.json means a bundle has one byte spelling end to end,
    // which is what makes the whole-bundle digest reproducible.
    signature.to_canonical_bytes()
}

fn append_member<W: std::io::Write>(
    builder: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
) -> Result<(), CapsuleImportError> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(V3_MEMBER_MODE);
    header.set_mtime(V3_MEMBER_MTIME);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, bytes)
        .map_err(|source| CapsuleImportError::io(&format!("append outer member {path:?}"), source))
}
