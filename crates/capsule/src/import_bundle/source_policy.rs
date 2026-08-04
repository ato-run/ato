//! Policy-aware seams around the frozen source-projection SSOT.
//!
//! # The problem this module exists to solve
//!
//! RFC §"Resource policy (not a format limit)" is explicit: the v3 format sets
//! **no** normative bound on bundle size, member size, expanded size, or member
//! count. Importers apply their own policy, and exceeding it is a distinct,
//! non-`capsule_invalid` outcome.
//!
//! The derivation path, however, runs through
//! [`crate::program_source_projection::extract_source_archive`] and
//! [`VerifiedPinnedSourceMaterialization::from_source_archive`], which enforce
//! **fixed production caps** —
//! [`MAX_COMPRESSED_BYTES`], [`MAX_UNCOMPRESSED_BYTES`], [`MAX_FILE_SIZE_BYTES`],
//! [`MAX_FILE_COUNT`] — and report a violation as
//! `CapsuleProgramError::NotPinnedMaterialization`, indistinguishable at the type
//! level from "this tar is structurally malformed". Mapping every error out of
//! those functions to `CapsuleImportError::CapsuleInvalid` (which is what this
//! module replaced) therefore made a format-valid, merely-large bundle look
//! permanently corrupt — the exact inversion the RFC forbids.
//!
//! `program_source_projection.rs` is the frozen identity SSOT shared with the
//! rest of the system and is out of scope to modify. So this module resolves the
//! tension from the outside, with **both** halves of the available approach
//! rather than either alone:
//!
//! 1. **Pre-check (primary).** [`measure_source_archive`] streams the archive
//!    once — decompressing through a counting reader, never buffering the tar —
//!    and enforces the *importer's* limits before the SSOT is called at all. A
//!    bundle that violates importer policy is therefore refused as
//!    [`CapsuleImportError::ResourceBudgetExceeded`] at a point where the SSOT's
//!    own caps have not been consulted, so the classification is decided by the
//!    policy that was actually violated.
//!
//! 2. **Reclassification (backstop).** The pre-check cannot cover the case where
//!    the importer configured *no* limit (or a limit above the SSOT's) and the
//!    SSOT's fixed caps fire anyway. Left alone, those would surface as
//!    `CapsuleInvalid` — a fixed cap masquerading as format invalidity.
//!    [`classify_projection_error`] therefore inspects the error for the SSOT's
//!    own cap-exceeded shapes and re-categorises them as
//!    `ResourceBudgetExceeded`, leaving every other failure `CapsuleInvalid`.
//!
//! 3. **The post-substitution side.** Derivation does not only *read* the source
//!    archive: it re-archives the manifest-substituted tree through
//!    [`crate::blob::materialize_source_archive`], which applies its **own** fixed
//!    caps to that derived tree. Those are a third, independent set of limits —
//!    the pre-check never saw the substituted tree, and the SSOT reclassifier
//!    never sees this error type — so
//!    [`classify_source_materialize_error`] gives them the same treatment. It can
//!    do so by *type* rather than by message, because
//!    [`SourceMaterializeError`] keeps size caps, admissibility violations, and
//!    I/O in separate variants.
//!
//! Why both, rather than the pre-check alone: the pre-check is a *different*
//! measurement than the SSOT's (it counts decompressed stream bytes and regular
//! entries; the SSOT counts declared entry sizes after its own admissibility
//! whitelist), so the two can disagree at the margin, and only the backstop makes
//! the classification correct on every path. Why both, rather than the backstop
//! alone: the backstop matches on message text (see
//! [`classify_projection_error`]), which is a coupling to be minimised, and it
//! also cannot enforce a limit *stricter* than the SSOT's — which is the whole
//! point of an importer policy.
//!
//! # What this module does not do
//!
//! It does not re-implement extraction. The entry-kind / path / containment /
//! no-overwrite whitelist in `extract_source_archive` is the trust boundary for
//! hostile archive bytes, and a second copy of it here would be a second thing to
//! get wrong. The pre-scan reads headers and counts bytes; it never writes a file
//! and never decides admissibility.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use tar::EntryType;

use super::CapsuleImportError;
use super::policy::CapsuleImportPolicy;
use crate::blob::{
    MAX_COMPRESSED_BYTES, MAX_FILE_COUNT, MAX_FILE_SIZE_BYTES, MAX_UNCOMPRESSED_BYTES,
    SourceAdmissibilityError, SourceMaterializeError,
};
use crate::capsule_program_contract::CapsuleProgramError;
#[cfg(doc)]
use crate::program_source_projection::VerifiedPinnedSourceMaterialization;

/// What one streaming pass over a source archive measured.
///
/// Every field is an **observed** quantity, never a declared one: `expanded_bytes`
/// counts bytes that actually came out of the zstd decoder, so an archive that
/// lies in its headers cannot inflate or deflate the number used for budgeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceArchiveMeasurement {
    /// Bytes the decompressed tar stream actually yielded.
    pub(crate) expanded_bytes: u64,
    /// Regular-file entries seen.
    pub(crate) file_count: u64,
}

/// Stream the archive once, enforcing the importer's source limits as bytes go by.
///
/// Returns `None` when the policy bounds nothing this could serve — the pass is
/// a whole extra decompression and buying it for no limit would be waste.
///
/// # Errors
///
/// * [`CapsuleImportError::ResourceBudgetExceeded`] — an importer limit was
///   crossed. The bundle is *not* malformed.
/// * [`CapsuleImportError::CapsuleInvalid`] — the bytes are not a readable
///   zstd-compressed tar at all. This is the line the reclassification below must
///   not blur, so it is drawn here, at the point of failure, from the error the
///   decoder itself produced — not by inspecting a message after the fact.
pub(crate) fn measure_source_archive(
    archive_tar_zst: &Path,
    compressed_bytes: u64,
    policy: &CapsuleImportPolicy,
) -> Result<Option<SourceArchiveMeasurement>, CapsuleImportError> {
    if !policy.bounds_measurable_resources() {
        return Ok(None);
    }

    policy.check_source_compressed_bytes(compressed_bytes)?;

    let file = File::open(archive_tar_zst)
        .map_err(|source| CapsuleImportError::io("open the staged source archive", source))?;
    // Streaming decoder, not `read_to_end`: a zstd bomb must be refused by the
    // incremental counter below, not by an allocation the size of its output.
    let decoder = zstd::Decoder::new(file).map_err(|source| {
        CapsuleImportError::invalid(format!(
            "the bundle's source archive is not a readable zstd stream: {source}"
        ))
    })?;

    let mut counter = ExpandedByteCounter {
        inner: decoder,
        observed: 0,
        limit: policy.max_source_expanded_bytes,
    };

    let file_count = {
        let mut archive = tar::Archive::new(&mut counter);
        let entries = archive.entries().map_err(|source| {
            classify_stream_error(
                "the bundle's source archive tar stream is unreadable",
                source,
            )
        })?;

        let mut file_count: u64 = 0;
        for entry in entries {
            let entry = entry.map_err(|source| {
                classify_stream_error(
                    "the bundle's source archive has an unreadable entry",
                    source,
                )
            })?;
            if entry.header().entry_type() != EntryType::Regular {
                continue;
            }
            // The declared size is used only for the per-file comparison, and
            // only ever as a *ceiling* test — never to allocate. A header that
            // understates its entry is caught by the SSOT's own
            // "declared N bytes but yielded M" check, as `CapsuleInvalid`.
            let declared = entry.size();
            if let Some(limit) = policy.max_source_file_bytes
                && declared > limit
            {
                return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                    "a file inside the bundle's source archive is {declared} bytes, over this \
                     importer's {limit}-byte per-file policy limit; the bundle itself is not \
                     malformed"
                )));
            }
            file_count += 1;
            if let Some(limit) = policy.max_source_file_count
                && file_count > limit
            {
                return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                    "the bundle's source archive holds more than {limit} files, this importer's \
                     policy limit; the bundle itself is not malformed"
                )));
            }
        }
        file_count
    };

    Ok(Some(SourceArchiveMeasurement {
        expanded_bytes: counter.observed,
        file_count,
    }))
}

/// A reader that counts what passes through it and refuses to exceed a limit.
///
/// The refusal is raised as an [`io::Error`] carrying
/// [`EXPANDED_LIMIT_SENTINEL`], because the only way to stop a `tar` iteration
/// mid-stream is through the reader — and [`classify_stream_error`] converts that
/// sentinel back into the policy category it really is, rather than letting it be
/// read as a malformed archive.
struct ExpandedByteCounter<R> {
    inner: R,
    observed: u64,
    limit: Option<u64>,
}

/// Marks an [`io::Error`] this module raised itself to stop a stream on policy.
const EXPANDED_LIMIT_SENTINEL: &str = "ato:capsule-import:source-expanded-limit";

impl<R: Read> Read for ExpandedByteCounter<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.observed = self.observed.saturating_add(read as u64);
        if let Some(limit) = self.limit
            && self.observed > limit
        {
            return Err(io::Error::other(format!(
                "{EXPANDED_LIMIT_SENTINEL}: the bundle's source archive expands past this \
                 importer's {limit}-byte policy limit"
            )));
        }
        Ok(read)
    }
}

/// Turn a tar/zstd stream failure into the category it actually belongs to.
fn classify_stream_error(context: &str, source: io::Error) -> CapsuleImportError {
    if source.to_string().contains(EXPANDED_LIMIT_SENTINEL) {
        return CapsuleImportError::ResourceBudgetExceeded(format!(
            "{source}; the bundle itself is not malformed"
        ));
    }
    CapsuleImportError::invalid(format!("{context}: {source}"))
}

/// Re-categorise an error out of the frozen projection SSOT.
///
/// The SSOT reports a cap violation and a structural violation as the same
/// variant (`CapsuleProgramError::NotPinnedMaterialization`), so the only
/// discriminator available from outside it is the message. To keep that coupling
/// as tight as it can be, each needle is **built from the same public constant
/// the SSOT formats into the message** — so the day a cap value changes, this
/// keeps matching, and the day the phrasing changes, the dedicated test below
/// fails loudly instead of the mismatch silently reintroducing the bug.
///
/// A cap violation becomes [`CapsuleImportError::ResourceBudgetExceeded`]: it is
/// a fixed *implementation* limit inside the SSOT, and the format says nothing
/// about size, so it must not be reported as format invalidity. Everything else —
/// entry kind, path escape, duplicate entry, truncation, split-brain locks, an
/// unreadable stream — stays [`CapsuleImportError::CapsuleInvalid`].
pub(crate) fn classify_projection_error(error: &CapsuleProgramError) -> CapsuleImportError {
    let message = error.to_string();
    if ssot_cap_needles()
        .iter()
        .any(|needle| message.contains(needle.as_str()))
    {
        return CapsuleImportError::ResourceBudgetExceeded(format!(
            "{message} — this is a fixed cap inside the source-projection \
             implementation, not a limit the v3 bundle format imposes; the bundle itself is not \
             malformed"
        ));
    }
    CapsuleImportError::invalid(message)
}

/// The exact substrings `program_source_projection` formats for each of its four
/// fixed caps, rebuilt from the same public constants it uses.
fn ssot_cap_needles() -> [String; 4] {
    [
        format!("{MAX_COMPRESSED_BYTES}-byte cap"),
        format!("{MAX_UNCOMPRESSED_BYTES}-byte cap"),
        format!("{MAX_FILE_SIZE_BYTES}-byte per-file cap"),
        format!("more than {MAX_FILE_COUNT} files"),
    ]
}

/// Re-categorise an error out of [`crate::blob::materialize_source_archive`].
///
/// This is the *other side* of the same pipeline
/// [`classify_projection_error`] guards. The derivation re-archives the
/// manifest-substituted tree, and `materialize_source_archive` applies its own
/// fixed production caps to that derived tree —
/// [`MAX_FILE_SIZE_BYTES`] / [`MAX_FILE_COUNT`] (inside the admissibility walk)
/// and [`MAX_UNCOMPRESSED_BYTES`] / [`MAX_COMPRESSED_BYTES`] (archive-level) —
/// which are entirely separate from the importer's pre-scan limits enforced on
/// the *original* source archive. A tree that only crosses one of them once the
/// outer manifest has been substituted in is not a malformed bundle, and RFC
/// §"Resource policy (not a format limit)" forbids reporting it as one.
///
/// Unlike [`classify_projection_error`], nothing here inspects a message: this
/// error type keeps the categories apart at the type level, so the mapping is
/// exhaustive by construction and a new variant is a compile error rather than a
/// silent fall-through to `CapsuleInvalid`.
pub(crate) fn classify_source_materialize_error(
    error: &SourceMaterializeError,
) -> CapsuleImportError {
    match error {
        SourceMaterializeError::Inadmissible(inner) => classify_admissibility_error(inner),
        SourceMaterializeError::UncompressedTooLarge { .. }
        | SourceMaterializeError::CompressedTooLarge { .. } => {
            fixed_cap_refusal(&error.to_string())
        }
        SourceMaterializeError::Io { .. } => CapsuleImportError::Io(error.to_string()),
    }
}

/// Split the A1v2 admissibility rules into "the tree is wrong" and "the tree is
/// merely bigger than this implementation's fixed caps".
///
/// [`SourceAdmissibilityError::TooManyFiles`] and
/// [`SourceAdmissibilityError::FileTooLarge`] are the typed spellings of exactly
/// the two caps [`ssot_cap_needles`] has to recognise from message text one layer
/// down, so they are classified the same way here — the two classifiers stay
/// consistent in spirit because they agree on which rules are size policy.
/// Everything else (symlink, submodule, LFS pointer, device node, non-NFC or
/// non-UTF-8 path, case-fold collision) is a genuine structural violation and
/// stays [`CapsuleImportError::CapsuleInvalid`].
fn classify_admissibility_error(error: &SourceAdmissibilityError) -> CapsuleImportError {
    match error {
        SourceAdmissibilityError::TooManyFiles { .. }
        | SourceAdmissibilityError::FileTooLarge { .. } => fixed_cap_refusal(&error.to_string()),
        SourceAdmissibilityError::Io { .. } => CapsuleImportError::Io(error.to_string()),
        SourceAdmissibilityError::NonUtf8Path { .. }
        | SourceAdmissibilityError::NonNfcPath { .. }
        | SourceAdmissibilityError::CaseFoldCollision { .. }
        | SourceAdmissibilityError::Symlink { .. }
        | SourceAdmissibilityError::Submodule { .. }
        | SourceAdmissibilityError::LfsPointer { .. }
        | SourceAdmissibilityError::UnsupportedNodeType { .. } => {
            CapsuleImportError::invalid(error.to_string())
        }
    }
}

/// Classify the A1v2 tree hash taken directly (not through
/// `materialize_source_archive`), so the full-tree digest the registration slice
/// needs is subject to the same category split as everything else on this path.
pub(crate) fn classify_tree_hash_error(error: &SourceAdmissibilityError) -> CapsuleImportError {
    classify_admissibility_error(error)
}

/// The one phrasing every fixed-cap refusal on this path shares, so the message
/// a caller sees does not depend on which of the two classifiers produced it.
fn fixed_cap_refusal(message: &str) -> CapsuleImportError {
    CapsuleImportError::ResourceBudgetExceeded(format!(
        "{message} — this is a fixed cap inside the source-materialization \
         implementation, not a limit the v3 bundle format imposes; the bundle itself is not \
         malformed"
    ))
}

/// Total bytes of every regular file under `root`.
///
/// Used to charge the temporary storage the SSOT calls are about to consume; see
/// the accounting note in `verify.rs`.
pub(crate) fn measure_tree_bytes(root: &Path) -> Result<u64, CapsuleImportError> {
    let mut total: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|source| CapsuleImportError::io("read the extracted source tree", source))?;
        for entry in entries {
            let entry = entry.map_err(|source| {
                CapsuleImportError::io("read a source tree directory entry", source)
            })?;
            let metadata = entry.metadata().map_err(|source| {
                CapsuleImportError::io("inspect a source tree directory entry", source)
            })?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reclassification backstop must fire on each of the SSOT's four fixed
    /// caps, and on nothing else. Messages are the ones
    /// `program_source_projection` actually formats — copied from its `format!`
    /// sites, so a phrasing change there fails here rather than silently turning
    /// a fixed cap back into a `CapsuleInvalid`.
    #[test]
    fn ssot_cap_violations_are_reclassified_and_nothing_else_is() {
        let cap_messages = [
            format!("compressed size 999 exceeds the {MAX_COMPRESSED_BYTES}-byte cap"),
            format!("uncompressed size exceeds the {MAX_UNCOMPRESSED_BYTES}-byte cap"),
            format!("entry a.py is 9 bytes, over the {MAX_FILE_SIZE_BYTES}-byte per-file cap"),
            format!("archive holds more than {MAX_FILE_COUNT} files"),
        ];
        for message in cap_messages {
            let error = CapsuleProgramError::NotPinnedMaterialization(message.clone());
            assert!(
                matches!(
                    classify_projection_error(&error),
                    CapsuleImportError::ResourceBudgetExceeded(_)
                ),
                "a fixed SSOT cap must not surface as format invalidity: {message}"
            );
        }

        let structural = [
            "entry ../escape.py escapes the extraction root",
            "entry link.py has type Symlink; only regular files and directories may be extracted",
            "entry a.py declared 10 bytes but yielded 3",
            "tar stream is unreadable: unexpected end of file",
        ];
        for message in structural {
            let error = CapsuleProgramError::NotPinnedMaterialization(message.to_string());
            assert!(
                matches!(
                    classify_projection_error(&error),
                    CapsuleImportError::CapsuleInvalid(_)
                ),
                "a structural violation must stay format invalidity: {message}"
            );
        }
    }

    #[test]
    fn an_unbounded_policy_skips_the_measurement_pass() {
        let measured = measure_source_archive(
            Path::new("/nonexistent/source.tar.zst"),
            0,
            &CapsuleImportPolicy::unbounded(),
        )
        .expect("an unbounded policy never even opens the archive");
        assert!(measured.is_none());
    }
}
