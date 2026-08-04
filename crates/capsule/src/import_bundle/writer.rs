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
//!
//! # Streaming
//!
//! [`write_capsule_bundle_v3_to`] is the real entry point: the source archive —
//! the one member the format sets no bound on — is hashed in a streaming pass and
//! then streamed into the outer TAR, so peak memory is a fixed buffer rather than
//! a function of untrusted input size. [`write_capsule_bundle_v3`] is a thin
//! convenience wrapper over it for callers that already hold both members in
//! memory.
//!
//! The manifest *is* buffered, deliberately: it is signed and digested
//! byte-for-byte, it is bounded by what a human writes, and it must be handed to
//! the digest and to the TAR as the identical byte sequence.
//!
//! # Header format
//!
//! Members are written with [`Header::new_ustar`]. RFC §"Container layout" calls
//! the outer container a deterministic USTAR-compatible TAR, and USTAR is
//! sufficient here because all four member paths are short, ASCII, and carry no
//! metadata a USTAR header cannot hold — no long-name or extended-attribute
//! record is ever emitted. `Header::new_gnu` would *not* satisfy that: it writes
//! the GNU magic/version pair (`"ustar "`/`" \0"`) rather than USTAR's
//! (`"ustar\0"`/`"00"`), so the bytes would contradict the contract even for
//! these simple entries. `writer_emits_ustar_magic_and_version` pins exactly
//! those bytes.

use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
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

/// What a completed write produced.
///
/// The bundle digest is the value a Store hands a client back as
/// `expected_bundle_digest`, so a writer that streamed its output into a file or
/// a socket still learns it without a second pass over the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsuleBundleWriteReceipt {
    /// SHA-256 over every byte written to the output.
    pub bundle_digest: Sha256Digest,
    /// How many bytes were written.
    pub bundle_size_bytes: u64,
    /// SHA-256 over the exact JCS `index.json` bytes that were signed.
    pub index_digest: Sha256Digest,
}

/// Build a complete v3 bundle in memory.
///
/// A thin convenience wrapper over [`write_capsule_bundle_v3_to`] for callers
/// that already hold both members as slices; the two produce byte-identical
/// output for the same input (pinned by
/// `streaming_and_buffered_writers_agree_byte_for_byte`).
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
    let mut output = Cursor::new(Vec::new());
    write_capsule_bundle_v3_to(
        &mut output,
        input.manifest_bytes,
        Cursor::new(input.source_archive_bytes),
        input.claimed_issuer,
        signer,
    )?;
    Ok(output.into_inner())
}

/// Stream a complete v3 bundle into `output`.
///
/// `source_archive` is read twice — once to measure and digest it for
/// `index.json`, once to copy it into the container — which is why it is
/// `Read + Seek` rather than `Read`. Both passes use a fixed buffer, so an
/// arbitrarily large source archive never becomes an arbitrarily large
/// allocation. `output` needs only [`Write`]: the TAR is emitted strictly
/// forward, and the digest is taken as the bytes go by rather than by seeking
/// back over them.
///
/// # Errors
///
/// [`CapsuleImportError`] on read, canonicalization, signing, or TAR-assembly
/// failure.
pub fn write_capsule_bundle_v3_to<W: Write, S: Read + Seek>(
    output: W,
    manifest: impl Read,
    mut source_archive: S,
    claimed_issuer: ClaimedIssuer,
    signer: &dyn CapsuleIndexSigner,
) -> Result<CapsuleBundleWriteReceipt, CapsuleImportError> {
    // The manifest is buffered on purpose: the same bytes must reach the digest
    // and the container, and it is bounded by what an author writes.
    let mut manifest_bytes = Vec::new();
    let mut manifest = manifest;
    manifest
        .read_to_end(&mut manifest_bytes)
        .map_err(|source| CapsuleImportError::io("read the outer manifest", source))?;

    // The source archive is not buffered: measured and digested in one streaming
    // pass, then rewound and streamed into the container.
    let (source_digest, source_size) = digest_stream(&mut source_archive)?;
    source_archive
        .seek(SeekFrom::Start(0))
        .map_err(|source| CapsuleImportError::io("rewind the source archive", source))?;

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
                sha256: Sha256Digest::of_bytes(&manifest_bytes),
                size_bytes: SizeBytes::of_measured(manifest_bytes.len() as u64),
            },
            IndexMember {
                role: MemberRole::Source,
                path: SOURCE_MEMBER_PATH.to_string(),
                media_type: SOURCE_MEDIA_TYPE.to_string(),
                sha256: source_digest,
                size_bytes: SizeBytes::of_measured(source_size),
            },
        ],
    };
    debug_assert!(
        MANIFEST_MEMBER_PATH.as_bytes() < SOURCE_MEMBER_PATH.as_bytes(),
        "the emitted member order must be ascending UTF-8 byte order of path"
    );

    let index_bytes = index.to_canonical_bytes()?;
    let index_digest = Sha256Digest::of_bytes(&index_bytes);
    let signature_bytes = build_signature_member(&index_bytes, claimed_issuer, signer)?;

    let mut builder = Builder::new(DigestingWriter {
        inner: output,
        hasher: Sha256::new(),
        written: 0,
    });
    // Ascending byte order, matching the index's own ordering rule so the
    // container and its manifest agree without a second convention.
    append_member(&mut builder, MANIFEST_MEMBER_PATH, &manifest_bytes)?;
    append_member(&mut builder, INDEX_MEMBER_PATH, &index_bytes)?;
    append_member(&mut builder, SIGNATURE_MEMBER_PATH, &signature_bytes)?;
    append_streamed_member(
        &mut builder,
        SOURCE_MEMBER_PATH,
        source_size,
        &mut source_archive,
    )?;
    builder
        .finish()
        .map_err(|source| CapsuleImportError::io("finish the outer v3 TAR", source))?;
    let digesting = builder
        .into_inner()
        .map_err(|source| CapsuleImportError::io("flush the outer v3 TAR", source))?;

    Ok(CapsuleBundleWriteReceipt {
        bundle_digest: Sha256Digest::from_raw(digesting.hasher.finalize().into()),
        bundle_size_bytes: digesting.written,
        index_digest,
    })
}

/// A `Write` that hashes and counts everything passing through it.
struct DigestingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> Write for DigestingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// SHA-256 and measure a stream from its current position, with a fixed buffer.
fn digest_stream<R: Read>(reader: &mut R) -> Result<(Sha256Digest, u64), CapsuleImportError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| CapsuleImportError::io("read the source archive", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as u64);
    }
    Ok((Sha256Digest::from_raw(hasher.finalize().into()), size))
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

fn append_member<W: Write>(
    builder: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
) -> Result<(), CapsuleImportError> {
    append_streamed_member(builder, path, bytes.len() as u64, bytes)
}

/// Append one member whose size is known but whose bytes arrive as a stream.
///
/// The size must be known up front because a TAR header precedes its data, which
/// is exactly why [`write_capsule_bundle_v3_to`] measures the source archive in
/// its own pass before writing anything.
fn append_streamed_member<W: Write, R: Read>(
    builder: &mut Builder<W>,
    path: &str,
    size: u64,
    data: R,
) -> Result<(), CapsuleImportError> {
    // USTAR, not GNU: see the module note on header format. All four member paths
    // fit a USTAR name field, so no long-name extension is ever emitted.
    let mut header = Header::new_ustar();
    header.set_entry_type(EntryType::Regular);
    header.set_size(size);
    header.set_mode(V3_MEMBER_MODE);
    header.set_mtime(V3_MEMBER_MTIME);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, data)
        .map_err(|source| CapsuleImportError::io(&format!("append outer member {path:?}"), source))
}
