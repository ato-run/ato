//! `ProgramSourceProjectionV1` — the pinned program source projection of a
//! Capsule declaration (ADR-014 Decision §1).
//!
//! The projection is a pure function of (tree, selected root): the control
//! files — `<root>/capsule.toml` plus the ONE selected canonical lock path —
//! are resolved by exact path first and then excluded; **every other path is
//! ordinary source and is hashed, regardless of its file name or content**.
//! There is no "manifest-shaped TOML" predicate and no content sniffing: a
//! nested `fixtures/capsule.lock` or `examples/capsule.toml` is test-data
//! bytes and changes the digest like any other source file.
//!
//! # Input boundary
//!
//! ADR-014 §1 admits **only** a pinned source materialization (an immutable
//! archive / `source_materialize` output, extracted and validated); a local
//! working tree is inadmissible in Phase 0. That precondition is carried in
//! the type system by [`VerifiedPinnedSourceMaterialization`], which every
//! derivation API takes instead of a bare `&Path`.
//!
//! There are two minting paths, and only two:
//!
//! * [`VerifiedPinnedSourceMaterialization::from_source_archive`] — the earned
//!   one. It extracts a content-addressed `.tar.zst`
//!   ([`materialize_source_archive`](crate::foundation::blob::materialize_source_archive)'s
//!   output) into a process-private directory the returned value owns, so the
//!   proof holds *by construction*.
//! * [`VerifiedPinnedSourceMaterialization::assert_pinned_materialization`] —
//!   the escape hatch, for a caller that already holds a materializer's
//!   extracted output tree. The caller states the obligation; this crate only
//!   rejects inputs that provably are not one.
//!
//! # One tree per derivation
//!
//! A pinned root is immutable *by contract*, not by enforcement: nothing stops
//! another process from rewriting it between two reads. A derivation that read
//! the manifest from the original tree and hashed a separate copy could pair
//! manifest intent with source bytes that never coexisted, and a regular file
//! could be swapped for a symlink between an existence check and the copy. So
//! everything after the admissibility gate reads from ONE process-private
//! staging copy:
//!
//! ```text
//! pinned materialization root
//!   1. A1v2 admissibility over the ORIGINAL tree, in full — including the
//!      control files. A control file that is a symlink, FIFO, or device fails
//!      closed here; exclusion never hides it from admissibility.
//!   ─ staging copy (process-private, excludes nothing yet) ─────────────────
//!   2. Verify <staging>/capsule.toml exists as a regular file.
//!   3. Resolve CapsuleControlFiles in the staging tree; coexistence of
//!      capsule.lock and ato.lock.json rejects here (split-brain — never
//!      exclude both, never choose silently).
//!   4. Exclude exactly the resolved control-file paths. Nothing else.
//!   5. The staging tree with those paths removed IS the projected tree; bytes
//!      and the executable bit are preserved by the copy (A1 file identity
//!      includes the executable bit).
//!   6. materialized_source_tree_hash(projected root) — the existing, frozen
//!      A1 digest, called and never modified.
//! ```
//!
//! Steps 2–6 are exactly ADR-014 §1's normative order; the staging copy is an
//! isolation mechanism inserted between steps 1 and 2, not a change to the
//! projection's semantics. Cost: one full copy of the pinned tree (the copy is
//! the projected tree, so there is no second copy) plus one extra walk of the
//! original for the admissibility gate.
//!
//! Self-reference invariant: the digest is identical across {no lock,
//! `capsule.lock`, `ato.lock.json`} at the selected root — the canonical lock
//! never reaches the preimage, so `capsule_program_id` is stable across the
//! lock-file rename migration and across lock rewrites that embed
//! `program_identity`.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tar::EntryType;
use tempfile::TempDir;

use crate::capsule_program_contract::{
    CapsuleProgramError, ProgramSourceContract, ProgramSourceDigest,
    ProgramSourceProjectionSchemaV1,
};
use crate::common::lock_presence::{
    CAPSULE_LOCK_FILE_NAME, CanonicalLockSelectionError, DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
    LexicalEntryState, lexical_entry_state, select_canonical_lock_path,
};
use crate::foundation::blob::source_archive::{MAX_COMPRESSED_BYTES, MAX_UNCOMPRESSED_BYTES};
use crate::foundation::blob::source_tree::{
    MAX_FILE_COUNT, MAX_FILE_SIZE_BYTES, materialized_source_tree_hash,
};

/// The Capsule manifest file name at the selected root.
const CAPSULE_MANIFEST_FILE_NAME: &str = "capsule.toml";

/// A Git checkout's own metadata directory. Its presence at the selected root
/// means the input is a working tree, which ADR-014 §1 refuses in Phase 0.
const GIT_METADATA_DIR_NAME: &str = ".git";

// ─────────────────────────────────────────────────────────────────────────────
// Proof-carrying input boundary
// ─────────────────────────────────────────────────────────────────────────────

/// A proof-carrying wrapper over a filesystem root asserted to be a **pinned
/// source materialization** (ADR-014 §1): an immutable archive /
/// `source_materialize` output, extracted and validated.
///
/// It cannot be minted from a bare `PathBuf`: the fields are private and there
/// is no public constructor — no `new`, no `From<PathBuf>`, no `TryFrom<&Path>`,
/// no `Deserialize`. Phase 0 has exactly two public minting paths:
///
/// * [`VerifiedPinnedSourceMaterialization::from_source_archive`] mints the
///   proof **by construction** from a content-addressed source archive: the
///   archive bytes are immutable and named by their own hash, and the
///   extraction target is a fresh process-private directory the returned value
///   owns, so nothing about the resulting root is asserted.
/// * [`VerifiedPinnedSourceMaterialization::assert_pinned_materialization`] is
///   the escape hatch for a caller that already holds a materializer's
///   extracted output tree; it records a caller assertion and only rejects
///   inputs that provably are not a pinned materialization.
///
/// The staging copy taken during derivation mints the same
/// by-construction proof internally (a process-private directory no other
/// process holds a path to), which is why every read after the admissibility
/// gate is provably from a pinned tree.
///
/// The wrapper mirrors
/// [`VerifiedExecutionId`](crate::execution_contract::VerifiedExecutionId), with
/// one honest difference on the *asserted* path: pinnedness of a tree handed in
/// from outside is a property of the producer of the bytes and cannot be
/// recomputed from the bytes, so `assert_pinned_materialization` records a
/// precondition rather than re-deriving a hash. What both paths enforce, fail
/// closed, is the shape a pinned materialization must have — an existing
/// directory with no root-level `.git`.
///
/// A local working tree is inadmissible in Phase 0 (ADR-014 §1 / Consequences:
/// "Phase 0 refuses dirty working trees"). Admitting one needs its own
/// follow-up ADR: a working tree can be mutated *during* the read, so even a
/// staging copy of one is a torn snapshot rather than a pinned materialization,
/// and the ADR would have to define what identity such a copy carries.
///
/// A bare path cannot be wrapped (the field is private):
///
/// ```compile_fail
/// use std::path::PathBuf;
/// use capsule::program_source_projection::VerifiedPinnedSourceMaterialization;
///
/// // The field is private and there is no `new`/`From`/`TryFrom`/`Deserialize`.
/// let _proof = VerifiedPinnedSourceMaterialization { root: PathBuf::from("/tmp/pinned") };
/// ```
///
/// nor converted (no conversion impl exists):
///
/// ```compile_fail
/// use std::path::PathBuf;
/// use capsule::program_source_projection::VerifiedPinnedSourceMaterialization;
///
/// let root = PathBuf::from("/tmp/pinned");
/// // There is no `From`/`Into` from a path: this is a type error.
/// let _proof: VerifiedPinnedSourceMaterialization = root.into();
/// ```
///
/// and a bare path cannot stand in for the proof at the derivation entrypoint:
///
/// ```compile_fail
/// use std::path::Path;
/// use capsule::capsule_program_contract::derive_capsule_program_contract;
///
/// // A &Path is not a &VerifiedPinnedSourceMaterialization: type error.
/// let _ = derive_capsule_program_contract(Path::new("/tmp/pinned"));
/// ```
#[derive(Debug, Clone)]
pub struct VerifiedPinnedSourceMaterialization {
    root: PathBuf,
    /// Ownership guard for a root this value extracted itself
    /// ([`Self::from_source_archive`]): the extracted directory must outlive
    /// every handle to the proof, so the `TempDir` is kept behind an `Arc` that
    /// `Clone` shares — the last surviving handle removes the directory. The
    /// asserted path leaves this `None`: that root belongs to the caller and
    /// this value must never delete it.
    ///
    /// Never read — the field exists for its `Drop`, which is precisely the
    /// point: the extracted tree must not disappear while a handle to the proof
    /// is alive, and must disappear when the last one is gone.
    #[allow(dead_code)]
    owned_root: Option<Arc<TempDir>>,
}

/// Identity is the pinned root, not the ownership guard: two proofs naming the
/// same root are the same proof regardless of which one is responsible for
/// cleaning it up (and two archive-minted proofs always have distinct roots,
/// because each owns a freshly created private directory).
impl PartialEq for VerifiedPinnedSourceMaterialization {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for VerifiedPinnedSourceMaterialization {}

impl VerifiedPinnedSourceMaterialization {
    /// Mint the proof **by construction**, by extracting a content-addressed
    /// source archive (`materialize_source_archive`'s `.tar.zst`) into a
    /// process-private directory this value owns.
    ///
    /// Why this *earns* the proof instead of asserting it:
    ///
    /// * the archive bytes are **immutable and content-addressed** — the
    ///   `.tar.zst` is named by `source_archive_hash` over its exact bytes, so
    ///   the input is a frozen artifact, not a tree someone can still be
    ///   writing to;
    /// * the extraction target is a **fresh private temp directory** created
    ///   here, whose path no other writer holds, and which this value keeps
    ///   alive for its whole lifetime (see `owned_root`);
    /// * therefore the resulting root **is** a pinned materialization — the
    ///   defining "immutable, produced by a materializer, nobody else writes
    ///   it" property is established by this function rather than promised by
    ///   the caller. Contrast
    ///   [`Self::assert_pinned_materialization`], which is the escape hatch for
    ///   a caller that already extracted a materializer's output itself and can
    ///   only *state* that property.
    ///
    /// The extractor is the trust boundary for hostile archive bytes; see
    /// `extract_source_archive` for the entry-kind / path / cap whitelist it
    /// enforces before a single byte is written. After extraction the root goes
    /// through the same `ensure_pinned_materialization_shape` checks the
    /// asserted path runs, so both minting paths converge on one invariant.
    pub fn from_source_archive(archive_tar_zst: &Path) -> Result<Self, CapsuleProgramError> {
        let extracted = TempDir::new().map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "failed to create the source-archive extraction directory: {source}"
            ))
        })?;
        let root = extracted.path().to_path_buf();
        // A failure here drops `extracted`, which removes any partially written
        // tree: a rejected archive leaves nothing behind.
        extract_source_archive(archive_tar_zst, &root)?;
        ensure_pinned_materialization_shape(&root)?;
        Ok(Self {
            root,
            owned_root: Some(Arc::new(extracted)),
        })
    }

    /// Mint the proof by **caller assertion**: `root` IS a pinned source
    /// materialization — the extracted output of a content-addressed
    /// materializer (`source_materialize`) or an equivalent immutable archive
    /// extraction that no other writer holds open.
    ///
    /// Prefer [`Self::from_source_archive`] whenever the archive itself is at
    /// hand: it establishes the same property by construction instead of taking
    /// the caller's word for it. This constructor stays for callers that
    /// already hold a materializer's *extracted* output tree — a builder that
    /// unpacked the archive itself, a CAS materializer handing over the
    /// directory it just populated — where re-extracting would be pure waste.
    ///
    /// The caller discharges the obligation ADR-014 §1 places on the input; this
    /// constructor only rejects inputs that provably are NOT one:
    ///
    /// * a missing root, or a root that is not a directory (a symlinked root is
    ///   refused too — it is not the tree it points at);
    /// * a root-level `.git` entry of any node type. A Git checkout is a working
    ///   tree, which Phase 0 forbids outright, and `.git` is not on ADR-014 §1's
    ///   exhaustive control-file list ("manifest + the ONE resolved lock,
    ///   nothing else"), so it could neither be excluded without widening that
    ///   normative list nor hashed without importing Git's nondeterministic
    ///   index/pack bytes into the identity. Note that A1v2 rejects a NESTED
    ///   `.git` (submodule signal) and a root `.gitmodules`, but not the root's
    ///   own `.git` — this closes exactly that hole.
    pub fn assert_pinned_materialization(root: &Path) -> Result<Self, CapsuleProgramError> {
        ensure_pinned_materialization_shape(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            owned_root: None,
        })
    }

    /// The pinned root. Reading it directly re-opens the mutation window the
    /// staging copy exists to close — inside this crate, derive from
    /// [`StagedCapsuleSource`] instead.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Fail-closed shape checks for a pinned materialization root. Cheap enough
/// (two `symlink_metadata` calls) to run both when the proof is minted and
/// again when it is used, so a tree that changed in between still fails closed.
fn ensure_pinned_materialization_shape(root: &Path) -> Result<(), CapsuleProgramError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{} does not exist",
                root.display()
            )));
        }
        Err(source) => return Err(projection_io("inspect the pinned root", root, source)),
    }

    let git = root.join(GIT_METADATA_DIR_NAME);
    let git_state =
        lexical_entry_state(&git).map_err(|source| projection_io("inspect", &git, source))?;
    match git_state {
        LexicalEntryState::Absent => Ok(()),
        LexicalEntryState::PresentRegularFile | LexicalEntryState::PresentInvalidNode(_) => {
            Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{} contains a root-level {GIT_METADATA_DIR_NAME}: a Git checkout is a working \
                 tree, and ADR-014 §1 admits only a pinned source materialization (immutable \
                 archive / source_materialize output) in Phase 0",
                root.display(),
            )))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Source-archive extraction (the by-construction minting path)
// ─────────────────────────────────────────────────────────────────────────────

/// A rejection of the archive itself — the input cannot yield a pinned
/// materialization, so it fails as [`CapsuleProgramError::NotPinnedMaterialization`]
/// rather than as a projection error over an already-admitted tree.
fn not_a_source_archive(archive: &Path, reason: impl AsRef<str>) -> CapsuleProgramError {
    CapsuleProgramError::NotPinnedMaterialization(format!(
        "{} is not a usable content-addressed source archive: {}",
        archive.display(),
        reason.as_ref(),
    ))
}

/// Decompress `archive_tar_zst` into an in-memory `tar` stream, bounded by the
/// production archive caps.
///
/// The caps are `source_archive`'s public `MAX_COMPRESSED_BYTES` /
/// `MAX_UNCOMPRESSED_BYTES`. `source_archive::ArchiveCaps` itself is private —
/// deliberately, so no API can lower the production thresholds — but
/// `ArchiveCaps::PRODUCTION` is built from exactly these two public constants
/// (pinned by `source_archive`'s `production_caps_are_100mib_and_250mib` test),
/// so reading them directly applies the production caps without reaching into a
/// private type.
///
/// The compressed cap is checked from the file's own length *before* any
/// decompression, and the decompressed stream is read through a
/// `take(cap + 1)` so a zstd bomb cannot allocate past the cap: the extra byte
/// is what distinguishes "exactly at the cap" from "over it". Buffering the
/// whole `tar` mirrors `materialize_source_archive`, which builds it in memory
/// under the same bound.
fn decode_source_archive(archive_tar_zst: &Path) -> Result<Vec<u8>, CapsuleProgramError> {
    let metadata = match fs::metadata(archive_tar_zst) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(not_a_source_archive(
                archive_tar_zst,
                "the file does not exist",
            ));
        }
        Err(source) => {
            return Err(projection_io(
                "inspect the source archive",
                archive_tar_zst,
                source,
            ));
        }
    };
    if !metadata.is_file() {
        return Err(not_a_source_archive(
            archive_tar_zst,
            "the path is not a regular file",
        ));
    }
    if metadata.len() > MAX_COMPRESSED_BYTES {
        return Err(not_a_source_archive(
            archive_tar_zst,
            format!(
                "compressed size {} exceeds the {MAX_COMPRESSED_BYTES}-byte cap",
                metadata.len()
            ),
        ));
    }

    let file = fs::File::open(archive_tar_zst)
        .map_err(|source| projection_io("open the source archive", archive_tar_zst, source))?;
    let decoder = zstd::Decoder::new(file).map_err(|source| {
        not_a_source_archive(archive_tar_zst, format!("zstd decode failed: {source}"))
    })?;
    let mut tar_bytes = Vec::new();
    decoder
        .take(MAX_UNCOMPRESSED_BYTES.saturating_add(1))
        .read_to_end(&mut tar_bytes)
        .map_err(|source| {
            not_a_source_archive(archive_tar_zst, format!("zstd decode failed: {source}"))
        })?;
    if tar_bytes.len() as u64 > MAX_UNCOMPRESSED_BYTES {
        return Err(not_a_source_archive(
            archive_tar_zst,
            format!("uncompressed size exceeds the {MAX_UNCOMPRESSED_BYTES}-byte cap"),
        ));
    }
    Ok(tar_bytes)
}

/// Validate one archive entry path into a relative path safe to join onto the
/// extraction root, or say why it is not.
///
/// Only [`Component::Normal`] is accepted. That single rule covers absolute
/// paths (`RootDir`), Windows drive prefixes (`Prefix`), `..` traversal
/// (`ParentDir`), and `.` (`CurDir`), and it is deliberately stricter than
/// `tar`'s own `Entry::unpack_in`, which *silently strips* leading `/` and `.`
/// components and *silently skips* a `..` entry by returning `Ok(false)` rather
/// than an error. Silently rewriting a hostile path is not a proof; the archive
/// is refused instead.
fn safe_archive_entry_path(raw: &Path) -> Result<PathBuf, String> {
    let mut safe = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("entry path {} is absolute", raw.display()));
            }
            Component::ParentDir => {
                return Err(format!(
                    "entry path {} contains a `..` traversal component",
                    raw.display()
                ));
            }
            Component::CurDir => {
                return Err(format!(
                    "entry path {} contains a `.` component",
                    raw.display()
                ));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err("entry path is empty".to_string());
    }
    Ok(safe)
}

/// Extract a content-addressed `.tar.zst` into `dest`, an empty directory this
/// process owns.
///
/// This is the trust boundary for hostile archive bytes, so it whitelists
/// rather than sanitizes and fails the whole archive on the first violation
/// (the caller drops `dest`, so a rejected archive leaves nothing behind):
///
/// * **entry kind** — only `Regular` and `Directory` are extracted. Symlink,
///   hardlink, character/block device, FIFO, GNU sparse, pax global-header,
///   `Continuous`, and any unknown type byte are rejected. No symlink is ever
///   created, so no later entry can be redirected through one; no device or
///   FIFO node is ever created, so extraction cannot produce a node A1v2 would
///   have to reject later. A `Regular`/`Directory` entry that also carries a
///   link-name field is malformed and rejected too.
/// * **path** — [`safe_archive_entry_path`] admits `Component::Normal` only,
///   so absolute paths, `..`, `.`, and drive prefixes are rejected rather than
///   stripped.
/// * **containment** — the joined target is re-checked with
///   `starts_with(dest)`. With `Normal`-only components this is already
///   lexically guaranteed and stays true on disk because no symlink is ever
///   created inside `dest`; the check is a cheap belt to that argument's
///   braces.
/// * **no overwrite** — regular files are created with `create_new`, so two
///   entries claiming one path is a rejection instead of a silent
///   last-writer-wins.
/// * **caps** — the production per-file (`MAX_FILE_SIZE_BYTES`), file-count
///   (`MAX_FILE_COUNT`), and aggregate (`MAX_UNCOMPRESSED_BYTES`) caps, the
///   same constants `materialize_source_archive` enforces on the way in.
/// * **declared size** — the bytes actually copied must equal the header's
///   declared size, so a truncated member is a rejection, not a short file.
///
/// Permission bits are normalized exactly the way the archive builder writes
/// them (`0o755` when the owner-execute bit is set, else `0o644`), which is the
/// only permission state A1 folds into the tree identity — so a round trip
/// through the archive preserves the digest.
fn extract_source_archive(archive_tar_zst: &Path, dest: &Path) -> Result<(), CapsuleProgramError> {
    let tar_bytes = decode_source_archive(archive_tar_zst)?;
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    let entries = archive.entries().map_err(|source| {
        not_a_source_archive(
            archive_tar_zst,
            format!("tar stream is unreadable: {source}"),
        )
    })?;

    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;
    for entry in entries {
        let mut entry = entry.map_err(|source| {
            not_a_source_archive(
                archive_tar_zst,
                format!("tar entry is unreadable: {source}"),
            )
        })?;

        let entry_type = entry.header().entry_type();
        let mode = entry.header().mode().map_err(|source| {
            not_a_source_archive(
                archive_tar_zst,
                format!("tar entry has an unreadable mode field: {source}"),
            )
        })?;
        let declared_size = entry.size();
        let has_link_name = entry.link_name_bytes().is_some();
        let raw_path = entry
            .path()
            .map_err(|source| {
                not_a_source_archive(
                    archive_tar_zst,
                    format!("tar entry has an unreadable path: {source}"),
                )
            })?
            .into_owned();

        if !matches!(entry_type, EntryType::Regular | EntryType::Directory) {
            return Err(not_a_source_archive(
                archive_tar_zst,
                format!(
                    "entry {} has type {:?}; only regular files and directories may be extracted",
                    raw_path.display(),
                    entry_type
                ),
            ));
        }
        if has_link_name {
            return Err(not_a_source_archive(
                archive_tar_zst,
                format!("entry {} carries a link name", raw_path.display()),
            ));
        }

        let relative = safe_archive_entry_path(&raw_path)
            .map_err(|reason| not_a_source_archive(archive_tar_zst, reason))?;
        let target = dest.join(&relative);
        if !target.starts_with(dest) {
            return Err(not_a_source_archive(
                archive_tar_zst,
                format!("entry {} escapes the extraction root", raw_path.display()),
            ));
        }

        match entry_type {
            EntryType::Directory => {
                fs::create_dir_all(&target)
                    .map_err(|source| projection_io("create directory", &target, source))?;
            }
            _ => {
                if declared_size > MAX_FILE_SIZE_BYTES {
                    return Err(not_a_source_archive(
                        archive_tar_zst,
                        format!(
                            "entry {} is {declared_size} bytes, over the \
                             {MAX_FILE_SIZE_BYTES}-byte per-file cap",
                            raw_path.display()
                        ),
                    ));
                }
                file_count += 1;
                if file_count > MAX_FILE_COUNT {
                    return Err(not_a_source_archive(
                        archive_tar_zst,
                        format!("archive holds more than {MAX_FILE_COUNT} files"),
                    ));
                }
                total_bytes = total_bytes.saturating_add(declared_size);
                if total_bytes > MAX_UNCOMPRESSED_BYTES {
                    return Err(not_a_source_archive(
                        archive_tar_zst,
                        format!("extracted content exceeds the {MAX_UNCOMPRESSED_BYTES}-byte cap"),
                    ));
                }

                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|source| projection_io("create directory", parent, source))?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|source| {
                        if source.kind() == io::ErrorKind::AlreadyExists {
                            not_a_source_archive(
                                archive_tar_zst,
                                format!("entry {} is declared twice", raw_path.display()),
                            )
                        } else {
                            projection_io("create file", &target, source)
                        }
                    })?;
                let copied = io::copy(&mut entry, &mut file)
                    .map_err(|source| projection_io("write file", &target, source))?;
                if copied != declared_size {
                    return Err(not_a_source_archive(
                        archive_tar_zst,
                        format!(
                            "entry {} declared {declared_size} bytes but yielded {copied}",
                            raw_path.display()
                        ),
                    ));
                }
                drop(file);
                set_extracted_file_mode(&target, mode)?;
            }
        }
    }
    Ok(())
}

/// Normalize an extracted file's permissions to the two states the archive
/// builder emits and A1 folds into the tree identity.
#[cfg(unix)]
fn set_extracted_file_mode(path: &Path, mode: u32) -> Result<(), CapsuleProgramError> {
    use std::os::unix::fs::PermissionsExt;

    let normalized = if mode & 0o100 != 0 { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(normalized))
        .map_err(|source| projection_io("set permissions on", path, source))
}

/// Without POSIX permissions A1 treats every file as non-executable, so there
/// is nothing to normalize.
#[cfg(not(unix))]
fn set_extracted_file_mode(_path: &Path, _mode: u32) -> Result<(), CapsuleProgramError> {
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Control files
// ─────────────────────────────────────────────────────────────────────────────

/// The control files of a selected capsule root (ADR-014 §1): the manifest
/// plus the ONE selected canonical lock path, if any. These are the only
/// paths the projection excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleControlFiles {
    pub manifest: PathBuf,
    pub lock: Option<PathBuf>,
}

/// Resolves the control files at `selected_root` (§1 steps 2–3): the manifest
/// must exist as a regular file, and the lock path is selected by exact path —
/// `capsule.lock` canonical, `ato.lock.json` deprecated alias, coexistence
/// fail-closed, neither = `None`. No content is read.
///
/// Presence is the shared, fail-closed, LEXICAL rule of
/// [`crate::common::lock_presence`] — the same one
/// `routing::input_resolver::resolve_canonical_lock_path` uses, so both paths
/// reach the same verdict for the same root. A dangling symlink under a lock
/// name is present (hence split-brain beside a real lock, never silently
/// skipped); a non-`NotFound` metadata error is never read as absent; and a
/// selected lock that is not a regular file is rejected rather than excluded
/// from the digest wholesale.
///
/// In the derivation flow this runs against the staging copy, after the A1v2
/// pass (step 1) has already rejected symlinks and special nodes in the
/// original tree.
pub fn resolve_capsule_control_files(
    selected_root: &Path,
) -> Result<CapsuleControlFiles, CapsuleProgramError> {
    let manifest = selected_root.join(CAPSULE_MANIFEST_FILE_NAME);
    let manifest_state = lexical_entry_state(&manifest)
        .map_err(|source| projection_io("inspect", &manifest, source))?;
    match manifest_state {
        LexicalEntryState::PresentRegularFile => {}
        LexicalEntryState::PresentInvalidNode(kind) => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "{} must be a regular file, found {kind}",
                manifest.display(),
            )));
        }
        LexicalEntryState::Absent => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "required manifest {} does not exist",
                manifest.display(),
            )));
        }
    }

    let lock = select_canonical_lock_path(selected_root)
        .map_err(lock_selection_error)?
        .into_path();

    Ok(CapsuleControlFiles { manifest, lock })
}

fn lock_selection_error(error: CanonicalLockSelectionError) -> CapsuleProgramError {
    match error {
        CanonicalLockSelectionError::Coexistence { root } => {
            CapsuleProgramError::SourceProjection(format!(
                "both {canonical} and {alias} exist at {root}; no automatic lock-path \
                 choice is made — remove one of the two files (keep {canonical})",
                canonical = CAPSULE_LOCK_FILE_NAME,
                alias = DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
                root = root.display(),
            ))
        }
        CanonicalLockSelectionError::NotRegularFile { path, kind } => {
            CapsuleProgramError::SourceProjection(format!(
                "{} must be a regular file, found {kind}",
                path.display(),
            ))
        }
        CanonicalLockSelectionError::Io { path, source } => projection_io("inspect", &path, source),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Staged derivation
// ─────────────────────────────────────────────────────────────────────────────

/// The single immutable tree a derivation reads from: a process-private copy
/// of the pinned root, A1-gated before the copy, with the control files
/// resolved **inside** the copy.
///
/// While this value is alive the manifest is still present, so the manifest
/// bytes and the source bytes provably come from one tree state. Consuming it
/// with [`Self::into_projected`] removes exactly the resolved control files;
/// the remaining tree IS the `ProgramSourceProjectionV1`. The staging directory
/// is removed when the value is dropped, so nothing escapes but the digest.
pub struct StagedCapsuleSource {
    /// The pinned root this copy was taken from — used only to attribute error
    /// messages to a path the caller recognizes.
    origin: PathBuf,
    staging: TempDir,
    control_files: CapsuleControlFiles,
}

impl StagedCapsuleSource {
    /// ADR-014 §1 step 1 over the ORIGINAL tree, then the staging copy, then
    /// steps 2–3 inside the copy.
    pub fn stage(
        pinned: &VerifiedPinnedSourceMaterialization,
    ) -> Result<Self, CapsuleProgramError> {
        let origin = pinned.root();
        // The proof may have been minted long before it is used; re-check the
        // shape so a tree that turned into a Git checkout in between fails
        // closed rather than being hashed.
        ensure_pinned_materialization_shape(origin)?;

        // Step 1: A1v2 admissibility over the ORIGINAL tree, control files
        // included. The hash is discarded — this is the gate, not the digest.
        materialized_source_tree_hash(origin).map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "A1v2 admissibility rejected the source tree at {}: {source}",
                origin.display(),
            ))
        })?;

        let staging = TempDir::new().map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "failed to create the staging directory: {source}"
            ))
        })?;
        copy_tree(origin, staging.path())?;

        // Steps 2–3, resolved in the copy: from here on the original tree is
        // never read again, so nothing an outside process does to it can reach
        // the manifest intent or the digest.
        let control_files = resolve_capsule_control_files(staging.path())
            .map_err(|error| attribute_to_origin(error, staging.path(), origin))?;

        Ok(Self {
            origin: origin.to_path_buf(),
            staging,
            control_files,
        })
    }

    /// The staging root. Manifest loading and every `SourceExistingPath`
    /// existence check resolve against this path.
    pub fn root(&self) -> &Path {
        self.staging.path()
    }

    /// `<staging>/capsule.toml` — the only manifest a derivation may read.
    pub fn manifest_path(&self) -> &Path {
        &self.control_files.manifest
    }

    pub fn control_files(&self) -> &CapsuleControlFiles {
        &self.control_files
    }

    /// Rewrites a staging path in `error` back to the pinned root, so a caller
    /// is never shown a process-private temporary path.
    pub fn attribute_to_origin(&self, error: CapsuleProgramError) -> CapsuleProgramError {
        attribute_to_origin(error, self.staging.path(), &self.origin)
    }

    /// Step 4–5: remove exactly the resolved control files. What remains is the
    /// projected tree — the same file set the ADR's "materialize the projected
    /// tree" step describes, reached without a second copy.
    pub fn into_projected(self) -> Result<ProjectedCapsuleSource, CapsuleProgramError> {
        let Self {
            origin,
            staging,
            control_files,
        } = self;
        let remove = |path: &Path| -> Result<(), CapsuleProgramError> {
            fs::remove_file(path).map_err(|source| {
                attribute_to_origin(
                    projection_io("exclude control file", path, source),
                    staging.path(),
                    &origin,
                )
            })
        };
        remove(&control_files.manifest)?;
        if let Some(lock) = control_files.lock.as_deref() {
            remove(lock)?;
        }
        Ok(ProjectedCapsuleSource { staging })
    }
}

/// The staging tree with the control files removed: the
/// `ProgramSourceProjectionV1` itself.
pub struct ProjectedCapsuleSource {
    staging: TempDir,
}

impl ProjectedCapsuleSource {
    /// The projected root. `SourceExistingPath` checks that must see exactly
    /// what the digest covers resolve against this path.
    pub fn root(&self) -> &Path {
        self.staging.path()
    }

    /// Step 6: the frozen A1 digest over the projected root.
    pub fn source_contract(&self) -> Result<ProgramSourceContract, CapsuleProgramError> {
        let blob_hash = materialized_source_tree_hash(self.staging.path()).map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "failed to hash the projected source tree: {source}"
            ))
        })?;
        Ok(ProgramSourceContract {
            digest: ProgramSourceDigest::parse(&blob_hash)?,
            projection_schema: ProgramSourceProjectionSchemaV1,
        })
    }
}

/// Derives the pinned [`ProgramSourceContract`] of a pinned materialization by
/// the §1 order. The staging copy is removed before returning; only the digest
/// escapes.
pub fn project_program_source(
    pinned: &VerifiedPinnedSourceMaterialization,
) -> Result<ProgramSourceContract, CapsuleProgramError> {
    StagedCapsuleSource::stage(pinned)?
        .into_projected()?
        .source_contract()
}

/// Copies `source_dir` into `dest_dir` recursively. `fs::copy` preserves unix
/// permission bits, so the A1 executable-bit identity survives staging. The
/// A1v2 pass has already rejected symlinks and special nodes; any encountered
/// here means the tree changed after the gate, and staging fails closed.
///
/// `common::fs::copy_dir_recursive` is deliberately not reused: its policies
/// *skip* symlinks and special nodes, which would silently drop a post-gate
/// mutation out of the digest instead of rejecting it.
fn copy_tree(source_dir: &Path, dest_dir: &Path) -> Result<(), CapsuleProgramError> {
    let entries = fs::read_dir(source_dir)
        .map_err(|source| projection_io("read directory", source_dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| projection_io("read directory", source_dir, source))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| projection_io("inspect entry", &path, source))?;
        let file_type = metadata.file_type();
        let destination = dest_dir.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&destination)
                .map_err(|source| projection_io("create directory", &destination, source))?;
            copy_tree(&path, &destination)?;
        } else if file_type.is_file() {
            fs::copy(&path, &destination)
                .map_err(|source| projection_io("copy file", &path, source))?;
        } else {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "unexpected {} at {} during staging (tree changed after the \
                 admissibility pass)",
                node_kind(file_type),
                path.display(),
            )));
        }
    }
    Ok(())
}

fn node_kind(file_type: fs::FileType) -> &'static str {
    if file_type.is_dir() {
        "a directory"
    } else if file_type.is_symlink() {
        "a symlink"
    } else {
        "an unsupported node type"
    }
}

fn projection_io(action: &str, path: &Path, source: std::io::Error) -> CapsuleProgramError {
    CapsuleProgramError::SourceProjection(format!(
        "failed to {action} {}: {source}",
        path.display()
    ))
}

/// Maps a staging path back onto the pinned root inside an error message. The
/// staging directory is an implementation detail; a caller must be able to act
/// on the path it supplied.
fn attribute_to_origin(
    error: CapsuleProgramError,
    staging: &Path,
    origin: &Path,
) -> CapsuleProgramError {
    let (Some(staging), Some(origin)) = (staging.to_str(), origin.to_str()) else {
        return error;
    };
    match error {
        CapsuleProgramError::SourceProjection(message) => {
            CapsuleProgramError::SourceProjection(message.replace(staging, origin))
        }
        CapsuleProgramError::ManifestLoad(message) => {
            CapsuleProgramError::ManifestLoad(message.replace(staging, origin))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn pinned(root: &Path) -> VerifiedPinnedSourceMaterialization {
        VerifiedPinnedSourceMaterialization::assert_pinned_materialization(root)
            .expect("pinned materialization")
    }

    fn project(root: &Path) -> Result<ProgramSourceContract, CapsuleProgramError> {
        project_program_source(&pinned(root))
    }

    fn write_file(root: &Path, rel: &str, contents: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// A base tree that deliberately contains control-file NAMES at nested
    /// paths — those are ordinary source and must be hashed.
    fn write_base_tree(root: &Path) {
        write_file(root, "capsule.toml", b"[capsule]\nname = \"demo\"\n");
        write_file(root, "src/main.py", b"print('hi')\n");
        write_file(root, "fixtures/ato.lock.json", b"{\"fixture\": true}\n");
        write_file(
            root,
            "examples/capsule.toml",
            b"[capsule]\nname = \"example\"\n",
        );
    }

    /// A lock body embedding a program_identity-shaped payload: even a lock
    /// that stores the derived id must not reach the digest preimage.
    const LOCK_BODY: &[u8] = br#"{
  "schema": "ato.lock/v1",
  "program_identity": {
    "capsule_program_id": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "program_contract": { "schema": "ato.capsule-program/v1" }
  }
}
"#;

    #[test]
    fn projection_digest_is_fixed_point_across_lock_spellings() {
        let no_lock = TempDir::new().unwrap();
        write_base_tree(no_lock.path());

        let canonical_lock = TempDir::new().unwrap();
        write_base_tree(canonical_lock.path());
        write_file(canonical_lock.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);

        let alias_lock = TempDir::new().unwrap();
        write_base_tree(alias_lock.path());
        write_file(
            alias_lock.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );

        let base = project(no_lock.path()).unwrap();
        let with_canonical = project(canonical_lock.path()).unwrap();
        let with_alias = project(alias_lock.path()).unwrap();

        assert_eq!(base.digest, with_canonical.digest);
        assert_eq!(base.digest, with_alias.digest);
        assert_eq!(base, with_canonical);
        assert_eq!(base, with_alias);
    }

    #[test]
    fn rejects_coexisting_lock_names_at_root() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        write_file(tmp.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        write_file(
            tmp.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );

        let err = project(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains(CAPSULE_LOCK_FILE_NAME), "{message}");
        assert!(
            message.contains(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME),
            "{message}"
        );
        // The message names the caller's root, not the staging copy.
        assert!(
            message.contains(&tmp.path().display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn resolve_selects_exactly_one_lock_path() {
        let neither = TempDir::new().unwrap();
        write_base_tree(neither.path());
        let control = resolve_capsule_control_files(neither.path()).unwrap();
        assert_eq!(
            control.manifest,
            neither.path().join(CAPSULE_MANIFEST_FILE_NAME)
        );
        assert_eq!(control.lock, None);

        let canonical = TempDir::new().unwrap();
        write_base_tree(canonical.path());
        write_file(canonical.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        let control = resolve_capsule_control_files(canonical.path()).unwrap();
        assert_eq!(
            control.lock,
            Some(canonical.path().join(CAPSULE_LOCK_FILE_NAME))
        );

        let alias = TempDir::new().unwrap();
        write_base_tree(alias.path());
        write_file(
            alias.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );
        let control = resolve_capsule_control_files(alias.path()).unwrap();
        assert_eq!(
            control.lock,
            Some(alias.path().join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME))
        );
    }

    /// Major 2 regression: the projection and the input resolver must reach the
    /// SAME verdict for a dangling canonical lock beside a valid alias. Before
    /// the shared helper the resolver's `exists()` followed the link and picked
    /// the alias while the projection rejected the pair.
    #[cfg(unix)]
    #[test]
    fn dangling_canonical_lock_verdict_matches_the_input_resolver() {
        use crate::routing::input_resolver::resolve_canonical_lock_path;

        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        write_file(
            tmp.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );
        std::os::unix::fs::symlink("nowhere", tmp.path().join(CAPSULE_LOCK_FILE_NAME)).unwrap();

        let projection = resolve_capsule_control_files(tmp.path());
        let resolver = resolve_canonical_lock_path(tmp.path());
        assert!(
            projection.is_err(),
            "projection must reject the split brain"
        );
        assert!(
            resolver.is_err(),
            "resolver must reject the same split brain"
        );
    }

    /// A lock NAME occupied by a directory is rejected: excluding it would drop
    /// its whole subtree out of the digest.
    #[test]
    fn directory_under_the_lock_name_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        fs::create_dir(tmp.path().join(CAPSULE_LOCK_FILE_NAME)).unwrap();
        write_file(tmp.path(), "capsule.lock/inner.txt", b"hidden\n");

        let err = project(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains("must be a regular file"), "{message}");
        assert!(message.contains("a directory"), "{message}");
    }

    #[test]
    fn nested_control_file_names_are_ordinary_source() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        let baseline = project(tmp.path()).unwrap().digest;

        write_file(tmp.path(), "fixtures/ato.lock.json", b"{\"fixture\": 2}\n");
        let after_lock_fixture = project(tmp.path()).unwrap().digest;
        assert_ne!(baseline, after_lock_fixture);

        write_file(
            tmp.path(),
            "examples/capsule.toml",
            b"[capsule]\nname = \"changed\"\n",
        );
        let after_manifest_fixture = project(tmp.path()).unwrap().digest;
        assert_ne!(after_lock_fixture, after_manifest_fixture);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_flip_changes_projection_digest() {
        use std::os::unix::fs::PermissionsExt;

        let plain = TempDir::new().unwrap();
        write_base_tree(plain.path());
        write_file(plain.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            plain.path().join("bin/run"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let executable = TempDir::new().unwrap();
        write_base_tree(executable.path());
        write_file(executable.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            executable.path().join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let plain_digest = project(plain.path()).unwrap().digest;
        let executable_digest = project(executable.path()).unwrap().digest;
        assert_ne!(plain_digest, executable_digest);
    }

    #[cfg(unix)]
    #[test]
    fn staged_copy_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let source = TempDir::new().unwrap();
        write_file(source.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            source.path().join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let destination = TempDir::new().unwrap();
        copy_tree(source.path(), destination.path()).unwrap();

        let mode = fs::metadata(destination.path().join("bin/run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "fs::copy must preserve the executable bit, got mode {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_named_capsule_lock_rejected_by_admissibility_pass() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        symlink("capsule.toml", tmp.path().join(CAPSULE_LOCK_FILE_NAME)).unwrap();

        let err = project(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(
            message.contains("A1v2 admissibility") && message.contains("symlink"),
            "a control-file symlink must fail the step-1 gate, not be excluded: {message}"
        );
    }

    #[test]
    fn missing_root_manifest_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.py", b"print('hi')\n");

        let err = project(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains(CAPSULE_MANIFEST_FILE_NAME), "{message}");
        assert!(message.contains("does not exist"), "{message}");
        assert!(
            message.contains(&tmp.path().display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn root_manifest_directory_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.py", b"print('hi')\n");
        fs::create_dir(tmp.path().join(CAPSULE_MANIFEST_FILE_NAME)).unwrap();

        let err = project(tmp.path()).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(message.contains("must be a regular file"), "{message}");
        assert!(message.contains("a directory"), "{message}");
    }

    // ── pinned-materialization boundary ──────────────────────────────────

    #[test]
    fn root_level_git_is_not_a_pinned_materialization() {
        let checkout = TempDir::new().unwrap();
        write_base_tree(checkout.path());
        write_file(checkout.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        write_file(checkout.path(), ".git/config", b"[core]\n");

        let err =
            VerifiedPinnedSourceMaterialization::assert_pinned_materialization(checkout.path())
                .unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains(".git"), "{message}");

        // A `.git` FILE (a gitfile / worktree pointer) is the same verdict.
        let gitfile = TempDir::new().unwrap();
        write_base_tree(gitfile.path());
        write_file(
            gitfile.path(),
            ".git",
            b"gitdir: /elsewhere/.git/worktrees/x\n",
        );
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::assert_pinned_materialization(gitfile.path()),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    /// A `.git` that appears after the proof was minted still fails closed at
    /// staging time, and a `.git` never reaches the digest either way.
    #[test]
    fn root_level_git_appearing_after_minting_fails_at_staging() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        let proof = pinned(tmp.path());
        write_file(tmp.path(), ".git/HEAD", b"ref: refs/heads/main\n");

        assert!(matches!(
            project_program_source(&proof),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    #[test]
    fn missing_or_non_directory_root_is_not_a_pinned_materialization() {
        let tmp = TempDir::new().unwrap();
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::assert_pinned_materialization(
                &tmp.path().join("absent")
            ),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));

        write_file(tmp.path(), "file.txt", b"x\n");
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::assert_pinned_materialization(
                &tmp.path().join("file.txt")
            ),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    /// The staging copy is the derivation's only input after the gate: mutating
    /// the original tree while the staged handle is alive cannot change either
    /// the manifest bytes it exposes or the digest it produces.
    #[test]
    fn staged_tree_is_insulated_from_post_gate_mutation() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());

        let staged = StagedCapsuleSource::stage(&pinned(tmp.path())).unwrap();
        let manifest_before = fs::read(staged.manifest_path()).unwrap();

        // An outside writer rewrites the pinned tree after the gate.
        write_file(
            tmp.path(),
            "capsule.toml",
            b"[capsule]\nname = \"swapped\"\n",
        );
        write_file(tmp.path(), "src/main.py", b"print('swapped')\n");

        assert_eq!(fs::read(staged.manifest_path()).unwrap(), manifest_before);
        let staged_digest = staged.into_projected().unwrap().source_contract().unwrap();
        let mutated_digest = project(tmp.path()).unwrap();
        assert_ne!(
            staged_digest.digest, mutated_digest.digest,
            "the staged derivation must reflect the tree as gated, not as mutated"
        );
    }

    // ── archive-minted proof (`from_source_archive`) ─────────────────────

    use crate::foundation::blob::source_archive::materialize_source_archive;
    use tar::{Builder, Header};

    /// Freeze `tree` with the real materializer and return the archive path
    /// (kept alive by the returned `TempDir`).
    fn materialize(tree: &Path) -> (TempDir, PathBuf) {
        let out_dir = TempDir::new().unwrap();
        let archive = out_dir.path().join("source.tar.zst");
        materialize_source_archive(tree, &archive).expect("materialize source archive");
        (out_dir, archive)
    }

    /// Write a `.tar.zst` from hand-crafted headers, bypassing
    /// `materialize_source_archive` entirely — the only way to produce entry
    /// classes the deterministic builder can never emit. `Builder::append`
    /// writes the header verbatim (it does not re-derive the path or the
    /// checksum), so a header whose name field was poked in directly survives
    /// into the archive.
    fn write_crafted_archive(out: &Path, entries: Vec<(Header, Vec<u8>)>) {
        let mut builder = Builder::new(Vec::new());
        for (mut header, data) in entries {
            header.set_cksum();
            builder.append(&header, data.as_slice()).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let compressed = zstd::encode_all(tar_bytes.as_slice(), 3).unwrap();
        fs::write(out, compressed).unwrap();
    }

    /// A benign regular-file entry, so a rejection is provably about the
    /// hostile entry beside it and not about an empty archive.
    fn benign_entry() -> (Header, Vec<u8>) {
        let body = b"[capsule]\nname = \"demo\"\n".to_vec();
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(body.len() as u64);
        header.set_path(CAPSULE_MANIFEST_FILE_NAME).unwrap();
        (header, body)
    }

    fn regular_header(size: u64) -> Header {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(size);
        header
    }

    /// Poke raw bytes into the GNU header's name field. `Header::set_path`
    /// refuses `..` outright ("paths in archives must not have `..`"), so a
    /// traversal entry can only be built by writing the name field directly —
    /// which is exactly what a hostile producer does.
    fn set_raw_name(header: &mut Header, raw: &[u8]) {
        let name = &mut header.as_gnu_mut().expect("gnu header").name;
        name.fill(0);
        name[..raw.len()].copy_from_slice(raw);
    }

    /// THE load-bearing test: the two minting paths must produce the same
    /// program source digest for the same tree. If they ever diverge, the
    /// by-construction proof would be minting a *different* program identity
    /// than the assertion it replaces.
    #[test]
    fn archive_minted_and_asserted_roots_derive_the_same_digest() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        write_file(tree.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        write_file(tree.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                tree.path().join("bin/run"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let (_archive_dir, archive) = materialize(tree.path());

        let from_archive = project_program_source(
            &VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap(),
        )
        .unwrap();
        let from_assertion = project(tree.path()).unwrap();

        assert_eq!(
            from_archive.digest, from_assertion.digest,
            "extracting the content-addressed archive must yield the same \
             ProgramSourceProjectionV1 digest as asserting over the original tree"
        );
        assert_eq!(from_archive, from_assertion);
    }

    /// The extracted root is owned by the value: alive for as long as any
    /// handle exists, removed when the last one drops.
    #[test]
    fn archive_minted_root_outlives_the_call_and_dies_with_the_last_handle() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        let (_archive_dir, archive) = materialize(tree.path());

        let proof = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap();
        let root = proof.root().to_path_buf();
        assert!(root.is_dir(), "the extracted root outlives the constructor");
        assert!(root.join(CAPSULE_MANIFEST_FILE_NAME).is_file());
        assert!(root.join("src/main.py").is_file());

        let cloned = proof.clone();
        assert_eq!(proof, cloned, "equality compares roots, not guards");
        drop(proof);
        assert!(
            root.is_dir(),
            "a cloned handle must keep the extracted root alive"
        );

        drop(cloned);
        assert!(
            !root.exists(),
            "the extracted root is removed with the last handle"
        );
    }

    /// An absolute entry path is refused, not silently re-rooted. `tar`'s own
    /// `unpack_in` strips the leading `/` and writes the file inside `dst`;
    /// this extractor rejects the archive instead. Verified by pointing the
    /// entry at a path in a test-owned sandbox and asserting nothing appears
    /// there.
    #[test]
    fn absolute_entry_path_is_rejected_and_writes_nothing_outside() {
        let sandbox = TempDir::new().unwrap();
        let escape_target = sandbox.path().join("escaped-absolute.txt");
        let dest = sandbox.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let archive = sandbox.path().join("hostile-absolute.tar.zst");

        let body = b"pwned\n".to_vec();
        let mut header = regular_header(body.len() as u64);
        header.set_path_absolute(&escape_target).unwrap();
        write_crafted_archive(&archive, vec![benign_entry(), (header, body)]);

        let err = extract_source_archive(&archive, &dest).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains("is absolute"), "{message}");
        assert!(
            !escape_target.exists(),
            "an absolute entry must never be written outside the extraction root"
        );

        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(&archive),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    /// A `..` traversal entry is refused, not silently skipped. `tar`'s
    /// `unpack_in` returns `Ok(false)` for one — a skip the caller can miss.
    #[test]
    fn parent_dir_traversal_entry_is_rejected_and_writes_nothing_outside() {
        let sandbox = TempDir::new().unwrap();
        let dest = sandbox.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let escape_target = sandbox.path().join("escaped-traversal.txt");
        let archive = sandbox.path().join("hostile-traversal.tar.zst");

        let body = b"pwned\n".to_vec();
        let mut header = regular_header(body.len() as u64);
        // `set_path` would return "paths in archives must not have `..`", so
        // the name field is written directly.
        assert!(header.set_path("../escaped-traversal.txt").is_err());
        set_raw_name(&mut header, b"../escaped-traversal.txt");
        write_crafted_archive(&archive, vec![benign_entry(), (header, body)]);

        let err = extract_source_archive(&archive, &dest).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains("`..` traversal"), "{message}");
        assert!(
            !escape_target.exists(),
            "a `..` entry must never be written outside the extraction root"
        );

        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(&archive),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    /// A symlink entry is rejected by the extractor. A1v2 would reject an
    /// in-tree symlink later anyway, but the extractor must not create the
    /// link in the first place — a symlink on disk is a redirect a *subsequent*
    /// entry in the same archive could be written through.
    #[test]
    fn symlink_entry_is_rejected_before_any_link_is_created() {
        let sandbox = TempDir::new().unwrap();
        let dest = sandbox.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let archive = sandbox.path().join("hostile-symlink.tar.zst");

        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_path("link.txt").unwrap();
        header.set_link_name("/etc/passwd").unwrap();
        write_crafted_archive(&archive, vec![benign_entry(), (header, Vec::new())]);

        let err = extract_source_archive(&archive, &dest).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains("Symlink"), "{message}");
        assert!(
            fs::symlink_metadata(dest.join("link.txt")).is_err(),
            "no symlink may be created before the archive is rejected"
        );

        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(&archive),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    /// The rest of the non-extractable entry classes, one archive each.
    #[test]
    fn hardlink_device_and_fifo_entries_are_rejected() {
        for (label, entry_type, link_name) in [
            ("hardlink", EntryType::Link, Some("capsule.toml")),
            ("fifo", EntryType::Fifo, None),
            ("char device", EntryType::Char, None),
            ("block device", EntryType::Block, None),
            ("gnu sparse", EntryType::GNUSparse, None),
            ("pax global header", EntryType::XGlobalHeader, None),
            ("continuous", EntryType::Continuous, None),
        ] {
            let sandbox = TempDir::new().unwrap();
            let dest = sandbox.path().join("dest");
            fs::create_dir(&dest).unwrap();
            let archive = sandbox.path().join("hostile.tar.zst");

            let mut header = Header::new_gnu();
            header.set_entry_type(entry_type);
            header.set_mode(0o644);
            header.set_size(0);
            header.set_path("hostile-node").unwrap();
            if let Some(link_name) = link_name {
                header.set_link_name(link_name).unwrap();
            }
            write_crafted_archive(&archive, vec![benign_entry(), (header, Vec::new())]);

            let Err(err) = extract_source_archive(&archive, &dest) else {
                panic!("{label} entry must be rejected");
            };
            assert!(
                matches!(err, CapsuleProgramError::NotPinnedMaterialization(_)),
                "{label}: got {err:?}"
            );
            assert!(
                fs::symlink_metadata(dest.join("hostile-node")).is_err(),
                "{label}: no node may be created before the archive is rejected"
            );
        }
    }

    /// Invariant convergence: an archive whose extracted tree carries a
    /// root-level `.git` is rejected by the same shape check the assertion
    /// path runs.
    #[test]
    fn archive_extracting_to_a_root_level_git_is_rejected() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        write_file(tree.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        write_file(tree.path(), ".git/config", b"[core]\n");
        let (_archive_dir, archive) = materialize(tree.path());

        let err = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains(GIT_METADATA_DIR_NAME), "{message}");

        // Same verdict as the assertion path over the same tree.
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::assert_pinned_materialization(tree.path()),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));
    }

    #[test]
    fn missing_or_non_archive_input_is_rejected_cleanly() {
        let sandbox = TempDir::new().unwrap();

        let missing = sandbox.path().join("absent.tar.zst");
        let err = VerifiedPinnedSourceMaterialization::from_source_archive(&missing).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains("does not exist"), "{message}");

        // Not zstd at all.
        let garbage = sandbox.path().join("garbage.tar.zst");
        fs::write(&garbage, b"this is not a zstd frame\n").unwrap();
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(&garbage),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));

        // Valid zstd, but the payload is not a tar stream.
        let not_tar = sandbox.path().join("not-tar.tar.zst");
        fs::write(&not_tar, zstd::encode_all(&b"plain text"[..], 3).unwrap()).unwrap();
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(&not_tar),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));

        // A directory is not an archive.
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::from_source_archive(sandbox.path()),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));

        // An archive that extracts to a tree with no root manifest still mints
        // the pinned proof (that is the projection's job to reject, not the
        // input boundary's) — but it must fail at derivation.
        let no_manifest = TempDir::new().unwrap();
        write_file(no_manifest.path(), "src/main.py", b"print('hi')\n");
        let (_dir, archive) = materialize(no_manifest.path());
        let proof = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap();
        assert!(matches!(
            project_program_source(&proof),
            Err(CapsuleProgramError::SourceProjection(_))
        ));
    }

    /// Control-file exclusion is by removal from the staging copy; the file set
    /// that reaches the digest is exactly "everything but the resolved control
    /// files".
    #[test]
    fn projected_tree_contains_everything_but_the_control_files() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        write_file(tmp.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);

        let projected = StagedCapsuleSource::stage(&pinned(tmp.path()))
            .unwrap()
            .into_projected()
            .unwrap();
        let root = projected.root();

        assert!(!root.join(CAPSULE_MANIFEST_FILE_NAME).exists());
        assert!(!root.join(CAPSULE_LOCK_FILE_NAME).exists());
        assert!(root.join("src/main.py").is_file());
        assert!(root.join("fixtures/ato.lock.json").is_file());
        assert!(root.join("examples/capsule.toml").is_file());
    }
}
